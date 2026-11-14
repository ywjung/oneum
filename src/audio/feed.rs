//! A feed: one PCM ring buffer and the decode thread that fills it.
//!
//! A new feed is created for every user-initiated start or seek. When a track ends
//! naturally the same feed continues with the next track, writing into the same ring,
//! which is what makes transitions gapless. Output of an abandoned feed lands in a ring
//! nobody reads, so stale audio can never leak into a newer feed.
use super::crossfeed::{Crossfeed, Crossfeeder};
use super::decoder::{SourceInfo, TrackDecoder, map_file_error, sanitize};
use super::dsp::{ReplayGainMode, ReplayGainSettings, replaygain_factor, to_stereo};
use super::eq::{EqShared, Equalizer};
use super::limiter::PeakLimiter;
use super::peq::{Corrector, PeqShared};
use super::render::FeedConsumer;
use super::resample::StereoResampler;
use super::source::{FileInMemory, LoadCompletion};
use super::{PlaybackError, PlaybackErrorKind};
use crate::model::{ReplayGainTags, TrackId};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Ring capacity in seconds of audio. Large enough to ride out scheduling hiccups,
/// small enough that queue edits near a track boundary rarely need a re-feed.
const RING_SECONDS: f64 = 1.0;
/// How long the decode thread waits for the controller to announce the next track.
const NEXT_WAIT: Duration = Duration::from_millis(300);
const GAIN_RAMP_SECONDS: f32 = 0.05;
/// How long the decode thread sleeps when the ring has no room. The ring holds a second
/// of audio, so this is far from letting it run dry, and a paused feed costs almost nothing.
const RING_FULL_SLEEP: Duration = Duration::from_millis(10);
/// The decode thread waits for at least this much room (in seconds of audio) before
/// writing again, instead of trickling a few frames in after every sleep.
const RING_REFILL_SECONDS: f64 = 0.1;
/// Memory playback: how often the decode thread looks whether the next track can be
/// loaded ahead.
const PRELOAD_CHECK: Duration = Duration::from_millis(250);

/// Unit tests only: a feed whose first request has this item id panics while decoding.
#[cfg(test)]
pub(crate) const TEST_PANIC_ITEM: u64 = 0x0DEC_0DE0_DEAD;

/// What to play: enough for the decode thread to work without touching the library.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackRequest {
    /// Queue item this playback belongs to.
    pub item_id: u64,
    pub track_id: TrackId,
    pub path: PathBuf,
    pub replaygain: ReplayGainTags,
}

#[derive(Debug, Clone, PartialEq)]
pub enum NextTrack {
    /// The controller has not decided yet.
    Pending,
    /// Nothing follows; the feed ends after the current track.
    End,
    Track(TrackRequest),
}

/// Slot through which the controller tells the decode thread what follows the current track.
pub struct NextSlot(Mutex<NextTrack>);

impl NextSlot {
    pub fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(NextTrack::Pending)))
    }
    pub fn set(&self, next: NextTrack) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = next;
        }
    }
    /// What is announced right now, without taking it.
    fn peek(&self) -> NextTrack {
        match self.0.lock() {
            Ok(slot) => slot.clone(),
            Err(_) => NextTrack::End,
        }
    }
    /// Takes the announced track, leaving `Pending`. `End` stays in place.
    fn take(&self) -> NextTrack {
        match self.0.lock() {
            Ok(mut slot) => match &*slot {
                NextTrack::Track(_) => std::mem::replace(&mut *slot, NextTrack::Pending),
                other => other.clone(),
            },
            Err(_) => NextTrack::End,
        }
    }
}

/// ReplayGain settings readable from decode threads without locking.
pub struct RgShared {
    mode: AtomicU8,
    preamp_bits: AtomicU32,
    prevent_clipping: AtomicBool,
}

impl RgShared {
    pub fn new(settings: ReplayGainSettings) -> Arc<Self> {
        let s = Arc::new(Self {
            mode: AtomicU8::new(0),
            preamp_bits: AtomicU32::new(0),
            prevent_clipping: AtomicBool::new(true),
        });
        s.set(settings);
        s
    }
    pub fn set(&self, settings: ReplayGainSettings) {
        let mode = match settings.mode {
            ReplayGainMode::Off => 0,
            ReplayGainMode::Track => 1,
            ReplayGainMode::Album => 2,
        };
        self.mode.store(mode, Ordering::Relaxed);
        self.preamp_bits
            .store(settings.preamp_db.to_bits(), Ordering::Relaxed);
        self.prevent_clipping
            .store(settings.prevent_clipping, Ordering::Relaxed);
    }
    pub fn get(&self) -> ReplayGainSettings {
        ReplayGainSettings {
            mode: match self.mode.load(Ordering::Relaxed) {
                1 => ReplayGainMode::Track,
                2 => ReplayGainMode::Album,
                _ => ReplayGainMode::Off,
            },
            preamp_db: f32::from_bits(self.preamp_bits.load(Ordering::Relaxed)),
            prevent_clipping: self.prevent_clipping.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum FeedEvent {
    /// The decode thread started writing this track at output frame `at_frame` of the feed.
    TrackStarted {
        epoch: u16,
        item_id: u64,
        track_id: TrackId,
        at_frame: u64,
        /// Position inside the track (ms) that `at_frame` corresponds to.
        start_ms: u64,
        info: SourceInfo,
        /// What the peak limiter's counter (`FeedParams::limited`) stood at when the
        /// track began.
        limited_before: u64,
    },
    /// The track could not be opened or broke mid-way. The feed ends after what was written.
    TrackFailed {
        epoch: u16,
        item_id: u64,
        track_id: TrackId,
        error: PlaybackError,
    },
    /// The decode thread wrote its last frame; `total_frames` is the feed length.
    Finished { epoch: u16, total_frames: u64 },
}

pub struct FeedHandle {
    pub epoch: u16,
    cancel: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    written: Arc<AtomicU64>,
    consumer: Option<FeedConsumer>,
}

pub struct FeedParams {
    pub epoch: u16,
    pub out_rate: u32,
    pub first: TrackRequest,
    pub start_ms: u64,
    pub next: Arc<NextSlot>,
    pub rg: Arc<RgShared>,
    pub events: Sender<FeedEvent>,
    /// Memory playback: load each file into memory while it plays, and the next one
    /// ahead of time.
    pub memory: bool,
    /// Counts the frames whose gain the peak limiter turned down (all feeds together).
    pub limited: Arc<AtomicU64>,
    /// The equalizer's settings; followed while the feed runs.
    pub eq: Arc<EqShared>,
    /// The headphone crossfeed setting (`Crossfeed::to_u8`); followed like the equalizer.
    pub crossfeed: Arc<AtomicU8>,
    /// The parametric correction profile; followed like the equalizer.
    pub peq: Arc<PeqShared>,
}

impl FeedHandle {
    /// Starts decoding in a new thread. Returns `None` only if the thread cannot be spawned.
    pub fn spawn(params: FeedParams) -> Option<FeedHandle> {
        let capacity = ((f64::from(params.out_rate) * RING_SECONDS) as usize).max(4096) * 2;
        let (producer, consumer) = rtrb::RingBuffer::new(capacity);
        let cancel = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let written = Arc::new(AtomicU64::new(0));
        let epoch = params.epoch;
        let from_start = params.start_ms == 0;
        let worker = Worker {
            current: (params.first.item_id, params.first.track_id),
            refill: ((f64::from(params.out_rate) * RING_REFILL_SECONDS) as usize * 2)
                .clamp(2, capacity / 2),
            params,
            producer,
            cancel: cancel.clone(),
            finished: finished.clone(),
            written: written.clone(),
            preload: None,
            current_load: None,
            current_file: None,
            next_in_memory: None,
            last_preload_check: Instant::now(),
        };
        std::thread::Builder::new()
            .name(format!("oneum-decode-{epoch}"))
            .spawn(move || worker.run())
            .ok()?;
        Some(FeedHandle {
            epoch,
            cancel,
            finished: finished.clone(),
            written,
            consumer: Some(FeedConsumer {
                epoch,
                consumer,
                finished,
                from_start,
            }),
        })
    }

    /// Output frames written to the ring so far.
    pub fn written_frames(&self) -> u64 {
        self.written.load(Ordering::Relaxed)
    }
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }
    pub fn take_consumer(&mut self) -> Option<FeedConsumer> {
        self.consumer.take()
    }
    pub fn put_back_consumer(&mut self, consumer: FeedConsumer) {
        self.consumer = Some(consumer);
    }
}

impl Drop for FeedHandle {
    fn drop(&mut self) {
        // The thread notices within a few milliseconds; it is not joined so that a read
        // stuck on a dead drive can never block the controller.
        self.cancel.store(true, Ordering::Relaxed);
    }
}

struct Worker {
    /// `(item, track)` being decoded, for the failure report of a panic.
    current: (u64, TrackId),
    /// Samples of room to wait for when the ring is full.
    refill: usize,
    params: FeedParams,
    producer: rtrb::Producer<f32>,
    cancel: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    written: Arc<AtomicU64>,
    /// Memory playback: the track after the current one, being opened and loaded ahead.
    preload: Option<Preload>,
    /// Memory playback: turns true when the current track is entirely in memory.
    current_load: Option<LoadCompletion>,
    /// Memory playback: the audio file the current track is read from, in memory. The
    /// next track of the same file (a CUE sheet's) reads it too instead of loading it
    /// again.
    current_file: Option<(PathBuf, FileInMemory)>,
    /// The announced next track, found to be read from `current_file`.
    next_in_memory: Option<PathBuf>,
    last_preload_check: Instant,
}

/// A file being opened ahead of its turn, on a thread of its own: opening can block for
/// seconds (a drive that has to spin up, a share that does not answer), and the decode
/// thread must go on feeding the track that is playing meanwhile.
struct Preload {
    path: PathBuf,
    /// The opened file, once: `None` inside when it is not suited or could not be opened.
    opened: Receiver<Option<FileInMemory>>,
}

impl Preload {
    fn start(path: &std::path::Path) -> Option<Self> {
        let (tx, opened) = channel();
        let target = path.to_path_buf();
        std::thread::Builder::new()
            .name("oneum-preopen".into())
            .spawn(move || {
                // A failure is not reported from here: the track is opened again when
                // its turn comes, and fails then if it is still unreadable.
                // A track of a CUE sheet loads the sheet's audio file.
                let file = crate::library::cue::real_file(&target);
                let _ = tx.send(FileInMemory::start(&file).ok().flatten());
            })
            .ok()?;
        Some(Self {
            path: path.to_path_buf(),
            opened,
        })
    }

    /// The file, if it has been opened by now and is suited for memory playback.
    fn take(self) -> Option<FileInMemory> {
        self.opened.try_recv().ok().flatten()
    }
}

impl Worker {
    /// Opens the decoder for `req`: from memory when memory playback is on (using the
    /// file loaded ahead, if it is this one), from the file otherwise.
    fn open_decoder(&mut self, req: &TrackRequest) -> Result<TrackDecoder, PlaybackError> {
        self.current_load = None;
        if !self.params.memory {
            return TrackDecoder::open(&req.path);
        }
        // What is decided about the track after the one opened now is decided again.
        self.next_in_memory = None;
        // A track of a CUE sheet is read from the sheet's audio file (the sheet is read
        // once, for the file and the stretch).
        let segment = crate::library::cue::resolve(&req.path);
        let real = segment
            .as_ref()
            .map_or_else(|| req.path.clone(), |s| s.audio.clone());
        let ahead = self
            .preload
            .take()
            .filter(|p| p.path == req.path)
            .and_then(Preload::take);
        let same_file = self
            .current_file
            .as_ref()
            .filter(|(path, _)| *path == real)
            .map(|(_, file)| file.share());
        let file = match ahead.or(same_file) {
            Some(file) => Some(file),
            None => FileInMemory::start(&real).map_err(|e| map_file_error(&e))?,
        };
        match file {
            Some(file) => {
                self.current_load = Some(file.completion());
                self.current_file = Some((real, file.share()));
                TrackDecoder::open_in_memory(file)?.within(segment.as_ref())
            }
            // Too large for memory: streamed like without memory playback.
            None => {
                self.current_file = None;
                TrackDecoder::open_located(&req.path, segment.as_ref())
            }
        }
    }

    /// Memory playback: once the current track is in memory, start loading the one the
    /// controller has announced to follow (on a thread of its own, see [`Preload`]). One
    /// file is read at a time, and the disk is
    /// used long before the track boundary: a drive that went to sleep during a long
    /// track has the whole track to wake up.
    fn preload_next(&mut self) {
        if !self.params.memory || self.last_preload_check.elapsed() < PRELOAD_CHECK {
            return;
        }
        self.last_preload_check = Instant::now();
        if self.current_load.as_ref().is_some_and(|c| !c.is_complete()) {
            return;
        }
        match self.params.next.peek() {
            NextTrack::Track(next) => {
                // Once per track that is announced: a file that cannot be loaded ahead
                // (too large, unreadable) is not tried again and again.
                let decided = self.preload.as_ref().is_some_and(|p| p.path == next.path)
                    || self.next_in_memory.as_ref() == Some(&next.path);
                if decided {
                    return;
                }
                // The next track of the same CUE sheet's file: it is in memory already.
                let in_memory = crate::library::cue::sheet_of(&next.path).is_some()
                    && self.current_file.as_ref().is_some_and(|(path, _)| {
                        *path == crate::library::cue::real_file(&next.path).as_ref()
                    });
                if in_memory {
                    self.preload = None;
                    self.next_in_memory = Some(next.path.clone());
                } else {
                    self.preload = Preload::start(&next.path);
                }
            }
            NextTrack::End => self.preload = None,
            // Not decided yet (or already taken): keep what is there.
            NextTrack::Pending => {}
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Blocks until all samples are in the ring. Returns false when cancelled.
    fn push_all(&mut self, samples: &[f32]) -> bool {
        let mut offset = 0;
        while offset < samples.len() {
            if self.cancelled() {
                return false;
            }
            // Keep whole frames together so the reader always sees an even sample count.
            let free = self.producer.slots() & !1;
            let rest = samples.len() - offset;
            if free < rest.min(self.refill) {
                std::thread::sleep(RING_FULL_SLEEP);
                continue;
            }
            let n = free.min(rest);
            let Ok(mut chunk) = self.producer.write_chunk(n) else {
                continue;
            };
            let (a, b) = chunk.as_mut_slices();
            a.copy_from_slice(&samples[offset..offset + a.len()]);
            b.copy_from_slice(&samples[offset + a.len()..offset + n]);
            chunk.commit_all();
            offset += n;
        }
        true
    }

    /// Sends `samples` through the peak limiter and on into the ring. Returns the
    /// frames written (the limiter keeps its look-ahead back), `None` when cancelled.
    fn push_limited(
        &mut self,
        limiter: &mut PeakLimiter,
        samples: &[f32],
        scratch: &mut Vec<f32>,
    ) -> Option<u64> {
        scratch.clear();
        let before = limiter.limited_frames();
        limiter.process(samples, scratch);
        self.finish_push(limiter, before, scratch)
    }

    /// Writes what the limiter still holds: the end of the feed.
    fn push_limiter_tail(
        &mut self,
        limiter: &mut PeakLimiter,
        scratch: &mut Vec<f32>,
    ) -> Option<u64> {
        scratch.clear();
        let before = limiter.limited_frames();
        limiter.flush(scratch);
        self.finish_push(limiter, before, scratch)
    }

    fn finish_push(&mut self, limiter: &PeakLimiter, before: u64, ready: &[f32]) -> Option<u64> {
        let newly = limiter.limited_frames() - before;
        if newly > 0 {
            self.params.limited.fetch_add(newly, Ordering::Relaxed);
        }
        self.push_all(ready).then_some((ready.len() / 2) as u64)
    }

    fn wait_next(&self) -> Option<TrackRequest> {
        let deadline = Instant::now() + NEXT_WAIT;
        loop {
            match self.params.next.take() {
                NextTrack::Track(req) => return Some(req),
                NextTrack::End => return None,
                NextTrack::Pending => {
                    if self.cancelled() || Instant::now() >= deadline {
                        return None;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }

    fn send(&self, event: FeedEvent) {
        let _ = self.params.events.send(event);
    }

    /// Thread body. Whatever happens inside the decoder, the feed always ends with
    /// `finished` set and a `Finished` event (unless it was cancelled), so the player can
    /// never be left waiting for audio that will not come.
    fn run(mut self) {
        let epoch = self.params.epoch;
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.decode_tracks()));
        match outcome {
            Ok(true) => {}
            // Cancelled: nobody listens to this feed any more.
            Ok(false) => return,
            Err(_) => {
                tracing::error!("decode thread panicked; skipping the track");
                let (item_id, track_id) = self.current;
                self.send(FeedEvent::TrackFailed {
                    epoch,
                    item_id,
                    track_id,
                    error: PlaybackError {
                        kind: PlaybackErrorKind::Corrupt,
                        detail: "decoder panicked".into(),
                    },
                });
            }
        }
        let written = self.written.load(Ordering::Relaxed);
        self.finished.store(true, Ordering::Release);
        self.send(FeedEvent::Finished {
            epoch,
            total_frames: written,
        });
    }

    /// Decodes track after track into the ring. Returns false when cancelled.
    fn decode_tracks(&mut self) -> bool {
        let epoch = self.params.epoch;
        let out_rate = self.params.out_rate;
        let mut req = self.params.first.clone();
        let mut start_ms = self.params.start_ms;
        let mut written: u64 = 0;
        let mut resampler: Option<StereoResampler> = None;
        // Output frames written before the current resampler instance was created.
        let mut chain_base: u64 = 0;
        let mut decoded: Vec<f32> = Vec::new();
        let mut stereo: Vec<f32> = Vec::new();
        let mut resampled: Vec<f32> = Vec::new();
        // The last stage before the ring, one for the whole feed: it runs across track
        // boundaries like the audio does. `written` counts what has left it.
        let mut limiter = PeakLimiter::new(out_rate);
        let mut limited: Vec<f32> = Vec::new();
        // The equalizer, in front of the limiter and like it one for the whole feed.
        let mut eq_version = self.params.eq.version();
        let mut eq = Equalizer::new(out_rate, self.params.eq.get());
        let crossfeed_now =
            |params: &FeedParams| Crossfeed::from_u8(params.crossfeed.load(Ordering::Relaxed));
        let mut crossfeed = Crossfeeder::new(out_rate, crossfeed_now(&self.params));
        // The parametric correction, between the equalizer and the crossfeed.
        let mut peq_version = self.params.peq.version();
        let mut corrector = Corrector::new(out_rate, self.params.peq.get().as_deref());
        let mut gain_now;
        // The next track's decoder when it was opened early: a DSD track's last frames
        // are computed with the beginning of the one after it.
        let mut opened: Option<Result<TrackDecoder, PlaybackError>> = None;

        'tracks: loop {
            self.current = (req.item_id, req.track_id);
            let fail = |error: PlaybackError| FeedEvent::TrackFailed {
                epoch,
                item_id: req.item_id,
                track_id: req.track_id,
                error,
            };
            let opening = match opened.take() {
                Some(early) => early,
                None => self.open_decoder(&req),
            };
            let mut dec = match opening {
                Ok(d) => d,
                Err(e) => {
                    self.send(fail(e));
                    break 'tracks;
                }
            };
            dec.defer_tail();
            let info = dec.info().clone();
            if start_ms > 0 {
                let frame = start_ms.saturating_mul(u64::from(info.sample_rate)) / 1000;
                if let Err(e) = dec.seek(frame) {
                    self.send(fail(e));
                    break 'tracks;
                }
            }
            // Keep the resampler across tracks of the same rate so the signal stays continuous.
            let needs_resampling = info.sample_rate != out_rate;
            let reusable = resampler
                .as_ref()
                .is_some_and(|r| needs_resampling && r.in_rate() == info.sample_rate);
            if !reusable {
                if let Some(mut old) = resampler.take() {
                    resampled.clear();
                    if old.flush(&mut resampled).is_ok() {
                        eq.process(&mut resampled);
                        corrector.process(&mut resampled);
                        crossfeed.process(&mut resampled);
                        sanitize(&mut resampled);
                        match self.push_limited(&mut limiter, &resampled, &mut limited) {
                            Some(frames) => written += frames,
                            None => return false,
                        }
                        self.written.store(written, Ordering::Relaxed);
                    }
                }
                if needs_resampling {
                    match StereoResampler::new(info.sample_rate, out_rate) {
                        Ok(r) => {
                            resampler = Some(r);
                            chain_base = written + limiter.held_frames();
                        }
                        Err(detail) => {
                            self.send(fail(PlaybackError {
                                kind: PlaybackErrorKind::Unsupported,
                                detail,
                            }));
                            break 'tracks;
                        }
                    }
                }
            }
            let at_frame = match &resampler {
                Some(r) => chain_base + r.expected_out_frames(),
                None => written + limiter.held_frames(),
            };
            self.send(FeedEvent::TrackStarted {
                epoch,
                item_id: req.item_id,
                track_id: req.track_id,
                at_frame,
                start_ms,
                info: info.clone(),
                limited_before: self.params.limited.load(Ordering::Relaxed),
            });
            // A different track is a different signal: its gain applies from its first sample.
            gain_now = replaygain_factor(&req.replaygain, &self.params.rg.get());
            let ramp_step = 1.0 / (GAIN_RAMP_SECONDS * info.sample_rate as f32);
            // What follows, when it had to be known before this track's end (see above).
            let mut followed: Option<Option<TrackRequest>> = None;

            loop {
                if self.cancelled() {
                    return false;
                }
                #[cfg(test)]
                assert!(
                    self.params.first.item_id != TEST_PANIC_ITEM,
                    "test: decoder panic"
                );
                self.preload_next();
                let mut last = false;
                match dec.next(&mut decoded) {
                    Ok(true) => {}
                    Ok(false) if dec.holds_tail() => {
                        // DSD: the last frames depend on what follows. When that is
                        // DSD of the same kind, the two convert as one stream.
                        let next = self.wait_next();
                        let mut early = next.as_ref().map(|n| self.open_decoder(n));
                        let following = match early.as_mut() {
                            Some(Ok(d)) => Some(d),
                            _ => None,
                        };
                        if let Err(e) = dec.finish(following, &mut decoded) {
                            self.send(fail(e));
                            break 'tracks;
                        }
                        followed = Some(next);
                        opened = early;
                        last = true;
                    }
                    Ok(false) => break,
                    Err(e) => {
                        self.send(fail(e));
                        break 'tracks;
                    }
                }
                if !decoded.is_empty() {
                    stereo.clear();
                    to_stereo(&decoded, info.channels, &mut stereo);
                    // Settings may change while playing: ramp towards the new gain.
                    let target = replaygain_factor(&req.replaygain, &self.params.rg.get());
                    if (gain_now - target).abs() > f32::EPSILON {
                        for frame in stereo.as_chunks_mut::<2>().0 {
                            if gain_now < target {
                                gain_now = (gain_now + ramp_step).min(target);
                            } else if gain_now > target {
                                gain_now = (gain_now - ramp_step).max(target);
                            }
                            frame[0] *= gain_now;
                            frame[1] *= gain_now;
                        }
                    } else if gain_now != 1.0 {
                        for s in &mut stereo {
                            *s *= gain_now;
                        }
                    }
                    let ready = match resampler.as_mut() {
                        Some(rs) => {
                            resampled.clear();
                            if let Err(detail) = rs.process(&stereo, &mut resampled) {
                                self.send(fail(PlaybackError {
                                    kind: PlaybackErrorKind::Corrupt,
                                    detail,
                                }));
                                break 'tracks;
                            }
                            &mut resampled
                        }
                        None => &mut stereo,
                    };
                    // The settings may change while playing: the equalizer glides.
                    let version = self.params.eq.version();
                    if version != eq_version {
                        eq_version = version;
                        eq.set(self.params.eq.get());
                    }
                    // A sample that the gain or the rate conversion pushed out
                    // of the number range must not get into the filters (it
                    // would stay in them), and what leaves them must be numbers.
                    sanitize(ready);
                    eq.process(ready);
                    let version = self.params.peq.version();
                    if version != peq_version {
                        peq_version = version;
                        corrector.set(self.params.peq.get().as_deref());
                    }
                    corrector.process(ready);
                    crossfeed.set(crossfeed_now(&self.params));
                    crossfeed.process(ready);
                    sanitize(ready);
                    match self.push_limited(&mut limiter, ready, &mut limited) {
                        Some(frames) => written += frames,
                        None => return false,
                    }
                    self.written.store(written, Ordering::Relaxed);
                }
                if last {
                    break;
                }
            }
            let next = match followed.take() {
                Some(next) => next,
                None => self.wait_next(),
            };
            match next {
                Some(next) => {
                    req = next;
                    start_ms = 0;
                }
                None => break 'tracks,
            }
        }

        if let Some(mut rs) = resampler.take() {
            resampled.clear();
            if rs.flush(&mut resampled).is_ok() {
                eq.process(&mut resampled);
                corrector.process(&mut resampled);
                crossfeed.process(&mut resampled);
                sanitize(&mut resampled);
                match self.push_limited(&mut limiter, &resampled, &mut limited) {
                    Some(frames) => written += frames,
                    None => return false,
                }
            }
        }
        match self.push_limiter_tail(&mut limiter, &mut limited) {
            Some(frames) => written += frames,
            None => return false,
        }
        self.written.store(written, Ordering::Relaxed);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    fn params(item_id: u64, events: Sender<FeedEvent>) -> FeedParams {
        FeedParams {
            epoch: 7,
            out_rate: 44_100,
            first: TrackRequest {
                item_id,
                track_id: 3,
                path: fixture("gap_a.wav"),
                replaygain: ReplayGainTags::default(),
            },
            start_ms: 0,
            next: {
                let slot = NextSlot::new();
                slot.set(NextTrack::End);
                slot
            },
            rg: RgShared::new(ReplayGainSettings::default()),
            limited: Arc::new(AtomicU64::new(0)),
            eq: EqShared::new(Default::default()),
            crossfeed: Arc::new(AtomicU8::new(0)),
            peq: PeqShared::new(None),
            events,
            memory: false,
        }
    }

    fn collect(rx: &std::sync::mpsc::Receiver<FeedEvent>) -> Vec<FeedEvent> {
        let mut events = Vec::new();
        while let Ok(event) = rx.recv_timeout(Duration::from_secs(10)) {
            let last = matches!(event, FeedEvent::Finished { .. });
            events.push(event);
            if last {
                break;
            }
        }
        events
    }

    /// A panic inside the decoder must end the feed like any other failure: the track is
    /// reported as failed and the feed as finished, so the player moves on.
    #[test]
    fn decoder_panic_is_reported_as_a_failed_track() {
        let (tx, rx) = channel();
        let mut feed = FeedHandle::spawn(params(TEST_PANIC_ITEM, tx)).expect("spawn");
        let _consumer = feed.take_consumer();
        let events = collect(&rx);
        assert!(
            events.iter().any(|e| matches!(
                e,
                FeedEvent::TrackFailed {
                    epoch: 7,
                    item_id: TEST_PANIC_ITEM,
                    track_id: 3,
                    error,
                } if error.kind == PlaybackErrorKind::Corrupt
            )),
            "{events:?}"
        );
        assert!(matches!(
            events.last(),
            Some(FeedEvent::Finished { epoch: 7, .. })
        ));
        assert!(feed.is_finished());
    }

    /// Memory playback: a track that is in memory plays to its end although its file
    /// is emptied underneath it. Without memory playback the same thing cuts it short
    /// (the second half of this test), which is what makes the first half meaningful.
    #[test]
    fn a_track_in_memory_does_not_need_its_file_any_more() {
        fn frames_after_emptying_the_second_file(memory: bool) -> u64 {
            let dir = tempfile::tempdir().expect("tempdir");
            let (a, b) = (dir.path().join("a.wav"), dir.path().join("b.wav"));
            std::fs::copy(fixture("gap_a.wav"), &a).expect("copy");
            std::fs::copy(fixture("gap_b.wav"), &b).expect("copy");
            let (tx, rx) = channel();
            let mut p = params(1, tx);
            p.memory = memory;
            p.first.path = a;
            p.next = NextSlot::new();
            p.next.set(NextTrack::Track(TrackRequest {
                item_id: 2,
                track_id: 4,
                path: b.clone(),
                replaygain: ReplayGainTags::default(),
            }));
            let next = p.next.clone();
            let mut feed = FeedHandle::spawn(p).expect("spawn");
            let mut consumer = feed.take_consumer().expect("consumer").consumer;
            // The ring holds the first track exactly, so the decode thread opens the
            // second one right away and then waits for room.
            let mut events = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(10);
            while !events
                .iter()
                .any(|e| matches!(e, FeedEvent::TrackStarted { item_id: 2, .. }))
            {
                assert!(Instant::now() < deadline, "second track never opened");
                if let Ok(event) = rx.recv_timeout(Duration::from_millis(50)) {
                    events.push(event);
                }
            }
            next.set(NextTrack::End);
            // Time for the loader to read 176 kB (it started when the track was opened).
            std::thread::sleep(Duration::from_millis(300));
            std::fs::File::create(&b).expect("truncate");
            let mut read = 0u64;
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                let available = consumer.slots();
                if available > 0 {
                    consumer.read_chunk(available).expect("chunk").commit_all();
                    read += available as u64;
                } else if feed.is_finished() && consumer.slots() == 0 {
                    break;
                }
                assert!(Instant::now() < deadline, "feed never finished");
                std::thread::sleep(Duration::from_millis(1));
            }
            read / 2
        }
        assert_eq!(frames_after_emptying_the_second_file(true), 88_200);
        assert!(frames_after_emptying_the_second_file(false) < 88_200);
    }

    /// Memory playback must not let the next file hold up the one that is playing: the
    /// next "file" here is a FIFO, whose open blocks for ever (a stand-in for a drive
    /// that does not answer). The track in front of it still plays to its end.
    #[cfg(unix)]
    #[test]
    fn a_next_file_that_never_opens_does_not_stall_the_playing_track() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Three seconds of audio, so that the look-ahead certainly starts mid-track.
        let long = dir.path().join("long.wav");
        let frames: u32 = 44_100 * 3;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + frames * 4).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&44_100u32.to_le_bytes());
        wav.extend_from_slice(&(44_100u32 * 4).to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(frames * 4).to_le_bytes());
        wav.resize(wav.len() + frames as usize * 4, 0);
        std::fs::write(&long, wav).expect("write");
        let stuck = dir.path().join("stuck.wav");
        let made = std::process::Command::new("mkfifo")
            .arg(&stuck)
            .status()
            .expect("mkfifo runs");
        assert!(made.success());

        let (tx, rx) = channel();
        let mut p = params(1, tx);
        p.memory = true;
        p.first.path = long;
        p.next = NextSlot::new();
        p.next.set(NextTrack::Track(TrackRequest {
            item_id: 2,
            track_id: 4,
            path: stuck,
            replaygain: ReplayGainTags::default(),
        }));
        let next = p.next.clone();
        let mut feed = FeedHandle::spawn(p).expect("spawn");
        let mut consumer = feed.take_consumer().expect("consumer").consumer;
        // Read at about four times real time; the look-ahead (every 250 ms) is tried
        // several times while the track is being decoded.
        let mut read = 0u64;
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut ended = false;
        loop {
            let available = consumer.slots().min(2 * 1764);
            if available > 0 {
                consumer.read_chunk(available).expect("chunk").commit_all();
                read += available as u64;
            } else if feed.is_finished() && consumer.slots() == 0 {
                break;
            }
            // Before the track is over the controller withdraws the stuck file, as the
            // player does when the queue changes; otherwise the decode thread would go
            // on to open it, which is the stall watchdog's business, not this test's.
            if !ended && read / 2 >= u64::from(frames) * 2 / 3 {
                next.set(NextTrack::End);
                ended = true;
            }
            assert!(
                Instant::now() < deadline,
                "the playing track stalled after {} of {frames} frames",
                read / 2
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(read / 2, u64::from(frames));
        let events = collect(&rx);
        assert!(matches!(events.last(), Some(FeedEvent::Finished { .. })));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, FeedEvent::TrackFailed { .. }))
        );
    }

    /// The reader is slower than the decoder: everything still arrives, in order, although
    /// the decode thread now waits for room instead of trickling frames in.
    #[test]
    fn slow_reader_receives_every_frame() {
        let (tx, rx) = channel();
        let mut feed = FeedHandle::spawn(params(1, tx)).expect("spawn");
        let mut consumer = feed.take_consumer().expect("consumer").consumer;
        let mut read: u64 = 0;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let available = consumer.slots();
            if available > 0 {
                let chunk = consumer.read_chunk(available).expect("chunk");
                read += available as u64;
                chunk.commit_all();
            } else if feed.is_finished() && consumer.slots() == 0 {
                break;
            }
            assert!(Instant::now() < deadline, "feed never finished");
            std::thread::sleep(Duration::from_millis(1));
        }
        let total = collect(&rx)
            .iter()
            .find_map(|e| match e {
                FeedEvent::Finished { total_frames, .. } => Some(*total_frames),
                _ => None,
            })
            .expect("finished event");
        assert_eq!(read, total * 2);
        assert!(total > 0);
    }
}
