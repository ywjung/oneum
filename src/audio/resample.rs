//! Streaming stereo sample-rate conversion (rubato FFT resampler) with exact output length.
//!
//! What the conversion does to the music is pinned down by the tests at the end of this
//! file: flat to 20 kHz, nothing folded back from above the new Nyquist frequency, and
//! distortion plus noise at the floor of 32-bit float samples.
use super::upsample::{Filter, Upsampler};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler, WindowFunction};

/// Length of one FFT block, counted in frames of the lower of the two rates.
///
/// The block length is the length of the anti-aliasing filter, and with it how close to
/// the Nyquist frequency the filter can stay flat. With 512 frames a conversion to or
/// from 44.1 kHz is within 0.001 dB up to 21 kHz and down by more than 100 dB at
/// 22 kHz. (rubato's own default sizes the block by the *input* rate alone, which for
/// 96 or 192 kHz → 44.1 kHz leaves 147 frames and costs 5.6 dB at 20 kHz.)
const BLOCK_FRAMES: u64 = 512;

pub struct StereoResampler {
    inner: Fft<f32>,
    in_rate: u32,
    out_rate: u32,
    /// Interleaved input not yet consumed (always less than one input chunk after `process`).
    pending: Vec<f32>,
    scratch: Vec<f32>,
    /// Output frames still to drop to compensate the resampler's start-up delay.
    delay_left: usize,
    total_in_frames: u64,
    total_out_frames: u64,
}

impl StereoResampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Result<Self, String> {
        if in_rate == 0 || out_rate == 0 {
            return Err("sample rate of zero".into());
        }
        // Input frames per block so that the block has `BLOCK_FRAMES` frames on the
        // side with the lower rate. One block per chunk: the whole chunk is one filter.
        let chunk = (BLOCK_FRAMES * u64::from(in_rate.max(out_rate))).div_ceil(u64::from(out_rate));
        let inner = Fft::<f32>::new_custom(
            in_rate as usize,
            out_rate as usize,
            chunk as usize,
            1,
            2,
            WindowFunction::BlackmanHarris2,
            FixedSync::Input,
        )
        .map_err(|e| e.to_string())?;
        let delay_left = inner.output_delay();
        let scratch = vec![0.0; inner.output_frames_max() * 2];
        Ok(Self {
            inner,
            in_rate,
            out_rate,
            pending: Vec::with_capacity(chunk as usize * 4),
            scratch,
            delay_left,
            total_in_frames: 0,
            total_out_frames: 0,
        })
    }

    pub fn in_rate(&self) -> u32 {
        self.in_rate
    }

    /// Output frames that the input fed so far corresponds to (independent of buffering).
    pub fn expected_out_frames(&self) -> u64 {
        (u128::from(self.total_in_frames) * u128::from(self.out_rate))
            .div_ceil(u128::from(self.in_rate)) as u64
    }

    pub fn total_out_frames(&self) -> u64 {
        self.total_out_frames
    }

    /// Feeds interleaved stereo input and appends whatever output is ready to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), String> {
        self.total_in_frames += (input.len() / 2) as u64;
        self.pending.extend_from_slice(input);
        // Move the buffer out so `run` can borrow `self` mutably; it is put back below.
        let mut pending = std::mem::take(&mut self.pending);
        let mut offset = 0;
        let mut result = Ok(());
        loop {
            let need = self.inner.input_frames_next();
            if (pending.len() - offset) / 2 < need {
                break;
            }
            result = self.run(
                &pending[offset..offset + need * 2],
                need,
                None,
                out,
                u64::MAX,
            );
            if result.is_err() {
                break;
            }
            offset += need * 2;
        }
        pending.drain(..offset);
        self.pending = pending;
        result
    }

    /// Flushes the tail so that the total output length is exactly
    /// `ceil(total_in * out_rate / in_rate)` frames. The resampler is reusable afterwards
    /// only via a new instance.
    pub fn flush(&mut self, out: &mut Vec<f32>) -> Result<(), String> {
        let target = self.expected_out_frames();
        // Nothing comes out before a whole FFT block has gone in, and for two rates
        // with little in common a block is long (44,101 → 48,000 Hz: 44,101 frames).
        // So: as many rounds as it takes to push two blocks through, and some.
        let chunk = self.inner.input_frames_max().max(1);
        let rounds = 64 + 2 * self.inner.fft_size_in().div_ceil(chunk);
        let mut guard = 0;
        while self.total_out_frames < target && guard < rounds {
            guard += 1;
            let need = self.inner.input_frames_next();
            let have = (self.pending.len() / 2).min(need);
            let mut chunk = std::mem::take(&mut self.pending);
            chunk.resize(need * 2, 0.0);
            let remaining = target - self.total_out_frames;
            let result = self.run(&chunk, need, Some(have), out, remaining);
            chunk.clear();
            self.pending = chunk;
            result?;
        }
        Ok(())
    }

    fn run(
        &mut self,
        chunk: &[f32],
        frames: usize,
        partial: Option<usize>,
        out: &mut Vec<f32>,
        max_out_frames: u64,
    ) -> Result<(), String> {
        let input = InterleavedSlice::new(chunk, 2, frames).map_err(|e| e.to_string())?;
        let cap = self.scratch.len() / 2;
        let mut output =
            InterleavedSlice::new_mut(&mut self.scratch, 2, cap).map_err(|e| e.to_string())?;
        let indexing = partial.map(|len| Indexing::new().partial_len(len));
        let (_, produced) = self
            .inner
            .process_into_buffer(&input, &mut output, indexing.as_ref())
            .map_err(|e| e.to_string())?;
        let skip = self.delay_left.min(produced);
        self.delay_left -= skip;
        let usable = ((produced - skip) as u64).min(max_out_frames) as usize;
        out.extend_from_slice(&self.scratch[skip * 2..(skip + usable) * 2]);
        self.total_out_frames += usable as u64;
        Ok(())
    }
}

/// A conversion from one sample rate to another: the usual one, or, where the rate
/// goes up by a whole factor and the listener chose another filter for that, that one
/// (`upsample.rs`).
pub enum Converter {
    Usual(Box<StereoResampler>),
    Chosen(Upsampler),
}

impl Converter {
    pub fn new(in_rate: u32, out_rate: u32, filter: Filter) -> Result<Self, String> {
        match Upsampler::new(in_rate, out_rate, filter) {
            Some(upsampler) => Ok(Converter::Chosen(upsampler)),
            None => StereoResampler::new(in_rate, out_rate).map(|r| Converter::Usual(Box::new(r))),
        }
    }

    pub fn in_rate(&self) -> u32 {
        match self {
            Converter::Usual(r) => r.in_rate(),
            Converter::Chosen(u) => u.in_rate(),
        }
    }

    /// Output frames that the input fed so far corresponds to (independent of buffering).
    pub fn expected_out_frames(&self) -> u64 {
        match self {
            Converter::Usual(r) => r.expected_out_frames(),
            Converter::Chosen(u) => u.expected_out_frames(),
        }
    }

    /// Feeds interleaved stereo input and appends whatever output is ready to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), String> {
        match self {
            Converter::Usual(r) => r.process(input, out),
            Converter::Chosen(u) => {
                u.process(input, out);
                Ok(())
            }
        }
    }

    /// The tail: after it the output is exactly as long as it should be.
    pub fn flush(&mut self, out: &mut Vec<f32>) -> Result<(), String> {
        match self {
            Converter::Usual(r) => r.flush(out),
            Converter::Chosen(u) => {
                u.flush(out);
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, frames: usize, freq: f32) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let s = (i as f32 * freq * std::f32::consts::TAU / rate as f32).sin() * 0.5;
                [s, s]
            })
            .collect()
    }

    fn resample_all(in_rate: u32, out_rate: u32, input: &[f32], feed_chunk: usize) -> Vec<f32> {
        let mut rs = StereoResampler::new(in_rate, out_rate).expect("resampler");
        let mut out = Vec::new();
        for chunk in input.chunks(feed_chunk * 2) {
            rs.process(chunk, &mut out).expect("process");
        }
        rs.flush(&mut out).expect("flush");
        out
    }

    #[test]
    fn output_length_is_exact() {
        for (i, o, frames) in [
            (44_100, 48_000, 44_100),
            (48_000, 44_100, 48_000),
            (96_000, 44_100, 12_345),
            (22_050, 48_000, 777),
            // Rates with nothing in common: one FFT block is a whole second.
            (44_101, 48_000, 500),
            (48_000, 44_101, 30_000),
        ] {
            let out = resample_all(i, o, &sine(i, frames, 440.0), 333);
            let expected = (frames as u64 * u64::from(o)).div_ceil(u64::from(i)) as usize;
            assert_eq!(out.len() / 2, expected, "{i}->{o} with {frames} frames");
        }
    }

    #[test]
    fn tone_is_preserved_without_clipping() {
        let out = resample_all(44_100, 48_000, &sine(44_100, 44_100, 1000.0), 4096);
        // Skip the edges, then check amplitude and the 1 kHz period (48 samples at 48 kHz).
        let mid = &out[9600..out.len() - 9600];
        let peak = mid.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.5).abs() < 0.01, "peak {peak}");
        let left: Vec<f32> = mid.as_chunks::<2>().0.iter().map(|f| f[0]).collect();
        let crossings = left
            .windows(2)
            .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
            .count();
        let secs = left.len() as f32 / 48_000.0;
        assert!(
            (crossings as f32 / secs - 1000.0).abs() < 2.0,
            "freq {}",
            crossings as f32 / secs
        );
    }

    #[test]
    fn chunking_does_not_change_the_result() {
        let input = sine(44_100, 20_000, 700.0);
        let a = resample_all(44_100, 48_000, &input, 64);
        let b = resample_all(44_100, 48_000, &input, 5000);
        assert_eq!(a.len(), b.len());
        let max_diff = a
            .iter()
            .zip(&b)
            .fold(0f32, |m, (x, y)| m.max((x - y).abs()));
        assert!(max_diff < 1e-6, "max diff {max_diff}");
    }

    // --- What the conversion does to the signal, measured. ---

    /// A sine of `freq` Hz at −1 dBFS, resampled; the left channel as f64.
    fn converted_tone(in_rate: u32, out_rate: u32, freq: f64) -> Vec<f64> {
        let frames = in_rate as usize;
        let input: Vec<f32> = (0..frames)
            .flat_map(|i| {
                let phase = i as f64 * freq * std::f64::consts::TAU / f64::from(in_rate);
                let s = (phase.sin() * TONE_AMPLITUDE) as f32;
                [s, s]
            })
            .collect();
        resample_all(in_rate, out_rate, &input, 4096)
            .as_chunks::<2>()
            .0
            .iter()
            .map(|f| f64::from(f[0]))
            .collect()
    }

    const TONE_AMPLITUDE: f64 = 0.891;

    fn db(ratio: f64) -> f64 {
        20.0 * ratio.max(1e-30).log10()
    }

    /// Least-squares fit of a sine of `freq` Hz to the middle half of `signal`:
    /// `(amplitude of the sine, RMS of everything else)`.
    fn fit_tone(signal: &[f64], rate: u32, freq: f64) -> (f64, f64) {
        let mid = &signal[signal.len() / 4..signal.len() * 3 / 4];
        let w = freq * std::f64::consts::TAU / f64::from(rate);
        let (mut ss, mut cc, mut sc, mut xs, mut xc) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for (k, &v) in mid.iter().enumerate() {
            let (s, c) = (k as f64 * w).sin_cos();
            ss += s * s;
            cc += c * c;
            sc += s * c;
            xs += v * s;
            xc += v * c;
        }
        let det = ss * cc - sc * sc;
        let a = (xs * cc - xc * sc) / det;
        let b = (xc * ss - xs * sc) / det;
        let rest: f64 = mid
            .iter()
            .enumerate()
            .map(|(k, &v)| {
                let (s, c) = (k as f64 * w).sin_cos();
                (v - a * s - b * c).powi(2)
            })
            .sum();
        (a.hypot(b), (rest / mid.len() as f64).sqrt())
    }

    const RATE_PAIRS: [(u32, u32); 8] = [
        (44_100, 48_000),
        (48_000, 44_100),
        (44_100, 96_000),
        (96_000, 44_100),
        (96_000, 48_000),
        (44_100, 88_200),
        (192_000, 44_100),
        (192_000, 48_000),
    ];

    #[test]
    fn the_audible_band_passes_unchanged() {
        for (i, o) in RATE_PAIRS {
            for freq in [100.0, 1_000.0, 10_000.0, 16_000.0, 19_000.0, 20_000.0] {
                let (amplitude, _) = fit_tone(&converted_tone(i, o, freq), o, freq);
                let gain = db(amplitude / TONE_AMPLITUDE);
                assert!(gain.abs() < 0.001, "{i}->{o} at {freq} Hz: {gain:+.4} dB");
            }
        }
    }

    #[test]
    fn distortion_and_noise_stay_at_the_floor_of_float_samples() {
        for (i, o) in RATE_PAIRS {
            for freq in [1_000.0, 10_000.0, 19_000.0] {
                let (amplitude, rest) = fit_tone(&converted_tone(i, o, freq), o, freq);
                // Signal to everything else, both as RMS.
                let sinad = db(amplitude / std::f64::consts::SQRT_2 / rest);
                assert!(sinad > 125.0, "{i}->{o} at {freq} Hz: SINAD {sinad:.1} dB");
            }
        }
    }

    #[test]
    fn nothing_above_the_new_nyquist_frequency_folds_back() {
        for (i, o) in RATE_PAIRS {
            if i <= o {
                continue;
            }
            let nyquist = f64::from(o) / 2.0;
            for above in [1.01, 1.05, 1.2, 1.6] {
                let freq = nyquist * above;
                if freq >= f64::from(i) / 2.0 {
                    continue;
                }
                let out = converted_tone(i, o, freq);
                let mid = &out[out.len() / 4..out.len() * 3 / 4];
                let rms = (mid.iter().map(|v| v * v).sum::<f64>() / mid.len() as f64).sqrt();
                let leak = db(rms * std::f64::consts::SQRT_2 / TONE_AMPLITUDE);
                assert!(leak < -120.0, "{i}->{o}: {freq:.0} Hz leaks {leak:.1} dB");
            }
        }
    }

    #[test]
    fn images_of_an_upsampled_tone_are_removed() {
        // 20 kHz at 44.1 kHz has an image at 24.1 kHz once the rate is doubled or more.
        for o in [88_200, 96_000] {
            let out = converted_tone(44_100, o, 20_000.0);
            let (image, _) = fit_tone(&out, o, 24_100.0);
            let level = db(image / TONE_AMPLITUDE);
            assert!(level < -120.0, "44100->{o}: image at {level:.1} dB");
        }
    }
}
