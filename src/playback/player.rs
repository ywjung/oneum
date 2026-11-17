//! The player thread: the single entry point for every playback command.
//!
//! GUI, menu, media keys and Finder all send [`Command`]s through one channel. The thread
//! owns the queue and the audio engine, turns engine events into state changes, and
//! publishes a [`PlayerSnapshot`] for readers. Nothing here touches the UI or the database
//! directly; the outside world is reached through [`PlayerHost`].
use super::queue::{ItemId, Previous, Queue, QueueItem, QueueState, RepeatMode};
use crate::audio::decoder::SourceInfo;
use crate::audio::dsp::replaygain_factor;
use crate::audio::{
    Crossfeed, Engine, EngineEvent, EqSettings, LevelMeter, Levels, NextSlot, NextTrack,
    OutputError, OutputSettings, OutputStatus, PeqProfile, PlaybackErrorKind, ReplayGainSettings,
    Scope, TrackRequest,
};
use crate::model::{ReplayGainTags, TrackId};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const TICK_ACTIVE: Duration = Duration::from_millis(20);
const TICK_IDLE: Duration = Duration::from_millis(200);
const POSITION_PUBLISH_INTERVAL: Duration = Duration::from_millis(500);
const SESSION_SAVE_INTERVAL: Duration = Duration::from_secs(10);
/// The sleep timer turns the volume down over its last seconds instead of cutting off.
const SLEEP_FADE: Duration = Duration::from_secs(5);
/// A crossfade shorter than this is none: the songs are joined as they are.
const CROSSFADE_MIN_MS: u64 = 500;
/// A section that is repeated (`Command::MarkSection`) is at least this long.
const SECTION_MIN_MS: u64 = 500;
/// It ends at least this long before its song does: at the song's end the next one
/// would begin instead of the section again.
const SECTION_BEFORE_END_MS: u64 = 300;
/// After the sleep timer's pause, how long the volume stays down: past the renderer's
/// own short fade-out, so that the volume coming back up is not heard as a tick.
const SLEEP_RESTORE_AFTER: Duration = Duration::from_millis(120);
const QUEUE_SAVE_DEBOUNCE: Duration = Duration::from_secs(1);
/// "Resume long tracks": tracks at least this long start where they were left off…
const RESUME_MIN_DURATION_MS: u64 = 20 * 60 * 1000;
/// …if at least this much of them had been heard…
const RESUME_MIN_POSITION_MS: u64 = 30 * 1000;
/// …and not all but this last bit (then it counts as finished).
const RESUME_END_MS: u64 = 60 * 1000;
/// After a session save failed (a full disk, a locked database), the wait before the
/// next attempt.
const SESSION_RETRY_AFTER: Duration = Duration::from_secs(2);
/// Playback stops after this many tracks in a row could not be played.
pub const MAX_CONSECUTIVE_FAILURES: usize = 10;
/// A play is counted once this much of the track has been heard (or half of it, if shorter).
const PLAY_CREDIT_MS: u64 = 240_000;
/// A paused feed is released after this long: the decode thread ends and the file is
/// closed, so the drive it is on can be ejected. Playback resumes from the same position.
const PAUSE_RELEASE: Duration = Duration::from_secs(30);
/// How long audio decoded before a setting was switched off can still be on its way to
/// the device: the ring's second, the equalizer's fade and settling, and a margin.
const PROCESSED_TAIL: Duration = Duration::from_secs(2);
/// Radio: how many songs are added when the queue is about to run out.
const RADIO_BATCH: usize = 5;

/// What the player needs to know about a track in order to play it.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayableTrack {
    pub path: PathBuf,
    pub replaygain: ReplayGainTags,
    pub duration_ms: u64,
    /// False when the library already knows the file is missing or its drive is offline.
    pub available: bool,
    /// Sample rate as the library read it from the file; 0 when unknown. Used to set the
    /// output device up before the file is opened (rate matching).
    pub sample_rate: u32,
    /// Bits per sample of a lossless file, when the library knows it.
    pub bit_depth: Option<u8>,
    /// The album it belongs to as the file says (see [`PlayerHost::album_of`]).
    pub album_key: u64,
}

/// Persisted across restarts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub queue: QueueState,
    pub position_ms: u64,
    pub volume: f32,
}

/// The player's view of the rest of the app. Also the seam used by tests.
pub trait PlayerHost: Send + Sync + 'static {
    fn track(&self, id: TrackId) -> Option<PlayableTrack>;
    /// Whether the track is (still) there; asked for every queue item whenever the
    /// library changes, so a host can answer it without building a [`PlayableTrack`].
    fn has_track(&self, id: TrackId) -> bool {
        self.track(id).is_some()
    }
    /// The album a track counts as for shuffling by album: a host that shows albums
    /// tagged inconsistently as one gives that one's key.
    fn album_of(&self, id: TrackId) -> Option<u64> {
        self.track(id).map(|t| t.album_key)
    }
    /// Whether the track's file names an album. Songs without one that lie in one
    /// folder count as an album where albums are shown or shuffled; they are loose
    /// songs all the same, and follow each other as songs of different albums do.
    fn names_an_album(&self, _id: TrackId) -> bool {
        true
    }
    /// The stars the listener gave a track (0: none), for a shuffle that goes by them.
    fn rating_of(&self, _id: TrackId) -> u8 {
        0
    }
    /// A track was listened to (see `PLAY_CREDIT_MS`).
    fn record_play(&self, id: TrackId);
    /// Radio: up to `count` tracks to follow `seed` when the queue has run out, none of
    /// them in `exclude`. A host without a library to pick from has none.
    fn radio_picks(&self, _seed: TrackId, _exclude: &[TrackId], _count: usize) -> Vec<TrackId> {
        Vec::new()
    }
    /// Where the user left off in a long track (see "resume long tracks").
    fn resume_point(&self, _id: TrackId) -> Option<u64> {
        None
    }
    /// Remembers where the user left off in a long track, or forgets it (`None`).
    fn set_resume_point(&self, _id: TrackId, _position_ms: Option<u64>) {}
    fn load_session(&self) -> Option<Session>;
    /// `queue_changed` is false when only the position or volume differ from the last
    /// successful save. Returns false if the session could not be saved; the player then
    /// keeps it marked as unsaved (the queue too, if it was part of it) and tries again.
    fn save_session(&self, session: &Session, queue_changed: bool) -> bool;
    /// The snapshot changed (repaint, update Now Playing). Called from the player thread.
    fn changed(&self);
    /// Asked right before sound is made: whether it may be made now. A phone gives the
    /// sound to one app at a time; where something else has it (a call), the answer is
    /// no, the player waits as through an interruption, and the host says when it is
    /// over ([`Command::InterruptionOver`]).
    fn may_sound(&self) -> bool {
        true
    }
    /// An interruption is over and the player has answered it: it plays on (`true`) or
    /// stays as it is.
    fn interruption_settled(&self, _resumed: bool) {}
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Replace the queue with `tracks` and play from `start_index` (double-click in a list).
    PlayTracks {
        tracks: Vec<TrackId>,
        start_index: usize,
    },
    PlayNext(Vec<TrackId>),
    Enqueue(Vec<TrackId>),
    /// Play this queue item (double-click in the queue panel).
    JumpTo(ItemId),
    RemoveItems(Vec<ItemId>),
    MoveItems {
        ids: Vec<ItemId>,
        to_index: usize,
    },
    ClearQueue,
    Play,
    Pause,
    TogglePlayPause,
    Stop,
    Next,
    Previous,
    SeekTo(u64),
    SeekRelative(i64),
    SetVolume(f32),
    SetShuffle(bool),
    SetRepeat(RepeatMode),
    SetReplayGain(ReplayGainSettings),
    SetEq(EqSettings),
    SetCrossfeed(Crossfeed),
    /// The parametric correction profile (headphone correction); `None` switches it off.
    SetCorrection(Option<PeqProfile>),
    /// Room correction: everything is filtered with this impulse response (or with
    /// none). What plays starts over at the place it is at.
    SetRoom(Option<Arc<crate::audio::room::Impulse>>),
    /// Radio on or off: when the queue runs out, go on with songs like the last one.
    SetRadio(bool),
    /// Shuffle whole albums instead of single tracks.
    SetShuffleByAlbum(bool),
    /// Shuffle with the listener's stars in mind: songs with more of them are more
    /// likely to come early (see `Queue::set_shuffle_by_rating`).
    SetShuffleByRating(bool),
    /// Start long tracks where they were left off.
    SetResumeLongTracks(bool),
    /// Stop when the song that is on now has ended, with the next one ready to play.
    /// Applies to this one song: it is over once playback has stopped there or another
    /// song was started.
    SetStopAfterCurrent(bool),
    /// Marks a section of the current song to play over and over ("A–B repeat"): the
    /// first time where it begins, the second time where it ends (from then on playing
    /// goes back to its beginning whenever it gets there), the third time it is given
    /// up. It belongs to this one song: another one being current ends it. Not kept in
    /// the session.
    MarkSection,
    /// Gives the section up, however far it was marked.
    ClearSection,
    /// Crossfade: for how many seconds the end of a song is mixed with the beginning
    /// of the one that follows it by itself (0: not at all). Songs of one album are
    /// joined as they are whatever this says, and so is every song the listener
    /// starts.
    SetCrossfade(u8),
    /// How fast songs go, in percent of their own speed (50 to 200; 100: as they
    /// are). The pitch stays. A song that is playing goes on from where it is at the
    /// new speed.
    SetSpeed(u16),
    /// Sleep timer: pause when this much time has passed (`None` switches it off).
    /// The sound fades out over the last seconds; the volume setting is not changed.
    SetSleepTimer(Option<Duration>),
    /// `None` follows the system default device.
    SetOutputDevice(Option<String>),
    /// Rate matching, exclusive access, memory playback.
    SetOutput(OutputSettings),
    /// After a lost device: open the preferred (or default) device again and continue.
    RetryOutput,
    SystemWillSleep,
    /// Something else took the sound (a call): silent until it gives it back.
    Interrupted,
    /// It gave the sound back. Playing goes on if `resume` says it may, the
    /// interruption was what stopped it, and the listener has not decided otherwise
    /// in between (stopped, paused or started something).
    InterruptionOver {
        resume: bool,
    },
    /// Write the session now (the app is leaving the screen and may be ended there).
    SaveSession,
    /// Tracks may have left the library; drop them from the queue.
    LibraryChanged,
    Shutdown,
}

impl Command {
    /// Whether the command is the listener's word on playing or not playing. While
    /// something else has the sound, such a word stands: playing does not start again
    /// by itself when the sound comes back.
    fn decides_playing(&self) -> bool {
        matches!(
            self,
            Command::PlayTracks { .. }
                | Command::JumpTo(_)
                | Command::ClearQueue
                | Command::Play
                | Command::Pause
                | Command::TogglePlayPause
                | Command::Stop
                | Command::RetryOutput
                | Command::SystemWillSleep
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayState {
    /// Nothing to play.
    Idle,
    /// A track was requested and is being opened/buffered.
    Loading,
    Playing,
    Paused,
    /// A seek is in progress; `position_ms` already shows the target.
    Seeking,
    /// The queue was played to its end.
    Ended,
    /// Playback stopped because of an error (see the notice); the queue is kept.
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    /// A track could not be played and was skipped.
    TrackSkipped {
        track_id: TrackId,
        kind: PlaybackErrorKind,
    },
    /// Too many tracks in a row failed; playback stopped.
    TooManyFailures,
    /// The output device was disconnected; playback is paused.
    DeviceLost,
    /// No usable output device.
    OutputUnavailable(OutputError),
}

/// A section of the current song marked for repeating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionRepeat {
    /// Where it begins, in the song's time.
    pub from_ms: u64,
    /// Where it ends, once that is marked: it is repeated from then on.
    pub to_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NowPlaying {
    pub item_id: ItemId,
    pub track_id: TrackId,
    pub duration_ms: u64,
}

#[derive(Debug, Clone)]
pub struct PlayerSnapshot {
    pub state: PlayState,
    pub current: Option<NowPlaying>,
    /// Position at `position_at`. While `state` is `Playing`, readers add the elapsed time.
    pub position_ms: u64,
    pub position_at: Instant,
    pub volume: f32,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub queue: Arc<Vec<QueueItem>>,
    /// Changes whenever `queue` does.
    pub queue_revision: u64,
    pub queue_index: Option<usize>,
    pub notice: Option<Notice>,
    /// Increments with every new notice.
    pub notice_seq: u64,
    /// Name of the device the stream is open on.
    pub output_device: Option<String>,
    pub device_lost: bool,
    pub replaygain: ReplayGainSettings,
    pub device_pref: Option<String>,
    /// Callbacks that ran out of audio while playing (diagnostics).
    pub underruns: u64,
    /// Format of the track being decoded (for the info panel).
    pub source: Option<SourceInfo>,
    pub output_sample_rate: Option<u32>,
    /// The open output stream: device, rate, hardware format, exclusive access.
    pub output: Option<OutputStatus>,
    pub output_settings: OutputSettings,
    /// Width of the device's integer format while dither is being added for it.
    pub dither_bits: Option<u32>,
    /// The equalizer as it is set.
    pub eq: EqSettings,
    /// The headphone crossfeed as it is set.
    pub crossfeed: Crossfeed,
    /// Name of the parametric correction profile that is applied, if one is.
    pub correction: Option<String>,
    /// Name of the room correction that is applied, if one is.
    pub room: Option<String>,
    /// Radio is on (see `Command::SetRadio`).
    pub radio: bool,
    /// Playback stops when the current song ends (see `Command::SetStopAfterCurrent`).
    pub stop_after_current: bool,
    /// When the sleep timer will pause playback, while one is set.
    pub sleep_at: Option<Instant>,
    /// The section of the current song that is marked for repeating, if one is (see
    /// `Command::MarkSection`).
    pub section: Option<SectionRepeat>,
    /// Seconds of crossfade between songs, as it is set (0: none).
    pub crossfade_secs: u8,
    /// How fast songs go, in percent of their own speed, as it is set.
    pub speed_percent: u16,
    /// Frames of the current track, as far as it has been decoded, that would have
    /// exceeded full scale and had their gain turned down instead of being clipped.
    pub peak_limited_frames: u64,
    /// Time from the most recent start request (play a track, seek) until its audio
    /// reached the output callback, in milliseconds.
    pub last_start_latency_ms: Option<u32>,
}

impl PlayerSnapshot {
    fn initial(volume: f32, rg: ReplayGainSettings) -> Self {
        Self {
            state: PlayState::Idle,
            current: None,
            position_ms: 0,
            position_at: Instant::now(),
            volume,
            shuffle: false,
            repeat: RepeatMode::Off,
            queue: Arc::new(Vec::new()),
            queue_revision: 0,
            queue_index: None,
            notice: None,
            notice_seq: 0,
            output_device: None,
            device_lost: false,
            replaygain: rg,
            device_pref: None,
            underruns: 0,
            source: None,
            output_sample_rate: None,
            output: None,
            output_settings: OutputSettings::default(),
            dither_bits: None,
            eq: EqSettings::default(),
            crossfeed: Crossfeed::Off,
            correction: None,
            room: None,
            radio: false,
            stop_after_current: false,
            sleep_at: None,
            section: None,
            crossfade_secs: 0,
            speed_percent: 100,
            peak_limited_frames: 0,
            last_start_latency_ms: None,
        }
    }

    /// Position right now, extrapolated while playing and clamped to the duration.
    pub fn position_now_ms(&self) -> u64 {
        let mut pos = self.position_ms;
        if self.state == PlayState::Playing {
            // The song moves on by its speed.
            pos +=
                self.position_at.elapsed().as_millis() as u64 * u64::from(self.speed_percent) / 100;
        }
        match self.current {
            Some(c) if c.duration_ms > 0 => pos.min(c.duration_ms),
            _ => pos,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PlayerConfig {
    pub default_volume: f32,
    pub replaygain: ReplayGainSettings,
    pub eq: EqSettings,
    pub crossfeed: Crossfeed,
    /// Seconds of crossfade between songs (0: none).
    pub crossfade_secs: u8,
    /// How fast songs go, in percent of their own speed (100: as they are).
    pub speed_percent: u16,
    /// The parametric correction profile to start with.
    pub correction: Option<PeqProfile>,
    /// The room correction to start with.
    pub room: Option<Arc<crate::audio::room::Impulse>>,
    pub radio: bool,
    pub shuffle_by_album: bool,
    /// Shuffle by stars (see `Command::SetShuffleByRating`).
    pub shuffle_by_rating: bool,
    pub resume_long_tracks: bool,
    pub device_pref: Option<String>,
    pub output: OutputSettings,
    /// Playback that is wanted but produces no audio for this long is treated as a
    /// failure (a read stuck on a dead drive, a device that stopped asking for audio).
    pub stall_timeout: Duration,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self {
            default_volume: 0.8,
            replaygain: ReplayGainSettings::default(),
            eq: EqSettings::default(),
            crossfeed: Crossfeed::Off,
            crossfade_secs: 0,
            speed_percent: 100,
            correction: None,
            room: None,
            radio: false,
            shuffle_by_album: false,
            shuffle_by_rating: false,
            resume_long_tracks: false,
            device_pref: None,
            output: OutputSettings::default(),
            stall_timeout: Duration::from_secs(15),
        }
    }
}

/// Cloneable handle used by the UI and platform code.
#[derive(Clone)]
pub struct PlayerHandle {
    tx: Sender<Command>,
    snapshot: Arc<Mutex<PlayerSnapshot>>,
    /// The engine's level meter (the engine is built on the player thread).
    meter: Arc<OnceLock<Arc<LevelMeter>>>,
    /// The engine's sample scope, likewise.
    scope: Arc<OnceLock<Arc<Scope>>>,
    /// When the player thread last went round its loop.
    pulse: Arc<Pulse>,
}

/// The player thread's sign of life: it notes the time every time it goes round its
/// loop (at least five times a second). A thread stuck inside a call that never returns,
/// which is what an output device driver that has hung looks like from here, stops
/// doing so, and that is all anybody can know about it.
struct Pulse {
    since: Instant,
    last_beat_ms: AtomicU64,
}

impl Pulse {
    fn new() -> Self {
        Self {
            since: Instant::now(),
            last_beat_ms: AtomicU64::new(0),
        }
    }
    fn beat(&self) {
        self.last_beat_ms
            .store(self.since.elapsed().as_millis() as u64, Ordering::Relaxed);
    }
    fn silent_for(&self) -> Duration {
        let last = Duration::from_millis(self.last_beat_ms.load(Ordering::Relaxed));
        self.since.elapsed().saturating_sub(last)
    }
}

impl PlayerHandle {
    pub fn send(&self, command: Command) {
        let _ = self.tx.send(command);
    }

    pub fn snapshot(&self) -> PlayerSnapshot {
        match self.snapshot.lock() {
            Ok(s) => s.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Signal level of what is playing right now (for the VU meter). Silence until the
    /// engine exists. Reading takes the peaks: one reader only.
    pub fn levels(&self) -> Levels {
        self.meter.get().map(|m| m.read()).unwrap_or_default()
    }

    /// How long the player thread has not been heard from. Normally well under half a
    /// second; switching an output device's format can take a few seconds. Much longer
    /// means the thread is stuck in a call that does not return (a hung device driver),
    /// and no command will be acted on until it does.
    pub fn unresponsive_for(&self) -> Duration {
        self.pulse.silent_for()
    }

    /// The most recent samples of what is playing (for the spectrum display), oldest
    /// first; silence until the engine exists.
    pub fn scope_samples(&self, out: &mut [f32]) {
        match self.scope.get() {
            Some(scope) => scope.latest(out),
            None => out.fill(0.0),
        }
    }
}

/// Starts the player thread with a real output device.
pub fn spawn<H: PlayerHost>(
    host: Arc<H>,
    config: PlayerConfig,
) -> (PlayerHandle, std::thread::JoinHandle<()>) {
    let cfg = config.clone();
    spawn_with_engine(host, config, move |volume| {
        let mut engine = Engine::new(cfg.device_pref.clone(), volume, cfg.replaygain);
        engine.set_output_settings(cfg.output);
        engine
    })
}

/// Starts the player thread with a caller-supplied engine (tests use an offline engine).
/// The engine is built on the player thread because device streams are not `Send`.
pub fn spawn_with_engine<H: PlayerHost>(
    host: Arc<H>,
    config: PlayerConfig,
    make_engine: impl FnOnce(f32) -> Engine + Send + 'static,
) -> (PlayerHandle, std::thread::JoinHandle<()>) {
    let (tx, rx) = channel();
    let snapshot = Arc::new(Mutex::new(PlayerSnapshot::initial(
        config.default_volume,
        config.replaygain,
    )));
    let shared = snapshot.clone();
    let meter = Arc::new(OnceLock::new());
    let meter_slot = meter.clone();
    let scope = Arc::new(OnceLock::new());
    let scope_slot = scope.clone();
    let pulse = Arc::new(Pulse::new());
    let thread_pulse = pulse.clone();
    let thread = std::thread::Builder::new()
        .name("oneum-player".into())
        .spawn(move || {
            let session = host.load_session();
            let volume = session
                .as_ref()
                .map_or(config.default_volume, |s| s.volume)
                .clamp(0.0, 1.0);
            let engine = make_engine(volume);
            engine.set_eq(config.eq);
            engine.set_crossfeed(config.crossfeed);
            engine.set_correction(config.correction.clone());
            let _ = meter_slot.set(engine.shared().meter().clone());
            let _ = scope_slot.set(engine.shared().scope().clone());
            Player::new(host, engine, rx, shared, config, session, volume).run(&thread_pulse);
        })
        .unwrap_or_else(|e| {
            // Without a player thread the app cannot play anything; commands are dropped.
            tracing::error!("cannot start player thread: {e}");
            std::thread::spawn(|| {})
        });
    (
        PlayerHandle {
            tx,
            snapshot,
            meter,
            scope,
            pulse,
        },
        thread,
    )
}

/// A track the decode thread has started writing into the current feed.
#[derive(Debug, Clone)]
struct Segment {
    item_id: ItemId,
    track_id: TrackId,
    at_frame: u64,
    start_ms: u64,
    duration_ms: u64,
    info: SourceInfo,
    /// The samples of this track are not the file's own by the time they reach the
    /// device (see `Player::source_is_processed`).
    processed: bool,
    /// What the engine's count of peak-limited frames stood at when this track began.
    limited_before: u64,
}

/// How much of the current playthrough of a queue item has really been heard.
///
/// This lives outside the feed on purpose: a seek, a queue edit or a device change
/// replaces the feed, but it is still the same playthrough and must count once.
#[derive(Debug, Default)]
struct Listen {
    item_id: Option<ItemId>,
    /// How much of the song was heard, in microseconds (frames are converted at the
    /// feed's rate and speed).
    heard_us: u64,
    credited: bool,
    /// Feed position up to which audio has been accounted: `(epoch, frames)`.
    mark: Option<(u16, u64)>,
}

impl Listen {
    fn reset(&mut self) {
        *self = Self::default();
    }

    /// Accounts for feed `epoch` having played up to `frames` while `seg` is audible.
    fn hear(&mut self, seg: &Segment, epoch: u16, frames: u64, rate: u32, speed_percent: u16) {
        if self.item_id != Some(seg.item_id) {
            *self = Self {
                item_id: Some(seg.item_id),
                ..Self::default()
            };
        }
        let from = match self.mark {
            Some((e, f)) if e == epoch => f.max(seg.at_frame),
            _ => seg.at_frame,
        };
        let upto = frames.max(from);
        self.heard_us += song_ms((upto - from).saturating_mul(1_000), rate, speed_percent);
        self.mark = Some((epoch, upto));
    }

    /// True exactly once per playthrough: when `threshold_ms` of audio has been heard.
    fn take_credit(&mut self, threshold_ms: u64) -> bool {
        if self.credited || self.heard_us / 1000 < threshold_ms {
            return false;
        }
        self.credited = true;
        true
    }
}

/// Listening time after which a track of `length_ms` counts as played.
fn credit_threshold_ms(length_ms: u64) -> u64 {
    if length_ms > 0 {
        (length_ms / 2).min(PLAY_CREDIT_MS)
    } else {
        PLAY_CREDIT_MS
    }
}

/// One feed of the engine and what the player knows about its contents.
struct Chain {
    epoch: u16,
    slot: Arc<NextSlot>,
    rate: u32,
    /// Rate the device was asked to run at for this feed (rate matching); `None` when
    /// the device was left alone. A following track that asks for another rate cannot
    /// continue this feed.
    target_rate: Option<u32>,
    first_item: QueueItem,
    /// Decoded tracks, oldest first. The front one is what is heard once the renderer
    /// has switched to this feed.
    segments: VecDeque<Segment>,
    /// Position to show until audio of this feed is heard.
    pending_start_ms: u64,
    /// A track of this feed failed (while opening, or mid-way).
    failed: Option<ItemId>,
    is_seek: bool,
    /// How fast the songs of this feed go, in percent of their own speed.
    speed_percent: u16,
}

impl Chain {
    /// How much of a song `frames` frames of this feed's output are, in milliseconds:
    /// at twice the speed, a second of output is two seconds of the song.
    fn song_ms(&self, frames: u64) -> u64 {
        song_ms(frames, self.rate, self.speed_percent)
    }
}

/// See [`Chain::song_ms`].
fn song_ms(frames: u64, rate: u32, speed_percent: u16) -> u64 {
    (u128::from(frames) * 10 * u128::from(speed_percent) / u128::from(rate.max(1))) as u64
}

enum StartError {
    Track(PlaybackErrorKind),
    Output(OutputError),
}

struct Player<H: PlayerHost> {
    host: Arc<H>,
    engine: Engine,
    rx: Receiver<Command>,
    shared: Arc<Mutex<PlayerSnapshot>>,
    queue: Queue,
    queue_revision: u64,
    queue_arc: Arc<Vec<QueueItem>>,
    /// `Queue::revision` that `queue_arc` was copied at.
    copied_queue_revision: u64,
    state: PlayState,
    /// What the user asked for; the engine follows once audio is available.
    want_playing: bool,
    /// Playing was stopped because something else took the sound, and is to go on when
    /// the sound comes back (see [`Command::InterruptionOver`]).
    interrupted: bool,
    chain: Option<Chain>,
    /// Position used when there is no feed (restored session, prepared-while-paused, lost device).
    resume_ms: u64,
    volume: f32,
    replaygain: ReplayGainSettings,
    eq: EqSettings,
    crossfeed: Crossfeed,
    /// Name of the correction profile that is applied (one that changes nothing is
    /// not applied).
    correction: Option<String>,
    /// Name of the room correction that is applied, if one is.
    room: Option<String>,
    radio: bool,
    /// Start long tracks where they were left off (see `start_fresh`).
    resume_long_tracks: bool,
    /// The queue item after which playback stops.
    stop_after: Option<ItemId>,
    /// Sleep timer: when to pause.
    sleep_at: Option<Instant>,
    /// The section marked for repeating, with the queue item it belongs to.
    section: Option<(ItemId, SectionRepeat)>,
    /// Seconds of crossfade between songs (0: none), see `Command::SetCrossfade`.
    crossfade_secs: u8,
    /// How fast songs go, in percent of their own speed, see `Command::SetSpeed`.
    speed_percent: u16,
    /// When the volume the sleep timer faded out is to be set back (see
    /// [`SLEEP_RESTORE_AFTER`]).
    sleep_restore_at: Option<Instant>,
    /// While set: audio decoded with processing that has since been switched off is
    /// still being played (see `review_processed`).
    processed_until: Option<Instant>,
    notice: Option<Notice>,
    notice_seq: u64,
    device_lost: bool,
    consecutive_failures: usize,
    dirty: bool,
    /// The queue's items changed (the queue table must be rewritten).
    queue_dirty_since: Option<Instant>,
    /// Only the current item, repeat mode or the like changed (cheap to save).
    meta_dirty_since: Option<Instant>,
    listen: Listen,
    stall_timeout: Duration,
    /// Since when playback is wanted but no audio is coming.
    starved_since: Option<Instant>,
    /// Ticks in a row that saw no audio moving.
    starved_ticks: u32,
    /// Renderer position at the previous tick, to tell whether audio is moving.
    last_render_position: (u16, u64),
    /// Since when a feed exists although playback is paused.
    paused_since: Option<Instant>,
    last_session_save: Instant,
    /// Set after a failed session save: no timed save is tried before then.
    session_retry_at: Option<Instant>,
    last_position_publish: Instant,
    running: bool,
    start_requested_at: Option<Instant>,
    last_start_latency_ms: Option<u32>,
}

impl<H: PlayerHost> Player<H> {
    fn new(
        host: Arc<H>,
        mut engine: Engine,
        rx: Receiver<Command>,
        shared: Arc<Mutex<PlayerSnapshot>>,
        config: PlayerConfig,
        session: Option<Session>,
        volume: f32,
    ) -> Self {
        engine.set_speed(f32::from(crate::audio::pace::speed_of(config.speed_percent)) / 100.0);
        engine.set_room(config.room.clone());
        let (mut queue, resume_ms) = match session {
            Some(s) => {
                let mut queue = Queue::from_state(s.queue);
                // Tracks may have left the library since the last run.
                let current_removed = queue.retain_tracks(|t| host.has_track(t));
                (queue, if current_removed { 0 } else { s.position_ms })
            }
            None => (Queue::new(), 0),
        };
        let lookup = host.clone();
        queue.set_album_lookup(Box::new(move |track| {
            lookup
                .album_of(track)
                .unwrap_or(u64::MAX ^ track.cast_unsigned())
        }));
        let stars = host.clone();
        queue.set_rating_lookup(Box::new(move |track| stars.rating_of(track)));
        // As it was: a restored shuffled queue keeps its order.
        queue.restore_shuffle_by_album(config.shuffle_by_album);
        queue.restore_shuffle_by_rating(config.shuffle_by_rating);
        let state = if queue.current().is_some() {
            PlayState::Paused
        } else {
            PlayState::Idle
        };
        let queue_arc = Arc::new(queue.items().to_vec());
        let copied_queue_revision = queue.revision();
        Self {
            host,
            engine,
            rx,
            shared,
            queue,
            queue_revision: 1,
            queue_arc,
            copied_queue_revision,
            state,
            want_playing: false, // never start making sound on launch
            interrupted: false,
            chain: None,
            resume_ms,
            volume,
            replaygain: config.replaygain,
            eq: config.eq.sanitized(),
            crossfeed: config.crossfeed,
            correction: config
                .correction
                .as_ref()
                .filter(|p| !p.is_flat())
                .map(|p| p.name.clone()),
            room: config.room.as_ref().map(|impulse| impulse.name.clone()),
            radio: config.radio,
            resume_long_tracks: config.resume_long_tracks,
            stop_after: None,
            sleep_at: None,
            section: None,
            crossfade_secs: config.crossfade_secs,
            speed_percent: crate::audio::pace::speed_of(config.speed_percent),
            sleep_restore_at: None,
            processed_until: None,
            notice: None,
            notice_seq: 0,
            device_lost: false,
            consecutive_failures: 0,
            dirty: true,
            queue_dirty_since: None,
            meta_dirty_since: None,
            listen: Listen::default(),
            stall_timeout: config.stall_timeout,
            starved_since: None,
            starved_ticks: 0,
            last_render_position: (0, 0),
            paused_since: None,
            last_session_save: Instant::now(),
            session_retry_at: None,
            last_position_publish: Instant::now(),
            running: true,
            start_requested_at: None,
            last_start_latency_ms: None,
        }
    }

    fn run(mut self, pulse: &Pulse) {
        let mut events = Vec::new();
        while self.running {
            pulse.beat();
            // While the sleep timer is fading the sound out, the volume moves every tick.
            let fading = self
                .sleep_at
                .is_some_and(|at| at.saturating_duration_since(Instant::now()) <= SLEEP_FADE);
            let busy = fading
                || self.sleep_restore_at.is_some()
                || self.chain.is_some()
                    && (self.want_playing
                        || matches!(self.state, PlayState::Loading | PlayState::Seeking));
            match self
                .rx
                .recv_timeout(if busy { TICK_ACTIVE } else { TICK_IDLE })
            {
                Ok(command) => {
                    self.handle(command);
                    // Drain a burst (e.g. slider drags) before doing any work for it.
                    while let Ok(command) = self.rx.try_recv() {
                        self.handle(command);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => self.running = false,
            }
            events.clear();
            self.engine.poll(&mut events);
            for event in events.drain(..) {
                self.on_engine_event(event);
            }
            self.follow_playback();
            // "Stop after this song" belongs to one song: another one being current
            // (next, previous, a double-click, the song removed) ends it.
            if self.stop_after.is_some() && self.stop_after != self.queue.current().map(|i| i.id) {
                self.stop_after = None;
                self.dirty = true;
            }
            self.follow_section();
            self.follow_sleep_timer();
            self.watch_for_stall();
            self.release_paused_feed();
            if self
                .processed_until
                .is_some_and(|until| Instant::now() >= until)
            {
                self.processed_until = None;
                self.sync_processed();
                self.dirty = true;
            }
            self.publish_if_needed();
            self.save_session_if_due();
        }
        self.engine.set_playing(false);
        self.save_session(true);
        self.engine.close_output();
    }

    // ----- helpers ---------------------------------------------------------------------

    fn mark_queue_changed(&mut self) {
        self.sync_queue_copy();
        self.dirty = true;
    }

    /// Brings the published copy of the queue in line with the queue. The queue also
    /// changes on its own (it is reshuffled when a shuffled "repeat all" pass wraps), so
    /// this is checked before every publish rather than trusted to each command.
    fn sync_queue_copy(&mut self) {
        if self.queue.revision() == self.copied_queue_revision {
            return;
        }
        self.copied_queue_revision = self.queue.revision();
        self.queue_revision += 1;
        self.queue_arc = Arc::new(self.queue.items().to_vec());
        self.queue_dirty_since.get_or_insert_with(Instant::now);
    }

    fn set_notice(&mut self, notice: Notice) {
        self.notice = Some(notice);
        self.notice_seq += 1;
        self.dirty = true;
    }

    fn position_ms(&self) -> u64 {
        match &self.chain {
            Some(chain) => {
                let (epoch, frames) = self.engine.played();
                match chain.segments.front() {
                    Some(seg) if epoch == chain.epoch => {
                        seg.start_ms + chain.song_ms(frames.saturating_sub(seg.at_frame))
                    }
                    _ => chain.pending_start_ms,
                }
            }
            None => self.resume_ms,
        }
    }

    fn current_duration_ms(&self) -> u64 {
        if let Some(seg) = self.chain.as_ref().and_then(|c| c.segments.front()) {
            return seg.duration_ms;
        }
        self.queue
            .current()
            .and_then(|i| self.host.track(i.track))
            .map_or(0, |t| t.duration_ms)
    }

    fn request_for(&self, item: QueueItem) -> Result<TrackRequest, PlaybackErrorKind> {
        match self.host.track(item.track) {
            Some(t) if t.available => Ok(TrackRequest {
                item_id: item.id,
                track_id: item.track,
                path: t.path,
                replaygain: t.replaygain,
                crossfade_ms: 0,
            }),
            _ => Err(PlaybackErrorKind::FileMissing),
        }
    }

    /// Radio: when `item_id` is the last thing the queue has to play, adds songs that
    /// go with it to the end of the queue. Called when that item starts being decoded,
    /// so what is added follows without a gap like anything else in the queue.
    /// Returns true if the queue grew.
    fn extend_with_radio(&mut self, item_id: ItemId) -> bool {
        // Repeat is a way of not ending already (and with shuffle its next round is
        // only decided at the wrap, so "nothing follows" does not mean the end).
        if !self.radio
            || self.queue.repeat() != RepeatMode::Off
            || self.queue.peek_after(item_id).is_some()
        {
            return false;
        }
        let Some(seed) = self.queue.items().iter().find(|i| i.id == item_id) else {
            return false;
        };
        let in_queue: Vec<TrackId> = self.queue.items().iter().map(|i| i.track).collect();
        let picks = self.host.radio_picks(seed.track, &in_queue, RADIO_BATCH);
        if picks.is_empty() {
            return false;
        }
        self.queue.append(&picks);
        self.mark_queue_changed();
        true
    }

    /// Tells the decode thread what follows `item_id`.
    fn announce_next_after(&self, item_id: ItemId) {
        let Some(chain) = &self.chain else { return };
        if self.stop_after == Some(item_id) {
            // Nothing follows in this feed: playback stops when it has been played.
            chain.slot.set(NextTrack::End);
            return;
        }
        let next = match self.queue.peek_after(item_id) {
            // An unplayable next track ends the feed; it is skipped when we get there.
            // So does one the device should run at another rate for: the feed ends at
            // the boundary and the next track starts a new one on the reconfigured device.
            Some(next) if self.same_device_rate(chain, next.track) => {
                let after = self
                    .queue
                    .items()
                    .iter()
                    .find(|item| item.id == item_id)
                    .map(|item| item.track);
                // (A song of which a section is repeated does not end by itself:
                // nothing is mixed into what would be the middle of a round.)
                let repeated = self
                    .section
                    .as_ref()
                    .is_some_and(|(of, section)| *of == item_id && section.to_ms.is_some());
                self.request_for(next)
                    .map(|mut request| {
                        request.crossfade_ms = after
                            .filter(|_| !repeated)
                            .map_or(0, |after| self.crossfade_between(after, next.track));
                        request
                    })
                    .map_or(NextTrack::End, NextTrack::Track)
            }
            _ => NextTrack::End,
        };
        chain.slot.set(next);
    }

    /// For how long the end of `ending` is mixed with the beginning of `beginning`
    /// when the one follows the other by itself, in milliseconds (0: they are joined
    /// as they are). The songs of one album follow each other as the album has them,
    /// without a gap and without a fade; a fade never takes more than half of either
    /// song.
    fn crossfade_between(&self, ending: TrackId, beginning: TrackId) -> u32 {
        let one_album = self.host.album_of(ending) == self.host.album_of(beginning)
            && self.host.names_an_album(ending)
            && self.host.names_an_album(beginning);
        if self.crossfade_secs == 0 || one_album {
            return 0;
        }
        let length = |track: TrackId| self.host.track(track).map_or(0, |t| t.duration_ms);
        // (A song lasts as long as its length says at its own speed only.)
        let lasts = |track: TrackId| length(track) * 100 / u64::from(self.speed_percent.max(1));
        let fade = (u64::from(self.crossfade_secs) * 1000)
            .min(lasts(ending) / 2)
            .min(lasts(beginning) / 2);
        if fade < CROSSFADE_MIN_MS {
            0
        } else {
            fade as u32
        }
    }

    /// Sample rate of a track as the library knows it.
    fn rate_hint(&self, track: TrackId) -> Option<u32> {
        self.host
            .track(track)
            .map(|t| t.sample_rate)
            .filter(|r| *r > 0)
    }

    /// Whether `track` can follow in `chain` without the device changing its rate.
    /// Always true unless rate matching is on.
    fn same_device_rate(&self, chain: &Chain, track: TrackId) -> bool {
        match self.engine.preferred_rate(self.rate_hint(track)) {
            Some(rate) => rate == chain.target_rate.unwrap_or(chain.rate),
            None => true,
        }
    }

    /// Whether what is decoded from this track is no longer the file's own samples by
    /// the time it reaches the device: decoded from a lossy codec, resampled, mapped to
    /// stereo, scaled by ReplayGain, equalized, or wider than the device's sample format. Such
    /// audio is dithered when the device takes integers (see `Renderer`).
    fn source_is_processed(&self, info: &SourceInfo, track_id: TrackId) -> bool {
        let track = self.host.track(track_id);
        // (WavPack may hold floats: those are 32 bits wide, wider than any device
        // takes, and count as processed by their width.)
        let integer_pcm = matches!(info.codec.as_str(), "flac" | "alac" | "wavpack" | "ape")
            || info.codec.starts_with("pcm_s")
            || info.codec.starts_with("pcm_u");
        let device_bits = self.engine.hardware_bits().map(|(bits, _)| bits);
        // Unknown widths count as "does not fit": dither is harmless, truncation is not.
        let fits = match (track.as_ref().and_then(|t| t.bit_depth), device_bits) {
            (Some(source), Some(device)) => u32::from(source) <= device,
            _ => false,
        };
        let gain = track.map_or(1.0, |t| replaygain_factor(&t.replaygain, &self.replaygain));
        // (With crossfade on, where two songs meet is a mix of both: all of it counts
        // as processed rather than following each meeting to the sample.)
        self.eq.is_active()
            || self.speed_percent != 100
            || self.crossfade_secs > 0
            || self.crossfeed != Crossfeed::Off
            || self.correction.is_some()
            || self.room.is_some()
            || !integer_pcm
            || !fits
            || info.channels != 2
            || Some(info.sample_rate) != self.engine.sample_rate()
            || gain != 1.0
    }

    /// Tells the output whether to dither (on an integer device): while any track that
    /// is in the feed, audible or decoded ahead, is not the file's own samples. The
    /// boundary between two tracks is not followed to the sample: for up to the second
    /// the feed is ahead, an untouched track next to a processed one is dithered too,
    /// which is harmless; the other way round would leave rounding undithered.
    fn sync_processed(&mut self) {
        let processed = self.processed_until.is_some()
            || self
                .chain
                .as_ref()
                .is_some_and(|c| c.segments.iter().any(|s| s.processed));
        self.engine.set_processed(processed);
    }

    /// A setting changed that decides whether the tracks in the feed are still the
    /// files' own samples: looks at each of them again.
    fn review_processed(&mut self) {
        let was_processed = self
            .chain
            .as_ref()
            .is_some_and(|c| c.segments.iter().any(|s| s.processed));
        let verdicts: Vec<bool> = self.chain.as_ref().map_or_else(Vec::new, |c| {
            c.segments
                .iter()
                .map(|s| self.source_is_processed(&s.info, s.track_id))
                .collect()
        });
        if let Some(chain) = self.chain.as_mut() {
            for (segment, processed) in chain.segments.iter_mut().zip(verdicts) {
                segment.processed = processed;
            }
        }
        let is_processed = self
            .chain
            .as_ref()
            .is_some_and(|c| c.segments.iter().any(|s| s.processed));
        if was_processed && !is_processed {
            // What was decoded with the old settings is still on its way out (up to
            // a second in the ring, and the equalizer takes half a second more to
            // fade away): it keeps its dither until it has been played.
            self.processed_until = Some(Instant::now() + PROCESSED_TAIL);
        }
        self.sync_processed();
        self.dirty = true;
    }

    /// See `PlayerSnapshot::peak_limited_frames`.
    fn peak_limited_frames(&self) -> u64 {
        let Some(chain) = self.chain.as_ref() else {
            return 0;
        };
        let mut segments = chain.segments.iter();
        let Some(current) = segments.next() else {
            return 0;
        };
        // The count runs on with the track that is decoded ahead: up to where that
        // one began, it is this track's.
        let end = segments
            .next()
            .map_or_else(|| self.engine.limited_frames(), |next| next.limited_before);
        end.saturating_sub(current.limited_before)
    }

    /// Gives the output device back (exclusive access, changed sample rate) when
    /// nothing is going to be played on it for now.
    fn release_device(&mut self) {
        if self.engine.holds_device() {
            self.engine.close_output();
            self.dirty = true;
        }
    }

    fn drop_chain(&mut self) {
        self.chain = None;
        self.engine.stop_feeds();
    }

    fn become_idle(&mut self) {
        self.drop_chain();
        self.want_playing = false;
        self.engine.set_playing(false);
        self.resume_ms = 0;
        self.state = PlayState::Idle;
        self.dirty = true;
        self.release_device();
    }

    /// Stops with the current item ready to be played from its start.
    fn halt(&mut self) {
        self.drop_chain();
        self.want_playing = false;
        self.engine.set_playing(false);
        self.resume_ms = 0;
        self.state = PlayState::Paused;
        self.listen.reset();
        self.dirty = true;
        self.release_device();
    }

    /// Sleep timer: fades the sound out over the last seconds, then pauses. The volume
    /// the user set is left as it is, and is what the next "play" starts with.
    fn follow_sleep_timer(&mut self) {
        if self.sleep_restore_at.is_some_and(|at| Instant::now() >= at) {
            self.sleep_restore_at = None;
            self.engine.set_volume(self.volume);
        }
        let Some(at) = self.sleep_at else { return };
        let left = at.saturating_duration_since(Instant::now());
        if left.is_zero() {
            self.sleep_at = None;
            // The timer's word stands like the listener's: no playing on after a call.
            self.interrupted = false;
            self.pause();
            // Not at once: the pause fades its last milliseconds out, and a volume rising
            // meanwhile would turn them into a tick.
            self.sleep_restore_at = Some(Instant::now() + SLEEP_RESTORE_AFTER);
            self.dirty = true;
        } else if left < SLEEP_FADE {
            let fraction = left.as_secs_f32() / SLEEP_FADE.as_secs_f32();
            self.engine.set_volume(self.volume * fraction);
        }
    }

    fn end_of_queue(&mut self) {
        self.drop_chain();
        self.want_playing = false;
        self.engine.set_playing(false);
        self.resume_ms = 0;
        self.state = PlayState::Ended;
        self.dirty = true;
        self.release_device();
        self.save_session(false);
    }

    fn note_failure(&mut self, track_id: TrackId, kind: PlaybackErrorKind) {
        tracing::warn!(track_id, ?kind, "track could not be played");
        self.consecutive_failures += 1;
        self.set_notice(Notice::TrackSkipped { track_id, kind });
    }

    fn give_up(&mut self) {
        self.drop_chain();
        self.want_playing = false;
        self.engine.set_playing(false);
        self.resume_ms = 0;
        self.state = PlayState::Error;
        self.set_notice(Notice::TooManyFailures);
        self.release_device();
    }

    fn output_failed(&mut self, error: OutputError) {
        tracing::warn!("output unavailable: {error}");
        self.drop_chain();
        self.want_playing = false;
        self.engine.set_playing(false);
        self.device_lost = true;
        self.state = if self.queue.current().is_some() {
            PlayState::Paused
        } else {
            PlayState::Idle
        };
        self.set_notice(Notice::OutputUnavailable(error));
    }

    fn try_start(
        &mut self,
        item: QueueItem,
        start_ms: u64,
        is_seek: bool,
    ) -> Result<(), StartError> {
        let request = self.request_for(item).map_err(StartError::Track)?;
        let source_rate = self.rate_hint(item.track);
        self.engine
            .open_output_for(source_rate)
            .map_err(StartError::Output)?;
        let target_rate = self.engine.preferred_rate(source_rate);
        self.device_lost = false;
        let slot = NextSlot::new();
        let epoch = self
            .engine
            .start_feed(request, start_ms, slot.clone())
            .map_err(StartError::Output)?;
        let rate = self.engine.sample_rate().unwrap_or(48_000);
        self.chain = Some(Chain {
            epoch,
            slot,
            rate,
            target_rate,
            first_item: item,
            segments: VecDeque::new(),
            pending_start_ms: start_ms,
            failed: None,
            is_seek,
            speed_percent: self.speed_percent,
        });
        self.announce_next_after(item.id);
        self.engine.set_playing(true);
        self.start_requested_at = Some(Instant::now());
        Ok(())
    }

    /// Starts (or prepares) the queue's current item at `start_ms`, skipping items that
    /// cannot be played.
    fn start_current(&mut self, start_ms: u64, is_seek: bool) {
        let mut start_ms = start_ms;
        self.dirty = true;
        loop {
            let Some(item) = self.queue.current() else {
                self.become_idle();
                return;
            };
            self.resume_ms = start_ms;
            if self.want_playing && !self.host.may_sound() {
                // Something else has the sound: the song stands ready and starts when
                // the sound comes, as after an interruption.
                self.want_playing = false;
                self.interrupted = true;
            }
            if !self.want_playing {
                // Prepared but silent: no device and no decoding until "play".
                self.drop_chain();
                self.engine.set_playing(false);
                self.state = PlayState::Paused;
                return;
            }
            self.chain = None;
            // Radio: if this is the last song the queue has, what follows is decided
            // now, so that the decode thread knows it from the start (a very short
            // last song is decoded to its end before its start is even reported).
            self.extend_with_radio(item.id);
            match self.try_start(item, start_ms, is_seek) {
                Ok(()) => {
                    self.state = if is_seek {
                        PlayState::Seeking
                    } else {
                        PlayState::Loading
                    };
                    return;
                }
                Err(StartError::Output(error)) => {
                    self.output_failed(error);
                    return;
                }
                Err(StartError::Track(kind)) => {
                    self.note_failure(item.track, kind);
                    if self.consecutive_failures
                        >= MAX_CONSECUTIVE_FAILURES.min(self.queue.len().max(1))
                    {
                        self.give_up();
                        return;
                    }
                    if self.queue.advance_after_failure().is_none() {
                        self.end_of_queue();
                        return;
                    }
                    // "Stop after this song" and the song cannot be played: it is over,
                    // so playback stops here as it would have at its end.
                    if self.stop_after == Some(item.id) {
                        self.stop_after = None;
                        self.halt();
                        self.save_session(false);
                        return;
                    }
                    start_ms = 0;
                }
            }
        }
    }

    /// After a queue or mode change: make sure what the decode thread has prepared (and
    /// will prepare) still matches the queue. If it has already decoded a track that no
    /// longer follows, restart the feed at the current position.
    fn sync_next(&mut self) {
        let Some(chain) = &self.chain else { return };
        let mut consistent = true;
        let mut last = chain.first_item.id;
        let mut segments = chain.segments.iter();
        if let Some(first) = segments.next() {
            last = first.item_id;
            for seg in segments {
                if self.stop_after == Some(last)
                    || self.queue.peek_after(last).map(|i| i.id) != Some(seg.item_id)
                {
                    consistent = false;
                    break;
                }
                last = seg.item_id;
            }
        }
        if consistent {
            // The change may have left the song that is playing as the last one.
            self.extend_with_radio(last);
            self.announce_next_after(last);
        } else {
            let position = self.position_ms();
            self.start_current(position, true);
        }
    }

    // ----- commands --------------------------------------------------------------------

    fn handle(&mut self, command: Command) {
        if command.decides_playing() {
            self.interrupted = false;
        }
        match command {
            Command::PlayTracks {
                tracks,
                start_index,
            } => {
                self.remember_position();
                self.queue.replace(&tracks, start_index);
                self.mark_queue_changed();
                self.want_playing = true;
                self.consecutive_failures = 0;
                self.listen.reset();
                self.start_fresh();
            }
            Command::PlayNext(tracks) => self.add_tracks(&tracks, true),
            Command::Enqueue(tracks) => self.add_tracks(&tracks, false),
            Command::JumpTo(id) => {
                self.remember_position();
                if self.queue.jump_to(id).is_some() {
                    self.want_playing = true;
                    self.consecutive_failures = 0;
                    self.listen.reset();
                    self.meta_dirty_since.get_or_insert_with(Instant::now);
                    self.start_fresh();
                }
            }
            Command::RemoveItems(ids) => {
                let current_removed = self.queue.remove(&ids);
                self.mark_queue_changed();
                self.after_removal(current_removed);
            }
            Command::MoveItems { ids, to_index } => {
                self.queue.move_items(&ids, to_index);
                self.mark_queue_changed();
                self.sync_next();
            }
            Command::ClearQueue => {
                self.queue.clear();
                self.mark_queue_changed();
                self.become_idle();
            }
            Command::Play => self.play(),
            Command::Pause => self.pause(),
            Command::TogglePlayPause => {
                if self.want_playing {
                    self.pause();
                } else {
                    self.play();
                }
            }
            Command::Stop => {
                self.remember_position();
                if self.queue.current().is_some() {
                    self.stop_after = None;
                    self.halt();
                }
            }
            Command::SetSleepTimer(after) => {
                self.sleep_at = after.map(|after| Instant::now() + after);
                // Switched off (or set anew) in the middle of the fade: full volume again.
                self.sleep_restore_at = None;
                self.engine.set_volume(self.volume);
                self.dirty = true;
            }
            Command::MarkSection => self.mark_section(),
            Command::ClearSection => {
                if self.section.take().is_some() {
                    self.dirty = true;
                }
            }
            Command::SetStopAfterCurrent(on) => {
                let wanted = self.queue.current().map(|i| i.id).filter(|_| on);
                if wanted != self.stop_after {
                    self.stop_after = wanted;
                    self.dirty = true;
                    // What follows the current song in the feed changes with it.
                    self.sync_next();
                }
            }
            Command::Next => {
                self.consecutive_failures = 0;
                self.remember_position();
                match self.queue.next_manual() {
                    Some(_) => {
                        self.listen.reset();
                        self.meta_dirty_since.get_or_insert_with(Instant::now);
                        self.start_fresh();
                    }
                    None if self.queue.current().is_some() => self.end_of_queue(),
                    None => {}
                }
            }
            Command::Previous => {
                self.consecutive_failures = 0;
                let position = self.position_ms();
                self.remember_position();
                match self.queue.previous_manual(position) {
                    Some(Previous::Restart) => {
                        // Started over on purpose: the place it was left at is not
                        // where to come back to (until 30 s are heard, none is saved).
                        if self.resume_long_tracks
                            && self.current_duration_ms() >= RESUME_MIN_DURATION_MS
                            && let Some(item) = self.queue.current()
                        {
                            self.host.set_resume_point(item.track, None);
                        }
                        self.seek_to(0);
                    }
                    Some(Previous::Item(_)) => {
                        self.listen.reset();
                        self.meta_dirty_since.get_or_insert_with(Instant::now);
                        self.start_fresh();
                    }
                    None => {}
                }
            }
            Command::SeekTo(ms) => self.seek_to(ms),
            Command::SeekRelative(delta) => {
                let target = self.position_ms().saturating_add_signed(delta);
                self.seek_to(target);
            }
            Command::SetVolume(volume) => {
                let volume = if volume.is_finite() {
                    volume.clamp(0.0, 1.0)
                } else {
                    self.volume
                };
                self.volume = volume;
                self.sleep_restore_at = None;
                self.engine.set_volume(volume);
                // Part of the session: saved soon, also while nothing plays (then
                // nothing else would save it).
                self.meta_dirty_since.get_or_insert_with(Instant::now);
                self.dirty = true;
            }
            Command::SetShuffle(on) => {
                self.queue.set_shuffle(on);
                self.mark_queue_changed();
                self.sync_next();
            }
            Command::SetRepeat(mode) => {
                self.queue.set_repeat(mode);
                self.meta_dirty_since.get_or_insert_with(Instant::now);
                self.dirty = true;
                self.sync_next();
            }
            Command::SetReplayGain(settings) => {
                self.replaygain = settings;
                self.engine.set_replaygain(settings);
                // The gain of what is in the feed may have changed with it.
                self.review_processed();
            }
            Command::SetCrossfeed(setting) => {
                self.crossfeed = setting;
                self.engine.set_crossfeed(setting);
                self.review_processed();
            }
            Command::SetCrossfade(secs) => {
                if secs != self.crossfade_secs {
                    self.crossfade_secs = secs;
                    self.dirty = true;
                    // What the feed is told of the song that follows changes with it.
                    self.sync_next();
                    self.review_processed();
                }
            }
            Command::SetSpeed(percent) => {
                let percent = crate::audio::pace::speed_of(percent);
                if percent != self.speed_percent {
                    // Where the song is, by the speed it has gone at so far.
                    let position = self.position_ms();
                    self.speed_percent = percent;
                    self.engine.set_speed(f32::from(percent) / 100.0);
                    self.dirty = true;
                    if self.chain.is_some() {
                        self.start_current(position, true);
                    }
                    self.review_processed();
                }
            }
            Command::SetRoom(impulse) => {
                let position = self.position_ms();
                self.room = impulse.as_ref().map(|impulse| impulse.name.clone());
                self.engine.set_room(impulse);
                self.dirty = true;
                if self.chain.is_some() {
                    self.start_current(position, true);
                }
                self.review_processed();
            }
            Command::SetCorrection(profile) => {
                let profile = profile.map(PeqProfile::sanitized);
                self.correction = profile
                    .as_ref()
                    .filter(|p| !p.is_flat())
                    .map(|p| p.name.clone());
                self.engine.set_correction(profile);
                self.review_processed();
            }
            Command::SetShuffleByAlbum(on) => {
                self.queue.set_shuffle_by_album(on);
                self.mark_queue_changed();
                self.sync_next();
            }
            Command::SetShuffleByRating(on) => {
                self.queue.set_shuffle_by_rating(on);
                self.mark_queue_changed();
                self.sync_next();
            }
            Command::SetResumeLongTracks(on) => self.resume_long_tracks = on,
            Command::SetRadio(on) => {
                self.radio = on;
                self.dirty = true;
                // Switched on while the last song of the queue is on: it applies to
                // this very queue.
                let last = self
                    .chain
                    .as_ref()
                    .and_then(|c| c.segments.back().map(|s| s.item_id));
                if on
                    && let Some(last) = last
                    && self.extend_with_radio(last)
                {
                    self.sync_next();
                }
            }
            Command::SetEq(settings) => {
                self.eq = settings.sanitized();
                self.engine.set_eq(self.eq);
                self.review_processed();
            }
            Command::SetOutputDevice(pref) => {
                let position = self.position_ms();
                self.engine.set_device_pref(pref);
                self.chain = None;
                self.engine.close_output();
                self.device_lost = false;
                self.start_current(position, true);
            }
            Command::SetOutput(settings) => {
                let before = self.engine.output_settings();
                self.engine.set_output_settings(settings);
                self.dirty = true;
                // Memory playback applies from the next feed on its own. How the device
                // is used changes now: reopen the output and continue where we were.
                if before.of_the_stream() != settings.of_the_stream() {
                    let playing_from = self.chain.is_some().then(|| self.position_ms());
                    self.chain = None;
                    self.engine.close_output();
                    if let Some(position) = playing_from {
                        self.start_current(position, true);
                    }
                }
            }
            Command::RetryOutput => {
                self.device_lost = false;
                self.want_playing = true;
                let position = self.resume_ms;
                self.start_current(position, false);
            }
            Command::SystemWillSleep => {
                let position = self.position_ms();
                self.want_playing = false;
                self.chain = None;
                self.engine.set_playing(false);
                self.engine.close_output();
                if self.queue.current().is_some() {
                    self.resume_ms = position;
                    self.state = PlayState::Paused;
                }
                self.dirty = true;
                self.save_session(false);
            }
            Command::Interrupted => {
                if self.want_playing || self.state == PlayState::Playing {
                    self.pause();
                    self.interrupted = true;
                }
            }
            Command::InterruptionOver { resume } => {
                // Only a song that still stands paused where the interruption left it:
                // a queue that was run to its end meanwhile (Next at the last song) is
                // not started over, which "play" would do.
                let waiting = std::mem::take(&mut self.interrupted)
                    && self.state == PlayState::Paused
                    && self.queue.current().is_some();
                if waiting && resume {
                    self.play();
                }
                self.host.interruption_settled(self.want_playing);
            }
            Command::SaveSession => self.save_session(false),
            Command::LibraryChanged => {
                let before = self.queue.len();
                let current_removed = self.queue.retain_tracks(|t| self.host.has_track(t));
                if self.queue.len() != before {
                    self.mark_queue_changed();
                    self.after_removal(current_removed);
                }
            }
            Command::Shutdown => self.running = false,
        }
    }

    fn add_tracks(&mut self, tracks: &[TrackId], play_next: bool) {
        if tracks.is_empty() {
            return;
        }
        let had_current = self.queue.current().is_some();
        if play_next {
            self.queue.play_next(tracks);
        } else {
            self.queue.append(tracks);
        }
        self.mark_queue_changed();
        if had_current {
            self.sync_next();
        } else {
            // First items in an empty queue: prepared, not started.
            self.resume_ms = 0;
            self.state = PlayState::Paused;
        }
    }

    fn after_removal(&mut self, current_removed: bool) {
        if !current_removed {
            self.sync_next();
        } else if self.queue.current().is_some() {
            // Continue with the item that followed (playing stays playing, paused stays paused).
            self.start_current(0, false);
        } else {
            self.become_idle();
        }
    }

    fn play(&mut self) {
        if self.queue.is_empty() {
            return;
        }
        self.dirty = true;
        self.consecutive_failures = 0;
        if matches!(self.state, PlayState::Ended | PlayState::Error)
            || self.queue.current().is_none()
        {
            self.queue.rewind();
            self.want_playing = true;
            self.listen.reset();
            self.start_current(0, false);
            return;
        }
        if self.chain.is_some() {
            if !self.host.may_sound() {
                // Something else has the sound: it stays paused, and goes on when the
                // sound comes.
                self.interrupted = true;
                return;
            }
            self.want_playing = true;
            self.engine.set_playing(true);
            if self.state == PlayState::Paused {
                self.state = PlayState::Playing;
            }
        } else {
            self.want_playing = true;
            let position = self.resume_ms;
            self.start_current(position, false);
        }
    }

    fn pause(&mut self) {
        if !self.want_playing && self.state != PlayState::Playing {
            return;
        }
        self.want_playing = false;
        self.engine.set_playing(false);
        if self.queue.current().is_some() {
            self.state = PlayState::Paused;
        }
        self.dirty = true;
        self.save_session(false);
    }

    /// One step of marking a section to repeat (`Command::MarkSection`): where it
    /// begins, then where it ends, then nothing any more.
    fn mark_section(&mut self) {
        let Some(item) = self.queue.current().map(|i| i.id) else {
            return;
        };
        let position = self.position_ms();
        let marked = self.section.take().filter(|(of, _)| *of == item);
        self.section = match marked {
            None => Some(SectionRepeat {
                from_ms: position,
                to_ms: None,
            }),
            Some((
                _,
                SectionRepeat {
                    from_ms,
                    to_ms: None,
                },
            )) => {
                // Marked from its end backwards (the listener went back in the song
                // after marking): the two marks change places.
                let (from, to) = (from_ms.min(position), from_ms.max(position));
                // A section can be heard, and ends before the song does.
                let duration = self.current_duration_ms();
                let last = match duration.checked_sub(SECTION_BEFORE_END_MS) {
                    Some(last) if duration > 0 => last,
                    _ => u64::MAX,
                };
                let to = to.max(from.saturating_add(SECTION_MIN_MS)).min(last);
                // (Too close to the end of the song for a section: none.)
                (to >= from.saturating_add(SECTION_MIN_MS)).then_some(SectionRepeat {
                    from_ms: from,
                    to_ms: Some(to),
                })
            }
            Some(_) => None,
        }
        .map(|section| (item, section));
        self.dirty = true;
        // (Whether the next song is mixed in goes by whether a section is repeated.)
        self.announce_next_after(item);
    }

    /// The section that is repeated belongs to one song: another one being current
    /// ends it. While its song plays, reaching its end goes back to its beginning.
    fn follow_section(&mut self) {
        let Some((item, section)) = self.section else {
            return;
        };
        if self.queue.current().map(|i| i.id) != Some(item) {
            self.section = None;
            self.dirty = true;
            return;
        }
        if let Some(to) = section.to_ms
            && self.state == PlayState::Playing
            && self.chain.is_some()
            && self.position_ms() >= to
        {
            // (Not `seek_to`: where the song stands is not written down for every
            // round of the section.)
            self.start_current(section.from_ms, true);
        }
    }

    fn seek_to(&mut self, ms: u64) {
        if self.queue.current().is_none() {
            return;
        }
        let duration = self.current_duration_ms();
        let target = if duration > 0 { ms.min(duration) } else { ms };
        // Where the song stands is part of the session: saved soon, also while paused
        // (the regular saves only run while it plays).
        self.meta_dirty_since.get_or_insert_with(Instant::now);
        if self.chain.is_some() {
            self.start_current(target, true);
        } else {
            self.resume_ms = target;
            self.dirty = true;
        }
    }

    // ----- engine events ---------------------------------------------------------------

    fn on_engine_event(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::TrackStarted {
                epoch,
                item_id,
                track_id,
                at_frame,
                start_ms,
                info,
                limited_before,
            } => {
                let Some(chain) = self.chain.as_mut().filter(|c| c.epoch == epoch) else {
                    return;
                };
                if let Some(last) = chain.segments.back() {
                    // Gapless continuation: only valid if the queue still wants it.
                    let expected = self.queue.peek_after(last.item_id).map(|i| i.id);
                    if expected != Some(item_id) || self.stop_after == Some(last.item_id) {
                        let position = self.position_ms();
                        self.start_current(position, true);
                        return;
                    }
                }
                let from_info = info
                    .total_frames
                    .map(|f| f.saturating_mul(1000) / u64::from(info.sample_rate.max(1)))
                    .filter(|d| *d > 0);
                let duration_ms = from_info
                    .or_else(|| self.host.track(track_id).map(|t| t.duration_ms))
                    .unwrap_or(0);
                let processed = self.source_is_processed(&info, track_id);
                if let Some(chain) = self.chain.as_mut() {
                    chain.segments.push_back(Segment {
                        processed,
                        limited_before,
                        item_id,
                        track_id,
                        at_frame,
                        start_ms,
                        duration_ms,
                        info,
                    });
                }
                self.sync_processed();
                self.extend_with_radio(item_id);
                self.announce_next_after(item_id);
                self.dirty = true;
            }
            EngineEvent::TrackFailed {
                epoch,
                item_id,
                track_id,
                error,
            } => {
                let Some(chain) = self.chain.as_mut().filter(|c| c.epoch == epoch) else {
                    return;
                };
                tracing::debug!("decode failure detail: {}", error.detail);
                let nothing_heard_yet = chain.segments.is_empty();
                chain.failed = Some(item_id);
                self.note_failure(track_id, error.kind);
                if nothing_heard_yet {
                    // The requested track itself failed: there is no audio to wait for.
                    self.skip_failed_current();
                }
            }
            EngineEvent::FeedFinished { .. } => {}
            EngineEvent::Drained { epoch } => {
                if self.chain.as_ref().is_some_and(|c| c.epoch == epoch) {
                    self.on_drained();
                }
            }
            EngineEvent::DeviceLost => {
                let position = self.position_ms();
                self.chain = None;
                self.want_playing = false;
                self.engine.set_playing(false);
                self.device_lost = true;
                if self.queue.current().is_some() {
                    self.resume_ms = position;
                    self.state = PlayState::Paused;
                }
                self.set_notice(Notice::DeviceLost);
                self.save_session(false);
            }
            EngineEvent::OutputChanged => {
                let position = self.position_ms();
                self.chain = None;
                self.start_current(position, true);
            }
        }
    }

    /// The current item could not be played (its file would not open, it gave no
    /// audio): on to the next one, as `start_current` does for a track that is known
    /// not to be there.
    fn skip_failed_current(&mut self) {
        let failed = self.queue.current().map(|item| item.id);
        if self.consecutive_failures >= MAX_CONSECUTIVE_FAILURES.min(self.queue.len().max(1)) {
            self.give_up();
        } else if self.queue.advance_after_failure().is_none() {
            self.end_of_queue();
        } else if failed.is_some() && self.stop_after == failed {
            // "Stop after this song" and the song cannot be played: it is over, so
            // playback stops here as it would have at its end, with the next one ready.
            self.stop_after = None;
            self.halt();
            self.save_session(false);
        } else {
            self.start_current(0, false);
        }
    }

    /// Everything in the feed has been played.
    fn on_drained(&mut self) {
        let (played_epoch, frames) = self.engine.shared().position();
        let Some(chain) = self.chain.as_mut() else {
            return;
        };
        let end_frames = (played_epoch == chain.epoch).then_some(frames);
        // Follow any boundaries not yet observed (very short final tracks).
        while chain.segments.len() > 1 {
            let (Some(finished), Some(next)) = (chain.segments.pop_front(), chain.segments.front())
            else {
                break;
            };
            let boundary = next.at_frame;
            let next_id = next.item_id;
            self.listen.hear(
                &finished,
                chain.epoch,
                boundary,
                chain.rate,
                chain.speed_percent,
            );
            let length_ms =
                finished.start_ms + chain.song_ms(boundary.saturating_sub(finished.at_frame));
            if self.listen.take_credit(credit_threshold_ms(length_ms)) {
                self.host.record_play(finished.track_id);
            }
            // Heard to the end: next time it starts from the beginning.
            if finished.duration_ms >= RESUME_MIN_DURATION_MS {
                self.host.set_resume_point(finished.track_id, None);
            }
            self.listen.reset();
            if self.queue.advance_auto().map(|i| i.id) != Some(next_id) {
                self.queue.jump_to(next_id);
            }
        }
        let failed = chain.failed.take();
        if let Some(seg) = chain.segments.front() {
            // The feed length is only known as "what was played": the track's real length.
            let end = end_frames.unwrap_or(seg.at_frame).max(seg.at_frame);
            self.listen
                .hear(seg, chain.epoch, end, chain.rate, chain.speed_percent);
            let length_ms = seg.start_ms + chain.song_ms(end - seg.at_frame);
            // A track that broke mid-way did not play to its end.
            if failed != Some(seg.item_id) {
                if self.listen.take_credit(credit_threshold_ms(length_ms)) {
                    self.host.record_play(seg.track_id);
                }
                // The feed ran out at its end (the queue is over, or what follows needs a
                // feed of its own): next time it starts from the beginning.
                if seg.duration_ms >= RESUME_MIN_DURATION_MS {
                    self.host.set_resume_point(seg.track_id, None);
                }
            }
        }
        self.listen.reset();
        self.meta_dirty_since.get_or_insert_with(Instant::now);
        let current = self.queue.current().map(|c| c.id);
        if current.is_some() && self.stop_after == current {
            // "Stop after this song": the next one is made current, so that "play"
            // goes on from there, and nothing is started.
            self.stop_after = None;
            self.consecutive_failures = 0;
            match self.queue.advance_auto() {
                Some(_) => {
                    self.halt();
                    self.save_session(false);
                }
                None => self.end_of_queue(),
            }
            return;
        }
        // Radio's last chance: the feed ended without anything having been lined up.
        if let Some(current) = current {
            self.extend_with_radio(current);
        }
        let next = match failed {
            Some(item_id) if current == Some(item_id) => self.advance_past_failure(),
            Some(item_id) => {
                // A track after the audible one could not be opened. The queue may have
                // changed since then (something queued to play next, the broken item
                // removed), so continue from the track that just ended and only skip the
                // broken item if it really is what follows.
                match self.queue.advance_auto() {
                    Some(item) if item.id == item_id => self.advance_past_failure(),
                    other => other.map(|_| ()).ok_or(false),
                }
            }
            None => {
                self.consecutive_failures = 0;
                self.queue.advance_auto().map(|_| ()).ok_or(false)
            }
        };
        match next {
            Ok(()) => self.start_current(0, false),
            Err(true) => self.give_up(),
            Err(false) => self.end_of_queue(),
        }
    }

    /// The queue's current item cannot be played: move to the next one.
    /// `Err(true)` = too many failures in a row, `Err(false)` = the queue is over.
    fn advance_past_failure(&mut self) -> Result<(), bool> {
        if self.consecutive_failures >= MAX_CONSECUTIVE_FAILURES.min(self.queue.len().max(1)) {
            return Err(true);
        }
        self.queue.advance_after_failure().map(|_| ()).ok_or(false)
    }

    /// Per tick: follow track boundaries, update the state from what is audible.
    fn follow_playback(&mut self) {
        let (played_epoch, frames) = self.engine.played();
        let renderer_epoch = self.engine.shared().position().0;
        let Some(chain) = self.chain.as_mut() else {
            return;
        };
        let mut boundary_passed = false;
        if played_epoch == chain.epoch {
            while chain.segments.len() > 1 && frames >= chain.segments[1].at_frame {
                boundary_passed = true;
                let boundary = chain.segments[1].at_frame;
                let Some(finished) = chain.segments.pop_front() else {
                    break;
                };
                // Played to its natural end.
                self.listen.hear(
                    &finished,
                    chain.epoch,
                    boundary,
                    chain.rate,
                    chain.speed_percent,
                );
                let length_ms =
                    finished.start_ms + chain.song_ms(boundary.saturating_sub(finished.at_frame));
                if self.listen.take_credit(credit_threshold_ms(length_ms)) {
                    self.host.record_play(finished.track_id);
                }
                // Heard to the end: next time it starts from the beginning.
                if finished.duration_ms >= RESUME_MIN_DURATION_MS {
                    self.host.set_resume_point(finished.track_id, None);
                }
                // The next segment is a new playthrough, also of the same item (repeat one).
                self.listen.reset();
                let Some(next) = chain.segments.front().map(|s| s.item_id) else {
                    break;
                };
                if self.queue.advance_auto().map(|i| i.id) != Some(next) {
                    self.queue.jump_to(next);
                }
                self.consecutive_failures = 0;
                self.dirty = true;
                // Only "which item is current" changed: no need to rewrite the queue table.
                self.meta_dirty_since.get_or_insert_with(Instant::now);
            }
        }
        if matches!(self.state, PlayState::Loading | PlayState::Seeking)
            && renderer_epoch == chain.epoch
            && !chain.segments.is_empty()
        {
            self.state = if self.want_playing {
                PlayState::Playing
            } else {
                PlayState::Paused
            };
            chain.is_seek = false;
            self.dirty = true;
            if let Some(requested) = self.start_requested_at.take() {
                // Until the callback switched to the new feed, plus what the device
                // reports as its own output latency.
                let device_ms = u64::from(self.engine.shared().latency_frames()) * 1000
                    / u64::from(chain.rate.max(1));
                let total = requested.elapsed().as_millis() as u64 + device_ms;
                self.last_start_latency_ms = Some(total.min(u64::from(u32::MAX)) as u32);
                tracing::debug!(latency_ms = total, "playback started");
            }
        }
        // Count the play once enough of the track has been heard.
        if played_epoch == chain.epoch
            && let Some(seg) = chain.segments.front()
        {
            self.listen
                .hear(seg, chain.epoch, frames, chain.rate, chain.speed_percent);
            if self
                .listen
                .take_credit(credit_threshold_ms(seg.duration_ms))
            {
                self.host.record_play(seg.track_id);
            }
            // Real audio proves this track plays: earlier failures are history.
            if self.consecutive_failures > 0
                && chain.failed != Some(seg.item_id)
                && frames.saturating_sub(seg.at_frame) >= u64::from(chain.rate / 4)
            {
                self.consecutive_failures = 0;
            }
        }
        if boundary_passed {
            // The track that ended has left the feed: it no longer asks for dither.
            self.sync_processed();
        }
    }

    /// Playback is wanted but nothing is coming: a read stuck on a dead drive, or a device
    /// that no longer asks for audio. Without this the player would wait forever.
    fn watch_for_stall(&mut self) {
        let Some(chain) = &self.chain else {
            self.starved_since = None;
            self.starved_ticks = 0;
            return;
        };
        let position = self.engine.shared().position();
        let moving = position != self.last_render_position;
        self.last_render_position = position;
        let starved = self.want_playing
            && match self.state {
                PlayState::Loading | PlayState::Seeking => true,
                PlayState::Playing => !moving,
                _ => false,
            };
        if !starved {
            self.starved_since = None;
            self.starved_ticks = 0;
            return;
        }
        let since = *self.starved_since.get_or_insert_with(Instant::now);
        self.starved_ticks += 1;
        // The clock alone is not proof: if this process was suspended (debugger, a
        // stalled system), a long time passes between two ticks without anything being
        // wrong. Silence must also have been *observed* over a fair share of the ticks.
        let needed_ticks =
            (self.stall_timeout.as_millis() / TICK_ACTIVE.as_millis().max(1) / 4).max(3) as u32;
        if since.elapsed() < self.stall_timeout || self.starved_ticks < needed_ticks {
            return;
        }
        self.starved_since = None;
        self.starved_ticks = 0;
        if self.engine.feed_has_audio(chain.epoch) {
            // Audio is waiting; the device is not taking it.
            let position = self.position_ms();
            self.output_failed(OutputError::Backend(
                "the output device stopped responding".into(),
            ));
            self.resume_ms = position;
            self.save_session(false);
            return;
        }
        let item = chain
            .segments
            .front()
            .map_or((chain.first_item.id, chain.first_item.track), |s| {
                (s.item_id, s.track_id)
            });
        tracing::warn!(track_id = item.1, "no audio from the decoder; skipping");
        if self.queue.current().map(|c| c.id) != Some(item.0) {
            self.queue.jump_to(item.0);
        }
        self.note_failure(item.1, PlaybackErrorKind::FileUnreadable);
        self.listen.reset();
        self.skip_failed_current();
    }

    /// Lets go of the file (and the decode thread) after a long pause, and of the output
    /// device if it is claimed: other apps get it back, at the rate it had.
    fn release_paused_feed(&mut self) {
        if self.want_playing || (self.chain.is_none() && !self.engine.holds_device()) {
            self.paused_since = None;
            return;
        }
        let since = *self.paused_since.get_or_insert_with(Instant::now);
        if since.elapsed() >= PAUSE_RELEASE {
            if self.chain.is_some() {
                let position = self.position_ms();
                self.drop_chain();
                self.resume_ms = position;
            }
            self.release_device();
            self.paused_since = None;
            self.dirty = true;
        }
    }

    // ----- publishing and persistence --------------------------------------------------

    fn publish_if_needed(&mut self) {
        let playing = self.state == PlayState::Playing;
        if !self.dirty
            && !(playing && self.last_position_publish.elapsed() >= POSITION_PUBLISH_INTERVAL)
        {
            return;
        }
        self.sync_queue_copy();
        let notify = self.dirty;
        self.dirty = false;
        self.last_position_publish = Instant::now();
        let current = self.queue.current().map(|item| NowPlaying {
            item_id: item.id,
            track_id: item.track,
            duration_ms: self.current_duration_ms(),
        });
        let snapshot = PlayerSnapshot {
            state: self.state,
            current,
            position_ms: self.position_ms(),
            position_at: Instant::now(),
            volume: self.volume,
            shuffle: self.queue.shuffle(),
            repeat: self.queue.repeat(),
            queue: self.queue_arc.clone(),
            queue_revision: self.queue_revision,
            queue_index: self.queue.current_index(),
            notice: self.notice.clone(),
            notice_seq: self.notice_seq,
            output_device: self
                .engine
                .output_device()
                .map(|(_, name)| name.to_string()),
            device_lost: self.device_lost,
            replaygain: self.replaygain,
            device_pref: self.engine.device_pref().map(str::to_string),
            underruns: self.engine.underruns(),
            source: self
                .chain
                .as_ref()
                .and_then(|c| c.segments.front())
                .map(|s| s.info.clone()),
            output_sample_rate: self.engine.sample_rate(),
            output: self.engine.output_status(),
            output_settings: self.engine.output_settings(),
            dither_bits: self.engine.dither_bits(),
            eq: self.eq,
            crossfeed: self.crossfeed,
            correction: self.correction.clone(),
            room: self.room.clone(),
            radio: self.radio,
            stop_after_current: self.stop_after.is_some(),
            sleep_at: self.sleep_at,
            section: self.section.map(|(_, section)| section),
            crossfade_secs: self.crossfade_secs,
            speed_percent: self.speed_percent,
            peak_limited_frames: self.peak_limited_frames(),
            last_start_latency_ms: self.last_start_latency_ms,
        };
        match self.shared.lock() {
            Ok(mut shared) => *shared = snapshot,
            Err(poisoned) => *poisoned.into_inner() = snapshot,
        }
        if notify {
            self.host.changed();
        }
    }

    /// Starts the current item: from the beginning, or for a long track (with "resume
    /// long tracks" on) where it was left off.
    fn start_fresh(&mut self) {
        let start = self
            .queue
            .current()
            .filter(|_| self.resume_long_tracks)
            .and_then(|item| {
                let duration = self.host.track(item.track)?.duration_ms;
                let at = self.host.resume_point(item.track)?;
                (duration >= RESUME_MIN_DURATION_MS
                    && at >= RESUME_MIN_POSITION_MS
                    && at + RESUME_END_MS < duration)
                    .then_some(at)
            })
            .unwrap_or(0);
        self.start_current(start, false);
    }

    /// With "resume long tracks" on, remembers where the current long track is (or
    /// forgets it when it was heard to its last minute).
    fn remember_position(&mut self) {
        if !self.resume_long_tracks {
            return;
        }
        let Some(item) = self.queue.current() else {
            return;
        };
        let duration = self.current_duration_ms();
        if duration < RESUME_MIN_DURATION_MS {
            return;
        }
        let position = self.position_ms();
        if position + RESUME_END_MS >= duration {
            self.host.set_resume_point(item.track, None);
        } else if position >= RESUME_MIN_POSITION_MS {
            self.host.set_resume_point(item.track, Some(position));
        }
    }

    fn save_session(&mut self, force: bool) {
        self.remember_position();
        let queue_changed = self.queue_dirty_since.is_some() || force;
        let session = Session {
            queue: self.queue.to_state(),
            position_ms: self.position_ms(),
            volume: self.volume,
        };
        let saved = self.host.save_session(&session, queue_changed);
        self.last_session_save = Instant::now();
        if saved {
            self.queue_dirty_since = None;
            self.meta_dirty_since = None;
            self.session_retry_at = None;
        } else {
            // Still unsaved (a queue change stays part of it), and tried again a little
            // later: not on every tick, and not given up on.
            self.meta_dirty_since.get_or_insert_with(Instant::now);
            self.session_retry_at = Some(Instant::now() + SESSION_RETRY_AFTER);
        }
    }

    fn save_session_if_due(&mut self) {
        if self.session_retry_at.is_some_and(|at| Instant::now() < at) {
            return;
        }
        let queue_due = self
            .queue_dirty_since
            .or(self.meta_dirty_since)
            .is_some_and(|t| t.elapsed() >= QUEUE_SAVE_DEBOUNCE);
        let position_due = self.state == PlayState::Playing
            && self.last_session_save.elapsed() >= SESSION_SAVE_INTERVAL;
        if queue_due || position_due {
            self.save_session(false);
        }
    }
}
