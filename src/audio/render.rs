//! The real-time side of the pipeline: what runs inside the audio output callback.
//!
//! Rules for everything in [`Renderer::render`]: no allocation or deallocation, no locks,
//! no I/O, no logging. It only reads pre-filled ring buffers and atomics.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

/// Frames processed per inner block (stack buffer size).
const BLOCK: usize = 256;
const FADE_SECONDS: f32 = 0.006;
/// After the ring ran dry: how much has to be there before playing goes on (what a
/// new feed is given before it is first heard, see the engine's prebuffer).
const REFILL_SECONDS: f32 = 0.12;
const VOLUME_RAMP_SECONDS: f32 = 0.03;
const EPOCH_SHIFT: u32 = 48;
const FRAME_MASK: u64 = (1 << EPOCH_SHIFT) - 1;

/// One reading of the [`LevelMeter`], per channel (left, right), as linear amplitude
/// where 1.0 is full scale.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Levels {
    /// Mean of the absolute sample values over the most recent callback: what a VU
    /// meter's rectifier sees.
    pub average: [f32; 2],
    /// Highest absolute sample value since the previous reading.
    pub peak: [f32; 2],
}

/// Signal level of what is being played, for the VU meter. Written by the output
/// callback (atomics only), read by the UI.
///
/// The level is that of the programme itself: after ReplayGain and the pause/seek fades,
/// before the app volume. A meter that sank with the volume slider would say nothing
/// about the music.
#[derive(Debug, Default)]
pub struct LevelMeter {
    average: [AtomicU32; 2],
    peak: [AtomicU32; 2],
}

impl LevelMeter {
    fn store(&self, average: [f32; 2], peak: [f32; 2]) {
        for ch in 0..2 {
            self.average[ch].store(average[ch].to_bits(), Ordering::Relaxed);
            // Bit patterns of non-negative floats order like the floats themselves.
            self.peak[ch].fetch_max(peak[ch].to_bits(), Ordering::Relaxed);
        }
    }

    /// The current levels. Peaks are handed out once: reading clears them.
    pub fn read(&self) -> Levels {
        let get = |a: &AtomicU32| f32::from_bits(a.load(Ordering::Relaxed));
        let take = |a: &AtomicU32| f32::from_bits(a.swap(0, Ordering::Relaxed));
        Levels {
            average: [get(&self.average[0]), get(&self.average[1])],
            peak: [take(&self.peak[0]), take(&self.peak[1])],
        }
    }

    /// Back to silence: nothing is rendering any more (the stream was closed).
    pub fn reset(&self) {
        for ch in 0..2 {
            self.average[ch].store(0, Ordering::Relaxed);
            self.peak[ch].store(0, Ordering::Relaxed);
        }
    }
}

/// Samples kept for the spectrum display: enough for its analysis at the highest sample
/// rates, where it looks at four times as many samples.
pub const SCOPE_LEN: usize = 8192;

/// The most recent samples of what is being played (left and right mixed to one
/// channel, before the app volume), for the spectrum display. One writer (the output
/// callback, atomics only), any reader; a reader may catch a few samples of the next
/// block mixed in, which a picture of the spectrum does not show.
pub struct Scope {
    samples: Box<[AtomicU32]>,
    written: AtomicUsize,
}

impl Default for Scope {
    fn default() -> Self {
        Self {
            samples: (0..SCOPE_LEN).map(|_| AtomicU32::new(0)).collect(),
            written: AtomicUsize::new(0),
        }
    }
}

impl Scope {
    fn push(&self, sample: f32) {
        let at = self.written.load(Ordering::Relaxed);
        self.samples[at % SCOPE_LEN].store(sample.to_bits(), Ordering::Relaxed);
        self.written.store(at.wrapping_add(1), Ordering::Relaxed);
    }

    /// Copies the latest `out.len()` samples (at most [`SCOPE_LEN`]), oldest first.
    /// What has not been played yet reads as silence.
    pub fn latest(&self, out: &mut [f32]) {
        let n = out.len().min(SCOPE_LEN);
        let end = self.written.load(Ordering::Relaxed);
        let have = end.min(n);
        let (silence, filled) = out.split_at_mut(out.len() - have);
        silence.fill(0.0);
        for (k, slot) in filled.iter_mut().enumerate() {
            let at = end - have + k;
            *slot = f32::from_bits(self.samples[at % SCOPE_LEN].load(Ordering::Relaxed));
        }
    }

    /// Forgets everything (the stream was closed).
    pub fn reset(&self) {
        self.written.store(0, Ordering::Relaxed);
        for sample in self.samples.iter() {
            sample.store(0, Ordering::Relaxed);
        }
    }
}

/// State shared between the callback and the control thread. Atomics only.
pub struct RenderShared {
    volume_bits: AtomicU32,
    playing: AtomicBool,
    /// `(epoch << 48) | frames consumed from that feed`.
    position: AtomicU64,
    underruns: AtomicU64,
    /// Epoch of the feed that has been played to its end (0 = none).
    drained_epoch: AtomicU32,
    latency_frames: AtomicU32,
    callbacks: AtomicU64,
    leaked_feeds: AtomicU32,
    /// When non-zero, only the feed with this epoch may be heard; any other feed is faded
    /// out and held until the required one arrives.
    required_epoch: AtomicU32,
    meter: Arc<LevelMeter>,
    scope: Arc<Scope>,
    /// Size of one step of the device's integer sample format (0.0 = no dither: the
    /// device takes floating point, or its format is not known).
    dither_lsb: AtomicU32,
    /// The samples of the feed are no longer the file's own (resampled, gain applied,
    /// decoded from a lossy codec, wider than the device): rounding them to the
    /// device's format needs dither.
    processed: AtomicBool,
}

impl RenderShared {
    pub fn new(volume_gain: f32) -> Arc<Self> {
        Arc::new(Self {
            volume_bits: AtomicU32::new(volume_gain.to_bits()),
            playing: AtomicBool::new(false),
            position: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            drained_epoch: AtomicU32::new(0),
            latency_frames: AtomicU32::new(0),
            callbacks: AtomicU64::new(0),
            leaked_feeds: AtomicU32::new(0),
            required_epoch: AtomicU32::new(0),
            meter: Arc::new(LevelMeter::default()),
            scope: Arc::new(Scope::default()),
            dither_lsb: AtomicU32::new(0f32.to_bits()),
            processed: AtomicBool::new(false),
        })
    }
    /// The samples for the spectrum display, fed by the callback.
    pub fn scope(&self) -> &Arc<Scope> {
        &self.scope
    }
    /// Tells the callback the device's integer sample format: `Some(bits)` for an
    /// integer format of that width, `None` for floating point or unknown.
    pub fn set_dither_bits(&self, bits: Option<u32>) {
        let lsb = match bits {
            Some(bits @ 8..=24) => 1.0 / (1u32 << (bits - 1)) as f32,
            // 32-bit integer is finer than the 24 bits a float sample carries.
            _ => 0.0,
        };
        self.dither_lsb.store(lsb.to_bits(), Ordering::Relaxed);
    }
    /// See the `processed` field.
    pub fn set_processed(&self, processed: bool) {
        self.processed.store(processed, Ordering::Relaxed);
    }
    /// True when dither is being added: the device is an integer device and the samples
    /// are not passed through untouched (processed, or the app volume is below 100 %).
    pub fn dither_active(&self) -> bool {
        f32::from_bits(self.dither_lsb.load(Ordering::Relaxed)) > 0.0
            && (self.processed.load(Ordering::Relaxed)
                || f32::from_bits(self.volume_bits.load(Ordering::Relaxed)) != 1.0)
    }
    /// The level meter fed by the callback.
    pub fn meter(&self) -> &Arc<LevelMeter> {
        &self.meter
    }
    /// Sets the linear output gain (already mapped from the volume slider).
    pub fn set_volume_gain(&self, gain: f32) {
        self.volume_bits
            .store(gain.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }
    pub fn set_playing(&self, playing: bool) {
        self.playing.store(playing, Ordering::Relaxed);
    }
    /// Silences every feed except `epoch` (see `required_epoch`). Called when the user
    /// starts another track or seeks, so the old audio stops at once instead of playing
    /// on until the new feed has buffered.
    pub fn require_epoch(&self, epoch: u16) {
        self.required_epoch
            .store(u32::from(epoch), Ordering::Relaxed);
    }
    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }
    /// `(epoch, frames consumed)` of the feed the callback is currently reading.
    pub fn position(&self) -> (u16, u64) {
        let v = self.position.load(Ordering::Relaxed);
        ((v >> EPOCH_SHIFT) as u16, v & FRAME_MASK)
    }
    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }
    pub fn drained_epoch(&self) -> u16 {
        self.drained_epoch.load(Ordering::Relaxed) as u16
    }
    pub fn latency_frames(&self) -> u32 {
        self.latency_frames.load(Ordering::Relaxed)
    }
    pub fn set_latency_frames(&self, frames: u32) {
        self.latency_frames.store(frames, Ordering::Relaxed);
    }
    pub fn callbacks(&self) -> u64 {
        self.callbacks.load(Ordering::Relaxed)
    }
    pub fn leaked_feeds(&self) -> u32 {
        self.leaked_feeds.load(Ordering::Relaxed)
    }
}

/// The reading end of one feed's PCM ring (interleaved stereo f32 at the output rate).
pub struct FeedConsumer {
    /// Never 0.
    pub epoch: u16,
    pub consumer: rtrb::Consumer<f32>,
    /// Set by the decode thread after it has written its very last sample.
    pub finished: Arc<AtomicBool>,
    /// The feed begins with the first sample of a track (not at a seek position). Such
    /// a feed is not faded in: how a track begins is part of the track, and a fade
    /// would take the edge off one that opens on a beat. Anywhere else playback starts
    /// in the middle of a waveform, where the fade is what prevents a click.
    pub from_start: bool,
}

/// Control-thread end of the queues connecting to a [`Renderer`].
pub struct RenderLink {
    pub incoming: rtrb::Producer<FeedConsumer>,
    pub garbage: rtrb::Consumer<FeedConsumer>,
    pub sample_rate: u32,
}

pub struct Renderer {
    shared: Arc<RenderShared>,
    incoming: rtrb::Consumer<FeedConsumer>,
    garbage: rtrb::Producer<FeedConsumer>,
    current: Option<FeedConsumer>,
    pending: Option<FeedConsumer>,
    consumed: u64,
    fade: f32,
    fade_step: f32,
    vol: f32,
    vol_step: f32,
    /// State of the dither noise generator (never 0).
    noise: u32,
    /// The ring ran dry while playing: what arrives now is waited for until there is
    /// enough of it to go on with (see `REFILL_SECONDS`).
    starved: bool,
    /// `REFILL_SECONDS` in frames.
    refill_frames: usize,
    block: [f32; BLOCK * 2],
}

/// Creates a renderer and the link used to hand it new feeds.
pub fn render_pair(shared: Arc<RenderShared>, sample_rate: u32) -> (Renderer, RenderLink) {
    let (in_tx, in_rx) = rtrb::RingBuffer::new(8);
    let (gc_tx, gc_rx) = rtrb::RingBuffer::new(32);
    let rate = sample_rate.max(1) as f32;
    let vol = f32::from_bits(shared.volume_bits.load(Ordering::Relaxed));
    let renderer = Renderer {
        shared,
        incoming: in_rx,
        garbage: gc_tx,
        current: None,
        pending: None,
        consumed: 0,
        fade: 0.0,
        fade_step: 1.0 / (FADE_SECONDS * rate),
        vol,
        vol_step: 1.0 / (VOLUME_RAMP_SECONDS * rate),
        noise: 0x9E37_79B9,
        starved: false,
        refill_frames: (REFILL_SECONDS * rate) as usize,
        block: [0.0; BLOCK * 2],
    };
    (
        renderer,
        RenderLink {
            incoming: in_tx,
            garbage: gc_rx,
            sample_rate,
        },
    )
}

impl Renderer {
    pub fn shared(&self) -> &Arc<RenderShared> {
        &self.shared
    }

    /// Hands a replaced feed back to the control thread so its memory is freed there.
    fn retire(&mut self, old: FeedConsumer) {
        if let Err(rtrb::PushError::Full(old)) = self.garbage.push(old) {
            // The control thread stopped draining. Leaking is the only option that does
            // not free memory on the real-time thread.
            std::mem::forget(old);
            self.shared.leaked_feeds.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The next dither value: triangular noise of ±1 device step (TPDF), which turns the
    /// rounding to the device's format into steady, signal-independent noise.
    fn tpdf(&mut self, lsb: f32) -> f32 {
        let mut next = || {
            // xorshift32: cheap, no state beyond one word, good enough for dither.
            self.noise ^= self.noise << 13;
            self.noise ^= self.noise >> 17;
            self.noise ^= self.noise << 5;
            (self.noise >> 8) as f32 / (1u32 << 24) as f32
        };
        (next() - next()) * lsb
    }

    /// Fills `out` (interleaved, `channels` per frame) with the next audio.
    pub fn render(&mut self, out: &mut [f32], channels: usize) {
        self.shared.callbacks.fetch_add(1, Ordering::Relaxed);
        let channels = channels.max(1);
        while let Ok(feed) = self.incoming.pop() {
            if let Some(old) = self.pending.replace(feed) {
                self.retire(old);
            }
        }
        let playing = self.shared.playing.load(Ordering::Relaxed);
        let required = self.shared.required_epoch.load(Ordering::Relaxed);
        let vol_target = f32::from_bits(self.shared.volume_bits.load(Ordering::Relaxed));
        let dither_lsb = f32::from_bits(self.shared.dither_lsb.load(Ordering::Relaxed));
        let processed = self.shared.processed.load(Ordering::Relaxed);
        let frames = out.len() / channels;
        let mut done = 0;
        let mut underrun = false;
        // Level of the programme over this callback (see `LevelMeter`).
        let mut level_sum = [0f32; 2];
        let mut level_peak = [0f32; 2];

        while done < frames {
            if self.pending.is_some() && (self.current.is_none() || self.fade <= 0.0 || !playing) {
                if let Some(old) = self.current.take() {
                    self.retire(old);
                }
                self.current = self.pending.take();
                self.consumed = 0;
                self.fade = 0.0;
                self.starved = false;
            }
            let Some(cur) = self.current.as_mut() else {
                break;
            };
            let superseded = required != 0 && u32::from(cur.epoch) != required;
            let fading_out = !playing || superseded || self.pending.is_some();
            if fading_out && self.fade <= 0.0 {
                break; // paused or superseded: hold silently without consuming
            }
            if self.consumed == 0 && cur.from_start && !fading_out {
                self.fade = 1.0; // see `FeedConsumer::from_start`
            }
            let mut n = (frames - done).min(BLOCK);
            if fading_out {
                n = n.min((self.fade / self.fade_step).ceil() as usize).max(1);
            }
            let mut avail = cur.consumer.slots() / 2;
            if self.starved && self.pending.is_none() {
                // What comes after the ring ran dry is not played piece by piece as
                // it lands (each piece faded in and cut off at its end: a stutter
                // with a click for every piece) but when there is enough of it to
                // go on with, or all there will be.
                let enough = self
                    .refill_frames
                    .min(cur.consumer.buffer().capacity() / 4)
                    .max(1);
                if avail >= enough || cur.finished.load(Ordering::Acquire) {
                    self.starved = false;
                } else {
                    underrun = playing && !superseded;
                    break;
                }
            }
            if avail == 0 {
                if self.pending.is_some() {
                    self.fade = 0.0; // nothing left to fade: swap right away
                    continue;
                }
                if cur.finished.load(Ordering::Acquire) {
                    // Re-check: the producer writes its last samples before setting the flag.
                    avail = cur.consumer.slots() / 2;
                    if avail == 0 {
                        self.shared
                            .drained_epoch
                            .store(u32::from(cur.epoch), Ordering::Relaxed);
                        self.fade = 0.0;
                        break;
                    }
                } else {
                    underrun = playing && !superseded;
                    self.fade = 0.0; // fade back in when data returns
                    self.starved = true;
                    break;
                }
            }
            n = n.min(avail);
            let Ok(chunk) = cur.consumer.read_chunk(n * 2) else {
                break;
            };
            let (a, b) = chunk.as_slices();
            self.block[..a.len()].copy_from_slice(a);
            self.block[a.len()..a.len() + b.len()].copy_from_slice(b);
            chunk.commit_all();
            self.consumed += n as u64;

            for k in 0..n {
                if fading_out {
                    self.fade = (self.fade - self.fade_step).max(0.0);
                } else if self.fade < 1.0 {
                    self.fade = (self.fade + self.fade_step).min(1.0);
                }
                if self.vol < vol_target {
                    self.vol = (self.vol + self.vol_step).min(vol_target);
                } else if self.vol > vol_target {
                    self.vol = (self.vol - self.vol_step).max(vol_target);
                }
                let g = self.fade * self.vol;
                // Untouched samples (full volume, nothing done to them) go out exactly
                // as they are; anything else is dithered for an integer device.
                let (dl, dr) = if dither_lsb > 0.0 && (processed || g != 1.0) {
                    (self.tpdf(dither_lsb), self.tpdf(dither_lsb))
                } else {
                    (0.0, 0.0)
                };
                let l = (self.block[k * 2] * g + dl).clamp(-1.0, 1.0);
                let r = (self.block[k * 2 + 1] * g + dr).clamp(-1.0, 1.0);
                self.shared
                    .scope
                    .push((self.block[k * 2] + self.block[k * 2 + 1]) * 0.5 * self.fade);
                let metered = [
                    (self.block[k * 2] * self.fade).abs(),
                    (self.block[k * 2 + 1] * self.fade).abs(),
                ];
                for ch in 0..2 {
                    level_sum[ch] += metered[ch];
                    level_peak[ch] = level_peak[ch].max(metered[ch]);
                }
                let frame = &mut out[(done + k) * channels..(done + k + 1) * channels];
                if channels == 1 {
                    frame[0] = (l + r) * 0.5;
                } else {
                    frame[0] = l;
                    frame[1] = r;
                    for extra in &mut frame[2..] {
                        *extra = 0.0;
                    }
                }
            }
            done += n;
        }
        for s in &mut out[done * channels..] {
            *s = 0.0;
        }
        if underrun {
            self.shared.underruns.fetch_add(1, Ordering::Relaxed);
        }
        // Silence written above (paused, starved, end of feed) counts as silence.
        let span = frames.max(1) as f32;
        self.shared
            .meter
            .store([level_sum[0] / span, level_sum[1] / span], level_peak);
        let epoch = self.current.as_ref().map_or(0, |c| u64::from(c.epoch));
        self.shared.position.store(
            (epoch << EPOCH_SHIFT) | (self.consumed & FRAME_MASK),
            Ordering::Relaxed,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A feed that starts somewhere inside a track (the usual case for these tests:
    /// it is the one that is faded in).
    fn feed(
        epoch: u16,
        samples: &[f32],
        finished: bool,
    ) -> (FeedConsumer, rtrb::Producer<f32>, Arc<AtomicBool>) {
        let (mut tx, rx) = rtrb::RingBuffer::new(samples.len().max(2) + 64);
        for s in samples {
            tx.push(*s).expect("ring has room");
        }
        let flag = Arc::new(AtomicBool::new(finished));
        (
            FeedConsumer {
                epoch,
                consumer: rx,
                finished: flag.clone(),
                from_start: false,
            },
            tx,
            flag,
        )
    }

    #[test]
    fn a_track_played_from_its_beginning_starts_with_its_own_first_sample() {
        let (shared, mut r, mut link) = setup(48_000);
        let (mut f, _tx, _) = feed(3, &[0.5; 8000], false);
        f.from_start = true;
        link.incoming.push(f).expect("push");
        // Handed over while paused: nothing is heard or consumed yet …
        let mut out = vec![1.0; 64];
        r.render(&mut out, 2);
        assert!(out.iter().all(|s| *s == 0.0));
        assert_eq!(shared.position(), (3, 0));
        // … and when playback starts, the first sample is the track's, at full level.
        shared.set_playing(true);
        let mut out = vec![0.0; 2000];
        r.render(&mut out, 2);
        assert!(out.iter().all(|s| *s == 0.5), "no fade-in");
        // Pausing and resuming in the middle is still faded both ways.
        shared.set_playing(false);
        let mut out = vec![0.0; 2000];
        r.render(&mut out, 2);
        assert!(out[0] < 0.5 && out[1998] == 0.0, "fade-out");
        shared.set_playing(true);
        let mut out = vec![0.0; 2000];
        r.render(&mut out, 2);
        assert!(out[0].abs() < 0.01 && out[1998] == 0.5, "fade back in");
    }

    fn setup(rate: u32) -> (Arc<RenderShared>, Renderer, RenderLink) {
        let shared = RenderShared::new(1.0);
        let (r, link) = render_pair(shared.clone(), rate);
        (shared, r, link)
    }

    #[test]
    fn silent_and_not_consuming_while_paused() {
        let (shared, mut r, mut link) = setup(48_000);
        let (f, _tx, _) = feed(1, &[0.5; 2000], false);
        link.incoming.push(f).expect("push");
        let mut out = vec![1.0; 512];
        r.render(&mut out, 2);
        assert!(out.iter().all(|s| *s == 0.0));
        assert_eq!(shared.position(), (1, 0));
        assert_eq!(shared.underruns(), 0);
    }

    #[test]
    fn fades_in_then_plays_at_unity_and_counts_frames() {
        let (shared, mut r, mut link) = setup(48_000);
        let (f, _tx, _) = feed(7, &[0.5; 4000], false);
        link.incoming.push(f).expect("push");
        shared.set_playing(true);
        let mut out = vec![0.0; 2000];
        r.render(&mut out, 2);
        assert!(out[0].abs() < 0.01, "starts from silence");
        assert!(
            out.windows(2).all(|w| (w[1] - w[0]).abs() < 0.01),
            "no jumps during fade-in"
        );
        assert_eq!(out[1998], 0.5);
        assert_eq!(shared.position(), (7, 1000));
    }

    #[test]
    fn underrun_is_counted_but_end_of_feed_is_not() {
        let (shared, mut r, mut link) = setup(48_000);
        let (f, mut tx, flag) = feed(1, &[0.25; 200], false);
        link.incoming.push(f).expect("push");
        shared.set_playing(true);
        let mut out = vec![0.0; 800];
        r.render(&mut out, 2);
        assert_eq!(shared.underruns(), 1);
        assert!(
            out[400..].iter().all(|s| *s == 0.0),
            "missing data is silence"
        );
        assert_eq!(shared.drained_epoch(), 0);
        // More data, then the decoder finishes: draining is not an underrun.
        for _ in 0..100 {
            tx.push(0.25).expect("room");
        }
        flag.store(true, Ordering::Release);
        r.render(&mut out, 2);
        r.render(&mut out, 2);
        assert_eq!(shared.underruns(), 1);
        assert_eq!(shared.drained_epoch(), 1);
        assert_eq!(shared.position(), (1, 150));
    }

    /// After the ring ran dry, a little that arrives is not played at once: playing
    /// goes on when there is enough to go on with, faded in, or when the feed ends.
    #[test]
    fn after_running_dry_playing_waits_for_enough_to_go_on_with() {
        let (shared, mut r, mut link) = setup(48_000);
        let (f, mut tx, flag) = feed(1, &[0.25; 200], false);
        link.incoming.push(f).expect("push");
        shared.set_playing(true);
        let mut out = vec![0.0; 800];
        r.render(&mut out, 2);
        assert_eq!(shared.underruns(), 1);
        assert_eq!(shared.position(), (1, 100));
        // Twenty frames arrive: too little to go on with (the ring holds 132).
        for _ in 0..40 {
            tx.push(0.25).expect("room");
        }
        r.render(&mut out, 2);
        assert!(out.iter().all(|s| *s == 0.0), "held back");
        assert_eq!(shared.position(), (1, 100));
        assert_eq!(shared.underruns(), 2, "silence while playing is counted");
        // Sixty more: enough. It goes on, from silence.
        for _ in 0..120 {
            tx.push(0.25).expect("room");
        }
        let mut out = vec![0.0; 120];
        r.render(&mut out, 2);
        assert!(
            out[0].abs() < 0.01 && out[118] > out[0],
            "fades in: {out:?}"
        );
        assert_eq!(shared.position(), (1, 160));
        assert_eq!(shared.underruns(), 2);
        // Dry again; the little that is left when the feed ends is played out.
        let mut out = vec![0.0; 800];
        r.render(&mut out, 2);
        assert_eq!(shared.underruns(), 3);
        for _ in 0..10 {
            tx.push(0.25).expect("room");
        }
        r.render(&mut out, 2);
        assert_eq!(shared.position(), (1, 180), "waiting");
        flag.store(true, Ordering::Release);
        r.render(&mut out, 2);
        r.render(&mut out, 2);
        assert_eq!(shared.position(), (1, 185));
        assert_eq!(shared.drained_epoch(), 1);
    }

    #[test]
    fn swap_fades_out_old_feed_and_returns_it() {
        let (shared, mut r, mut link) = setup(48_000);
        let (a, _atx, _) = feed(1, &[0.5; 96_000], false);
        link.incoming.push(a).expect("push");
        shared.set_playing(true);
        let mut out = vec![0.0; 4800];
        r.render(&mut out, 2);
        assert_eq!(*out.last().expect("sample"), 0.5);
        let (b, _btx, _) = feed(2, &[-0.5; 96_000], false);
        link.incoming.push(b).expect("push");
        r.render(&mut out, 2);
        // Old feed fades to zero, new feed fades in from zero: no step larger than the ramp.
        let left: Vec<f32> = out.as_chunks::<2>().0.iter().map(|f| f[0]).collect();
        assert!(
            left.windows(2).all(|w| (w[1] - w[0]).abs() < 0.01),
            "click at swap"
        );
        assert_eq!(*left.last().expect("sample"), -0.5);
        assert_eq!(shared.position().0, 2);
        let old = link.garbage.pop().expect("old feed handed back");
        assert_eq!(old.epoch, 1);
    }

    #[test]
    fn superseded_feed_is_silenced_until_the_required_one_arrives() {
        let (shared, mut r, mut link) = setup(48_000);
        let (a, _atx, _) = feed(1, &[0.5; 96_000], false);
        link.incoming.push(a).expect("push");
        shared.set_playing(true);
        let mut out = vec![0.0; 4800];
        r.render(&mut out, 2);
        // The user picked another track: feed 2 is required but still loading.
        shared.require_epoch(2);
        r.render(&mut out, 2);
        assert_eq!(*out.last().expect("sample"), 0.0, "old feed faded out");
        let held = shared.position();
        r.render(&mut out, 2);
        assert!(out.iter().all(|s| *s == 0.0));
        assert_eq!(
            shared.position(),
            held,
            "old feed is not consumed while superseded"
        );
        assert_eq!(shared.underruns(), 0);
        let (b, _btx, _) = feed(2, &[-0.5; 96_000], false);
        link.incoming.push(b).expect("push");
        r.render(&mut out, 2);
        assert_eq!(*out.last().expect("sample"), -0.5);
        assert_eq!(shared.position().0, 2);
    }

    #[test]
    fn pause_fades_out_and_stops_consuming() {
        let (shared, mut r, mut link) = setup(48_000);
        let (a, _atx, _) = feed(1, &[0.5; 96_000], false);
        link.incoming.push(a).expect("push");
        shared.set_playing(true);
        let mut out = vec![0.0; 4800];
        r.render(&mut out, 2);
        shared.set_playing(false);
        r.render(&mut out, 2);
        let left: Vec<f32> = out.as_chunks::<2>().0.iter().map(|f| f[0]).collect();
        assert!(left.windows(2).all(|w| (w[1] - w[0]).abs() < 0.01));
        assert_eq!(*left.last().expect("sample"), 0.0);
        let pos = shared.position().1;
        r.render(&mut out, 2);
        assert_eq!(shared.position().1, pos, "no consumption while paused");
    }

    #[test]
    fn volume_changes_are_ramped() {
        let (shared, mut r, mut link) = setup(48_000);
        let (a, _atx, _) = feed(1, &[1.0; 96_000], false);
        link.incoming.push(a).expect("push");
        shared.set_playing(true);
        let mut out = vec![0.0; 9600];
        r.render(&mut out, 2);
        shared.set_volume_gain(0.1);
        r.render(&mut out, 2);
        let left: Vec<f32> = out.as_chunks::<2>().0.iter().map(|f| f[0]).collect();
        assert!(left.windows(2).all(|w| (w[1] - w[0]).abs() < 0.01));
        assert!((left.last().expect("sample") - 0.1).abs() < 1e-4);
    }

    #[test]
    fn levels_follow_the_programme_not_the_volume() {
        let (shared, mut r, mut link) = setup(48_000);
        // Left at 0.5, right at 0.25, with one louder sample on the right.
        let mut samples: Vec<f32> = (0..9600).flat_map(|_| [0.5, -0.25]).collect();
        samples[14_001] = 0.9; // frame 7000, in the second callback below
        let (a, _atx, _) = feed(1, &samples, false);
        link.incoming.push(a).expect("push");
        shared.set_volume_gain(0.0); // the listener hears nothing…
        shared.set_playing(true);
        let mut out = vec![0.0; 9600];
        r.render(&mut out, 2); // (contains the fade-in)
        shared.meter().read();
        r.render(&mut out, 2);
        assert!(out.iter().all(|s| s.abs() < 1e-6));
        // …but the meter shows the music.
        let levels = shared.meter().read();
        assert!((levels.average[0] - 0.5).abs() < 1e-4, "{levels:?}");
        assert!((levels.average[1] - 0.25).abs() < 1e-3, "{levels:?}");
        assert_eq!(levels.peak, [0.5, 0.9]);
        // Peaks are handed out once; the average stays until the next callback.
        let again = shared.meter().read();
        assert_eq!(again.peak, [0.0, 0.0]);
        assert_eq!(again.average, levels.average);
        // Paused: the needle's target is silence.
        shared.set_playing(false);
        r.render(&mut out, 2);
        r.render(&mut out, 2);
        assert_eq!(shared.meter().read().average, [0.0, 0.0]);
        shared.meter().reset();
        assert_eq!(shared.meter().read(), Levels::default());
    }

    /// Dither: none when the samples pass through untouched, triangular noise of at most
    /// one device step otherwise.
    #[test]
    fn dither_only_touches_samples_that_are_no_longer_the_files_own() {
        let step = 1.0 / 32_768.0; // a 16-bit device
        let run = |volume: f32, processed: bool, bits: Option<u32>| -> Vec<f32> {
            let (shared, mut r, mut link) = setup(48_000);
            shared.set_volume_gain(volume);
            shared.set_dither_bits(bits);
            shared.set_processed(processed);
            let (a, _atx, _) = feed(1, &[0.25; 40_000], false);
            link.incoming.push(a).expect("push");
            shared.set_playing(true);
            let mut out = vec![0.0; 20_000];
            r.render(&mut out, 2); // fade-in and volume settle here
            r.render(&mut out, 2);
            out
        };
        // Full volume, untouched: exact, with or without an integer device.
        assert!(run(1.0, false, Some(16)).iter().all(|s| *s == 0.25));
        assert!(run(1.0, false, None).iter().all(|s| *s == 0.25));
        // A float device never gets dither, whatever is done to the signal.
        assert!(run(0.5, true, None).iter().all(|s| *s == 0.125));
        assert!(run(0.5, true, Some(32)).iter().all(|s| *s == 0.125));
        // Reduced volume on a 16-bit device: noise around the scaled value.
        for (volume, processed, expected) in [(0.5, false, 0.125), (1.0, true, 0.25)] {
            let out = run(volume, processed, Some(16));
            let errors: Vec<f32> = out.iter().map(|s| s - expected).collect();
            assert!(
                errors.iter().all(|e| e.abs() <= step * 1.0001),
                "more than one step"
            );
            assert!(
                errors.iter().any(|e| *e > step * 0.5) && errors.iter().any(|e| *e < -step * 0.5)
            );
            let mean = errors.iter().sum::<f32>() / errors.len() as f32;
            assert!(mean.abs() < step * 0.05, "dither is not centred: {mean}");
            // Triangular: the variance of the difference of two uniform numbers is 1/6.
            let variance =
                errors.iter().map(|e| (e / step).powi(2)).sum::<f32>() / errors.len() as f32;
            assert!((variance - 1.0 / 6.0).abs() < 0.02, "variance {variance}");
        }
        let (shared, _r, _link) = setup(48_000);
        shared.set_dither_bits(Some(24));
        assert!(!shared.dither_active(), "full volume, untouched");
        shared.set_processed(true);
        assert!(shared.dither_active());
        shared.set_dither_bits(None);
        assert!(!shared.dither_active());
    }

    #[test]
    fn the_scope_holds_the_latest_samples_in_order() {
        let (shared, mut r, mut link) = setup(48_000);
        // A ramp on both channels, so that the mono mix is the ramp itself.
        let samples: Vec<f32> = (0..6000).flat_map(|i| [i as f32 / 8000.0; 2]).collect();
        let (a, _atx, _) = feed(1, &samples, false);
        link.incoming.push(a).expect("push");
        shared.set_volume_gain(0.0); // the scope shows the music, not the volume
        shared.set_playing(true);
        let mut out = vec![0.0; 12_000];
        r.render(&mut out, 2);
        let mut latest = [0f32; 1000];
        shared.scope().latest(&mut latest);
        assert_eq!(latest[999], 5999.0 / 8000.0);
        assert!(
            latest.windows(2).all(|w| w[1] > w[0]),
            "in order, oldest first"
        );
        // More than has been played: silence in front.
        let scope = Scope::default();
        scope.push(0.5);
        scope.push(0.75);
        let mut four = [9.0f32; 4];
        scope.latest(&mut four);
        assert_eq!(four, [0.0, 0.0, 0.5, 0.75]);
        scope.reset();
        scope.latest(&mut four);
        assert_eq!(four, [0.0; 4]);
    }

    #[test]
    fn mono_and_multichannel_outputs() {
        let (shared, mut r, mut link) = setup(48_000);
        let samples: Vec<f32> = (0..4000).flat_map(|_| [0.4, 0.2]).collect();
        let (a, _atx, _) = feed(1, &samples, false);
        link.incoming.push(a).expect("push");
        shared.set_playing(true);
        let mut mono = vec![0.0; 1000];
        r.render(&mut mono, 1);
        assert!((mono[999] - 0.3).abs() < 1e-6);
        let mut six = vec![9.0; 600];
        r.render(&mut six, 6);
        assert_eq!(&six[594..600], &[0.4, 0.2, 0.0, 0.0, 0.0, 0.0]);
    }
}
