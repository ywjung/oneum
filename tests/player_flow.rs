//! End-to-end checks of the player thread against an offline engine: commands in,
//! snapshots and rendered audio out. No audio device is used.
use oneum::audio::decoder::TrackDecoder;
use oneum::audio::output::{OutputError, OutputSignals};
use oneum::audio::render::Renderer;
use oneum::audio::{Engine, OutputSettings, PlaybackErrorKind, ReplayGainSettings};
use oneum::model::{ReplayGainTags, TrackId};
use oneum::playback::player::{self, MAX_CONSECUTIVE_FAILURES};
use oneum::playback::{
    Command, Notice, PlayState, PlayableTrack, PlayerConfig, PlayerHandle, PlayerHost,
    PlayerSnapshot, QueueItem, QueueState, RepeatMode, Session,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const A: TrackId = 1; // gap_a.wav (1 s)
const B: TrackId = 2; // gap_b.wav (1 s)
const MONO: TrackId = 3; // tone_mono.wav (1 s)
const FLAC_A: TrackId = 4;
const GONE: TrackId = 50; // library believes it exists, file is missing
const OFFLINE: TrackId = 51; // library already knows it is unavailable
const FADE_GUARD: usize = 400;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures")).join(name)
}

fn decode_all(name: &str) -> Vec<f32> {
    let mut dec = TrackDecoder::open(&fixture(name)).expect("fixture opens");
    let (mut all, mut buf) = (Vec::new(), Vec::new());
    while dec.next(&mut buf).expect("fixture decodes") {
        all.extend_from_slice(&buf);
    }
    all
}

#[derive(Default)]
struct TestHost {
    tracks: HashMap<TrackId, PlayableTrack>,
    plays: Mutex<Vec<TrackId>>,
    session: Mutex<Option<Session>>,
    saved: Mutex<Option<Session>>,
    saves: AtomicUsize,
    /// Saves that had to rewrite the queue's items.
    queue_saves: AtomicUsize,
    /// While set, saving the session fails (a full disk, a locked database).
    fail_saves: AtomicBool,
    /// Where long tracks were left off.
    resume: Mutex<HashMap<TrackId, u64>>,
    failed_saves: AtomicUsize,
    /// While set, looking a track up does not return: a stand-in for the player thread
    /// being stuck in a call into a device driver that has hung.
    hang: AtomicBool,
    changes: AtomicUsize,
    /// What the host's "library" has to offer the radio, in order of preference.
    radio_pool: Vec<TrackId>,
    /// Seeds the radio was asked about.
    radio_asked: Mutex<Vec<TrackId>>,
}

impl TestHost {
    fn new() -> Self {
        let mut tracks = HashMap::new();
        let mut add = |id: TrackId, file: &str, available: bool| {
            tracks.insert(
                id,
                PlayableTrack {
                    path: fixture(file),
                    replaygain: ReplayGainTags::default(),
                    duration_ms: 1000,
                    available,
                    sample_rate: 0,
                    bit_depth: None,
                    album_key: 0,
                },
            );
        };
        add(A, "gap_a.wav", true);
        add(B, "gap_b.wav", true);
        add(MONO, "tone_mono.wav", true);
        add(FLAC_A, "gap_a.flac", true);
        add(GONE, "not_there.flac", true);
        add(OFFLINE, "gap_a.wav", false);
        for id in 100..130 {
            add(id, "missing_many.flac", true);
        }
        Self {
            tracks,
            ..Default::default()
        }
    }
    /// Adds a track outside the fixture set.
    fn with_track(mut self, id: TrackId, path: PathBuf, duration_ms: u64) -> Self {
        self.tracks.insert(
            id,
            PlayableTrack {
                path,
                replaygain: ReplayGainTags::default(),
                duration_ms,
                available: true,
                sample_rate: 0,
                bit_depth: None,
                album_key: 0,
            },
        );
        self
    }
    /// Sets what the library knows as the track's sample rate.
    fn with_rate(mut self, id: TrackId, sample_rate: u32) -> Self {
        self.tracks.get_mut(&id).expect("track exists").sample_rate = sample_rate;
        self
    }
    /// Sets what the library knows as the track's bit depth.
    fn with_bits(mut self, id: TrackId, bits: u8) -> Self {
        self.tracks.get_mut(&id).expect("track exists").bit_depth = Some(bits);
        self
    }
    fn plays(&self) -> Vec<TrackId> {
        self.plays.lock().expect("lock").clone()
    }
}

impl PlayerHost for TestHost {
    fn track(&self, id: TrackId) -> Option<PlayableTrack> {
        while self.hang.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(2));
        }
        self.tracks.get(&id).cloned()
    }
    fn record_play(&self, id: TrackId) {
        self.plays.lock().expect("lock").push(id);
    }
    fn radio_picks(&self, seed: TrackId, exclude: &[TrackId], count: usize) -> Vec<TrackId> {
        self.radio_asked.lock().expect("lock").push(seed);
        self.radio_pool
            .iter()
            .copied()
            .filter(|id| !exclude.contains(id))
            .take(count)
            .collect()
    }
    fn load_session(&self) -> Option<Session> {
        self.session.lock().expect("lock").clone()
    }
    fn resume_point(&self, id: TrackId) -> Option<u64> {
        self.resume.lock().expect("lock").get(&id).copied()
    }
    fn set_resume_point(&self, id: TrackId, position_ms: Option<u64>) {
        let mut resume = self.resume.lock().expect("lock");
        match position_ms {
            Some(ms) => resume.insert(id, ms),
            None => resume.remove(&id),
        };
    }
    fn save_session(&self, session: &Session, queue_changed: bool) -> bool {
        if self.fail_saves.load(Ordering::Relaxed) {
            self.failed_saves.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        *self.saved.lock().expect("lock") = Some(session.clone());
        self.saves.fetch_add(1, Ordering::Relaxed);
        if queue_changed {
            self.queue_saves.fetch_add(1, Ordering::Relaxed);
        }
        true
    }
    fn changed(&self) {
        self.changes.fetch_add(1, Ordering::Relaxed);
    }
}

/// Rendered audio grouped by feed epoch, in the order it was heard.
type HeardAudio = Arc<Mutex<Vec<(u16, Vec<f32>)>>>;

/// Stands in for the audio device: pulls from the renderer on its own thread, about six
/// times faster than real time, and records what was consumed per feed.
struct Rig {
    player: PlayerHandle,
    host: Arc<TestHost>,
    audio: HeardAudio,
    stop: Arc<AtomicBool>,
    /// Stands in for the error callback of the device stream.
    signals: Arc<OutputSignals>,
    /// Sleep between rendered blocks, in ms. 2 = about six times real time, 12 = real time.
    pace_ms: Arc<AtomicU64>,
    /// While set, the "device" does not ask for audio at all.
    frozen: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Rig {
    fn start(host: TestHost) -> Self {
        Self::start_with(host, Duration::from_secs(15))
    }

    fn start_with(host: TestHost, stall_timeout: Duration) -> Self {
        Self::start_configured(host, stall_timeout, |_| {})
    }

    /// `configure` sets the offline engine up before the player takes it.
    fn start_configured(
        host: TestHost,
        stall_timeout: Duration,
        configure: impl FnOnce(&mut Engine) + Send + 'static,
    ) -> Self {
        let host = Arc::new(host);
        let (renderer_tx, renderer_rx) =
            std::sync::mpsc::channel::<(Renderer, Arc<OutputSignals>)>();
        let (player, player_thread) = player::spawn_with_engine(
            host.clone(),
            PlayerConfig {
                default_volume: 1.0,
                stall_timeout,
                ..Default::default()
            },
            move |volume| {
                let (mut engine, renderer) =
                    Engine::new_offline(44_100, volume, ReplayGainSettings::default());
                configure(&mut engine);
                renderer_tx
                    .send((renderer, engine.output_signals()))
                    .expect("test is alive");
                engine
            },
        );
        let (mut renderer, signals) = renderer_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("engine created");
        let pace_ms = Arc::new(AtomicU64::new(2));
        let frozen = Arc::new(AtomicBool::new(false));
        let (pace2, frozen2) = (pace_ms.clone(), frozen.clone());
        let audio: HeardAudio = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (audio2, stop2) = (audio.clone(), stop.clone());
        let pump = std::thread::spawn(move || {
            const BLOCK: usize = 512;
            let mut out = vec![0f32; BLOCK * 2];
            while !stop2.load(Ordering::Relaxed) {
                if frozen2.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                let (epoch_before, before) = renderer.shared().position();
                renderer.render(&mut out, 2);
                let (epoch_after, after) = renderer.shared().position();
                let (consumed, tail) = if epoch_after == epoch_before {
                    ((after.saturating_sub(before) as usize).min(BLOCK), false)
                } else {
                    ((after as usize).min(BLOCK), true)
                };
                if consumed > 0 {
                    let data = if tail {
                        &out[(BLOCK - consumed) * 2..]
                    } else {
                        &out[..consumed * 2]
                    };
                    let mut audio = audio2.lock().expect("lock");
                    match audio.last_mut() {
                        Some((epoch, pcm)) if *epoch == epoch_after => pcm.extend_from_slice(data),
                        _ => audio.push((epoch_after, data.to_vec())),
                    }
                }
                std::thread::sleep(Duration::from_millis(pace2.load(Ordering::Relaxed)));
            }
        });
        Self {
            player,
            host,
            audio,
            stop,
            signals,
            pace_ms,
            frozen,
            threads: vec![player_thread, pump],
        }
    }

    fn send(&self, command: Command) {
        self.player.send(command);
    }

    fn wait(&self, what: &str, cond: impl Fn(&PlayerSnapshot) -> bool) -> PlayerSnapshot {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let snap = self.player.snapshot();
            if cond(&snap) {
                return snap;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; last snapshot: {snap:?}"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Waits for something that is not part of the snapshot (play counts, saves).
    fn wait_host(&self, what: &str, cond: impl Fn(&TestHost) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !cond(&self.host) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn feeds(&self) -> usize {
        self.audio.lock().expect("lock").len()
    }

    fn track(snap: &PlayerSnapshot) -> Option<TrackId> {
        snap.current.map(|c| c.track_id)
    }

    /// Audio of the most recent feed.
    fn last_feed_audio(&self) -> Vec<f32> {
        self.audio
            .lock()
            .expect("lock")
            .last()
            .map(|(_, pcm)| pcm.clone())
            .unwrap_or_default()
    }

    fn total_frames(&self) -> usize {
        self.audio
            .lock()
            .expect("lock")
            .iter()
            .map(|(_, pcm)| pcm.len() / 2)
            .sum()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.player.send(Command::Shutdown);
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

fn play(tracks: &[TrackId], start_index: usize) -> Command {
    Command::PlayTracks {
        tracks: tracks.to_vec(),
        start_index,
    }
}

fn max_abs_diff(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "length mismatch");
    a.iter().zip(b).fold(0f32, |m, (x, y)| m.max((x - y).abs()))
}

#[test]
fn plays_a_queue_to_the_end_gaplessly_and_counts_plays() {
    let rig = Rig::start(TestHost::new());
    assert_eq!(rig.player.snapshot().state, PlayState::Idle);
    rig.send(play(&[A, B], 0));
    rig.wait("first track", |s| {
        Rig::track(s) == Some(A) && s.state == PlayState::Playing
    });
    rig.wait("second track", |s| Rig::track(s) == Some(B));
    let ended = rig.wait("end of queue", |s| s.state == PlayState::Ended);
    assert_eq!(Rig::track(&ended), Some(B), "last item stays current");
    assert_eq!(ended.queue.len(), 2, "queue is kept");
    assert_eq!(rig.host.plays(), vec![A, B]);

    // One feed, both tracks, sample-identical to the uncut signal across the boundary.
    let audio = rig.audio.lock().expect("lock").clone();
    assert_eq!(audio.len(), 1, "the transition must not start a new feed");
    let expected: Vec<f32> = decode_all("gap_a.wav")
        .into_iter()
        .chain(decode_all("gap_b.wav"))
        .collect();
    assert_eq!(audio[0].1.len(), expected.len());
    assert_eq!(
        max_abs_diff(&audio[0].1[FADE_GUARD * 2..], &expected[FADE_GUARD * 2..]),
        0.0
    );
}

#[test]
fn play_after_the_end_restarts_from_the_first_item() {
    let rig = Rig::start(TestHost::new());
    rig.send(play(&[A, B], 1));
    rig.wait("end", |s| s.state == PlayState::Ended);
    rig.send(Command::Play);
    rig.wait("restart at first item", |s| {
        Rig::track(s) == Some(A) && s.state == PlayState::Playing
    });
}

#[test]
fn pause_resume_next_previous_and_seek() {
    let rig = Rig::start(TestHost::new());
    rig.send(play(&[A, B, MONO], 0));
    rig.wait("playing", |s| s.state == PlayState::Playing);
    rig.send(Command::Pause);
    let paused = rig.wait("paused", |s| s.state == PlayState::Paused);
    let frames = rig.total_frames();
    std::thread::sleep(Duration::from_millis(60));
    assert!(
        rig.total_frames() <= frames + 600,
        "no audio is consumed while paused"
    );
    assert_eq!(Rig::track(&paused), Some(A));

    // Next while paused prepares the next track and stays paused.
    rig.send(Command::Next);
    let next = rig.wait("next prepared", |s| Rig::track(s) == Some(B));
    assert_eq!(next.state, PlayState::Paused);
    assert_eq!(next.position_ms, 0);

    // Seeking while prepared just moves the start position.
    rig.send(Command::SeekTo(700));
    rig.wait("seek stored", |s| s.position_ms == 700);
    rig.send(Command::Play);
    rig.wait("playing B", |s| {
        s.state == PlayState::Playing && Rig::track(s) == Some(B)
    });
    rig.send(Command::Pause);
    let s = rig.wait("paused again", |s| s.state == PlayState::Paused);
    assert!(
        s.position_ms >= 700,
        "resumed from the seek position, got {}",
        s.position_ms
    );
    // B was started at 700 ms: the feed's audio matches the source from there.
    let heard = rig.last_feed_audio();
    let source = decode_all("gap_b.wav");
    let from = 30_870 * 2; // 700 ms at 44.1 kHz
    // Skip the fade-in at the start and the fade-out of the pause at the end.
    let end = heard.len() - FADE_GUARD * 2;
    assert!(end > FADE_GUARD * 2 + 2000);
    assert_eq!(
        max_abs_diff(
            &heard[FADE_GUARD * 2..end],
            &source[from + FADE_GUARD * 2..from + end]
        ),
        0.0
    );

    // Previous early in a track goes back one item; later it restarts the track.
    rig.send(Command::SeekTo(100));
    rig.wait("seek to 100", |s| s.position_ms == 100);
    rig.send(Command::Previous);
    rig.wait("previous item", |s| Rig::track(s) == Some(A));
    rig.send(Command::SeekTo(900));
    rig.wait("seek to 900", |s| s.position_ms == 900);
    rig.send(Command::SeekTo(5000));
    rig.wait("seek clamps to the duration", |s| s.position_ms == 1000);
    rig.send(Command::Previous);
    let s = rig.wait("restart", |s| s.position_ms == 0);
    assert_eq!(
        Rig::track(&s),
        Some(A),
        "previous after 3 s… here after >3 s equivalent: restart same track"
    );
}

#[test]
fn previous_restarts_only_after_three_seconds() {
    let rig = Rig::start(TestHost::new());
    rig.send(play(&[A, B], 1));
    rig.wait("playing", |s| s.state == PlayState::Playing);
    rig.send(Command::Pause);
    rig.wait("paused", |s| s.state == PlayState::Paused);
    // Under three seconds: go to the previous item.
    rig.send(Command::Previous);
    rig.wait("previous item", |s| Rig::track(s) == Some(A));
}

#[test]
fn next_at_the_end_stops_and_keeps_the_queue() {
    let rig = Rig::start(TestHost::new());
    rig.send(play(&[A, B], 1));
    rig.wait("playing", |s| s.state == PlayState::Playing);
    rig.send(Command::Next);
    let s = rig.wait("ended", |s| s.state == PlayState::Ended);
    assert_eq!(s.queue.len(), 2);
    // With repeat-all the same command wraps around instead.
    rig.send(Command::SetRepeat(RepeatMode::All));
    rig.send(play(&[A, B], 1));
    rig.wait("playing", |s| {
        s.state == PlayState::Playing && Rig::track(s) == Some(B)
    });
    rig.send(Command::Next);
    rig.wait("wrapped", |s| {
        Rig::track(s) == Some(A) && s.state == PlayState::Playing
    });
}

#[test]
fn repeat_one_loops_gaplessly_until_the_user_moves_on() {
    let rig = Rig::start(TestHost::new());
    rig.send(Command::SetRepeat(RepeatMode::One));
    rig.send(play(&[FLAC_A, B], 0));
    rig.wait("playing", |s| s.state == PlayState::Playing);
    let deadline = Instant::now() + Duration::from_secs(10);
    while rig.total_frames() < 44_100 * 3 + 5000 {
        assert!(Instant::now() < deadline, "loop did not keep playing");
        std::thread::sleep(Duration::from_millis(5));
    }
    let snap = rig.player.snapshot();
    assert_eq!(Rig::track(&snap), Some(FLAC_A));
    assert!(rig.host.plays().iter().filter(|t| **t == FLAC_A).count() >= 3);
    // The loop is one continuous feed: three passes of the same samples back to back.
    let audio = rig.audio.lock().expect("lock").clone();
    assert_eq!(audio.len(), 1);
    let once = decode_all("gap_a.flac");
    let three: Vec<f32> = once
        .iter()
        .chain(once.iter())
        .chain(once.iter())
        .copied()
        .collect();
    assert_eq!(
        max_abs_diff(
            &audio[0].1[FADE_GUARD * 2..three.len()],
            &three[FADE_GUARD * 2..]
        ),
        0.0
    );
    // Manual next leaves the loop but keeps the mode.
    rig.send(Command::Next);
    let s = rig.wait("moved on", |s| Rig::track(s) == Some(B));
    assert_eq!(s.repeat, RepeatMode::One);
}

#[test]
fn unplayable_tracks_are_skipped_with_a_notice() {
    let rig = Rig::start(TestHost::new());
    // GONE fails when opened by the decode thread, OFFLINE is rejected up front.
    rig.send(play(&[GONE, OFFLINE, A], 0));
    let s = rig.wait("skips to the playable track", |s| {
        Rig::track(s) == Some(A) && s.state == PlayState::Playing
    });
    assert!(s.notice_seq >= 2);
    assert!(matches!(
        s.notice,
        Some(Notice::TrackSkipped {
            track_id: OFFLINE,
            kind: PlaybackErrorKind::FileMissing
        })
    ));
    rig.wait("ended", |s| s.state == PlayState::Ended);
    assert_eq!(rig.host.plays(), vec![A]);
}

#[test]
fn a_broken_track_in_the_middle_is_skipped_after_the_current_one_finishes() {
    let rig = Rig::start(TestHost::new());
    rig.send(play(&[A, GONE, B], 0));
    rig.wait("first", |s| {
        Rig::track(s) == Some(A) && s.state == PlayState::Playing
    });
    let s = rig.wait("third", |s| {
        Rig::track(s) == Some(B) && s.state == PlayState::Playing
    });
    assert!(matches!(
        s.notice,
        Some(Notice::TrackSkipped { track_id: GONE, .. })
    ));
    rig.wait("ended", |s| s.state == PlayState::Ended);
    assert_eq!(rig.host.plays(), vec![A, B]);
    // A played in full before the skip.
    let audio = rig.audio.lock().expect("lock").clone();
    assert_eq!(audio[0].1.len() / 2, 44_100);
}

#[test]
fn many_failures_in_a_row_stop_playback() {
    let rig = Rig::start(TestHost::new());
    let all: Vec<TrackId> = (100..130).collect();
    rig.send(play(&all, 0));
    let s = rig.wait("gives up", |s| s.state == PlayState::Error);
    assert_eq!(s.notice, Some(Notice::TooManyFailures));
    assert!(
        s.queue_index
            .is_some_and(|i| i + 1 >= MAX_CONSECUTIVE_FAILURES && i < 30)
    );
    assert_eq!(s.queue.len(), 30, "queue is kept");
    assert!(rig.host.plays().is_empty());
}

#[test]
fn queue_editing_while_playing() {
    let rig = Rig::start(TestHost::new());
    rig.send(play(&[A], 0));
    rig.wait("playing", |s| s.state == PlayState::Playing);
    rig.send(Command::Pause);
    rig.wait("paused", |s| s.state == PlayState::Paused);
    rig.send(Command::Enqueue(vec![B, B]));
    rig.send(Command::PlayNext(vec![MONO]));
    let s = rig.wait("queue updated", |s| s.queue.len() == 4);
    let order: Vec<TrackId> = s.queue.iter().map(|i| i.track).collect();
    assert_eq!(order, vec![A, MONO, B, B]);
    assert_eq!(s.queue_index, Some(0));
    let ids: Vec<u64> = s.queue.iter().map(|i| i.id).collect();
    assert_ne!(ids[2], ids[3], "the same track twice is two items");

    // Move the last item to the front; the current item is unchanged.
    rig.send(Command::MoveItems {
        ids: vec![ids[3]],
        to_index: 0,
    });
    let s = rig.wait("moved", |s| s.queue.first().map(|i| i.id) == Some(ids[3]));
    assert_eq!(Rig::track(&s), Some(A));
    assert_eq!(s.queue_index, Some(1));

    // Removing the current item while paused prepares the following one, still paused.
    rig.send(Command::RemoveItems(vec![ids[0]]));
    let s = rig.wait("current removed", |s| {
        s.queue.len() == 3 && Rig::track(s) == Some(MONO)
    });
    assert_eq!(s.state, PlayState::Paused);

    // Removing the current item while playing continues with the next one.
    rig.send(Command::Play);
    rig.wait("playing mono", |s| {
        s.state == PlayState::Playing && Rig::track(s) == Some(MONO)
    });
    rig.send(Command::RemoveItems(vec![ids[1]]));
    rig.wait("continues with B", |s| {
        Rig::track(s) == Some(B) && s.state == PlayState::Playing
    });

    rig.send(Command::ClearQueue);
    let s = rig.wait("idle", |s| s.state == PlayState::Idle);
    assert!(s.queue.is_empty() && s.current.is_none());
}

#[test]
fn removing_the_last_playing_item_stops() {
    let rig = Rig::start(TestHost::new());
    rig.send(play(&[A, B], 1));
    let s = rig.wait("playing", |s| s.state == PlayState::Playing);
    let id = s.current.expect("current").item_id;
    rig.send(Command::RemoveItems(vec![id]));
    let s = rig.wait("idle", |s| s.state == PlayState::Idle);
    assert!(s.current.is_none());
    assert_eq!(s.queue.len(), 1);
}

#[test]
fn shuffle_toggle_keeps_the_current_track_and_restores_order() {
    let rig = Rig::start(TestHost::new());
    let many: Vec<TrackId> = [A, B, MONO, FLAC_A]
        .iter()
        .cycle()
        .take(40)
        .copied()
        .collect();
    rig.send(play(&many, 10));
    rig.wait("playing", |s| s.state == PlayState::Playing);
    rig.send(Command::Pause);
    let before = rig.wait("paused", |s| s.state == PlayState::Paused);
    let original: Vec<u64> = before.queue.iter().map(|i| i.id).collect();
    let current = before.current.expect("current").item_id;
    rig.send(Command::SetShuffle(true));
    let shuffled = rig.wait("shuffled", |s| s.shuffle);
    assert_eq!(shuffled.queue_index, Some(0));
    assert_eq!(shuffled.current.expect("current").item_id, current);
    assert_ne!(
        shuffled.queue.iter().map(|i| i.id).collect::<Vec<_>>(),
        original
    );
    rig.send(Command::SetShuffle(false));
    let restored = rig.wait("unshuffled", |s| !s.shuffle);
    assert_eq!(
        restored.queue.iter().map(|i| i.id).collect::<Vec<_>>(),
        original
    );
    assert_eq!(restored.current.expect("current").item_id, current);
}

#[test]
fn session_is_restored_paused_and_resumes_from_the_saved_position() {
    let host = TestHost::new();
    *host.session.lock().expect("lock") = Some(Session {
        queue: QueueState {
            items: vec![
                QueueItem { id: 7, track: A },
                QueueItem { id: 8, track: B },
                QueueItem { id: 9, track: 9999 },
            ],
            current: Some(1),
            shuffle: false,
            repeat: RepeatMode::All,
            unshuffled: vec![],
        },
        position_ms: 400,
        volume: 0.5,
    });
    let rig = Rig::start(host);
    let s = rig.wait("restored", |s| s.current.is_some());
    assert_eq!(s.state, PlayState::Paused, "never auto-plays on launch");
    assert_eq!(Rig::track(&s), Some(B));
    assert_eq!(s.position_ms, 400);
    assert_eq!(s.volume, 0.5);
    assert_eq!(s.repeat, RepeatMode::All);
    assert_eq!(s.queue.len(), 2, "tracks that left the library are dropped");
    std::thread::sleep(Duration::from_millis(80));
    assert_eq!(rig.total_frames(), 0, "silent until the user presses play");

    rig.send(Command::Play);
    rig.wait("playing", |s| s.state == PlayState::Playing);
    rig.send(Command::Pause);
    rig.wait("paused", |s| s.state == PlayState::Paused);
    let heard = rig.last_feed_audio();
    let source = decode_all("gap_b.wav");
    let from = 17_640 * 2; // 400 ms
    let n = heard.len().min(source.len() - from) - FADE_GUARD * 2; // minus the pause fade-out
    assert!(n > FADE_GUARD * 2 + 2000);
    // Volume 0.5 → gain 0.125 on the cubic curve.
    let expected: Vec<f32> = source[from..from + n].iter().map(|s| s * 0.125).collect();
    assert!(max_abs_diff(&heard[FADE_GUARD * 2..n], &expected[FADE_GUARD * 2..]) < 1e-6);
}

#[test]
fn session_is_saved_on_pause_and_shutdown() {
    let host = TestHost::new();
    let rig = Rig::start(host);
    rig.send(Command::SetVolume(0.3));
    rig.send(Command::SetShuffle(false));
    rig.send(play(&[A, B, MONO], 1));
    rig.wait("playing", |s| s.state == PlayState::Playing);
    rig.send(Command::Pause);
    rig.wait("paused", |s| s.state == PlayState::Paused);
    let host = rig.host.clone();
    drop(rig);
    let saved = host
        .saved
        .lock()
        .expect("lock")
        .clone()
        .expect("session saved");
    assert_eq!(
        saved
            .queue
            .items
            .iter()
            .map(|i| i.track)
            .collect::<Vec<_>>(),
        vec![A, B, MONO]
    );
    assert_eq!(saved.queue.current, Some(1));
    assert_eq!(saved.volume, 0.3);
    assert!(saved.position_ms > 0 && saved.position_ms <= 1000);
}

#[test]
fn rapid_seeks_and_track_changes_end_in_the_last_requested_state() {
    let rig = Rig::start(TestHost::new());
    rig.send(play(&[A, B, MONO], 0));
    rig.wait("playing", |s| s.state == PlayState::Playing);
    rig.send(Command::Pause);
    rig.wait("paused", |s| s.state == PlayState::Paused);
    rig.send(Command::Play);
    for i in 0..40u64 {
        match i % 4 {
            0 => rig.send(Command::Next),
            1 => rig.send(Command::SeekTo(i * 10)),
            2 => rig.send(Command::Previous),
            _ => rig.send(Command::SeekTo(900 - i * 10)),
        }
        if i % 7 == 0 {
            std::thread::sleep(Duration::from_millis(3));
        }
    }
    rig.send(play(&[B], 0));
    rig.send(Command::SeekTo(500));
    rig.send(Command::Pause);
    let s = rig.wait("settled", |s| {
        s.state == PlayState::Paused && Rig::track(s) == Some(B) && s.queue.len() == 1
    });
    assert!(s.position_ms >= 500, "position {}", s.position_ms);
    // Nothing of the final request has been heard yet (it was paused right away).
    // Resuming must play B from 500 ms — never audio of an earlier request.
    rig.send(Command::Play);
    rig.wait("playing", |s| s.state == PlayState::Playing);
    std::thread::sleep(Duration::from_millis(40));
    rig.send(Command::Pause);
    rig.wait("paused", |s| s.state == PlayState::Paused);
    let heard = rig.last_feed_audio();
    let source = decode_all("gap_b.wav");
    let from = 22_050 * 2;
    let n = heard.len().min(source.len() - from) - FADE_GUARD * 2;
    assert!(n > FADE_GUARD * 2 + 512, "heard {} samples", heard.len());
    assert_eq!(
        max_abs_diff(
            &heard[FADE_GUARD * 2..n],
            &source[from + FADE_GUARD * 2..from + n]
        ),
        0.0
    );
}

#[test]
fn random_command_storm_keeps_invariants() {
    let rig = Rig::start(TestHost::new());
    let mut rng = fastrand::Rng::with_seed(20261111);
    let pool = [A, B, MONO, FLAC_A, GONE, OFFLINE];
    for step in 0..400 {
        let pick = |rng: &mut fastrand::Rng| {
            (0..rng.usize(1..5))
                .map(|_| pool[rng.usize(..pool.len())])
                .collect::<Vec<_>>()
        };
        let snap = rig.player.snapshot();
        let some_ids: Vec<u64> = snap
            .queue
            .iter()
            .filter(|_| rng.bool())
            .take(3)
            .map(|i| i.id)
            .collect();
        let command = match rng.usize(..16) {
            0 => {
                let tracks = pick(&mut rng);
                let start_index = rng.usize(..tracks.len());
                Command::PlayTracks {
                    tracks,
                    start_index,
                }
            }
            1 => Command::PlayNext(pick(&mut rng)),
            2 => Command::Enqueue(pick(&mut rng)),
            3 => Command::RemoveItems(some_ids),
            4 => Command::MoveItems {
                ids: some_ids,
                to_index: rng.usize(..8),
            },
            5 => Command::Next,
            6 => Command::Previous,
            7 => Command::SeekTo(rng.u64(..1500)),
            8 => Command::TogglePlayPause,
            9 => Command::SetShuffle(rng.bool()),
            10 => Command::SetRepeat(
                [RepeatMode::Off, RepeatMode::All, RepeatMode::One][rng.usize(..3)],
            ),
            11 => Command::SeekRelative(rng.i64(-800..800)),
            12 => snap
                .queue
                .first()
                .map_or(Command::Play, |i| Command::JumpTo(i.id)),
            13 => Command::Play,
            14 => Command::LibraryChanged,
            _ => Command::SetVolume(rng.f32()),
        };
        rig.send(command);
        if step % 3 == 0 {
            std::thread::sleep(Duration::from_millis(rng.u64(0..6)));
        }
        let s = rig.player.snapshot();
        match (s.queue_index, s.current) {
            (Some(i), Some(c)) => assert_eq!(s.queue.get(i).map(|q| q.id), Some(c.item_id)),
            (None, None) => {}
            other => panic!("inconsistent snapshot: {other:?}"),
        }
        if s.queue.is_empty() {
            assert!(matches!(
                s.state,
                PlayState::Idle | PlayState::Ended | PlayState::Error | PlayState::Paused
            ));
        }
    }
    // The player is still responsive afterwards.
    rig.send(Command::SetRepeat(RepeatMode::Off));
    rig.send(Command::SetShuffle(false));
    rig.send(play(&[A], 0));
    rig.wait("still works", |s| {
        Rig::track(s) == Some(A) && s.state == PlayState::Playing
    });
    rig.wait("ends", |s| s.state == PlayState::Ended);
}

// ----- long tracks, play counts, devices and stalls ---------------------------------------

const LONG: TrackId = 60; // generated, 12 s
const STUCK: TrackId = 61; // a path whose open() never returns

/// Writes a quiet 16-bit stereo 44.1 kHz WAV of `seconds` and returns its path.
fn write_long_wav(dir: &std::path::Path, seconds: u32) -> PathBuf {
    let frames = 44_100 * seconds;
    let data_len = frames * 4;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&44_100u32.to_le_bytes());
    bytes.extend_from_slice(&(44_100u32 * 4).to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for n in 0..frames {
        let v = ((f64::from(n) * 440.0 * std::f64::consts::TAU / 44_100.0).sin() * 3000.0) as i16;
        bytes.extend_from_slice(&v.to_le_bytes());
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    let path = dir.join("long.wav");
    std::fs::write(&path, bytes).expect("write long fixture");
    path
}

/// A silent 21-minute WAV (8 kHz mono, so that it is small): a "long track".
fn write_21_minute_wav(dir: &std::path::Path) -> PathBuf {
    let frames: u32 = 21 * 60 * 8_000;
    let data_len = frames * 2;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&8_000u32.to_le_bytes());
    bytes.extend_from_slice(&16_000u32.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.resize(44 + data_len as usize, 0);
    let path = dir.join("long21.wav");
    std::fs::write(&path, bytes).expect("write");
    path
}

/// "Resume long tracks": a long track chosen again starts where it was left; with the
/// setting off, or after "previous" restarts it, from the beginning.
#[test]
fn a_long_track_starts_where_it_was_left_off() {
    const MIX: TrackId = 901;
    let dir = tempfile::tempdir().expect("tempdir");
    let host = TestHost::new().with_track(MIX, write_21_minute_wav(dir.path()), 21 * 60_000);
    let rig = Rig::start(host);
    rig.send(Command::SetResumeLongTracks(true));
    rig.send(play(&[MIX, A], 0));
    rig.wait("the long track plays", |s| {
        Rig::track(s) == Some(MIX) && s.state == PlayState::Playing
    });
    rig.send(Command::SeekTo(300_000));
    rig.wait("seeked", |s| {
        s.state == PlayState::Playing && s.position_ms >= 300_000
    });
    rig.send(Command::Next);
    rig.wait("the next track", |s| Rig::track(s) == Some(A));
    let saved = rig.host.resume_point(MIX).expect("remembered");
    assert!((300_000..310_000).contains(&saved), "{saved}");
    // Chosen again: it starts there.
    rig.send(play(&[MIX], 0));
    let s = rig.wait("resumed", |s| {
        Rig::track(s) == Some(MIX) && s.state == PlayState::Playing
    });
    assert!(s.position_ms >= saved, "{}", s.position_ms);
    // "Previous" after three seconds restarts it from the beginning, as always, and
    // the place it was left at is forgotten (started over on purpose).
    rig.send(Command::Previous);
    rig.wait("restarted", |s| {
        s.state == PlayState::Playing && s.position_ms < 10_000
    });
    assert_eq!(
        rig.host.resume_point(MIX),
        None,
        "forgotten when started over"
    );
    // Off: from the beginning.
    rig.send(Command::SeekTo(400_000));
    rig.wait("seeked again", |s| s.position_ms >= 400_000);
    rig.send(Command::SetResumeLongTracks(false));
    rig.send(play(&[A, MIX], 1));
    let s = rig.wait("from the start", |s| {
        Rig::track(s) == Some(MIX) && s.state == PlayState::Playing
    });
    assert!(s.position_ms < 10_000, "{}", s.position_ms);
}

/// A long track heard to its end as the queue runs out starts from the beginning next
/// time, also when no position was saved in its last minute (a seek into the last
/// seconds, between two of the regular saves).
#[test]
fn a_long_track_heard_to_the_end_of_the_queue_forgets_where_it_was() {
    const MIX: TrackId = 902;
    let dir = tempfile::tempdir().expect("tempdir");
    let host = TestHost::new().with_track(MIX, write_21_minute_wav(dir.path()), 21 * 60_000);
    host.resume.lock().expect("lock").insert(MIX, 600_000);
    let rig = Rig::start(host);
    rig.send(Command::SetResumeLongTracks(true));
    rig.send(play(&[MIX], 0));
    rig.wait("it starts where it was left", |s| {
        Rig::track(s) == Some(MIX) && s.state == PlayState::Playing && s.position_ms >= 600_000
    });
    // The save that follows the new queue (a second later) remembers the place again.
    std::thread::sleep(Duration::from_millis(1_500));
    let saved = rig.host.resume_point(MIX).expect("remembered");
    assert!(saved >= 600_000, "{saved}");
    rig.send(Command::SeekTo(21 * 60_000 - 1_500));
    rig.wait("the queue is over", |s| s.state == PlayState::Ended);
    assert_eq!(rig.host.resume_point(MIX), None, "heard to the end");
}

/// Shuffle by album through the player: albums stay whole.
#[test]
fn shuffling_by_album_through_the_player() {
    let mut host = TestHost::new();
    for id in 100..130 {
        host.tracks.get_mut(&id).expect("track").album_key = (id / 10) as u64;
    }
    let rig = Rig::start(host);
    rig.send(Command::SetShuffleByAlbum(true));
    rig.send(Command::Enqueue((100..130).collect()));
    rig.send(Command::SetShuffle(true));
    let s = rig.wait("shuffled", |s| s.shuffle && s.queue.len() == 30);
    let order: Vec<TrackId> = s.queue.iter().map(|i| i.track).collect();
    for album in order.chunks(10) {
        assert!(album.windows(2).all(|w| w[1] == w[0] + 1), "{order:?}");
    }
}

fn long_host(dir: &tempfile::TempDir) -> TestHost {
    TestHost::new().with_track(LONG, write_long_wav(dir.path(), 12), 12_000)
}

#[test]
fn a_playthrough_counts_once_however_often_it_is_refed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rig = Rig::start(long_host(&dir));
    rig.send(play(&[LONG], 0));
    // Half of the track heard: the play is counted.
    rig.wait_host("first credit", |h| h.plays() == vec![LONG]);
    // Each of these replaces the feed; the position passes the half-way mark again.
    rig.send(Command::SeekTo(5_000));
    rig.wait("seek", |s| {
        s.state == PlayState::Playing && s.position_ms >= 5_000
    });
    rig.send(Command::Pause);
    rig.wait("paused", |s| s.state == PlayState::Paused);
    rig.send(Command::SeekTo(5_500));
    rig.send(Command::Play);
    rig.send(Command::Enqueue(vec![A]));
    rig.send(Command::SetShuffle(true));
    rig.send(Command::SetShuffle(false));
    // Previous after three seconds restarts the track: still the same playthrough.
    rig.send(Command::Previous);
    rig.wait("restarted", |s| {
        s.state == PlayState::Playing && Rig::track(s) == Some(LONG) && s.position_ms < 4_000
    });
    rig.wait("the queued track follows", |s| Rig::track(s) == Some(A));
    rig.wait("end", |s| s.state == PlayState::Ended);
    assert_eq!(rig.host.plays(), vec![LONG, A]);
}

#[test]
fn skipping_through_a_track_is_not_a_play_but_choosing_it_again_is() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rig = Rig::start(long_host(&dir));
    rig.send(play(&[LONG], 0));
    rig.wait("playing", |s| s.state == PlayState::Playing);
    // Jump to the last second: the track "finishes" with one second heard out of twelve.
    rig.send(Command::SeekTo(11_000));
    rig.wait("end", |s| s.state == PlayState::Ended);
    assert_eq!(rig.host.plays(), Vec::<TrackId>::new());

    // Play again from the top and listen to more than half.
    rig.send(Command::Play);
    rig.wait_host("credit", |h| h.plays() == vec![LONG]);
    // Starting the same track once more is a new playthrough and counts again.
    let item = rig.player.snapshot().queue[0].id;
    rig.send(Command::JumpTo(item));
    rig.wait_host("second credit", |h| h.plays() == vec![LONG, LONG]);
}

#[test]
fn losing_the_device_pauses_and_retry_continues_from_there() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rig = Rig::start(long_host(&dir));
    rig.send(play(&[LONG, A], 0));
    rig.wait("playing", |s| {
        s.state == PlayState::Playing && s.position_ms >= 500
    });
    rig.signals.signal_lost();
    let lost = rig.wait("paused by the lost device", |s| s.device_lost);
    assert_eq!(lost.state, PlayState::Paused);
    assert_eq!(lost.notice, Some(Notice::DeviceLost));
    assert_eq!(Rig::track(&lost), Some(LONG));
    assert!(
        lost.position_ms >= 500,
        "position kept: {}",
        lost.position_ms
    );
    let frames = rig.total_frames();
    std::thread::sleep(Duration::from_millis(80));
    assert!(
        rig.total_frames() <= frames + 600,
        "nothing plays meanwhile"
    );
    assert_eq!(
        rig.host
            .saved
            .lock()
            .expect("lock")
            .as_ref()
            .map(|s| s.position_ms),
        Some(lost.position_ms),
        "the position is saved when the device goes away"
    );

    rig.send(Command::RetryOutput);
    let back = rig.wait("playing again", |s| s.state == PlayState::Playing);
    assert!(!back.device_lost);
    assert_eq!(Rig::track(&back), Some(LONG));
    assert!(back.position_ms >= lost.position_ms);
}

#[test]
fn an_invalidated_stream_or_a_device_change_continues_at_the_same_position() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rig = Rig::start(long_host(&dir));
    rig.send(play(&[LONG], 0));
    let before = rig.wait("playing", |s| {
        s.state == PlayState::Playing && s.position_ms >= 500
    });
    assert_eq!(rig.feeds(), 1);

    // The stream has to be rebuilt (sample-rate change, default device moved).
    rig.signals.signal_invalidated();
    let deadline = Instant::now() + Duration::from_secs(10);
    while rig.feeds() < 2 {
        assert!(Instant::now() < deadline, "no new feed after invalidation");
        std::thread::sleep(Duration::from_millis(2));
    }
    let after = rig.wait("playing on the new stream", |s| {
        s.state == PlayState::Playing
    });
    assert!(!after.device_lost);
    assert_eq!(Rig::track(&after), Some(LONG));
    assert!(after.position_ms >= before.position_ms);

    // Choosing another device does the same.
    rig.send(Command::SetOutputDevice(None));
    let deadline = Instant::now() + Duration::from_secs(10);
    while rig.feeds() < 3 {
        assert!(Instant::now() < deadline, "no new feed after device change");
        std::thread::sleep(Duration::from_millis(2));
    }
    let changed = rig.wait("playing on the chosen device", |s| {
        s.state == PlayState::Playing
    });
    assert_eq!(Rig::track(&changed), Some(LONG));
    assert!(changed.position_ms >= after.position_ms);
    assert_eq!(changed.notice, None);
}

#[test]
fn sleep_pauses_keeps_the_position_and_never_resumes_on_its_own() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rig = Rig::start(long_host(&dir));
    rig.send(play(&[LONG], 0));
    rig.wait("playing", |s| {
        s.state == PlayState::Playing && s.position_ms >= 500
    });
    rig.send(Command::SystemWillSleep);
    let asleep = rig.wait("paused for sleep", |s| s.state == PlayState::Paused);
    assert!(asleep.position_ms >= 500);
    assert!(!asleep.device_lost, "sleep is not an error");
    let frames = rig.total_frames();
    std::thread::sleep(Duration::from_millis(120));
    assert!(rig.total_frames() <= frames + 600);
    assert_eq!(rig.player.snapshot().state, PlayState::Paused);

    rig.send(Command::Play);
    let awake = rig.wait("playing after wake", |s| s.state == PlayState::Playing);
    assert!(awake.position_ms >= asleep.position_ms);
}

#[test]
fn a_file_that_never_opens_is_skipped_after_the_stall_timeout() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Opening a FIFO for reading blocks until somebody writes: a stand-in for a read
    // stuck on a dead network or USB drive.
    let fifo = dir.path().join("stuck.wav");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("mkfifo runs");
    assert!(made.success());
    let host = TestHost::new().with_track(STUCK, fifo, 1000);
    let rig = Rig::start_with(host, Duration::from_millis(300));
    rig.send(play(&[STUCK, A], 0));
    let s = rig.wait("skips the stuck file", |s| {
        Rig::track(s) == Some(A) && s.state == PlayState::Playing
    });
    assert_eq!(
        s.notice,
        Some(Notice::TrackSkipped {
            track_id: STUCK,
            kind: PlaybackErrorKind::FileUnreadable
        })
    );
    rig.wait("end", |s| s.state == PlayState::Ended);
    assert_eq!(rig.host.plays(), vec![A]);
}

#[test]
fn a_device_that_stops_asking_for_audio_is_reported_not_blamed_on_the_track() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rig = Rig::start_with(long_host(&dir), Duration::from_millis(300));
    rig.send(play(&[LONG, A], 0));
    rig.wait("playing", |s| s.state == PlayState::Playing);
    rig.frozen.store(true, Ordering::Relaxed);
    let s = rig.wait("output reported", |s| s.device_lost);
    assert_eq!(s.state, PlayState::Paused);
    assert!(matches!(
        s.notice,
        Some(Notice::OutputUnavailable(OutputError::Backend(_)))
    ));
    assert_eq!(Rig::track(&s), Some(LONG), "the track is not skipped");
    // The device is back.
    rig.frozen.store(false, Ordering::Relaxed);
    rig.send(Command::RetryOutput);
    rig.wait("playing again", |s| {
        s.state == PlayState::Playing && Rig::track(s) == Some(LONG)
    });
}

#[test]
fn a_broken_track_between_good_ones_never_adds_up_to_giving_up() {
    let rig = Rig::start(TestHost::new());
    rig.send(Command::SetRepeat(RepeatMode::All));
    rig.send(play(&[GONE, A], 0));
    // Every pass hits the broken track once. That is one failure, then music, again
    // and again: far more passes than the limit of failures in a row.
    rig.wait_host("many passes", |h| {
        h.plays().len() > MAX_CONSECUTIVE_FAILURES + 2
    });
    let s = rig.player.snapshot();
    assert_ne!(s.state, PlayState::Error);
    assert_ne!(s.notice, Some(Notice::TooManyFailures));
    assert!(rig.host.plays().iter().all(|t| *t == A));
}

#[test]
fn a_track_queued_after_a_failed_lookahead_is_still_played() {
    let rig = Rig::start(TestHost::new());
    // Real time, so that there is time to edit the queue while A plays.
    rig.pace_ms.store(12, Ordering::Relaxed);
    rig.send(play(&[A, GONE, B], 0));
    // The decode thread has already tried to open what follows A, and failed.
    rig.wait("lookahead failure", |s| {
        s.notice
            == Some(Notice::TrackSkipped {
                track_id: GONE,
                kind: PlaybackErrorKind::FileMissing,
            })
    });
    assert_eq!(Rig::track(&rig.player.snapshot()), Some(A));
    rig.send(Command::PlayNext(vec![MONO]));
    rig.wait("the queued track plays next", |s| {
        Rig::track(s) == Some(MONO) && s.state == PlayState::Playing
    });
    rig.pace_ms.store(2, Ordering::Relaxed);
    rig.wait("end", |s| s.state == PlayState::Ended);
    assert_eq!(rig.host.plays(), vec![A, MONO, B]);
}

#[test]
fn track_boundaries_do_not_rewrite_the_queue() {
    let rig = Rig::start(TestHost::new());
    rig.send(Command::SetRepeat(RepeatMode::One));
    rig.send(play(&[A, B], 0));
    // Several seconds of looping: many boundaries, several save intervals.
    rig.wait_host("loops", |h| h.plays().len() >= 12);
    rig.wait_host("state saved meanwhile", |h| {
        h.saves.load(Ordering::Relaxed) >= 2
    });
    assert_eq!(
        rig.host.queue_saves.load(Ordering::Relaxed),
        1,
        "only the initial queue is written"
    );
}

#[test]
fn a_queue_change_that_could_not_be_saved_is_saved_later() {
    let rig = Rig::start(TestHost::new());
    rig.send(Command::Enqueue(vec![A, B]));
    rig.wait_host("first queue saved", |h| {
        h.queue_saves.load(Ordering::Relaxed) == 1
    });
    // The disk refuses writes while the queue changes.
    rig.host.fail_saves.store(true, Ordering::Relaxed);
    rig.send(Command::Enqueue(vec![MONO]));
    rig.wait_host("the save was tried and failed", |h| {
        h.failed_saves.load(Ordering::Relaxed) >= 1
    });
    // Not retried on every tick.
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(rig.host.failed_saves.load(Ordering::Relaxed), 1);
    // Later saves still carry the queue, and nothing else has to happen first.
    rig.host.fail_saves.store(false, Ordering::Relaxed);
    rig.wait_host("the queue is saved after all", |h| {
        h.queue_saves.load(Ordering::Relaxed) == 2
    });
    let saved = rig.host.saved.lock().expect("lock").clone().expect("saved");
    let tracks: Vec<TrackId> = saved.queue.items.iter().map(|i| i.track).collect();
    assert_eq!(tracks, vec![A, B, MONO]);
}

#[test]
fn the_published_queue_follows_a_shuffled_wrap() {
    let rig = Rig::start(TestHost::new());
    rig.send(Command::SetShuffle(true));
    rig.send(Command::SetRepeat(RepeatMode::All));
    rig.send(play(&[A, B, MONO, FLAC_A], 0));
    // Every wrap reshuffles the queue. What is published must always be the order that
    // is being played: the current index points at the current item.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut orders = std::collections::HashSet::new();
    while rig.host.plays().len() < 26 {
        assert!(Instant::now() < deadline, "playback stalled");
        let s = rig.player.snapshot();
        if let (Some(index), Some(current)) = (s.queue_index, s.current) {
            assert_eq!(
                s.queue.get(index).map(|q| q.id),
                Some(current.item_id),
                "published queue is stale: {s:?}"
            );
            orders.insert(s.queue.iter().map(|q| q.id).collect::<Vec<_>>());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(orders.len() > 1, "the order changed across passes");
}

const T48: TrackId = 70; // tone_48k.wav
const T48_AGAIN: TrackId = 71; // the same file once more

/// A library that knows the sample rates of its tracks, as the real one does.
fn rated_host() -> TestHost {
    TestHost::new()
        .with_rate(A, 44_100)
        .with_rate(B, 44_100)
        .with_track(T48, fixture("tone_48k.wav"), 1000)
        .with_rate(T48, 48_000)
        .with_track(T48_AGAIN, fixture("tone_48k.wav"), 1000)
        .with_rate(T48_AGAIN, 48_000)
}

/// A rig whose pretend device offers 44.1 and 48 kHz, with rate matching on or off.
fn rate_rig(match_sample_rate: bool) -> Rig {
    Rig::start_configured(rated_host(), Duration::from_secs(15), move |engine| {
        engine.set_offline_rates(&[44_100, 48_000]);
        engine.set_output_settings(OutputSettings {
            match_sample_rate,
            ..OutputSettings::default()
        });
    })
}

/// With rate matching, tracks of one rate still join gaplessly in one feed, and the feed
/// ends exactly where the device has to change its rate. Nothing is lost at the break.
#[test]
fn rate_matching_starts_a_new_feed_only_where_the_rate_changes() {
    let rig = rate_rig(true);
    rig.send(play(&[A, B, T48, T48_AGAIN], 0));
    rig.wait("first track", |s| Rig::track(s) == Some(A));
    let ended = rig.wait("end of queue", |s| s.state == PlayState::Ended);
    assert_eq!(Rig::track(&ended), Some(T48_AGAIN));
    assert!(ended.output_settings.match_sample_rate);
    assert_eq!(rig.host.plays(), vec![A, B, T48, T48_AGAIN]);

    let audio = rig.audio.lock().expect("lock").clone();
    assert_eq!(
        audio.len(),
        2,
        "one feed for the 44.1 kHz pair, one for the 48 kHz pair"
    );
    // The 44.1 kHz pair is the uncut signal, sample for sample.
    let expected: Vec<f32> = decode_all("gap_a.wav")
        .into_iter()
        .chain(decode_all("gap_b.wav"))
        .collect();
    assert_eq!(audio[0].1.len(), expected.len());
    assert_eq!(
        max_abs_diff(&audio[0].1[FADE_GUARD * 2..], &expected[FADE_GUARD * 2..]),
        0.0
    );
    // The 48 kHz pair plays in full (the offline sink itself stays at 44.1 kHz, so it is
    // resampled here: twice one second, give or take the resampler's rounding).
    let frames = audio[1].1.len() / 2;
    assert!((88_190..=88_210).contains(&frames), "{frames} frames");
}

/// Without rate matching nothing changes: tracks of different rates share one feed.
#[test]
fn without_rate_matching_mixed_rates_stay_in_one_feed() {
    let rig = rate_rig(false);
    rig.send(play(&[A, T48, B], 0));
    rig.wait("end of queue", |s| s.state == PlayState::Ended);
    assert_eq!(rig.host.plays(), vec![A, T48, B]);
    assert_eq!(rig.feeds(), 1);
}

/// A track whose rate the library does not know never breaks a feed: the device cannot
/// be set up for it ahead of time, so it is converted like without rate matching.
#[test]
fn rate_matching_leaves_tracks_of_unknown_rate_alone() {
    let rig = Rig::start_configured(
        rated_host().with_rate(B, 0),
        Duration::from_secs(15),
        |engine| {
            engine.set_offline_rates(&[44_100, 48_000]);
            engine.set_output_settings(OutputSettings {
                match_sample_rate: true,
                ..OutputSettings::default()
            });
        },
    );
    rig.send(play(&[A, B], 0));
    rig.wait("end of queue", |s| s.state == PlayState::Ended);
    assert_eq!(rig.host.plays(), vec![A, B]);
    assert_eq!(rig.feeds(), 1);
}

/// Queue edits and mode changes keep working across a rate break: what is announced to
/// the decode thread is decided again each time.
#[test]
fn rate_matching_follows_queue_edits() {
    let rig = rate_rig(true);
    rig.pace_ms.store(12, Ordering::Relaxed); // real time: leave room to edit
    rig.send(play(&[A, T48], 0));
    rig.wait("playing", |s| s.state == PlayState::Playing);
    // Put a 44.1 kHz track in front of the 48 kHz one. (Whether it still joins the
    // running feed depends on how far the decode thread is; these tracks are one second
    // long, so it usually has finished the feed already.)
    rig.send(Command::PlayNext(vec![B]));
    rig.pace_ms.store(2, Ordering::Relaxed);
    rig.wait("end of queue", |s| s.state == PlayState::Ended);
    assert_eq!(rig.host.plays(), vec![A, B, T48]);
    assert!(
        rig.feeds() >= 2,
        "the 48 kHz track starts a feed of its own"
    );
}

/// Switching the output options while playing reopens the output and goes on from the
/// same position; switching memory playback alone does not interrupt anything.
#[test]
fn changing_output_settings_keeps_the_position() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rig = Rig::start(long_host(&dir));
    rig.pace_ms.store(12, Ordering::Relaxed);
    rig.send(play(&[LONG], 0));
    rig.wait("playing for a while", |s| {
        // The shown position runs on the clock and is a little ahead of this rig's
        // audio; ask for clearly more than what is checked below.
        s.state == PlayState::Playing && s.position_now_ms() >= 600
    });
    let feeds = rig.feeds();
    rig.send(Command::SetOutput(OutputSettings {
        memory_playback: true,
        ..OutputSettings::default()
    }));
    let snap = rig.wait("memory playback noted", |s| {
        s.output_settings.memory_playback
    });
    assert_eq!(snap.state, PlayState::Playing);
    assert_eq!(
        rig.feeds(),
        feeds,
        "memory playback alone starts no new feed"
    );

    rig.send(Command::SetOutput(OutputSettings {
        match_sample_rate: true,
        exclusive: true,
        memory_playback: true,
    }));
    let snap = rig.wait("reopened and playing", |s| {
        s.output_settings.exclusive && s.state == PlayState::Playing && rig.feeds() > feeds
    });
    assert!(
        snap.position_now_ms() >= 300,
        "continues, does not restart: {snap:?}"
    );
    assert!(
        snap.source.as_ref().is_some_and(|s| s.in_memory),
        "the new feed plays from memory"
    );
}

const MP3_A: TrackId = 80; // gap_a.mp3
const WIDE_A: TrackId = 81; // gap_a.wav, which the library takes for a 24-bit file

/// A rig whose pretend device takes 16-bit integers.
fn sixteen_bit_rig() -> Rig {
    let host = TestHost::new()
        .with_rate(A, 44_100)
        .with_bits(A, 16)
        .with_track(MP3_A, fixture("gap_a.mp3"), 1000)
        .with_rate(MP3_A, 44_100)
        .with_track(WIDE_A, fixture("gap_a.wav"), 1000)
        .with_rate(WIDE_A, 44_100)
        .with_bits(WIDE_A, 24);
    Rig::start_configured(host, Duration::from_secs(15), |engine| {
        engine.set_offline_hardware_bits(Some((16, false)));
    })
}

/// On an integer device, a file that fits the device and is played untouched goes out
/// sample for sample; anything else is dithered, by no more than one device step.
#[test]
fn an_integer_device_gets_dither_only_when_the_samples_are_not_the_files_own() {
    let step = 1.0 / 32_768.0;
    let expected = decode_all("gap_a.wav");

    // 16-bit file, 16-bit device, full volume: untouched, no dither.
    let rig = sixteen_bit_rig();
    rig.send(play(&[A], 0));
    let playing = rig.wait("playing", |s| s.state == PlayState::Playing);
    assert_eq!(playing.dither_bits, None);
    rig.wait("end", |s| s.state == PlayState::Ended);
    let audio = rig.last_feed_audio();
    assert_eq!(audio.len(), expected.len());
    assert_eq!(
        max_abs_diff(&audio[FADE_GUARD * 2..], &expected[FADE_GUARD * 2..]),
        0.0
    );
    drop(rig);

    // The same file taken for a 24-bit one does not fit a 16-bit device: dithered.
    let rig = sixteen_bit_rig();
    rig.send(play(&[WIDE_A], 0));
    rig.wait("dither for a wider file", |s| {
        s.state == PlayState::Playing && s.dither_bits == Some(16)
    });
    rig.wait("end", |s| s.state == PlayState::Ended);
    let audio = rig.last_feed_audio();
    let worst = max_abs_diff(&audio[FADE_GUARD * 2..], &expected[FADE_GUARD * 2..]);
    assert!(
        worst > 0.0 && worst <= step * 1.0001,
        "worst deviation {worst}"
    );
    drop(rig);

    // A lossy file is never "the file's own samples".
    let rig = sixteen_bit_rig();
    rig.send(play(&[MP3_A], 0));
    rig.wait("dither for a lossy file", |s| {
        s.state == PlayState::Playing && s.dither_bits == Some(16)
    });
    rig.wait("end", |s| s.state == PlayState::Ended);
    let (audio, decoded) = (rig.last_feed_audio(), decode_all("gap_a.mp3"));
    assert_eq!(audio.len(), decoded.len());
    let worst = max_abs_diff(&audio[FADE_GUARD * 2..], &decoded[FADE_GUARD * 2..]);
    assert!(
        worst > 0.0 && worst <= step * 1.0001,
        "worst deviation {worst}"
    );
    drop(rig);

    // Turning the volume down touches the samples of even a fitting file.
    let rig = sixteen_bit_rig();
    rig.send(Command::SetVolume(0.5));
    rig.send(play(&[A], 0));
    let snap = rig.wait("dither below full volume", |s| {
        s.state == PlayState::Playing && s.dither_bits == Some(16)
    });
    assert_eq!(snap.volume, 0.5);
}

/// A device that takes floating point (or whose format is unknown) never gets dither.
#[test]
fn a_float_device_never_gets_dither() {
    let host = TestHost::new()
        .with_track(MP3_A, fixture("gap_a.mp3"), 1000)
        .with_rate(MP3_A, 44_100);
    let rig = Rig::start_configured(host, Duration::from_secs(15), |engine| {
        engine.set_offline_hardware_bits(Some((32, true)));
    });
    rig.send(Command::SetVolume(0.5));
    rig.send(play(&[MP3_A, A], 0));
    let snap = rig.wait("second track", |s| Rig::track(s) == Some(A));
    assert_eq!(snap.dither_bits, None);
    rig.wait("end", |s| s.state == PlayState::Ended);
    assert_eq!(rig.player.snapshot().dither_bits, None);
}

/// Across a gapless boundary the dither follows the tracks: the lossy track is dithered
/// to its end, and the untouched file after it goes out exactly (once the boundary has
/// been noticed, which takes a moment).
#[test]
fn dither_ends_when_the_processed_track_has_left_the_feed() {
    let step = 1.0 / 32_768.0;
    let rig = sixteen_bit_rig();
    rig.pace_ms.store(12, Ordering::Relaxed); // real time: boundaries are noticed promptly
    rig.send(play(&[MP3_A, A], 0));
    rig.wait("the lossy track is dithered", |s| {
        s.state == PlayState::Playing && Rig::track(s) == Some(MP3_A) && s.dither_bits == Some(16)
    });
    rig.wait("the untouched track is not", |s| {
        Rig::track(s) == Some(A) && s.dither_bits.is_none()
    });
    rig.wait("end", |s| s.state == PlayState::Ended);
    assert_eq!(rig.feeds(), 1, "one gapless feed");
    let audio = rig.last_feed_audio();
    let (lossy, exact) = (decode_all("gap_a.mp3"), decode_all("gap_a.wav"));
    assert_eq!(audio.len(), lossy.len() + exact.len());
    // The middle of the lossy track: dithered, by no more than a step.
    let range = 10_000..70_000;
    let worst = max_abs_diff(&audio[range.clone()], &lossy[range]);
    assert!(worst > 0.0 && worst <= step * 1.0001, "lossy part: {worst}");
    // The second half of the file after it: sample for sample.
    let from = lossy.len() + exact.len() / 2;
    assert_eq!(
        max_abs_diff(&audio[from..], &exact[exact.len() / 2..]),
        0.0,
        "untouched part"
    );
}

/// The player thread gives a sign of life several times a second. When it is stuck in a
/// call that does not return, the sign stops, which is how the window knows to tell the
/// user; when the call returns, everything goes on.
#[test]
fn a_stuck_player_thread_is_noticed_and_recovers() {
    let rig = Rig::start(TestHost::new());
    std::thread::sleep(Duration::from_millis(450));
    assert!(
        rig.player.unresponsive_for() < Duration::from_millis(400),
        "idle: heard from within the last tick"
    );
    rig.host.hang.store(true, Ordering::Relaxed);
    rig.send(play(&[A, B], 0));
    std::thread::sleep(Duration::from_millis(700));
    let silent = rig.player.unresponsive_for();
    assert!(silent >= Duration::from_millis(500), "stuck for {silent:?}");
    assert_eq!(
        rig.player.snapshot().state,
        PlayState::Idle,
        "nothing moved"
    );
    // The call returns: the command that was waiting is carried out.
    rig.host.hang.store(false, Ordering::Relaxed);
    rig.wait("playing after all", |s| s.state == PlayState::Playing);
    assert!(rig.player.unresponsive_for() < Duration::from_millis(400));
    rig.wait("end", |s| s.state == PlayState::Ended);
    assert_eq!(rig.host.plays(), vec![A, B]);
}

// ----- peak protection ---------------------------------------------------------------

const HOT: TrackId = 90; // gap_a.wav with a ReplayGain tag asking for +12 dB

/// The snapshot says, per track, whether the peak limiter had to step in: for the
/// track that is heard, not for the one that is being decoded ahead of it.
#[test]
fn peak_protection_is_reported_for_the_track_it_acted_on() {
    let mut host = TestHost::new().with_track(HOT, fixture("gap_a.wav"), 1000);
    host.tracks.get_mut(&HOT).expect("track exists").replaygain = ReplayGainTags {
        track_gain_db: Some(12.0),
        track_peak: Some(0.354),
        ..Default::default()
    };
    let rig = Rig::start(host);
    rig.pace_ms.store(12, Ordering::Relaxed); // real time: boundaries are noticed promptly
    rig.send(Command::SetReplayGain(ReplayGainSettings {
        mode: oneum::audio::ReplayGainMode::Track,
        preamp_db: 0.0,
        prevent_clipping: false,
    }));
    rig.send(play(&[A, HOT, B], 0));
    rig.wait("the first track needs no protection", |s| {
        s.state == PlayState::Playing && Rig::track(s) == Some(A) && s.position_ms > 300
    });
    assert_eq!(rig.player.snapshot().peak_limited_frames, 0);
    rig.wait("the amplified track is protected", |s| {
        Rig::track(s) == Some(HOT) && s.peak_limited_frames > 10_000
    });
    rig.wait("the track after it is not", |s| {
        Rig::track(s) == Some(B) && s.peak_limited_frames == 0
    });
    rig.wait("end", |s| s.state == PlayState::Ended);
    assert_eq!(rig.feeds(), 1, "one gapless feed");
    // Nothing that was heard exceeds full scale, and the amplified second is at it.
    let audio = rig.last_feed_audio();
    let peak =
        |range: std::ops::Range<usize>| audio[range].iter().fold(0f32, |m, s| m.max(s.abs()));
    assert!(peak(0..audio.len()) <= 1.0);
    // (Interleaved stereo: a second is 88,200 samples.)
    assert_eq!(audio.len(), 3 * 88_200);
    assert!(peak(100_000..160_000) > 0.99, "{}", peak(100_000..160_000));
    assert!(peak(2_000..80_000) < 0.4);
    assert!(peak(190_000..260_000) < 0.4);
}

// ----- equalizer ----------------------------------------------------------------------

/// The equalizer reaches the audio that is playing (within the second that is decoded
/// ahead), counts as processing for an integer device, and leaves no trace when it is
/// switched off again.
#[test]
fn the_equalizer_is_applied_while_playing_and_dithered_for_an_integer_device() {
    use oneum::audio::EqSettings;
    let dir = tempfile::tempdir().expect("temp dir");
    let host = long_host(&dir).with_rate(LONG, 44_100).with_bits(LONG, 16);
    let rig = Rig::start_configured(host, Duration::from_secs(15), |engine| {
        engine.set_offline_hardware_bits(Some((16, false)));
    });
    rig.pace_ms.store(12, Ordering::Relaxed); // real time
    rig.send(play(&[LONG], 0));
    let playing = rig.wait("untouched: no dither", |s| {
        s.state == PlayState::Playing && s.position_ms > 300
    });
    assert_eq!(playing.dither_bits, None);
    assert!(!playing.eq.is_active());

    // Out of range on purpose: the player brings the values back.
    let mut gains = [0.0; 10];
    gains[5] = 40.0;
    rig.send(Command::SetEq(EqSettings {
        enabled: true,
        preamp_db: -12.0,
        gains_db: gains,
    }));
    let on = rig.wait("equalized: dithered", |s| {
        s.eq.is_active() && s.dither_bits == Some(16)
    });
    assert_eq!(on.eq.gains_db[5], 12.0);
    assert_eq!(on.eq.preamp_db, -12.0);

    rig.send(Command::SetEq(EqSettings::default()));
    rig.wait("switched off: the file's own samples again", |s| {
        !s.eq.is_active() && s.dither_bits.is_none()
    });
    assert_eq!(rig.player.snapshot().state, PlayState::Playing);
    assert_eq!(rig.feeds(), 1, "the equalizer never restarts the feed");
}

// ----- radio ---------------------------------------------------------------------------

/// With radio on, the queue does not end: when its last song starts, songs that go
/// with it are added, and they follow without a gap like anything else in the queue.
#[test]
fn radio_keeps_playing_when_the_queue_runs_out() {
    let mut host = TestHost::new();
    host.radio_pool = vec![A, B, FLAC_A];
    let rig = Rig::start(host);
    rig.send(Command::SetRadio(true));
    rig.send(play(&[A], 0));
    // A is in the queue already, so the radio adds the other two; when the last of
    // them starts there is nothing left to add, and the queue ends.
    let ended = rig.wait("the end of what the radio had", |s| {
        s.state == PlayState::Ended
    });
    assert!(ended.radio);
    let queued: Vec<TrackId> = ended.queue.iter().map(|i| i.track).collect();
    assert_eq!(queued, [A, B, FLAC_A]);
    assert_eq!(
        rig.feeds(),
        1,
        "the added songs follow gaplessly in the same feed"
    );
    let audio = rig.last_feed_audio();
    let expected: usize = ["gap_a.wav", "gap_b.wav", "gap_a.flac"]
        .iter()
        .map(|name| decode_all(name).len())
        .sum();
    assert_eq!(audio.len(), expected);
    // Asked for the song that was the last one when playback started, and then only
    // for the last of what it had to give (when it starts, and once more at the end).
    let asked = rig.host.radio_asked.lock().expect("lock").clone();
    assert_eq!(asked.first(), Some(&A));
    assert!(
        asked.len() >= 2 && asked[1..].iter().all(|seed| *seed == FLAC_A),
        "{asked:?}"
    );
}

#[test]
fn radio_is_off_unless_asked_for_and_leaves_repeat_alone() {
    let mut host = TestHost::new();
    host.radio_pool = vec![B, FLAC_A];
    let rig = Rig::start(host);
    // Off: the queue ends where it ends.
    rig.send(play(&[A], 0));
    let ended = rig.wait("end", |s| s.state == PlayState::Ended);
    assert_eq!(ended.queue.len(), 1);
    assert!(!ended.radio);
    assert!(rig.host.radio_asked.lock().expect("lock").is_empty());
    // Repeat is a way of not ending already: the radio stays out of it.
    rig.send(Command::SetRadio(true));
    rig.send(Command::SetRepeat(RepeatMode::One));
    rig.send(play(&[A], 0));
    rig.wait("the song comes round again", |s| {
        s.state == PlayState::Playing && rig.feeds() >= 1 && rig.host.plays().len() >= 2
    });
    assert_eq!(rig.player.snapshot().queue.len(), 1);
    assert!(rig.host.radio_asked.lock().expect("lock").is_empty());
}

/// Switched on while the last song of the queue is playing, it applies to that queue.
#[test]
fn radio_switched_on_during_the_last_song_extends_the_queue() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut host = long_host(&dir);
    host.radio_pool = vec![A, B];
    let rig = Rig::start(host);
    rig.pace_ms.store(12, Ordering::Relaxed); // real time
    rig.send(play(&[LONG], 0));
    rig.wait("playing", |s| {
        s.state == PlayState::Playing && s.position_ms > 200
    });
    assert_eq!(rig.player.snapshot().queue.len(), 1);
    rig.send(Command::SetRadio(true));
    let grown = rig.wait("the queue grows", |s| s.queue.len() == 3);
    assert_eq!(grown.state, PlayState::Playing);
    assert_eq!(Rig::track(&grown), Some(LONG));
    assert_eq!(rig.feeds(), 1, "the song that is playing is not restarted");
}

/// Crossfeed is processing like the equalizer: it reaches the player, and an integer
/// device gets dither while it is on.
#[test]
fn crossfeed_counts_as_processing() {
    use oneum::audio::Crossfeed;
    let dir = tempfile::tempdir().expect("temp dir");
    let host = long_host(&dir).with_rate(LONG, 44_100).with_bits(LONG, 16);
    let rig = Rig::start_configured(host, Duration::from_secs(15), |engine| {
        engine.set_offline_hardware_bits(Some((16, false)));
    });
    rig.pace_ms.store(12, Ordering::Relaxed); // real time
    rig.send(play(&[LONG], 0));
    let playing = rig.wait("untouched", |s| {
        s.state == PlayState::Playing && s.position_ms > 200
    });
    assert_eq!(
        (playing.crossfeed, playing.dither_bits),
        (Crossfeed::Off, None)
    );
    rig.send(Command::SetCrossfeed(Crossfeed::Medium));
    rig.wait("crossfed: dithered", |s| {
        s.crossfeed == Crossfeed::Medium && s.dither_bits == Some(16)
    });
    rig.send(Command::SetCrossfeed(Crossfeed::Off));
    rig.wait("off again", |s| {
        s.crossfeed == Crossfeed::Off && s.dither_bits.is_none()
    });
    assert_eq!(rig.feeds(), 1);
}

/// A queue edit can leave the song that is playing as the last one: the radio is asked
/// then too, not only when a song starts.
#[test]
fn radio_steps_in_when_an_edit_leaves_the_playing_song_last() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut host = long_host(&dir);
    host.radio_pool = vec![B, FLAC_A];
    let rig = Rig::start(host);
    rig.pace_ms.store(12, Ordering::Relaxed); // real time
    rig.send(Command::SetRadio(true));
    rig.send(play(&[LONG, A], 0));
    let playing = rig.wait("playing", |s| {
        s.state == PlayState::Playing && s.position_ms > 200
    });
    assert_eq!(playing.queue.len(), 2, "something follows: nothing to add");
    let last = playing.queue[1].id;
    rig.send(Command::RemoveItems(vec![last]));
    let grown = rig.wait("the radio fills in", |s| s.queue.len() == 3);
    let queued: Vec<TrackId> = grown.queue.iter().map(|i| i.track).collect();
    assert_eq!(queued, [LONG, B, FLAC_A]);
    assert_eq!(rig.feeds(), 1, "the song that is playing is not restarted");
}

/// Shuffle with Repeat All decides its next round only when it wraps, so at the last
/// song of a round "nothing follows" does not mean the queue ends: the radio must not
/// take it for that and grow the queue round after round.
#[test]
fn radio_does_not_grow_a_shuffled_repeating_queue() {
    let mut host = TestHost::new();
    host.radio_pool = vec![FLAC_A, MONO];
    let rig = Rig::start(host);
    rig.send(Command::SetRadio(true));
    rig.send(Command::SetRepeat(RepeatMode::All));
    rig.send(Command::SetShuffle(true));
    rig.send(play(&[A, B], 0));
    rig.wait_host("more than one round", |h| h.plays().len() >= 5);
    assert_eq!(rig.player.snapshot().queue.len(), 2);
    assert!(rig.host.radio_asked.lock().expect("lock").is_empty());
}

// ----- stop after the current song -------------------------------------------------------

/// Asked for at the start of a song: that song is played to its end and nothing after
/// it; the next one is ready, and "play" goes on from there.
#[test]
fn stop_after_current_stops_at_the_end_of_the_song() {
    let rig = Rig::start(TestHost::new());
    rig.send(play(&[A, B], 0));
    rig.send(Command::SetStopAfterCurrent(true));
    let stopped = rig.wait("stopped with the next song ready", |s| {
        s.state == PlayState::Paused && Rig::track(s) == Some(B)
    });
    assert!(!stopped.stop_after_current, "it applied to that one song");
    assert_eq!(stopped.position_ms, 0);
    assert_eq!(rig.host.plays(), vec![A]);
    assert_eq!(rig.feeds(), 1);
    assert_eq!(
        rig.last_feed_audio().len(),
        decode_all("gap_a.wav").len(),
        "the whole song and nothing of the next"
    );
    rig.send(Command::Play);
    rig.wait("the rest of the queue", |s| s.state == PlayState::Ended);
    assert_eq!(rig.host.plays(), vec![A, B]);
}

/// Asked for while the song is on (the next one may already have been decoded behind
/// it): still nothing of the next song is heard.
#[test]
fn stop_after_current_asked_for_in_the_middle_of_a_song() {
    let rig = Rig::start(TestHost::new());
    rig.pace_ms.store(12, Ordering::Relaxed); // real time
    rig.send(play(&[A, B, MONO], 0));
    rig.wait("the first song is on", |s| {
        s.state == PlayState::Playing && Rig::track(s) == Some(A) && s.position_now_ms() >= 200
    });
    rig.send(Command::SetStopAfterCurrent(true));
    rig.wait("marked", |s| s.stop_after_current);
    let stopped = rig.wait("stopped with the next song ready", |s| {
        s.state == PlayState::Paused && Rig::track(s) == Some(B)
    });
    assert!(!stopped.stop_after_current);
    assert_eq!(rig.host.plays(), vec![A]);
    let heard: usize = rig
        .audio
        .lock()
        .expect("lock")
        .iter()
        .map(|(_, pcm)| pcm.len())
        .sum();
    assert!(
        heard <= decode_all("gap_a.wav").len() + 2 * 4096,
        "nothing of the next song was played ({heard} samples heard)"
    );
}

/// Taking it back, or starting another song, ends it.
#[test]
fn stop_after_current_can_be_taken_back_and_ends_with_the_song() {
    let rig = Rig::start(TestHost::new());
    rig.send(play(&[A, B], 0));
    rig.send(Command::SetStopAfterCurrent(true));
    rig.send(Command::SetStopAfterCurrent(false));
    let ended = rig.wait("the queue plays to its end", |s| {
        s.state == PlayState::Ended
    });
    assert!(!ended.stop_after_current);
    assert_eq!(rig.host.plays(), vec![A, B]);

    let dir = tempfile::tempdir().expect("temp dir");
    let rig = Rig::start(long_host(&dir));
    rig.send(play(&[LONG, A, B], 0));
    rig.send(Command::SetStopAfterCurrent(true));
    rig.wait("marked", |s| s.stop_after_current);
    rig.send(Command::Next);
    rig.wait("another song ends it", |s| {
        !s.stop_after_current && Rig::track(s) == Some(A)
    });
    rig.wait("and playback goes on", |s| s.state == PlayState::Ended);
    // Nothing to mark without a song.
    rig.send(Command::ClearQueue);
    rig.send(Command::SetStopAfterCurrent(true));
    let idle = rig.wait("idle", |s| s.state == PlayState::Idle);
    assert!(!idle.stop_after_current);
}

// ----- sleep timer -----------------------------------------------------------------------

/// The sleep timer pauses playback when its time is up, with the song where it was;
/// the volume setting is untouched and "play" goes on at full level.
#[test]
fn sleep_timer_pauses_and_leaves_the_volume_alone() {
    let dir = tempfile::tempdir().expect("temp dir");
    let rig = Rig::start(long_host(&dir));
    rig.pace_ms.store(12, Ordering::Relaxed); // real time
    rig.send(play(&[LONG], 0));
    rig.wait("playing", |s| s.state == PlayState::Playing);
    rig.send(Command::SetSleepTimer(Some(Duration::from_millis(1500))));
    let set = rig.wait("the timer is set", |s| s.sleep_at.is_some());
    assert_eq!(set.state, PlayState::Playing);
    let asleep = rig.wait("paused by the timer", |s| s.state == PlayState::Paused);
    assert_eq!(asleep.sleep_at, None, "the timer is over");
    assert_eq!(asleep.volume, 1.0, "the volume setting was not changed");
    assert_eq!(Rig::track(&asleep), Some(LONG));
    assert!(
        (800..6000).contains(&asleep.position_ms),
        "paused where the song was: {} ms",
        asleep.position_ms
    );
    // What was heard last was faded: quieter than the start of the song.
    let peak = |pcm: &[f32]| pcm.iter().fold(0f32, |m, s| m.max(s.abs()));
    let heard = rig.last_feed_audio();
    let (start, end) = (&heard[..8_820], &heard[heard.len() - 8_820..]);
    assert!(
        peak(end) < peak(start) * 0.6,
        "{} vs {}",
        peak(end),
        peak(start)
    );
    // Play again: full level.
    let before = heard.len();
    rig.send(Command::Play);
    rig.wait("playing again", |s| {
        s.state == PlayState::Playing && rig.last_feed_audio().len() > before + 44_100
    });
    let again = rig.last_feed_audio();
    let resumed = &again[again.len() - 22_050..];
    assert!(
        (peak(resumed) - peak(start)).abs() < peak(start) * 0.05,
        "{} vs {}",
        peak(resumed),
        peak(start)
    );
}

#[test]
fn sleep_timer_can_be_switched_off() {
    let dir = tempfile::tempdir().expect("temp dir");
    let rig = Rig::start(long_host(&dir));
    rig.pace_ms.store(12, Ordering::Relaxed);
    rig.send(play(&[LONG], 0));
    rig.wait("playing", |s| s.state == PlayState::Playing);
    rig.send(Command::SetSleepTimer(Some(Duration::from_millis(900))));
    rig.wait("set", |s| s.sleep_at.is_some());
    rig.send(Command::SetSleepTimer(None));
    rig.wait("off", |s| s.sleep_at.is_none());
    std::thread::sleep(Duration::from_millis(1500));
    let later = rig.player.snapshot();
    assert_eq!(later.state, PlayState::Playing, "nothing pauses it now");
    // A timer set while nothing plays simply runs out.
    rig.send(Command::Pause);
    rig.wait("paused", |s| s.state == PlayState::Paused);
    rig.send(Command::SetSleepTimer(Some(Duration::from_millis(300))));
    rig.wait("set while paused", |s| s.sleep_at.is_some());
    let over = rig.wait("ran out", |s| s.sleep_at.is_none());
    assert_eq!(over.state, PlayState::Paused);
}

// ----- parametric correction -------------------------------------------------------------

/// The correction profile is set and taken away while playing, without restarting the
/// feed; the player says which profile is applied.
#[test]
fn a_correction_profile_is_applied_and_named() {
    use oneum::audio::PeqProfile;
    let dir = tempfile::tempdir().expect("temp dir");
    let rig = Rig::start(long_host(&dir));
    rig.pace_ms.store(12, Ordering::Relaxed);
    rig.send(play(&[LONG], 0));
    rig.wait("playing", |s| s.state == PlayState::Playing);
    assert_eq!(rig.player.snapshot().correction, None);
    let profile = PeqProfile::parse(
        "HD 600",
        "Preamp: -6 dB\nFilter 1: ON PK Fc 1000 Hz Gain 4 dB Q 1.4",
    )
    .expect("parses");
    rig.send(Command::SetCorrection(Some(profile)));
    let on = rig.wait("named", |s| s.correction.as_deref() == Some("HD 600"));
    assert_eq!(on.state, PlayState::Playing);
    // A profile that changes nothing is not "applied".
    rig.send(Command::SetCorrection(Some(PeqProfile {
        name: "Flat".into(),
        ..Default::default()
    })));
    rig.wait("nothing applied", |s| s.correction.is_none());
    rig.send(Command::SetCorrection(None));
    std::thread::sleep(Duration::from_millis(300));
    let after = rig.player.snapshot();
    assert_eq!(after.correction, None);
    assert_eq!(after.state, PlayState::Playing);
    assert_eq!(rig.feeds(), 1, "the feed was not restarted");
}

/// The same when the library does not know yet that the song cannot be played (its
/// file has gone since the last scan, and that is found out when it is opened).
#[test]
fn stop_after_a_song_whose_file_is_gone_stops_there() {
    let rig = Rig::start(TestHost::new());
    rig.send(play(&[A, GONE, B], 0));
    rig.send(Command::Pause);
    rig.send(Command::Next);
    rig.wait("the song without a file is current, paused", |s| {
        s.state == PlayState::Paused && Rig::track(s) == Some(GONE)
    });
    rig.send(Command::SetStopAfterCurrent(true));
    rig.wait("marked", |s| s.stop_after_current);
    rig.send(Command::Play);
    let stopped = rig.wait("stopped with the next song ready", |s| {
        s.state == PlayState::Paused && Rig::track(s) == Some(B)
    });
    assert!(!stopped.stop_after_current);
    std::thread::sleep(Duration::from_millis(400));
    let later = rig.player.snapshot();
    assert_eq!(later.state, PlayState::Paused, "nothing started on its own");
    assert!(!rig.host.plays().contains(&B));
}

/// The song marked "stop after this song" cannot be played (its drive is gone): it is
/// over at once, so playback stops there as it would have at its end, with the next
/// song ready. Before, the mark went with the skipped song and the queue played on.
#[test]
fn stop_after_a_song_that_cannot_be_played_stops_there() {
    let rig = Rig::start(TestHost::new());
    rig.send(play(&[A, OFFLINE, B], 0));
    rig.send(Command::Pause);
    rig.send(Command::Next);
    rig.wait("the unplayable song is current, paused", |s| {
        s.state == PlayState::Paused && Rig::track(s) == Some(OFFLINE)
    });
    rig.send(Command::SetStopAfterCurrent(true));
    rig.wait("marked", |s| s.stop_after_current);
    rig.send(Command::Play);
    let stopped = rig.wait("stopped with the next song ready", |s| {
        s.state == PlayState::Paused && Rig::track(s) == Some(B)
    });
    assert!(!stopped.stop_after_current);
    assert_eq!(stopped.position_ms, 0);
    std::thread::sleep(Duration::from_millis(300));
    let later = rig.player.snapshot();
    assert_eq!(later.state, PlayState::Paused, "nothing started on its own");
    assert!(!rig.host.plays().contains(&B));
    rig.send(Command::Play);
    rig.wait("the rest of the queue", |s| s.state == PlayState::Ended);
    assert!(rig.host.plays().contains(&B));
}
