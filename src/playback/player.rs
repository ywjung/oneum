//! The player thread: the single entry point for every playback command.
//!
//! GUI, menu, media keys and Finder all send [`Command`]s through one channel. The thread
//! owns the queue and the audio engine, turns engine events into state changes, and
//! publishes a [`PlayerSnapshot`] for readers. Nothing here touches the UI or the database
//! directly; the outside world is reached through [`PlayerHost`].
use super::queue::{ItemId, Previous, Queue, QueueItem, QueueState, RepeatMode};
use crate::audio::decoder::SourceInfo;
use crate::audio::{
    Engine, EngineEvent, NextSlot, NextTrack, OutputError, PlaybackErrorKind, ReplayGainSettings,
    TrackRequest,
};
use crate::model::{ReplayGainTags, TrackId};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const TICK_ACTIVE: Duration = Duration::from_millis(20);
const TICK_IDLE: Duration = Duration::from_millis(200);
const POSITION_PUBLISH_INTERVAL: Duration = Duration::from_millis(500);
const SESSION_SAVE_INTERVAL: Duration = Duration::from_secs(10);
const QUEUE_SAVE_DEBOUNCE: Duration = Duration::from_secs(1);
/// Playback stops after this many tracks in a row could not be played.
pub const MAX_CONSECUTIVE_FAILURES: usize = 10;
/// A play is counted once this much of the track has been heard (or half of it, if shorter).
const PLAY_CREDIT_MS: u64 = 240_000;
/// A paused feed is released after this long: the decode thread ends and the file is
/// closed, so the drive it is on can be ejected. Playback resumes from the same position.
const PAUSE_RELEASE: Duration = Duration::from_secs(30);

/// What the player needs to know about a track in order to play it.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayableTrack {
    pub path: PathBuf,
    pub replaygain: ReplayGainTags,
    pub duration_ms: u64,
    /// False when the library already knows the file is missing or its drive is offline.
    pub available: bool,
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
    /// A track was listened to (see `PLAY_CREDIT_MS`).
    fn record_play(&self, id: TrackId);
    fn load_session(&self) -> Option<Session>;
    /// `queue_changed` is false when only the position or volume differ from the last save.
    fn save_session(&self, session: &Session, queue_changed: bool);
    /// The snapshot changed (repaint, update Now Playing). Called from the player thread.
    fn changed(&self);
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
    /// `None` follows the system default device.
    SetOutputDevice(Option<String>),
    /// After a lost device: open the preferred (or default) device again and continue.
    RetryOutput,
    SystemWillSleep,
    /// Tracks may have left the library; drop them from the queue.
    LibraryChanged,
    Shutdown,
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
            last_start_latency_ms: None,
        }
    }

    /// Position right now, extrapolated while playing and clamped to the duration.
    pub fn position_now_ms(&self) -> u64 {
        let mut pos = self.position_ms;
        if self.state == PlayState::Playing {
            pos += self.position_at.elapsed().as_millis() as u64;
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
    pub device_pref: Option<String>,
    /// Playback that is wanted but produces no audio for this long is treated as a
    /// failure (a read stuck on a dead drive, a device that stopped asking for audio).
    pub stall_timeout: Duration,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self {
            default_volume: 0.8,
            replaygain: ReplayGainSettings::default(),
            device_pref: None,
            stall_timeout: Duration::from_secs(15),
        }
    }
}

/// Cloneable handle used by the UI and platform code.
#[derive(Clone)]
pub struct PlayerHandle {
    tx: Sender<Command>,
    snapshot: Arc<Mutex<PlayerSnapshot>>,
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
}

/// Starts the player thread with a real output device.
pub fn spawn<H: PlayerHost>(
    host: Arc<H>,
    config: PlayerConfig,
) -> (PlayerHandle, std::thread::JoinHandle<()>) {
    let cfg = config.clone();
    spawn_with_engine(host, config, move |volume| {
        Engine::new(cfg.device_pref.clone(), volume, cfg.replaygain)
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
    let thread = std::thread::Builder::new()
        .name("oneum-player".into())
        .spawn(move || {
            let session = host.load_session();
            let volume = session
                .as_ref()
                .map_or(config.default_volume, |s| s.volume)
                .clamp(0.0, 1.0);
            let engine = make_engine(volume);
            Player::new(host, engine, rx, shared, config, session, volume).run();
        })
        .unwrap_or_else(|e| {
            // Without a player thread the app cannot play anything; commands are dropped.
            tracing::error!("cannot start player thread: {e}");
            std::thread::spawn(|| {})
        });
    (PlayerHandle { tx, snapshot }, thread)
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
}

/// How much of the current playthrough of a queue item has really been heard.
///
/// This lives outside the feed on purpose: a seek, a queue edit or a device change
/// replaces the feed, but it is still the same playthrough and must count once.
#[derive(Debug, Default)]
struct Listen {
    item_id: Option<ItemId>,
    /// Audio heard, in microseconds (frames are converted at the feed's rate).
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
    fn hear(&mut self, seg: &Segment, epoch: u16, frames: u64, rate: u32) {
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
        self.heard_us += (upto - from).saturating_mul(1_000_000) / u64::from(rate.max(1));
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
    first_item: QueueItem,
    /// Decoded tracks, oldest first. The front one is what is heard once the renderer
    /// has switched to this feed.
    segments: VecDeque<Segment>,
    /// Position to show until audio of this feed is heard.
    pending_start_ms: u64,
    /// A track of this feed failed (while opening, or mid-way).
    failed: Option<ItemId>,
    is_seek: bool,
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
    state: PlayState,
    /// What the user asked for; the engine follows once audio is available.
    want_playing: bool,
    chain: Option<Chain>,
    /// Position used when there is no feed (restored session, prepared-while-paused, lost device).
    resume_ms: u64,
    volume: f32,
    replaygain: ReplayGainSettings,
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
    /// Renderer position at the previous tick, to tell whether audio is moving.
    last_render_position: (u16, u64),
    /// Since when a feed exists although playback is paused.
    paused_since: Option<Instant>,
    last_session_save: Instant,
    last_position_publish: Instant,
    running: bool,
    start_requested_at: Option<Instant>,
    last_start_latency_ms: Option<u32>,
}

impl<H: PlayerHost> Player<H> {
    fn new(
        host: Arc<H>,
        engine: Engine,
        rx: Receiver<Command>,
        shared: Arc<Mutex<PlayerSnapshot>>,
        config: PlayerConfig,
        session: Option<Session>,
        volume: f32,
    ) -> Self {
        let (queue, resume_ms) = match session {
            Some(s) => {
                let mut queue = Queue::from_state(s.queue);
                // Tracks may have left the library since the last run.
                let current_removed = queue.retain_tracks(|t| host.track(t).is_some());
                (queue, if current_removed { 0 } else { s.position_ms })
            }
            None => (Queue::new(), 0),
        };
        let state = if queue.current().is_some() {
            PlayState::Paused
        } else {
            PlayState::Idle
        };
        let queue_arc = Arc::new(queue.items().to_vec());
        Self {
            host,
            engine,
            rx,
            shared,
            queue,
            queue_revision: 1,
            queue_arc,
            state,
            want_playing: false, // never start making sound on launch
            chain: None,
            resume_ms,
            volume,
            replaygain: config.replaygain,
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
            last_render_position: (0, 0),
            paused_since: None,
            last_session_save: Instant::now(),
            last_position_publish: Instant::now(),
            running: true,
            start_requested_at: None,
            last_start_latency_ms: None,
        }
    }

    fn run(mut self) {
        let mut events = Vec::new();
        while self.running {
            let busy = self.chain.is_some()
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
            self.watch_for_stall();
            self.release_paused_feed();
            self.publish_if_needed();
            self.save_session_if_due();
        }
        self.engine.set_playing(false);
        self.save_session(true);
        self.engine.close_output();
    }

    // ----- helpers ---------------------------------------------------------------------

    fn mark_queue_changed(&mut self) {
        self.queue_revision += 1;
        self.queue_arc = Arc::new(self.queue.items().to_vec());
        self.queue_dirty_since.get_or_insert_with(Instant::now);
        self.dirty = true;
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
                        seg.start_ms
                            + frames.saturating_sub(seg.at_frame) * 1000
                                / u64::from(chain.rate.max(1))
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
            }),
            _ => Err(PlaybackErrorKind::FileMissing),
        }
    }

    /// Tells the decode thread what follows `item_id`.
    fn announce_next_after(&self, item_id: ItemId) {
        let Some(chain) = &self.chain else { return };
        let next = match self.queue.peek_after(item_id) {
            // An unplayable next track ends the feed; it is skipped when we get there.
            Some(next) => self
                .request_for(next)
                .map_or(NextTrack::End, NextTrack::Track),
            None => NextTrack::End,
        };
        chain.slot.set(next);
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
    }

    fn end_of_queue(&mut self) {
        self.drop_chain();
        self.want_playing = false;
        self.engine.set_playing(false);
        self.resume_ms = 0;
        self.state = PlayState::Ended;
        self.dirty = true;
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
        self.engine.open_output().map_err(StartError::Output)?;
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
            first_item: item,
            segments: VecDeque::new(),
            pending_start_ms: start_ms,
            failed: None,
            is_seek,
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
            if !self.want_playing {
                // Prepared but silent: no device and no decoding until "play".
                self.drop_chain();
                self.engine.set_playing(false);
                self.state = PlayState::Paused;
                return;
            }
            self.chain = None;
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
                if self.queue.peek_after(last).map(|i| i.id) != Some(seg.item_id) {
                    consistent = false;
                    break;
                }
                last = seg.item_id;
            }
        }
        if consistent {
            self.announce_next_after(last);
        } else {
            let position = self.position_ms();
            self.start_current(position, true);
        }
    }

    // ----- commands --------------------------------------------------------------------

    fn handle(&mut self, command: Command) {
        match command {
            Command::PlayTracks {
                tracks,
                start_index,
            } => {
                self.queue.replace(&tracks, start_index);
                self.mark_queue_changed();
                self.want_playing = true;
                self.consecutive_failures = 0;
                self.listen.reset();
                self.start_current(0, false);
            }
            Command::PlayNext(tracks) => self.add_tracks(&tracks, true),
            Command::Enqueue(tracks) => self.add_tracks(&tracks, false),
            Command::JumpTo(id) => {
                if self.queue.jump_to(id).is_some() {
                    self.want_playing = true;
                    self.consecutive_failures = 0;
                    self.listen.reset();
                    self.meta_dirty_since.get_or_insert_with(Instant::now);
                    self.start_current(0, false);
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
                if self.queue.current().is_some() {
                    self.drop_chain();
                    self.want_playing = false;
                    self.engine.set_playing(false);
                    self.resume_ms = 0;
                    self.state = PlayState::Paused;
                    self.listen.reset();
                    self.dirty = true;
                }
            }
            Command::Next => {
                self.consecutive_failures = 0;
                match self.queue.next_manual() {
                    Some(_) => {
                        self.listen.reset();
                        self.meta_dirty_since.get_or_insert_with(Instant::now);
                        self.start_current(0, false);
                    }
                    None if self.queue.current().is_some() => self.end_of_queue(),
                    None => {}
                }
            }
            Command::Previous => {
                self.consecutive_failures = 0;
                match self.queue.previous_manual(self.position_ms()) {
                    Some(Previous::Restart) => self.seek_to(0),
                    Some(Previous::Item(_)) => {
                        self.listen.reset();
                        self.meta_dirty_since.get_or_insert_with(Instant::now);
                        self.start_current(0, false);
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
                self.engine.set_volume(volume);
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
                self.dirty = true;
            }
            Command::SetOutputDevice(pref) => {
                let position = self.position_ms();
                self.engine.set_device_pref(pref);
                self.chain = None;
                self.engine.close_output();
                self.device_lost = false;
                self.start_current(position, true);
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
            Command::LibraryChanged => {
                let before = self.queue.len();
                let current_removed = self.queue.retain_tracks(|t| self.host.track(t).is_some());
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
        self.want_playing = true;
        if self.chain.is_some() {
            self.engine.set_playing(true);
            if self.state == PlayState::Paused {
                self.state = PlayState::Playing;
            }
        } else {
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

    fn seek_to(&mut self, ms: u64) {
        if self.queue.current().is_none() {
            return;
        }
        let duration = self.current_duration_ms();
        let target = if duration > 0 { ms.min(duration) } else { ms };
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
            } => {
                let Some(chain) = self.chain.as_mut().filter(|c| c.epoch == epoch) else {
                    return;
                };
                if let Some(last) = chain.segments.back() {
                    // Gapless continuation: only valid if the queue still wants it.
                    let expected = self.queue.peek_after(last.item_id).map(|i| i.id);
                    if expected != Some(item_id) {
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
                if let Some(chain) = self.chain.as_mut() {
                    chain.segments.push_back(Segment {
                        item_id,
                        track_id,
                        at_frame,
                        start_ms,
                        duration_ms,
                        info,
                    });
                }
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

    fn skip_failed_current(&mut self) {
        if self.consecutive_failures >= MAX_CONSECUTIVE_FAILURES.min(self.queue.len().max(1)) {
            self.give_up();
        } else if self.queue.advance_after_failure().is_some() {
            self.start_current(0, false);
        } else {
            self.end_of_queue();
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
            self.listen
                .hear(&finished, chain.epoch, boundary, chain.rate);
            let length_ms = finished.start_ms
                + boundary.saturating_sub(finished.at_frame) * 1000 / u64::from(chain.rate.max(1));
            if self.listen.take_credit(credit_threshold_ms(length_ms)) {
                self.host.record_play(finished.track_id);
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
            self.listen.hear(seg, chain.epoch, end, chain.rate);
            let length_ms =
                seg.start_ms + (end - seg.at_frame) * 1000 / u64::from(chain.rate.max(1));
            // A track that broke mid-way did not play to its end.
            if failed != Some(seg.item_id)
                && self.listen.take_credit(credit_threshold_ms(length_ms))
            {
                self.host.record_play(seg.track_id);
            }
        }
        self.listen.reset();
        self.meta_dirty_since.get_or_insert_with(Instant::now);
        let current = self.queue.current().map(|c| c.id);
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
        if played_epoch == chain.epoch {
            while chain.segments.len() > 1 && frames >= chain.segments[1].at_frame {
                let boundary = chain.segments[1].at_frame;
                let Some(finished) = chain.segments.pop_front() else {
                    break;
                };
                // Played to its natural end.
                self.listen
                    .hear(&finished, chain.epoch, boundary, chain.rate);
                let length_ms = finished.start_ms
                    + boundary.saturating_sub(finished.at_frame) * 1000
                        / u64::from(chain.rate.max(1));
                if self.listen.take_credit(credit_threshold_ms(length_ms)) {
                    self.host.record_play(finished.track_id);
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
            self.listen.hear(seg, chain.epoch, frames, chain.rate);
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
    }

    /// Playback is wanted but nothing is coming: a read stuck on a dead drive, or a device
    /// that no longer asks for audio. Without this the player would wait forever.
    fn watch_for_stall(&mut self) {
        let Some(chain) = &self.chain else {
            self.starved_since = None;
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
            return;
        }
        let since = *self.starved_since.get_or_insert_with(Instant::now);
        if since.elapsed() < self.stall_timeout {
            return;
        }
        self.starved_since = None;
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

    /// Lets go of the file (and the decode thread) after a long pause.
    fn release_paused_feed(&mut self) {
        if self.chain.is_none() || self.want_playing {
            self.paused_since = None;
            return;
        }
        let since = *self.paused_since.get_or_insert_with(Instant::now);
        if since.elapsed() >= PAUSE_RELEASE {
            let position = self.position_ms();
            self.drop_chain();
            self.resume_ms = position;
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

    fn save_session(&mut self, force: bool) {
        let queue_changed = self.queue_dirty_since.take().is_some() || force;
        self.meta_dirty_since = None;
        let session = Session {
            queue: self.queue.to_state(),
            position_ms: self.position_ms(),
            volume: self.volume,
        };
        self.host.save_session(&session, queue_changed);
        self.last_session_save = Instant::now();
    }

    fn save_session_if_due(&mut self) {
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
