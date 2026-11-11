//! A feed: one PCM ring buffer and the decode thread that fills it.
//!
//! A new feed is created for every user-initiated start or seek. When a track ends
//! naturally the same feed continues with the next track, writing into the same ring,
//! which is what makes transitions gapless. Output of an abandoned feed lands in a ring
//! nobody reads, so stale audio can never leak into a newer feed.
use super::decoder::{SourceInfo, TrackDecoder};
use super::dsp::{ReplayGainMode, ReplayGainSettings, replaygain_factor, to_stereo};
use super::render::FeedConsumer;
use super::resample::StereoResampler;
use super::{PlaybackError, PlaybackErrorKind};
use crate::model::{ReplayGainTags, TrackId};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
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
        let worker = Worker {
            current: (params.first.item_id, params.first.track_id),
            refill: ((f64::from(params.out_rate) * RING_REFILL_SECONDS) as usize * 2)
                .clamp(2, capacity / 2),
            params,
            producer,
            cancel: cancel.clone(),
            finished: finished.clone(),
            written: written.clone(),
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
}

impl Worker {
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
        let mut gain_now;

        'tracks: loop {
            self.current = (req.item_id, req.track_id);
            let fail = |error: PlaybackError| FeedEvent::TrackFailed {
                epoch,
                item_id: req.item_id,
                track_id: req.track_id,
                error,
            };
            let mut dec = match TrackDecoder::open(&req.path) {
                Ok(d) => d,
                Err(e) => {
                    self.send(fail(e));
                    break 'tracks;
                }
            };
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
                        if !self.push_all(&resampled) {
                            return false;
                        }
                        written += (resampled.len() / 2) as u64;
                        self.written.store(written, Ordering::Relaxed);
                    }
                }
                if needs_resampling {
                    match StereoResampler::new(info.sample_rate, out_rate) {
                        Ok(r) => {
                            resampler = Some(r);
                            chain_base = written;
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
                None => written,
            };
            self.send(FeedEvent::TrackStarted {
                epoch,
                item_id: req.item_id,
                track_id: req.track_id,
                at_frame,
                start_ms,
                info: info.clone(),
            });
            // A different track is a different signal: its gain applies from its first sample.
            gain_now = replaygain_factor(&req.replaygain, &self.params.rg.get());
            let ramp_step = 1.0 / (GAIN_RAMP_SECONDS * info.sample_rate as f32);

            loop {
                if self.cancelled() {
                    return false;
                }
                #[cfg(test)]
                assert!(
                    self.params.first.item_id != TEST_PANIC_ITEM,
                    "test: decoder panic"
                );
                match dec.next(&mut decoded) {
                    Ok(true) => {
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
                        let pushed = match resampler.as_mut() {
                            Some(rs) => {
                                resampled.clear();
                                if let Err(detail) = rs.process(&stereo, &mut resampled) {
                                    self.send(fail(PlaybackError {
                                        kind: PlaybackErrorKind::Corrupt,
                                        detail,
                                    }));
                                    break 'tracks;
                                }
                                let buf = std::mem::take(&mut resampled);
                                let ok = self.push_all(&buf);
                                written += (buf.len() / 2) as u64;
                                resampled = buf;
                                ok
                            }
                            None => {
                                let buf = std::mem::take(&mut stereo);
                                let ok = self.push_all(&buf);
                                written += (buf.len() / 2) as u64;
                                stereo = buf;
                                ok
                            }
                        };
                        if !pushed {
                            return false;
                        }
                        self.written.store(written, Ordering::Relaxed);
                    }
                    Ok(false) => break,
                    Err(e) => {
                        self.send(fail(e));
                        break 'tracks;
                    }
                }
            }
            match self.wait_next() {
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
                if !self.push_all(&resampled) {
                    return false;
                }
                written += (resampled.len() / 2) as u64;
            }
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
            events,
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
