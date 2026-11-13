//! Loudness measurement for volume leveling of files that carry no ReplayGain tags.
//!
//! The measure is the programme loudness of ITU-R BS.1770-4 / EBU R 128: the signal is
//! K-weighted (a high shelf and a high-pass), its energy is taken over 400 ms blocks
//! that overlap by 75 %, blocks below −70 LUFS are left out, and so are blocks more than
//! 10 LU below the mean of the rest. The gain that brings a track to the reference
//! level is what a ReplayGain 2.0 tag would hold.
//!
//! What is measured is what is played: the audio goes through the same stereo mapping
//! as in the feed, so a mono file counts as the same signal on both speakers.
use super::PlaybackError;
use super::decoder::TrackDecoder;
use super::dsp::to_stereo;
use std::f64::consts::PI;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Level a gain of 0 dB stands for (ReplayGain 2.0: −18 LUFS).
pub const REFERENCE_LUFS: f64 = -18.0;
const ABSOLUTE_GATE_LUFS: f64 = -70.0;
/// Blocks this far below the mean of the blocks above the absolute gate are left out.
const RELATIVE_GATE_LU: f64 = -10.0;
/// Offset of the loudness scale (BS.1770): a full-scale 997 Hz sine on one channel
/// reads −3.01 LUFS.
const SCALE_OFFSET: f64 = -0.691;
/// A gating block is this many steps of 100 ms.
const STEPS_PER_BLOCK: usize = 4;

#[derive(Debug, Clone, Copy)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
}

impl Biquad {
    /// The first stage of the K-weighting: a high shelf of about +4 dB above 1.7 kHz
    /// (the effect of the head). Derived from the analogue prototype, so it is right
    /// at every sample rate and equal to the standard's table at 48 kHz.
    fn shelf(sample_rate: f64) -> Self {
        const F0: f64 = 1_681.974_450_955_533;
        const GAIN_DB: f64 = 3.999_843_853_973_347;
        const Q: f64 = 0.707_175_236_955_419_6;
        let k = (PI * F0 / sample_rate).tan();
        let vh = 10f64.powf(GAIN_DB / 20.0);
        let vb = vh.powf(0.499_666_774_154_541_6);
        let a0 = 1.0 + k / Q + k * k;
        Biquad {
            b: [
                (vh + vb * k / Q + k * k) / a0,
                2.0 * (k * k - vh) / a0,
                (vh - vb * k / Q + k * k) / a0,
            ],
            a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / Q + k * k) / a0],
        }
    }

    /// The second stage: a high-pass at about 38 Hz.
    fn highpass(sample_rate: f64) -> Self {
        const F0: f64 = 38.135_470_876_024_44;
        const Q: f64 = 0.500_327_037_323_877_3;
        let k = (PI * F0 / sample_rate).tan();
        let a0 = 1.0 + k / Q + k * k;
        Biquad {
            b: [1.0, -2.0, 1.0],
            a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / Q + k * k) / a0],
        }
    }

    /// One sample through the filter (transposed direct form II).
    #[inline]
    fn run(&self, state: &mut [f64; 2], x: f64) -> f64 {
        let y = self.b[0] * x + state[0];
        state[0] = self.b[1] * x - self.a[0] * y + state[1];
        state[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// What measuring one piece of audio yields.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Measurement {
    /// K-weighted energy (mean square, both channels added) of every gating block.
    /// Kept so that an album can be measured as one programme from its tracks.
    pub blocks: Vec<f64>,
    /// Largest sample magnitude (1.0 = full scale).
    pub peak: f32,
}

impl Measurement {
    /// Programme loudness in LUFS; `None` for silence (or less than one block of audio).
    pub fn loudness(&self) -> Option<f64> {
        integrated(std::slice::from_ref(self))
    }
}

fn block_energy(lufs: f64) -> f64 {
    10f64.powf((lufs - SCALE_OFFSET) / 10.0)
}

/// Programme loudness in LUFS of the measurements taken together (an album from its
/// tracks). `None` when nothing passes the absolute gate.
pub fn integrated(parts: &[Measurement]) -> Option<f64> {
    let absolute = block_energy(ABSOLUTE_GATE_LUFS);
    let mean_above = |gate: f64| -> Option<f64> {
        let (sum, count) = parts
            .iter()
            .flat_map(|part| &part.blocks)
            .filter(|energy| **energy >= gate)
            .fold((0.0, 0u64), |(sum, count), energy| {
                (sum + energy, count + 1)
            });
        (count > 0).then(|| sum / count as f64)
    };
    let relative = mean_above(absolute)? * 10f64.powf(RELATIVE_GATE_LU / 10.0);
    let mean = mean_above(relative.max(absolute))?;
    Some(SCALE_OFFSET + 10.0 * mean.log10())
}

/// Gain in dB that brings audio of this loudness to the reference level. Silence gets
/// no gain.
pub fn gain_db(loudness: Option<f64>) -> f32 {
    match loudness {
        Some(lufs) if lufs.is_finite() => (REFERENCE_LUFS - lufs) as f32,
        _ => 0.0,
    }
}

/// Measures interleaved stereo audio pushed to it piece by piece.
pub struct Meter {
    shelf: Biquad,
    highpass: Biquad,
    /// Filter memory per channel and stage.
    state: [[[f64; 2]; 2]; 2],
    /// Frames in a step of 100 ms.
    step_frames: usize,
    frames_in_step: usize,
    /// Sum of squares of the weighted samples of the current step, both channels.
    step_sum: f64,
    /// The sums of the most recent steps.
    recent: [f64; STEPS_PER_BLOCK],
    steps: usize,
    blocks: Vec<f64>,
    peak: f32,
}

impl Meter {
    pub fn new(sample_rate: u32) -> Self {
        let rate = f64::from(sample_rate.max(1));
        Meter {
            shelf: Biquad::shelf(rate),
            highpass: Biquad::highpass(rate),
            state: [[[0.0; 2]; 2]; 2],
            step_frames: (sample_rate as usize / 10).max(1),
            frames_in_step: 0,
            step_sum: 0.0,
            recent: [0.0; STEPS_PER_BLOCK],
            steps: 0,
            blocks: Vec::new(),
            peak: 0.0,
        }
    }

    /// Adds interleaved stereo samples. Samples that are not numbers count as silence,
    /// like in playback.
    pub fn push(&mut self, stereo: &[f32]) {
        for frame in stereo.as_chunks::<2>().0 {
            for (channel, &sample) in frame.iter().enumerate() {
                let sample = if sample.is_finite() {
                    sample.clamp(-64.0, 64.0)
                } else {
                    0.0
                };
                self.peak = self.peak.max(sample.abs());
                let state = &mut self.state[channel];
                let shelved = self.shelf.run(&mut state[0], f64::from(sample));
                let weighted = self.highpass.run(&mut state[1], shelved);
                self.step_sum += weighted * weighted;
            }
            self.frames_in_step += 1;
            if self.frames_in_step == self.step_frames {
                self.recent[self.steps % STEPS_PER_BLOCK] = self.step_sum;
                self.steps += 1;
                self.step_sum = 0.0;
                self.frames_in_step = 0;
                if self.steps >= STEPS_PER_BLOCK {
                    let frames = (self.step_frames * STEPS_PER_BLOCK) as f64;
                    self.blocks.push(self.recent.iter().sum::<f64>() / frames);
                }
            }
        }
    }

    pub fn finish(self) -> Measurement {
        Measurement {
            blocks: self.blocks,
            peak: self.peak,
        }
    }
}

/// Decodes a whole file and measures it. `Ok(None)` when `cancel` was set meanwhile.
pub fn measure_file(
    path: &Path,
    cancel: &AtomicBool,
) -> Result<Option<Measurement>, PlaybackError> {
    // Only a regular file: a pipe put in a song's place would never deliver anything,
    // and the worker would wait for it for ever.
    if !std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
        return Err(PlaybackError {
            kind: super::PlaybackErrorKind::FileUnreadable,
            detail: "not a regular file".into(),
        });
    }
    let mut decoder = TrackDecoder::open(path)?;
    let (rate, channels) = {
        let info = decoder.info();
        (info.sample_rate, info.channels)
    };
    let mut meter = Meter::new(rate);
    let (mut packet, mut stereo) = (Vec::new(), Vec::new());
    while decoder.next(&mut packet)? {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        stereo.clear();
        to_stereo(&packet, channels, &mut stereo);
        meter.push(&stereo);
    }
    Ok(Some(meter.finish()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1 kHz sine on both channels, `peak_db` dBFS at its peaks.
    fn sine(rate: u32, seconds: f64, peak_db: f64) -> Vec<f32> {
        let amplitude = 10f64.powf(peak_db / 20.0);
        let frames = (f64::from(rate) * seconds) as usize;
        let mut out = Vec::with_capacity(frames * 2);
        for n in 0..frames {
            let s = (amplitude * (2.0 * PI * 1000.0 * n as f64 / f64::from(rate)).sin()) as f32;
            out.push(s);
            out.push(s);
        }
        out
    }

    fn measure(rate: u32, audio: &[f32]) -> Measurement {
        let mut meter = Meter::new(rate);
        // In uneven pieces, as a decoder hands them over.
        for piece in audio.chunks(2 * 1_117) {
            meter.push(piece);
        }
        meter.finish()
    }

    #[test]
    fn the_weighting_is_the_standards_at_48_khz() {
        let shelf = Biquad::shelf(48_000.0);
        let expected_b = [
            1.535_124_859_586_97,
            -2.691_696_189_406_38,
            1.198_392_810_852_85,
        ];
        let expected_a = [-1.690_659_293_182_41, 0.732_480_774_215_85];
        for (got, want) in shelf.b.iter().zip(expected_b) {
            assert!((got - want).abs() < 1e-9, "{got} vs {want}");
        }
        for (got, want) in shelf.a.iter().zip(expected_a) {
            assert!((got - want).abs() < 1e-9, "{got} vs {want}");
        }
        let highpass = Biquad::highpass(48_000.0);
        let expected_a = [-1.990_047_454_833_98, 0.990_072_250_366_21];
        assert_eq!(highpass.b, [1.0, -2.0, 1.0]);
        for (got, want) in highpass.a.iter().zip(expected_a) {
            assert!((got - want).abs() < 1e-9, "{got} vs {want}");
        }
    }

    /// EBU Tech 3341, case 1: a stereo 1 kHz sine at −23 dBFS reads −23.0 ±0.1 LUFS —
    /// at every sample rate, since the filters are derived for the rate at hand.
    #[test]
    fn a_reference_tone_reads_its_level_at_every_rate() {
        for rate in [22_050, 44_100, 48_000, 88_200, 96_000, 192_000] {
            let loudness = measure(rate, &sine(rate, 20.0, -23.0))
                .loudness()
                .expect("not silent");
            assert!((loudness + 23.0).abs() < 0.1, "{rate} Hz: {loudness}");
        }
        let quiet = measure(44_100, &sine(44_100, 20.0, -33.0)).loudness();
        assert!((quiet.expect("not silent") + 33.0).abs() < 0.1);
    }

    /// EBU Tech 3341, cases 3 and 5: quiet stretches more than 10 LU below the rest do
    /// not count, stretches within 10 LU do.
    #[test]
    fn the_gate_leaves_out_what_is_far_below_the_rest() {
        let rate = 48_000;
        let mut audio = sine(rate, 10.0, -36.0);
        audio.extend(sine(rate, 60.0, -23.0));
        audio.extend(sine(rate, 10.0, -36.0));
        let loudness = measure(rate, &audio).loudness().expect("not silent");
        assert!((loudness + 23.0).abs() < 0.1, "{loudness}");

        let mut audio = sine(rate, 20.0, -26.0);
        audio.extend(sine(rate, 20.1, -20.0));
        audio.extend(sine(rate, 20.0, -26.0));
        let loudness = measure(rate, &audio).loudness().expect("not silent");
        assert!((loudness + 23.0).abs() < 0.1, "{loudness}");

        // Silence around the music changes nothing; silence alone has no loudness.
        let mut audio = vec![0.0; 2 * 48_000 * 5];
        audio.extend(sine(rate, 20.0, -23.0));
        audio.extend(vec![0.0; 2 * 48_000 * 5]);
        let loudness = measure(rate, &audio).loudness().expect("not silent");
        assert!((loudness + 23.0).abs() < 0.1, "{loudness}");
        assert_eq!(measure(rate, &vec![0.0; 2 * 48_000 * 3]).loudness(), None);
        assert_eq!(measure(rate, &sine(rate, 0.2, -3.0)).loudness(), None);
        assert_eq!(gain_db(None), 0.0);
    }

    #[test]
    fn gain_peak_and_samples_that_are_not_numbers() {
        let m = measure(44_100, &sine(44_100, 5.0, -12.0));
        // −12 dBFS is 6 dB above the reference of −18.
        assert!((gain_db(m.loudness()) + 6.0).abs() < 0.1);
        assert!((m.peak - 10f32.powf(-12.0 / 20.0)).abs() < 1e-3);

        let mut audio = sine(44_100, 5.0, -12.0);
        audio[1000] = f32::NAN;
        audio[1001] = f32::INFINITY;
        let m = measure(44_100, &audio);
        assert!(m.blocks.iter().all(|e| e.is_finite()));
        assert!((gain_db(m.loudness()) + 6.0).abs() < 0.1);
    }

    /// An album is one programme: a loud track and a quiet one together are not the
    /// mean of their two readings, and the quiet one may fall below the gate.
    #[test]
    fn an_album_is_measured_from_its_tracks() {
        let rate = 44_100;
        let loud = measure(rate, &sine(rate, 30.0, -14.0));
        let soft = measure(rate, &sine(rate, 30.0, -20.0));
        let together = integrated(&[loud.clone(), soft.clone()]).expect("not silent");
        // Energies add: 10·log10((10^-1.4 + 10^-2.0) / 2) = −16.03.
        assert!((together + 16.03).abs() < 0.1, "{together}");
        let whisper = measure(rate, &sine(rate, 30.0, -40.0));
        let gated = integrated(&[loud.clone(), whisper]).expect("not silent");
        assert!((gated + 14.0).abs() < 0.1, "{gated}");
        assert_eq!(integrated(&[]), None);
        assert_eq!(integrated(std::slice::from_ref(&loud)), loud.loudness());
    }

    #[test]
    fn a_file_is_measured_as_it_is_played() {
        let fixtures = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures"));
        let never = AtomicBool::new(false);
        let stereo = measure_file(&fixtures.join("gap_a.wav"), &never)
            .expect("decodes")
            .expect("not cancelled");
        assert!(stereo.loudness().is_some());
        assert!(stereo.peak > 0.0 && stereo.peak <= 1.0);
        // The same file measured twice gives the same numbers.
        let again = measure_file(&fixtures.join("gap_a.wav"), &never)
            .expect("decodes")
            .expect("not cancelled");
        assert_eq!(stereo, again);
        // Cancelled, or not a file that can be decoded.
        let cancelled = AtomicBool::new(true);
        assert_eq!(
            measure_file(&fixtures.join("gap_a.wav"), &cancelled).expect("opens"),
            None
        );
        assert!(measure_file(&fixtures.join("no_such_file.wav"), &never).is_err());
    }
}
