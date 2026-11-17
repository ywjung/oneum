//! Raising a song's sample rate by a whole factor (44.1 → 88.2, 176.4 kHz and so on)
//! with a filter of the listener's choosing.
//!
//! The usual conversion (`resample.rs`) is as sharp as a filter gets: flat to the top
//! of the song's band and nothing above it. Such a filter rings for a long time
//! before and after anything sudden in the music. The two filters here trade some of
//! that sharpness for another behaviour in time, as DACs let one choose:
//!
//! - [`Filter::Slow`]: a short, symmetric filter. It rings for a fraction of the
//!   time, rolls off gently from 18 kHz (for a 44.1 kHz song) and lets some of what
//!   lies just above the song's band through.
//! - [`Filter::Minimum`]: a minimum-phase filter. Flat to 20 kHz like the sharp one,
//!   but all of its ringing comes after the sound, none before it.
//!
//! What each does is pinned down by the tests at the end of this file.
use rustfft::FftPlanner;
use rustfft::num_complex::Complex;
use serde::{Deserialize, Serialize};
use std::f64::consts::PI;

/// How a song's sample rate is raised when the output runs at a whole multiple of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Filter {
    /// The usual conversion: linear phase, as steep as it gets.
    #[default]
    Sharp,
    /// Linear phase with a slow roll-off: short ringing.
    Slow,
    /// Minimum phase: no ringing before a sound.
    Minimum,
}

impl Filter {
    /// The filter of that name ("sharp", "slow", "minimum").
    pub fn named(name: &str) -> Option<Filter> {
        match name {
            "sharp" => Some(Filter::Sharp),
            "slow" => Some(Filter::Slow),
            "minimum" => Some(Filter::Minimum),
            _ => None,
        }
    }

    /// Whether a conversion from `in_rate` to `out_rate` is done with this filter
    /// (and not the usual way): a whole factor up, and a filter other than the usual.
    pub fn applies(self, in_rate: u32, out_rate: u32) -> bool {
        self != Filter::Sharp
            && in_rate != 0
            && out_rate.is_multiple_of(in_rate)
            && FACTORS.contains(&(out_rate / in_rate))
    }
}

/// The factors this is done for.
const FACTORS: std::ops::RangeInclusive<u32> = 2..=8;
/// How far down the filters push what they stop, in dB.
const STOP_DB: f64 = 100.0;

/// The modified Bessel function of the first kind, order zero.
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let half = x / 2.0;
    for k in 1..200 {
        term *= (half / f64::from(k)) * (half / f64::from(k));
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// A low-pass filter by the window method (Kaiser's window): `pass` and `stop` are the
/// edges of its transition, as fractions of the rate it runs at. An odd number of
/// coefficients, symmetric, with a gain of one at 0 Hz.
fn lowpass(pass: f64, stop: f64) -> Vec<f64> {
    let transition = stop - pass;
    let taps = ((STOP_DB - 7.95) / (2.285 * 2.0 * PI * transition)).ceil() as usize | 1;
    let cutoff = (pass + stop) / 2.0;
    let beta = 0.1102 * (STOP_DB - 8.7);
    let mid = (taps - 1) as f64 / 2.0;
    let mut filter: Vec<f64> = (0..taps)
        .map(|n| {
            let t = n as f64 - mid;
            let sinc = if t == 0.0 {
                2.0 * cutoff
            } else {
                (2.0 * PI * cutoff * t).sin() / (PI * t)
            };
            let across = (1.0 - (t / mid) * (t / mid)).max(0.0).sqrt();
            sinc * bessel_i0(beta * across) / bessel_i0(beta)
        })
        .collect();
    let sum: f64 = filter.iter().sum();
    filter.iter_mut().for_each(|c| *c /= sum);
    filter
}

/// The minimum-phase filter with the same response in level as `filter`: the same
/// number of coefficients, with the weight at their beginning. (By way of the
/// cepstrum: the logarithm of the response, folded onto the causal side.)
fn minimum_phase(filter: &[f64]) -> Vec<f64> {
    let size = (filter.len() * 32).next_power_of_two().max(1 << 14);
    let mut planner = FftPlanner::<f64>::new();
    let (forward, inverse) = (
        planner.plan_fft_forward(size),
        planner.plan_fft_inverse(size),
    );
    let mut bins: Vec<Complex<f64>> = filter
        .iter()
        .map(|c| Complex::new(*c, 0.0))
        .chain(std::iter::repeat(Complex::new(0.0, 0.0)))
        .take(size)
        .collect();
    forward.process(&mut bins);
    // (Where the filter stops everything its response is next to nothing; the floor
    // keeps the logarithm finite, far below what the filter is asked to do.)
    let floor = 10f64.powf(-(STOP_DB + 60.0) / 20.0);
    for bin in &mut bins {
        *bin = Complex::new(bin.norm().max(floor).ln(), 0.0);
    }
    inverse.process(&mut bins);
    let scale = 1.0 / size as f64;
    for (n, bin) in bins.iter_mut().enumerate() {
        let weight = if n == 0 || n == size / 2 {
            1.0
        } else if n < size / 2 {
            2.0
        } else {
            0.0
        };
        *bin = Complex::new(bin.re * scale * weight, 0.0);
    }
    forward.process(&mut bins);
    for bin in &mut bins {
        *bin = bin.exp();
    }
    inverse.process(&mut bins);
    let mut out: Vec<f64> = bins[..filter.len()].iter().map(|c| c.re * scale).collect();
    let sum: f64 = out.iter().sum();
    out.iter_mut().for_each(|c| *c /= sum);
    out
}

/// The filter for `filter` at `factor` times the song's rate, and how many output
/// frames late what goes in comes out (which the upsampler takes off again).
fn design(filter: Filter, factor: u32) -> Option<(Vec<f64>, usize)> {
    // The edges as fractions of the song's rate, then of the rate the filter runs at.
    let at = |edge: f64| edge / f64::from(factor);
    match filter {
        Filter::Sharp => None,
        // From 18 kHz to 28.7 kHz for a 44.1 kHz song.
        Filter::Slow => {
            let taps = lowpass(at(0.408), at(0.65));
            let delay = (taps.len() - 1) / 2;
            Some((taps, delay))
        }
        // From 20 kHz to 24.1 kHz for a 44.1 kHz song: everything below 20 kHz is
        // kept and none of it comes back as an image above the band.
        Filter::Minimum => Some((minimum_phase(&lowpass(at(0.4535), at(0.5465))), 0)),
    }
}

/// Raises the rate of interleaved stereo by a whole factor. The output is exactly
/// `factor` times as long as the input, and (but for the filter's own way with time)
/// in step with it.
pub struct Upsampler {
    in_rate: u32,
    factor: usize,
    /// Coefficients a phase: `phases[p * per_phase..][..per_phase]`, each phase in
    /// the order the input is held in (oldest frame first).
    phases: Vec<f32>,
    per_phase: usize,
    /// The last `per_phase - 1` input frames, then those that came in since.
    held: Vec<f32>,
    /// Output frames still to drop: the filter's delay.
    delay_left: usize,
    total_in: u64,
    total_out: u64,
}

impl Upsampler {
    /// `None` when this is not a job for it: `out_rate` is not `in_rate` times a
    /// whole number from 2 to 8, or the filter asked for is the usual one.
    pub fn new(in_rate: u32, out_rate: u32, filter: Filter) -> Option<Self> {
        if !filter.applies(in_rate, out_rate) {
            return None;
        }
        let factor = out_rate / in_rate;
        let (taps, delay) = design(filter, factor)?;
        let factor = factor as usize;
        let per_phase = taps.len().div_ceil(factor);
        let mut phases = vec![0.0f32; per_phase * factor];
        for (n, tap) in taps.iter().enumerate() {
            let (phase, k) = (n % factor, n / factor);
            // Between the frames of the input there is nothing: what the filter loses
            // by that, the factor gives back. Newest input last: the first
            // coefficient of a phase meets the newest frame.
            phases[phase * per_phase + (per_phase - 1 - k)] = (tap * factor as f64) as f32;
        }
        Some(Upsampler {
            in_rate,
            factor,
            phases,
            per_phase,
            held: vec![0.0; (per_phase - 1) * 2],
            delay_left: delay,
            total_in: 0,
            total_out: 0,
        })
    }

    pub fn in_rate(&self) -> u32 {
        self.in_rate
    }

    /// Output frames that the input fed so far corresponds to.
    pub fn expected_out_frames(&self) -> u64 {
        self.total_in * self.factor as u64
    }

    /// Feeds interleaved stereo input and appends the output that is ready to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.total_in += (input.len() / 2) as u64;
        self.run(input, out, u64::MAX);
    }

    /// The rest: the output is then exactly as long as it should be. The upsampler
    /// has done its work after this.
    pub fn flush(&mut self, out: &mut Vec<f32>) {
        let target = self.expected_out_frames();
        let silence = [0.0f32; 512];
        while self.total_out < target {
            self.run(&silence, out, target - self.total_out);
        }
    }

    fn run(&mut self, input: &[f32], out: &mut Vec<f32>, at_most: u64) {
        self.held.extend_from_slice(input);
        let k = self.per_phase;
        let frames = self.held.len() / 2;
        let mut left = at_most;
        // The frames that have their whole past behind them: all that came in.
        'frames: for newest in (k - 1)..frames {
            let window = &self.held[(newest + 1 - k) * 2..(newest + 1) * 2];
            for phase in self.phases.chunks_exact(k) {
                if self.delay_left > 0 {
                    self.delay_left -= 1;
                    continue;
                }
                if left == 0 {
                    break 'frames;
                }
                let (mut l, mut r) = (0.0f32, 0.0f32);
                for (frame, c) in window.as_chunks::<2>().0.iter().zip(phase) {
                    l += frame[0] * c;
                    r += frame[1] * c;
                }
                out.extend_from_slice(&[l, r]);
                self.total_out += 1;
                left -= 1;
            }
        }
        self.held.drain(..(frames - (k - 1)) * 2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, frames: usize, freq: f64) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let s = (i as f64 * freq * 2.0 * PI / f64::from(rate)).sin() * 0.5;
                [s as f32, s as f32]
            })
            .collect()
    }

    fn raised(in_rate: u32, factor: u32, filter: Filter, input: &[f32], piece: usize) -> Vec<f32> {
        let mut up = Upsampler::new(in_rate, in_rate * factor, filter).expect("an upsampler");
        let mut out = Vec::new();
        for chunk in input.chunks(piece * 2) {
            up.process(chunk, &mut out);
        }
        up.flush(&mut out);
        out
    }

    /// The level of the tone at `freq` in the middle of the left channel, in dB
    /// relative to full scale (by its correlation with a sine and a cosine under a
    /// Hann window).
    fn level_db(samples: &[f32], rate: u32, freq: f64) -> f64 {
        let left: Vec<f64> = samples
            .as_chunks::<2>()
            .0
            .iter()
            .map(|f| f64::from(f[0]))
            .collect();
        let middle = &left[left.len() / 4..left.len() * 3 / 4];
        let n = middle.len() as f64;
        let (mut re, mut im, mut weight) = (0.0, 0.0, 0.0);
        for (i, s) in middle.iter().enumerate() {
            let w = 0.5 - 0.5 * (2.0 * PI * i as f64 / n).cos();
            let phase = 2.0 * PI * freq * i as f64 / f64::from(rate);
            re += s * w * phase.cos();
            im += s * w * phase.sin();
            weight += w;
        }
        20.0 * (2.0 * (re * re + im * im).sqrt() / weight)
            .max(1e-12)
            .log10()
    }

    /// How a tone of `freq` Hz in a 44.1 kHz song comes out, and its image above the
    /// band (at 44,100 − `freq` Hz), both in dB relative to the tone that went in.
    fn tone_and_image(filter: Filter, factor: u32, freq: f64) -> (f64, f64) {
        let rate = 44_100;
        let out = raised(rate, factor, filter, &sine(rate, 44_100, freq), 1024);
        let went_in = 20.0 * 0.5f64.log10();
        (
            level_db(&out, rate * factor, freq) - went_in,
            level_db(&out, rate * factor, f64::from(rate) - freq) - went_in,
        )
    }

    #[test]
    fn the_output_is_exactly_the_factor_times_as_long_however_it_is_fed() {
        for filter in [Filter::Slow, Filter::Minimum] {
            for (rate, factor, frames) in
                [(44_100, 2, 10_000), (48_000, 4, 4_801), (44_100, 8, 777)]
            {
                let input = sine(rate, frames, 1_000.0);
                let whole = raised(rate, factor, filter, &input, frames);
                assert_eq!(whole.len() / 2, frames * factor as usize);
                for piece in [1, 7, 333] {
                    let pieces = raised(rate, factor, filter, &input, piece);
                    assert_eq!(pieces, whole, "{filter:?} ×{factor} in pieces of {piece}");
                }
            }
        }
        // Not its job: the usual filter, a ratio that is no whole number, one beyond 8.
        assert!(Upsampler::new(44_100, 88_200, Filter::Sharp).is_none());
        assert!(Upsampler::new(44_100, 96_000, Filter::Slow).is_none());
        assert!(Upsampler::new(44_100, 44_100, Filter::Slow).is_none());
        assert!(Upsampler::new(22_050, 352_800, Filter::Minimum).is_none());
        assert!(Upsampler::new(0, 88_200, Filter::Minimum).is_none());
    }

    #[test]
    fn the_slow_filter_is_short_keeps_the_music_and_stays_in_step() {
        // A click comes out where it went in, the same before and after it.
        let mut input = vec![0.0f32; 2_000];
        input[2 * 500] = 1.0;
        input[2 * 500 + 1] = 1.0;
        for factor in [2u32, 4, 8] {
            let out = raised(44_100, factor, Filter::Slow, &input, 64);
            let left: Vec<f32> = out.as_chunks::<2>().0.iter().map(|f| f[0]).collect();
            let peak = (0..left.len())
                .max_by(|a, b| left[*a].abs().total_cmp(&left[*b].abs()))
                .unwrap();
            assert_eq!(peak, 500 * factor as usize, "×{factor}");
            assert!((left[peak] - 1.0).abs() < 0.2, "{}", left[peak]);
            for d in 1..40 {
                assert!((left[peak - d] - left[peak + d]).abs() < 1e-6);
            }
            // Short: beyond a millisecond from the click there is nothing left of it.
            let reach = (44.1 * f64::from(factor)) as usize;
            let far = left
                .iter()
                .enumerate()
                .filter(|(i, _)| i.abs_diff(peak) > reach)
                .map(|(_, s)| s.abs())
                .fold(0.0f32, f32::max);
            assert!(far < 1e-5, "×{factor}: {far}");
        }
        for factor in [2, 4] {
            let (low, _) = tone_and_image(Filter::Slow, factor, 1_000.0);
            assert!(low.abs() < 0.001, "1 kHz: {low} dB");
            let (high, _) = tone_and_image(Filter::Slow, factor, 18_000.0);
            assert!(high.abs() < 0.05, "18 kHz: {high} dB");
            // It rolls off slowly: 20 kHz is a little down, and of what lies close
            // under the top of the band some comes back above it.
            let (top, image) = tone_and_image(Filter::Slow, factor, 20_000.0);
            assert!((-3.0..-0.05).contains(&top), "20 kHz: {top} dB");
            assert!(
                (-30.0..-3.0).contains(&image),
                "the image of 20 kHz: {image} dB"
            );
            // What is well inside the band does not.
            let (_, image) = tone_and_image(Filter::Slow, factor, 15_000.0);
            assert!(image < -95.0, "the image of 15 kHz: {image} dB");
        }
    }

    #[test]
    fn the_minimum_phase_filter_keeps_the_band_and_rings_only_afterwards() {
        for factor in [2, 4, 8] {
            for freq in [1_000.0, 10_000.0, 20_000.0] {
                let (level, image) = tone_and_image(Filter::Minimum, factor, freq);
                assert!(level.abs() < 0.01, "×{factor} {freq} Hz: {level} dB");
                assert!(
                    image < -90.0,
                    "×{factor} the image of {freq} Hz: {image} dB"
                );
            }
        }
        // A click: nothing before it, then one rise to its peak (within four frames
        // of the song) without a swing the other way; all the ringing comes after.
        // (The symmetric filter of the same steepness swings 27 times before its
        // peak, which comes 34 frames of the song late.)
        let swings = |part: &[f64]| {
            part.windows(2)
                .filter(|w| w[0].abs() > 1e-9 && w[0].signum() != w[1].signum())
                .count()
        };
        let mut input = vec![0.0f32; 4_000];
        input[2 * 500] = 1.0;
        for factor in [2usize, 4, 8] {
            let out = raised(44_100, factor as u32, Filter::Minimum, &input, 64);
            let left: Vec<f64> = out
                .as_chunks::<2>()
                .0
                .iter()
                .map(|f| f64::from(f[0]))
                .collect();
            assert!(left[..500 * factor].iter().all(|s| *s == 0.0));
            let peak = (0..left.len())
                .max_by(|a, b| left[*a].abs().total_cmp(&left[*b].abs()))
                .unwrap();
            assert!(
                (500 * factor..504 * factor).contains(&peak),
                "×{factor}: {peak}"
            );
            assert_eq!(swings(&left[..=peak]), 0, "×{factor}");
            assert!(swings(&left[peak..]) > 20, "×{factor}");
            let sharp = lowpass(0.4535 / factor as f64, 0.5465 / factor as f64);
            assert_eq!(swings(&sharp[..=(sharp.len() - 1) / 2]), 27);
            assert_eq!((sharp.len() - 1) / 2 / factor, 34);
        }
    }

    #[test]
    fn the_filters_are_what_they_are_said_to_be() {
        let (slow, delay) = design(Filter::Slow, 2).unwrap();
        // Symmetric, a gain of one, and short: under a millisecond at 88.2 kHz.
        assert_eq!(delay, (slow.len() - 1) / 2);
        assert!(slow.len() < 88, "{}", slow.len());
        assert!(
            slow.iter()
                .zip(slow.iter().rev())
                .all(|(a, b)| (a - b).abs() < 1e-15)
        );
        assert!((slow.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        let (minimum, delay) = design(Filter::Minimum, 2).unwrap();
        assert_eq!(delay, 0);
        assert!((minimum.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        // Its weight is at its beginning.
        let first: f64 = minimum[..minimum.len() / 4].iter().map(|c| c * c).sum();
        let all: f64 = minimum.iter().map(|c| c * c).sum();
        assert!(first / all > 0.95, "{}", first / all);
        assert!(design(Filter::Sharp, 2).is_none());
        assert!((bessel_i0(0.0) - 1.0).abs() < 1e-15);
        assert!((bessel_i0(1.0) - 1.266_065_877_752_008).abs() < 1e-12);
    }
}
