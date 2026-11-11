//! The real-time side of the pipeline: what runs inside the audio output callback.
//!
//! Rules for everything in [`Renderer::render`]: no allocation or deallocation, no locks,
//! no I/O, no logging. It only reads pre-filled ring buffers and atomics.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// Frames processed per inner block (stack buffer size).
const BLOCK: usize = 256;
const FADE_SECONDS: f32 = 0.006;
const VOLUME_RAMP_SECONDS: f32 = 0.03;
const EPOCH_SHIFT: u32 = 48;
const FRAME_MASK: u64 = (1 << EPOCH_SHIFT) - 1;

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
        })
    }
    /// Sets the linear output gain (already mapped from the volume slider).
    pub fn set_volume_gain(&self, gain: f32) {
        self.volume_bits.store(gain.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }
    pub fn set_playing(&self, playing: bool) {
        self.playing.store(playing, Ordering::Relaxed);
    }
    /// Silences every feed except `epoch` (see `required_epoch`). Called when the user
    /// starts another track or seeks, so the old audio stops at once instead of playing
    /// on until the new feed has buffered.
    pub fn require_epoch(&self, epoch: u16) {
        self.required_epoch.store(u32::from(epoch), Ordering::Relaxed);
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
        block: [0.0; BLOCK * 2],
    };
    (renderer, RenderLink { incoming: in_tx, garbage: gc_rx, sample_rate })
}

impl Renderer {
    /// Hands a replaced feed back to the control thread so its memory is freed there.
    fn retire(&mut self, old: FeedConsumer) {
        if let Err(rtrb::PushError::Full(old)) = self.garbage.push(old) {
            // The control thread stopped draining. Leaking is the only option that does
            // not free memory on the real-time thread.
            std::mem::forget(old);
            self.shared.leaked_feeds.fetch_add(1, Ordering::Relaxed);
        }
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
        let frames = out.len() / channels;
        let mut done = 0;
        let mut underrun = false;

        while done < frames {
            if self.pending.is_some() && (self.current.is_none() || self.fade <= 0.0 || !playing) {
                if let Some(old) = self.current.take() {
                    self.retire(old);
                }
                self.current = self.pending.take();
                self.consumed = 0;
                self.fade = 0.0;
            }
            let Some(cur) = self.current.as_mut() else { break };
            let superseded = required != 0 && u32::from(cur.epoch) != required;
            let fading_out = !playing || superseded || self.pending.is_some();
            if fading_out && self.fade <= 0.0 {
                break; // paused or superseded: hold silently without consuming
            }
            let mut n = (frames - done).min(BLOCK);
            if fading_out {
                n = n.min((self.fade / self.fade_step).ceil() as usize).max(1);
            }
            let mut avail = cur.consumer.slots() / 2;
            if avail == 0 {
                if self.pending.is_some() {
                    self.fade = 0.0; // nothing left to fade: swap right away
                    continue;
                }
                if cur.finished.load(Ordering::Acquire) {
                    // Re-check: the producer writes its last samples before setting the flag.
                    avail = cur.consumer.slots() / 2;
                    if avail == 0 {
                        self.shared.drained_epoch.store(u32::from(cur.epoch), Ordering::Relaxed);
                        self.fade = 0.0;
                        break;
                    }
                } else {
                    underrun = playing && !superseded;
                    self.fade = 0.0; // fade back in when data returns
                    break;
                }
            }
            n = n.min(avail);
            let Ok(chunk) = cur.consumer.read_chunk(n * 2) else { break };
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
                let l = (self.block[k * 2] * g).clamp(-1.0, 1.0);
                let r = (self.block[k * 2 + 1] * g).clamp(-1.0, 1.0);
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
        let epoch = self.current.as_ref().map_or(0, |c| u64::from(c.epoch));
        self.shared.position.store((epoch << EPOCH_SHIFT) | (self.consumed & FRAME_MASK), Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(epoch: u16, samples: &[f32], finished: bool) -> (FeedConsumer, rtrb::Producer<f32>, Arc<AtomicBool>) {
        let (mut tx, rx) = rtrb::RingBuffer::new(samples.len().max(2) + 64);
        for s in samples {
            tx.push(*s).expect("ring has room");
        }
        let flag = Arc::new(AtomicBool::new(finished));
        (FeedConsumer { epoch, consumer: rx, finished: flag.clone() }, tx, flag)
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
        link.incoming.push(f).ok().expect("push");
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
        link.incoming.push(f).ok().expect("push");
        shared.set_playing(true);
        let mut out = vec![0.0; 2000];
        r.render(&mut out, 2);
        assert!(out[0].abs() < 0.01, "starts from silence");
        assert!(out.windows(2).all(|w| (w[1] - w[0]).abs() < 0.01), "no jumps during fade-in");
        assert_eq!(out[1998], 0.5);
        assert_eq!(shared.position(), (7, 1000));
    }

    #[test]
    fn underrun_is_counted_but_end_of_feed_is_not() {
        let (shared, mut r, mut link) = setup(48_000);
        let (f, mut tx, flag) = feed(1, &[0.25; 200], false);
        link.incoming.push(f).ok().expect("push");
        shared.set_playing(true);
        let mut out = vec![0.0; 800];
        r.render(&mut out, 2);
        assert_eq!(shared.underruns(), 1);
        assert!(out[400..].iter().all(|s| *s == 0.0), "missing data is silence");
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

    #[test]
    fn swap_fades_out_old_feed_and_returns_it() {
        let (shared, mut r, mut link) = setup(48_000);
        let (a, _atx, _) = feed(1, &[0.5; 96_000], false);
        link.incoming.push(a).ok().expect("push");
        shared.set_playing(true);
        let mut out = vec![0.0; 4800];
        r.render(&mut out, 2);
        assert_eq!(*out.last().expect("sample"), 0.5);
        let (b, _btx, _) = feed(2, &[-0.5; 96_000], false);
        link.incoming.push(b).ok().expect("push");
        r.render(&mut out, 2);
        // Old feed fades to zero, new feed fades in from zero: no step larger than the ramp.
        let left: Vec<f32> = out.chunks_exact(2).map(|f| f[0]).collect();
        assert!(left.windows(2).all(|w| (w[1] - w[0]).abs() < 0.01), "click at swap");
        assert_eq!(*left.last().expect("sample"), -0.5);
        assert_eq!(shared.position().0, 2);
        let old = link.garbage.pop().expect("old feed handed back");
        assert_eq!(old.epoch, 1);
    }

    #[test]
    fn superseded_feed_is_silenced_until_the_required_one_arrives() {
        let (shared, mut r, mut link) = setup(48_000);
        let (a, _atx, _) = feed(1, &[0.5; 96_000], false);
        link.incoming.push(a).ok().expect("push");
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
        assert_eq!(shared.position(), held, "old feed is not consumed while superseded");
        assert_eq!(shared.underruns(), 0);
        let (b, _btx, _) = feed(2, &[-0.5; 96_000], false);
        link.incoming.push(b).ok().expect("push");
        r.render(&mut out, 2);
        assert_eq!(*out.last().expect("sample"), -0.5);
        assert_eq!(shared.position().0, 2);
    }

    #[test]
    fn pause_fades_out_and_stops_consuming() {
        let (shared, mut r, mut link) = setup(48_000);
        let (a, _atx, _) = feed(1, &[0.5; 96_000], false);
        link.incoming.push(a).ok().expect("push");
        shared.set_playing(true);
        let mut out = vec![0.0; 4800];
        r.render(&mut out, 2);
        shared.set_playing(false);
        r.render(&mut out, 2);
        let left: Vec<f32> = out.chunks_exact(2).map(|f| f[0]).collect();
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
        link.incoming.push(a).ok().expect("push");
        shared.set_playing(true);
        let mut out = vec![0.0; 9600];
        r.render(&mut out, 2);
        shared.set_volume_gain(0.1);
        r.render(&mut out, 2);
        let left: Vec<f32> = out.chunks_exact(2).map(|f| f[0]).collect();
        assert!(left.windows(2).all(|w| (w[1] - w[0]).abs() < 0.01));
        assert!((left.last().expect("sample") - 0.1).abs() < 1e-4);
    }

    #[test]
    fn mono_and_multichannel_outputs() {
        let (shared, mut r, mut link) = setup(48_000);
        let samples: Vec<f32> = (0..4000).flat_map(|_| [0.4, 0.2]).collect();
        let (a, _atx, _) = feed(1, &samples, false);
        link.incoming.push(a).ok().expect("push");
        shared.set_playing(true);
        let mut mono = vec![0.0; 1000];
        r.render(&mut mono, 1);
        assert!((mono[999] - 0.3).abs() < 1e-6);
        let mut six = vec![9.0; 600];
        r.render(&mut six, 6);
        assert_eq!(&six[594..600], &[0.4, 0.2, 0.0, 0.0, 0.0, 0.0]);
    }
}
