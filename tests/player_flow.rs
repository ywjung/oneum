//! End-to-end checks of the player thread against an offline engine: commands in,
//! snapshots and rendered audio out. No audio device is used.
use oneum::audio::decoder::TrackDecoder;
use oneum::audio::render::Renderer;
use oneum::audio::{Engine, PlaybackErrorKind, ReplayGainSettings};
use oneum::model::{ReplayGainTags, TrackId};
use oneum::playback::player::{self, MAX_CONSECUTIVE_FAILURES};
use oneum::playback::{
    Command, Notice, PlayState, PlayableTrack, PlayerConfig, PlayerHandle, PlayerHost,
    PlayerSnapshot, QueueItem, QueueState, RepeatMode, Session,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
    changes: AtomicUsize,
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
    fn plays(&self) -> Vec<TrackId> {
        self.plays.lock().expect("lock").clone()
    }
}

impl PlayerHost for TestHost {
    fn track(&self, id: TrackId) -> Option<PlayableTrack> {
        self.tracks.get(&id).cloned()
    }
    fn record_play(&self, id: TrackId) {
        self.plays.lock().expect("lock").push(id);
    }
    fn load_session(&self) -> Option<Session> {
        self.session.lock().expect("lock").clone()
    }
    fn save_session(&self, session: &Session, _queue_changed: bool) {
        *self.saved.lock().expect("lock") = Some(session.clone());
        self.saves.fetch_add(1, Ordering::Relaxed);
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
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Rig {
    fn start(host: TestHost) -> Self {
        let host = Arc::new(host);
        let (renderer_tx, renderer_rx) = std::sync::mpsc::channel::<Renderer>();
        let (player, player_thread) = player::spawn_with_engine(
            host.clone(),
            PlayerConfig {
                default_volume: 1.0,
                ..Default::default()
            },
            move |volume| {
                let (engine, renderer) =
                    Engine::new_offline(44_100, volume, ReplayGainSettings::default());
                renderer_tx.send(renderer).expect("test is alive");
                engine
            },
        );
        let mut renderer = renderer_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("engine created");
        let audio: HeardAudio = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (audio2, stop2) = (audio.clone(), stop.clone());
        let pump = std::thread::spawn(move || {
            const BLOCK: usize = 512;
            let mut out = vec![0f32; BLOCK * 2];
            while !stop2.load(Ordering::Relaxed) {
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
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        Self {
            player,
            host,
            audio,
            stop,
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
