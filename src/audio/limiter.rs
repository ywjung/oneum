//! Protection against peaks above full scale.
//!
//! Samples can exceed full scale by the time they are ready for the device even when
//! the file itself never does: the peaks of a loud master lie *between* its samples
//! and come out when the rate is converted, lossy codecs decode to more than they were
//! given, and a positive ReplayGain adds on top. A device clips such samples flat,
//! which is distortion. This limiter turns the gain down for the few milliseconds
//! around such a peak instead, by exactly as much as the peak needs, the same for both
//! channels.
//!
//! It never touches anything else: as long as no sample exceeds full scale the output
//! is the input, bit for bit, only later by the look-ahead.
use std::collections::VecDeque;

/// How far the limiter looks ahead, and so how long the gain takes to come down before
/// a peak (a linear ramp of this length).
const LOOKAHEAD_SECONDS: f64 = 0.0015;
/// Time constant of the gain's way back up after a peak. Slow enough not to follow the
/// waveform itself (which would distort low notes), fast enough to be over before it
/// can be heard as a dip.
const RELEASE_SECONDS: f64 = 0.1;
/// Below this distance from unity the released gain is unity again, exactly.
const UNITY_SNAP: f32 = 1e-6;

pub struct PeakLimiter {
    /// Look-ahead in frames (at least 1).
    lookahead: usize,
    release: f32,
    /// The last `lookahead` frames taken in, oldest at `delay_at` once full.
    delay: Vec<[f32; 2]>,
    delay_at: usize,
    /// Frames taken in since the start (or the last flush).
    taken: u64,
    /// Candidates for the smallest gain needed by the last `lookahead + 1` frames:
    /// `(frame number, gain)`, gains increasing from front to back.
    needed: VecDeque<(u64, f32)>,
    /// How far the gain is turned down right now, release applied: the envelope is
    /// `1 - reduction`. Kept as the distance from unity because that is the number
    /// that decays; next to 1.0 a float has no room left for its last steps.
    reduction: f32,
    /// The last `lookahead + 1` envelope values, whose mean is the gain applied.
    window: Vec<f32>,
    window_at: usize,
    window_sum: f64,
    /// How many values in `window` are below unity.
    reducing: usize,
    /// Frames given out with their gain turned down.
    limited: u64,
}

impl PeakLimiter {
    pub fn new(sample_rate: u32) -> Self {
        let rate = f64::from(sample_rate.max(1));
        let lookahead = ((rate * LOOKAHEAD_SECONDS).round() as usize).max(1);
        Self {
            lookahead,
            release: (-1.0 / (rate * RELEASE_SECONDS)).exp() as f32,
            delay: vec![[0.0; 2]; lookahead],
            delay_at: 0,
            taken: 0,
            needed: VecDeque::with_capacity(lookahead + 1),
            reduction: 0.0,
            window: vec![1.0; lookahead + 1],
            window_at: 0,
            window_sum: (lookahead + 1) as f64,
            reducing: 0,
            limited: 0,
        }
    }

    /// Frames taken in that have not been given out yet.
    pub fn held_frames(&self) -> u64 {
        self.taken.min(self.lookahead as u64)
    }

    /// Frames given out so far whose gain was turned down.
    pub fn limited_frames(&self) -> u64 {
        self.limited
    }

    /// Takes interleaved stereo `input` and appends to `out` what is ready: the same
    /// number of frames, less the look-ahead at the very start.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        out.reserve(input.len());
        for frame in input.as_chunks::<2>().0 {
            if let Some(ready) = self.step(*frame) {
                out.extend_from_slice(&ready);
            }
        }
    }

    /// Gives out the frames still held. The limiter starts afresh afterwards.
    pub fn flush(&mut self, out: &mut Vec<f32>) {
        if self.taken > 0 {
            // Silence pushes out what is held: as many frames as came in and have
            // not left, however few that is.
            for _ in 0..self.lookahead {
                if let Some(ready) = self.step([0.0; 2]) {
                    out.extend_from_slice(&ready);
                }
            }
        }
        let limited = self.limited;
        *self = Self {
            lookahead: self.lookahead,
            release: self.release,
            delay: std::mem::take(&mut self.delay),
            delay_at: 0,
            taken: 0,
            needed: std::mem::take(&mut self.needed),
            reduction: 0.0,
            window: std::mem::take(&mut self.window),
            window_at: 0,
            window_sum: (self.lookahead + 1) as f64,
            reducing: 0,
            limited,
        };
        self.needed.clear();
        self.window.fill(1.0);
    }

    /// Takes one frame in; gives the frame from `lookahead` frames ago out, if any.
    fn step(&mut self, frame: [f32; 2]) -> Option<[f32; 2]> {
        // What is not a number is silence: a gain cannot bring it into range (infinity
        // times the gain it asks for, zero, is not a number either).
        let number = |sample: f32| if sample.is_finite() { sample } else { 0.0 };
        let frame = [number(frame[0]), number(frame[1])];
        let n = self.taken;
        let peak = frame[0].abs().max(frame[1].abs());
        let need = if peak > 1.0 { 1.0 / peak } else { 1.0 };

        // The smallest gain any of the last `lookahead + 1` frames needs.
        while self.needed.back().is_some_and(|&(_, g)| g >= need) {
            self.needed.pop_back();
        }
        self.needed.push_back((n, need));
        while self
            .needed
            .front()
            .is_some_and(|&(at, _)| at + (self.lookahead as u64) < n)
        {
            self.needed.pop_front();
        }
        let floor = self.needed.front().map_or(1.0, |&(_, g)| g);

        // Down at once, back up slowly.
        let mut reduction = self.reduction * self.release;
        if reduction < UNITY_SNAP {
            reduction = 0.0;
        }
        self.reduction = reduction.max(1.0 - floor);
        let envelope = 1.0 - self.reduction;

        // The gain applied is the mean of the last `lookahead + 1` envelope values.
        // Each of them is no higher than what the frame leaving the delay needs (it
        // was inside every one of their windows), so neither is their mean; and the
        // mean of a step is a ramp, which is what brings the gain down smoothly.
        let leaving = std::mem::replace(&mut self.window[self.window_at], envelope);
        self.window_at = (self.window_at + 1) % self.window.len();
        self.window_sum += f64::from(envelope) - f64::from(leaving);
        self.reducing -= usize::from(leaving < 1.0);
        self.reducing += usize::from(envelope < 1.0);
        if self.reducing == 0 {
            // Start the running sum afresh so rounding never accumulates in it.
            self.window_sum = self.window.len() as f64;
        }

        let delayed = std::mem::replace(&mut self.delay[self.delay_at], frame);
        self.delay_at = (self.delay_at + 1) % self.lookahead;
        self.taken += 1;
        if n < self.lookahead as u64 {
            return None;
        }
        if self.reducing == 0 {
            // Untouched: exactly the samples that came in.
            return Some(delayed);
        }
        let gain = (self.window_sum / self.window.len() as f64) as f32;
        self.limited += 1;
        // The clamp only takes off what rounding in the mean may have left over.
        Some([
            (delayed[0] * gain).clamp(-1.0, 1.0),
            (delayed[1] * gain).clamp(-1.0, 1.0),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 44_100;

    fn run(input: &[f32], chunk_frames: usize) -> (Vec<f32>, u64) {
        let mut limiter = PeakLimiter::new(RATE);
        let mut out = Vec::new();
        for chunk in input.chunks(chunk_frames * 2) {
            limiter.process(chunk, &mut out);
        }
        limiter.flush(&mut out);
        (out, limiter.limited_frames())
    }

    /// Deterministic noise in −1..=1.
    fn noise(frames: usize, mut state: u32) -> Vec<f32> {
        (0..frames * 2)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state >> 8) as f32 / (1u32 << 23) as f32 - 1.0
            })
            .collect()
    }

    fn sine(frames: usize, freq: f32, amplitude: f32) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let s = (i as f32 * freq * std::f32::consts::TAU / RATE as f32).sin() * amplitude;
                [s, s]
            })
            .collect()
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0, |m, s| m.max(s.abs()))
    }

    #[test]
    fn audio_within_full_scale_passes_bit_for_bit() {
        let mut input = noise(20_000, 0x1234_5678);
        // Full scale itself, both ways, is not above full scale.
        input[100] = 1.0;
        input[101] = -1.0;
        for chunk in [1, 7, 64, 1000, 50_000] {
            let (out, limited) = run(&input, chunk);
            assert_eq!(out.len(), input.len(), "chunks of {chunk}");
            assert!(
                out.iter()
                    .zip(&input)
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
                "chunks of {chunk}"
            );
            assert_eq!(limited, 0);
        }
    }

    #[test]
    fn a_tone_above_full_scale_comes_out_at_full_scale_and_still_a_sine() {
        // +3 dB over, like the classic inter-sample over of a full-scale fs/4 tone.
        let input = sine(RATE as usize, 1000.0, std::f32::consts::SQRT_2);
        let (out, limited) = run(&input, 512);
        assert_eq!(out.len(), input.len());
        assert!(peak(&out) <= 1.0, "peak {}", peak(&out));
        assert!(limited > 40_000);
        let left: Vec<f32> = out.as_chunks::<2>().0.iter().map(|f| f[0]).collect();
        let settled = &left[RATE as usize / 2..];
        assert!((peak(settled) - 1.0).abs() < 0.001, "{}", peak(settled));
        // Once settled the gain all but stands still (the highest sample of each cycle
        // is not always the same one, which moves it by a fraction of a percent), so
        // what comes out is still a sine. A device would have clipped it flat.
        let clipped: Vec<f32> = input
            .as_chunks::<2>()
            .0
            .iter()
            .skip(RATE as usize / 2)
            .map(|f| f[0].clamp(-1.0, 1.0))
            .collect();
        let limited_db = distortion_db(settled, 1000.0);
        let clipped_db = distortion_db(&clipped, 1000.0);
        assert!(clipped_db > -25.0, "clipping: {clipped_db:.1} dB");
        assert!(limited_db < -60.0, "limiting: {limited_db:.1} dB");
    }

    /// Everything in `signal` that is not the sine of `freq` Hz, relative to that sine.
    fn distortion_db(signal: &[f32], freq: f64) -> f64 {
        let w = freq * std::f64::consts::TAU / f64::from(RATE);
        let (mut a, mut b) = (0.0, 0.0);
        for (k, &v) in signal.iter().enumerate() {
            let (s, c) = (k as f64 * w).sin_cos();
            a += f64::from(v) * s;
            b += f64::from(v) * c;
        }
        let n = signal.len() as f64;
        let (a, b) = (2.0 * a / n, 2.0 * b / n);
        let rest: f64 = signal
            .iter()
            .enumerate()
            .map(|(k, &v)| {
                let (s, c) = (k as f64 * w).sin_cos();
                (f64::from(v) - a * s - b * c).powi(2)
            })
            .sum();
        10.0 * (rest / n / ((a * a + b * b) / 2.0)).log10()
    }

    #[test]
    fn a_single_peak_is_brought_down_smoothly_and_the_rest_is_left_alone() {
        let frames = RATE as usize * 2;
        let mut input = sine(frames, 440.0, 0.5);
        let at = RATE as usize / 2;
        input[at * 2] = 1.8;
        input[at * 2 + 1] = -0.3;
        let (out, limited) = run(&input, 4096);
        assert_eq!(out.len(), input.len());
        assert!(peak(&out) <= 1.0);
        // The peak itself lands at full scale; the other channel gets the same gain.
        assert!((out[at * 2] - 1.0).abs() < 1e-4, "{}", out[at * 2]);
        assert!((out[at * 2 + 1] - (-0.3 / 1.8)).abs() < 1e-4);
        // Before the look-ahead reaches the peak nothing has happened yet …
        let lookahead = PeakLimiter::new(RATE).lookahead;
        let before = (at - lookahead - 1) * 2;
        assert!(out[..before].iter().zip(&input).all(|(a, b)| a == b));
        // … the gain changes by small steps on the way down and back …
        let gain: Vec<f32> = (at - lookahead..at + RATE as usize)
            .filter(|&f| input[f * 2].abs() > 0.05)
            .map(|f| out[f * 2] / input[f * 2])
            .collect();
        assert!(gain.iter().all(|g| *g <= 1.0 + 1e-6 && *g > 0.5));
        // … and a second and a half later the samples are the file's own again.
        let after = (at + RATE as usize * 3 / 2) * 2;
        assert!(
            out[after..]
                .iter()
                .zip(&input[after..])
                .all(|(a, b)| a == b)
        );
        assert!(limited > 0 && limited < RATE as u64 * 3 / 2);
    }

    #[test]
    fn the_gain_never_jumps() {
        // Steady signal, one over: the largest step between neighbouring gains is the
        // ramp of the look-ahead, 1/(lookahead+1) of the reduction.
        let frames = RATE as usize;
        let mut input: Vec<f32> = vec![0.5; frames * 2];
        input[20_000] = 2.0;
        let (out, _) = run(&input, 333);
        let lookahead = PeakLimiter::new(RATE).lookahead as f32;
        let mut largest = 0f32;
        for f in 1..frames {
            if f * 2 == 20_000 || (f - 1) * 2 == 20_000 {
                continue;
            }
            let (now, before) = (out[f * 2 + 1] / 0.5, out[f * 2 - 1] / 0.5);
            largest = largest.max((now - before).abs());
        }
        assert!(largest <= 0.5 / lookahead * 1.01, "step {largest}");
    }

    #[test]
    fn chunking_does_not_change_the_result_and_flushing_starts_afresh() {
        let mut input = noise(30_000, 0xBEEF_0001);
        for s in input.iter_mut().step_by(977) {
            *s *= 2.5;
        }
        let (a, limited_a) = run(&input, 1);
        let (b, limited_b) = run(&input, 4000);
        assert_eq!(a, b);
        assert_eq!(limited_a, limited_b);
        assert!(peak(&a) <= 1.0);

        // One limiter used for two stretches gives what two limiters give.
        let mut limiter = PeakLimiter::new(RATE);
        let mut twice = Vec::new();
        limiter.process(&input, &mut twice);
        assert_eq!(limiter.held_frames(), limiter.lookahead as u64);
        limiter.flush(&mut twice);
        assert_eq!(limiter.held_frames(), 0);
        limiter.process(&input, &mut twice);
        limiter.flush(&mut twice);
        assert_eq!(twice.len(), a.len() * 2);
        assert_eq!(&twice[..a.len()], &a[..]);
        assert_eq!(&twice[a.len()..], &a[..]);
    }

    #[test]
    fn a_stretch_shorter_than_the_look_ahead_comes_out_whole() {
        let input = [0.25, -0.25, 1.5, 0.1, -0.75, 0.75];
        let (out, _) = run(&input, 2);
        assert_eq!(out.len(), input.len());
        assert!(peak(&out) <= 1.0);
        assert!(out[0] <= 0.25 && out[0] > 0.1);
    }

    /// Infinity asks for a gain of zero, and zero times infinity is not a number: such
    /// samples go in as silence, and the music around them comes out as it was.
    #[test]
    fn what_is_not_a_number_comes_out_as_silence() {
        let mut input = vec![0.25f32; 2 * 4_000];
        input[2_000] = f32::INFINITY;
        input[2_001] = f32::NEG_INFINITY;
        input[4_000] = f32::NAN;
        let (out, limited) = run(&input, 512);
        assert_eq!(out.len(), input.len());
        assert!(out.iter().all(|s| s.is_finite()));
        assert_eq!(limited, 0, "nothing was above full scale");
        assert_eq!((out[2_000], out[2_001], out[4_000]), (0.0, 0.0, 0.0));
        assert_eq!(out.iter().filter(|s| **s == 0.25).count(), input.len() - 3);
    }
}
