//! Streaming stereo sample-rate conversion (rubato FFT resampler) with exact output length.
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler};

const CHUNK_FRAMES: usize = 1024;

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
        let inner = Fft::<f32>::new(in_rate as usize, out_rate as usize, CHUNK_FRAMES, 2, FixedSync::Input)
            .map_err(|e| e.to_string())?;
        let delay_left = inner.output_delay();
        let scratch = vec![0.0; inner.output_frames_max() * 2];
        Ok(Self {
            inner,
            in_rate,
            out_rate,
            pending: Vec::with_capacity(CHUNK_FRAMES * 4),
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
        (u128::from(self.total_in_frames) * u128::from(self.out_rate)).div_ceil(u128::from(self.in_rate)) as u64
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
            result = self.run(&pending[offset..offset + need * 2], need, None, out, u64::MAX);
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
        let mut guard = 0;
        while self.total_out_frames < target && guard < 64 {
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
        let mut output = InterleavedSlice::new_mut(&mut self.scratch, 2, cap).map_err(|e| e.to_string())?;
        let indexing = partial.map(|len| Indexing::new().partial_len(len));
        let (_, produced) =
            self.inner.process_into_buffer(&input, &mut output, indexing.as_ref()).map_err(|e| e.to_string())?;
        let skip = self.delay_left.min(produced);
        self.delay_left -= skip;
        let usable = ((produced - skip) as u64).min(max_out_frames) as usize;
        out.extend_from_slice(&self.scratch[skip * 2..(skip + usable) * 2]);
        self.total_out_frames += usable as u64;
        Ok(())
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
        for (i, o, frames) in [(44_100, 48_000, 44_100), (48_000, 44_100, 48_000), (96_000, 44_100, 12_345), (22_050, 48_000, 777)] {
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
        let left: Vec<f32> = mid.chunks_exact(2).map(|f| f[0]).collect();
        let crossings = left.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        let secs = left.len() as f32 / 48_000.0;
        assert!((crossings as f32 / secs - 1000.0).abs() < 2.0, "freq {}", crossings as f32 / secs);
    }

    #[test]
    fn chunking_does_not_change_the_result() {
        let input = sine(44_100, 20_000, 700.0);
        let a = resample_all(44_100, 48_000, &input, 64);
        let b = resample_all(44_100, 48_000, &input, 5000);
        assert_eq!(a.len(), b.len());
        let max_diff = a.iter().zip(&b).fold(0f32, |m, (x, y)| m.max((x - y).abs()));
        assert!(max_diff < 1e-6, "max diff {max_diff}");
    }
}
