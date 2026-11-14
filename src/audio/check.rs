//! The file check: decodes a whole file and tells whether it is intact, how large its
//! dynamic range is (the "DR" value) and how high its true peak goes. The file is only
//! read.
//!
//! - **Intact**: decoded to the end without an undecodable part; for formats that keep
//!   an exact length (FLAC, ALAC, PCM, DSD), no audio missing at the end; and where
//!   the file carries a checksum of its audio (FLAC's MD5), the decoded audio matches it.
//! - **DR**: the estimate published by the Pleasurize Music Foundation
//!   (dr.loudness-war.info), as the usual meters compute it: per channel, 3-second
//!   blocks; the RMS of the loudest fifth of them (RMS taken as √2 × the root mean
//!   square, so that a sine's RMS is its peak); the second highest block peak; DR is
//!   their ratio in dB, averaged over the channels. Other meters can differ by about 1.
//! - **True peak**: the highest level of the waveform between the samples as well
//!   (ITU-R BS.1770: the audio oversampled 4 × below 96 kHz, 2 × below 192 kHz).
use super::decoder::TrackDecoder;
use super::dsd::kaiser_lowpass;
use super::{PlaybackError, PlaybackErrorKind};
use crate::model::{CheckVerdict, FileCheck};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Seconds per DR block.
const DR_BLOCK_SECONDS: u32 = 3;
/// The share of the loudest blocks whose RMS counts.
const DR_LOUDEST_SHARE: f64 = 0.2;
/// Coefficients per phase of the true-peak interpolator.
const TP_TAPS_PER_PHASE: usize = 12;
/// Formats whose files state their length exactly: audio missing at the end is damage.
fn exact_length(codec: &str) -> bool {
    codec == "flac" || codec == "alac" || codec == "dsd" || codec.starts_with("pcm")
}

/// Measures the DR value of interleaved audio pushed to it piece by piece.
pub struct DrMeter {
    channels: usize,
    block_frames: usize,
    frames_in_block: usize,
    sum_sq: Vec<f64>,
    peak: Vec<f32>,
    /// Per channel, (RMS, peak) of every block.
    blocks: Vec<Vec<(f64, f32)>>,
}

impl DrMeter {
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        let channels = channels.max(1);
        DrMeter {
            channels,
            block_frames: (sample_rate.max(1) * DR_BLOCK_SECONDS) as usize,
            frames_in_block: 0,
            sum_sq: vec![0.0; channels],
            peak: vec![0.0; channels],
            blocks: vec![Vec::new(); channels],
        }
    }

    pub fn push(&mut self, interleaved: &[f32]) {
        for frame in interleaved.chunks_exact(self.channels) {
            for (channel, &sample) in frame.iter().enumerate() {
                let sample = if sample.is_finite() { sample } else { 0.0 };
                self.sum_sq[channel] += f64::from(sample) * f64::from(sample);
                self.peak[channel] = self.peak[channel].max(sample.abs());
            }
            self.frames_in_block += 1;
            if self.frames_in_block == self.block_frames {
                self.close_block();
            }
        }
    }

    fn close_block(&mut self) {
        let frames = self.frames_in_block as f64;
        for channel in 0..self.channels {
            let rms = (2.0 * self.sum_sq[channel] / frames).sqrt();
            self.blocks[channel].push((rms, self.peak[channel]));
            self.sum_sq[channel] = 0.0;
            self.peak[channel] = 0.0;
        }
        self.frames_in_block = 0;
    }

    /// The DR value; `None` for less than one block of audio, or silence.
    pub fn finish(mut self) -> Option<f32> {
        // A last block of at least a second counts; a shorter tail does not.
        if self.frames_in_block * DR_BLOCK_SECONDS as usize >= self.block_frames {
            self.close_block();
        }
        let mut per_channel = Vec::with_capacity(self.channels);
        for blocks in &self.blocks {
            if blocks.is_empty() {
                return None;
            }
            let mut rms: Vec<f64> = blocks.iter().map(|b| b.0).collect();
            rms.sort_by(|a, b| b.total_cmp(a));
            let loudest = ((rms.len() as f64 * DR_LOUDEST_SHARE).floor() as usize).max(1);
            let rms_top =
                (rms[..loudest].iter().map(|r| r * r).sum::<f64>() / loudest as f64).sqrt();
            let mut peaks: Vec<f32> = blocks.iter().map(|b| b.1).collect();
            peaks.sort_by(|a, b| b.total_cmp(a));
            let peak = f64::from(*peaks.get(1).unwrap_or(&peaks[0]));
            if rms_top <= 0.0 || peak <= 0.0 {
                continue;
            }
            per_channel.push(20.0 * (peak / rms_top).log10());
        }
        if per_channel.is_empty() {
            return None;
        }
        Some((per_channel.iter().sum::<f64>() / per_channel.len() as f64) as f32)
    }
}

/// Measures the true peak of interleaved audio pushed to it piece by piece.
pub struct TruePeakMeter {
    channels: usize,
    factor: usize,
    /// Polyphase coefficients: `phases[p][k]` for the input `k` samples back.
    phases: Vec<[f32; TP_TAPS_PER_PHASE]>,
    /// The most a phase can make of inputs no larger than 1 (the sum of its
    /// coefficients' magnitudes).
    gain_bound: f32,
    /// Recent inputs per channel, kept twice over (`[i]` and `[i + taps]`) so that the
    /// last `taps` of them are always one contiguous slice, newest first.
    history: Vec<[f32; 2 * TP_TAPS_PER_PHASE]>,
    at: usize,
    peak: f32,
}

impl TruePeakMeter {
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        let channels = channels.max(1);
        let factor = if sample_rate < 96_000 {
            4
        } else if sample_rate < 192_000 {
            2
        } else {
            1
        };
        let taps = TP_TAPS_PER_PHASE * factor;
        // A low-pass at the original Nyquist frequency, at the oversampled rate, with
        // the gain that keeps the level (each phase sums to about one).
        let h = kaiser_lowpass(taps, 0.5 / factor as f64 * 0.98, 7.0);
        let phases: Vec<[f32; TP_TAPS_PER_PHASE]> = (0..factor)
            .map(|p| std::array::from_fn(|k| (h[p + k * factor] * factor as f64) as f32))
            .collect();
        let gain_bound = phases
            .iter()
            .map(|phase| phase.iter().map(|c| c.abs()).sum::<f32>())
            .fold(0.0, f32::max);
        TruePeakMeter {
            channels,
            factor,
            phases,
            gain_bound,
            history: vec![[0.0; 2 * TP_TAPS_PER_PHASE]; channels],
            at: 0,
            peak: 0.0,
        }
    }

    pub fn push(&mut self, interleaved: &[f32]) {
        for frame in interleaved.chunks_exact(self.channels) {
            self.at = if self.at == 0 {
                TP_TAPS_PER_PHASE - 1
            } else {
                self.at - 1
            };
            for (channel, &sample) in frame.iter().enumerate() {
                let sample = if sample.is_finite() { sample } else { 0.0 };
                self.peak = self.peak.max(sample.abs());
                let history = &mut self.history[channel];
                history[self.at] = sample;
                history[self.at + TP_TAPS_PER_PHASE] = sample;
                if self.factor == 1 {
                    continue;
                }
                let window = &history[self.at..self.at + TP_TAPS_PER_PHASE];
                // Nothing between these samples can reach the peak found so far.
                let largest = window.iter().fold(0f32, |m, x| m.max(x.abs()));
                if largest * self.gain_bound <= self.peak {
                    continue;
                }
                for phase in &self.phases {
                    let y: f32 = phase.iter().zip(window).map(|(c, x)| c * x).sum();
                    self.peak = self.peak.max(y.abs());
                }
            }
        }
    }

    /// The true peak (1.0 = full scale).
    pub fn finish(self) -> f32 {
        self.peak
    }
}

/// Checks one file. `Ok(None)` when `cancel` was set meanwhile; `Err` when the file
/// could not be opened at all for a reason that says nothing about its content (gone,
/// not readable, a format that is not played).
pub fn check_file(path: &Path, cancel: &AtomicBool) -> Result<Option<FileCheck>, PlaybackError> {
    // A track of a CUE sheet is a stretch of the sheet's file (the sheet is read once).
    let segment = crate::library::cue::resolve(path);
    let file = segment.as_ref().map_or(path, |s| s.audio.as_path());
    // Only a regular file: a pipe in a song's place would never deliver anything.
    if !std::fs::metadata(file).is_ok_and(|m| m.is_file()) {
        return Err(PlaybackError {
            kind: PlaybackErrorKind::FileUnreadable,
            detail: "not a regular file".into(),
        });
    }
    let damaged = || {
        Ok(Some(FileCheck {
            verdict: CheckVerdict::Damaged,
            dr: None,
            true_peak: None,
        }))
    };
    let mut decoder = match TrackDecoder::open_verifying(path, segment.as_ref()) {
        Ok(decoder) => decoder,
        Err(e) if e.kind == PlaybackErrorKind::Corrupt => return damaged(),
        Err(e) => return Err(e),
    };
    let (rate, channels, total, codec) = {
        let info = decoder.info();
        (
            info.sample_rate,
            info.channels,
            info.total_frames,
            info.codec.clone(),
        )
    };
    let mut dr = DrMeter::new(rate, channels);
    let mut peak = TruePeakMeter::new(rate, channels);
    let mut packet = Vec::new();
    let mut frames: u64 = 0;
    loop {
        match decoder.next(&mut packet) {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) if e.kind == PlaybackErrorKind::Corrupt => return damaged(),
            Err(e) => return Err(e),
        }
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        frames += (packet.len() / channels.max(1)) as u64;
        dr.push(&packet);
        peak.push(&packet);
    }
    let checksum = decoder.checksum_matches();
    let short =
        exact_length(&codec) && total.is_some_and(|total| frames + u64::from(rate / 100) < total);
    let verdict = if decoder.skipped_packets() > 0 || checksum == Some(false) || short {
        CheckVerdict::Damaged
    } else if checksum == Some(true) {
        CheckVerdict::Verified
    } else {
        CheckVerdict::Decoded
    };
    Ok(Some(FileCheck {
        verdict,
        dr: dr.finish(),
        true_peak: Some(peak.finish()),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    fn db(x: f32) -> f64 {
        20.0 * f64::from(x).log10()
    }

    /// Interleaved stereo sine: `amplitude` at `freq` Hz, starting at `phase`.
    fn sine(rate: u32, seconds: f64, freq: f64, amplitude: f64, phase: f64) -> Vec<f32> {
        let frames = (f64::from(rate) * seconds) as usize;
        (0..frames)
            .flat_map(|n| {
                let v =
                    (amplitude * (TAU * freq * n as f64 / f64::from(rate) + phase).sin()) as f32;
                [v, v]
            })
            .collect()
    }

    /// A sine at a quarter of the sample rate, a quarter cycle off the samples: every
    /// sample reads 0.707 of the wave's peak (−3 dB), the true peak is the wave's.
    #[test]
    fn the_true_peak_is_found_between_the_samples() {
        for rate in [44_100, 48_000] {
            let audio = sine(rate, 1.0, f64::from(rate) / 4.0, 0.9, TAU / 8.0);
            let samples = audio.iter().fold(0f32, |m, s| m.max(s.abs()));
            assert!(
                (db(samples) - db(0.9) + 3.01).abs() < 0.05,
                "{}",
                db(samples)
            );
            let mut meter = TruePeakMeter::new(rate, 2);
            meter.push(&audio);
            let tp = meter.finish();
            assert!((db(tp) - db(0.9)).abs() < 0.1, "{rate}: {} dBTP", db(tp));
        }
        // An ordinary tone: the true peak is its peak.
        let audio = sine(44_100, 1.0, 997.0, 0.5, 0.3);
        let mut meter = TruePeakMeter::new(44_100, 2);
        meter.push(&audio);
        assert!((db(meter.finish()) - db(0.5)).abs() < 0.05);
        // At 192 kHz the samples are taken as they are.
        let mut meter = TruePeakMeter::new(192_000, 2);
        meter.push(&[0.25, -0.5]);
        assert_eq!(meter.finish(), 0.5);
    }

    #[test]
    fn dr_of_known_signals() {
        // A sine: its RMS (as DR takes it) equals its peak, DR 0.
        let mut meter = DrMeter::new(8_000, 2);
        meter.push(&sine(8_000, 30.0, 440.0, 0.8, 0.0));
        let dr = meter.finish().expect("dr");
        assert!(dr.abs() < 0.05, "{dr}");
        // A quiet sine with one full-scale click in every block: DR = the ratio of
        // the click to the sine, 20·log10(1 / 0.25) = 12.04 dB.
        let mut audio = sine(8_000, 30.0, 440.0, 0.25, 0.0);
        for block in 0..10 {
            let at = (block * 3 * 8_000 + 1_000) * 2;
            audio[at] = 1.0;
            audio[at + 1] = 1.0;
        }
        let mut meter = DrMeter::new(8_000, 2);
        meter.push(&audio);
        let dr = meter.finish().expect("dr");
        assert!((dr - 12.04).abs() < 0.1, "{dr}");
        // One click alone does not decide: the second highest block peak counts.
        let mut audio = sine(8_000, 30.0, 440.0, 0.25, 0.0);
        audio[2_000] = 1.0;
        let mut meter = DrMeter::new(8_000, 2);
        meter.push(&audio);
        assert!(meter.finish().expect("dr").abs() < 0.1);
        // Less than a block, or silence: no value.
        let mut meter = DrMeter::new(8_000, 2);
        meter.push(&sine(8_000, 0.5, 440.0, 0.5, 0.0));
        assert_eq!(meter.finish(), None);
        let mut meter = DrMeter::new(8_000, 2);
        meter.push(&vec![0.0; 8_000 * 2 * 10]);
        assert_eq!(meter.finish(), None);
    }

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn intact_damaged_and_unreadable_files() {
        let cancel = AtomicBool::new(false);
        let check = |name: &str| check_file(&fixture(name), &cancel);
        // FLAC with its MD5: verified. Formats without a checksum: decoded.
        let flac = check("gap_a.flac").expect("checks").expect("result");
        assert_eq!(flac.verdict, CheckVerdict::Verified);
        assert!(flac.true_peak.is_some_and(|p| p > 0.0 && p < 1.5));
        for name in ["gap_a.wav", "gap_a.mp3", "gap_a.aac.m4a", "gap_a.ogg"] {
            let result = check(name).expect("checks").expect("result");
            assert_eq!(result.verdict, CheckVerdict::Decoded, "{name}");
        }
        // Damaged in the middle.
        let result = check("damaged_middle.flac")
            .expect("checks")
            .expect("result");
        assert_eq!(result.verdict, CheckVerdict::Damaged);
        // An MP3 cut short is not told apart: the length an MP3 states can be an
        // estimate, so audio missing at its end is not taken for damage.
        let result = check("truncated.mp3").expect("checks").expect("result");
        assert_eq!(result.verdict, CheckVerdict::Decoded);
        // Formats that state their length exactly, cut short: damaged.
        let dir = tempfile::tempdir().expect("tempdir");
        for name in ["gap_a.wav", "gap_a.flac"] {
            let bytes = std::fs::read(fixture(name)).expect("fixture");
            let cut = dir.path().join(name);
            std::fs::write(&cut, &bytes[..bytes.len() * 2 / 3]).expect("write");
            let result = check_file(&cut, &cancel).expect("checks").expect("result");
            assert_eq!(result.verdict, CheckVerdict::Damaged, "{name} cut short");
        }
        // Gone: not a verdict on the content.
        assert!(check("not_there.flac").is_err());
    }

    /// A FLAC whose audio differs from what its MD5 says: one byte of a frame's
    /// samples changed (the frame's own checksum fixed, so the frame still decodes).
    #[test]
    fn a_flac_whose_audio_does_not_match_its_md5_is_damaged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("changed.flac");
        let mut bytes = std::fs::read(fixture("gap_a.flac")).expect("fixture");
        // Flip the MD5 stored in STREAMINFO instead: the audio is intact, the
        // checksum no longer belongs to it. (STREAMINFO: after "fLaC" and the 4-byte
        // block header; the MD5 is its last 16 bytes.)
        assert_eq!(&bytes[..4], b"fLaC");
        let md5_at = 4 + 4 + 34 - 16;
        bytes[md5_at] ^= 0xFF;
        std::fs::write(&path, &bytes).expect("write");
        let cancel = AtomicBool::new(false);
        let result = check_file(&path, &cancel).expect("checks").expect("result");
        assert_eq!(result.verdict, CheckVerdict::Damaged);
        // A FLAC with no MD5 (all zeros: the encoder did not compute one): decoded.
        for b in &mut bytes[md5_at..md5_at + 16] {
            *b = 0;
        }
        std::fs::write(&path, &bytes).expect("write");
        let result = check_file(&path, &cancel).expect("checks").expect("result");
        assert_eq!(result.verdict, CheckVerdict::Decoded);
    }
}

/// Speed of the check on ten minutes of 44.1 kHz stereo music-like audio (a manual
/// measurement: `cargo test --release --lib audio::check -- --ignored --nocapture`).
#[cfg(test)]
#[test]
#[ignore]
fn check_speed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ten-minutes.wav");
    let (rate, seconds) = (44_100u32, 600u32);
    let frames = rate * seconds;
    let mut bytes = Vec::with_capacity(44 + frames as usize * 4);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + frames * 4).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 4).to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(frames * 4).to_le_bytes());
    let mut x: u32 = 1;
    for n in 0..frames {
        // A tone with noise and a slow swell: peaks keep rising for a while.
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let noise = (x >> 16) as f64 / 65_536.0 - 0.5;
        let swell = f64::from(n) / f64::from(frames);
        let v = ((f64::from(n) * 0.0627).sin() * 0.5 + noise * 0.3) * (0.3 + 0.7 * swell);
        let s = (v * 32_000.0) as i16;
        bytes.extend_from_slice(&s.to_le_bytes());
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(&path, bytes).expect("write");
    let started = std::time::Instant::now();
    let result = check_file(&path, &AtomicBool::new(false))
        .expect("checks")
        .expect("result");
    let took = started.elapsed().as_secs_f64();
    println!(
        "10 min of audio checked in {took:.2} s ({:.0}× real time): {result:?}",
        f64::from(seconds) / took
    );
}
