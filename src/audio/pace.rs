//! Playback speed: a song faster or slower, at the pitch it has.
//!
//! The work is done by the `wsola` crate (waveform similarity overlap-add): the sound
//! is cut into short overlapping pieces that are laid down again closer together or
//! further apart, each where it continues the one before it best. Nothing of this
//! runs at the speed a song has by itself: the decode thread then has no [`Pacer`].
use std::num::{NonZeroU16, NonZeroU32};
use wsola::{Config, TimeStretch};

/// The speeds that can be set, in percent of a song's own.
pub const SPEED_RANGE: std::ops::RangeInclusive<u16> = 50..=200;

/// `percent` as a speed that can be set (anything out of range: a song's own speed).
pub fn speed_of(percent: u16) -> u16 {
    if SPEED_RANGE.contains(&percent) {
        percent
    } else {
        100
    }
}

/// The rate the pacer works at for an output of `out_rate` Hz: the output's own up to
/// 48 kHz, and above that the 44.1 or 48 kHz the output is a multiple of. Finding
/// where a piece fits costs with the square of the rate (measured: sixteen times as
/// fast as it plays at 44.1 kHz, three times at 96 kHz, slower than it plays at
/// 192 kHz), and a song at another speed is not the recording to the last overtone
/// anyway. The sound is brought to the output's rate after the pacer.
pub fn working_rate(out_rate: u32) -> u32 {
    match out_rate {
        0..=48_000 => out_rate,
        rate if rate.is_multiple_of(44_100) => 44_100,
        _ => 48_000,
    }
}

/// The pieces are longer than the crate's own choice, which is made for speech: music
/// holds notes longer than a syllable lasts, and a longer piece keeps them steadier.
const PIECES: Config = Config {
    hop_ms: 20.0,
    search_ms: 15.0,
};

/// Changes the speed of stereo sound that is pushed through it piece by piece.
pub struct Pacer {
    stretch: TimeStretch,
    /// Room for what one call gives (a whole number of the stretcher's steps).
    room: Vec<f32>,
}

impl Pacer {
    /// For stereo at `rate` Hz, `speed` times as fast as it is (1.0: `None`, nothing
    /// is to be done; so for a rate or a speed that makes no sense).
    pub fn new(rate: u32, speed: f32) -> Option<Self> {
        if !speed.is_finite() || (speed - 1.0).abs() < 0.001 {
            return None;
        }
        let mut stretch =
            TimeStretch::with_config(NonZeroU32::new(rate)?, NonZeroU16::new(2)?, PIECES).ok()?;
        stretch.set_tempo(speed);
        let room = vec![0.0; stretch.min_output_buff_size() * 8];
        Some(Pacer { stretch, room })
    }

    fn pull(&mut self, out: &mut Vec<f32>) {
        while let Ok(n) = self.stretch.pull(&mut self.room) {
            if n == 0 {
                break;
            }
            out.extend_from_slice(&self.room[..n]);
        }
    }

    /// Takes `input` (interleaved stereo) and appends to `out` what can be given of
    /// the sound so far: less than was taken, or more, and sometimes nothing.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.stretch.push(input);
        self.pull(out);
    }

    /// At the end of a song: appends what is left of it to `out`.
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        self.stretch.set_draining();
        self.pull(out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 44_100;

    fn tone(hz: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|n| {
                let s = 0.5 * (std::f32::consts::TAU * hz * n as f32 / RATE as f32).sin();
                [s, s]
            })
            .collect()
    }

    /// Pushes `input` through a pacer in pieces of uneven size, as a decoder gives it.
    fn paced(input: &[f32], speed: f32) -> Vec<f32> {
        let mut pacer = Pacer::new(RATE, speed).expect("a pacer");
        let mut out = Vec::new();
        let mut at = 0;
        let mut piece = 2 * 1_000;
        while at < input.len() {
            let end = (at + piece).min(input.len());
            pacer.process(&input[at..end], &mut out);
            at = end;
            piece = if piece == 2 * 1_000 {
                2 * 4_096
            } else {
                2 * 1_000
            };
        }
        pacer.finish(&mut out);
        out
    }

    /// How strongly `hz` is in the left channel of `stereo` (the amplitude of that
    /// one frequency), over its middle half.
    fn level_at(stereo: &[f32], hz: f32) -> f32 {
        let left: Vec<f32> = stereo.as_chunks::<2>().0.iter().map(|f| f[0]).collect();
        let part = &left[left.len() / 4..left.len() * 3 / 4];
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (n, s) in part.iter().enumerate() {
            let turn = std::f64::consts::TAU * f64::from(hz) * n as f64 / f64::from(RATE);
            re += f64::from(*s) * turn.cos();
            im += f64::from(*s) * turn.sin();
        }
        (2.0 * (re * re + im * im).sqrt() / part.len() as f64) as f32
    }

    #[test]
    fn the_pacer_works_at_a_rate_it_keeps_up_with() {
        for (out, at) in [
            (44_100, 44_100),
            (48_000, 48_000),
            (22_050, 22_050),
            (88_200, 44_100),
            (96_000, 48_000),
            (176_400, 44_100),
            (192_000, 48_000),
            (384_000, 48_000),
        ] {
            assert_eq!(working_rate(out), at, "for {out} Hz");
        }
    }

    #[test]
    fn a_song_at_its_own_speed_needs_no_pacer() {
        assert!(Pacer::new(RATE, 1.0).is_none());
        assert!(Pacer::new(RATE, f32::NAN).is_none());
        assert!(Pacer::new(0, 1.5).is_none());
        assert_eq!(speed_of(125), 125);
        assert_eq!(speed_of(0), 100);
        assert_eq!(speed_of(900), 100);
    }

    /// Faster is shorter and slower is longer, by the speed; a tone stays the tone it
    /// was (its pitch and nearly all of its level), and nothing else of weight is
    /// made beside it.
    #[test]
    fn the_length_follows_the_speed_and_the_pitch_stays() {
        let seconds = 4;
        let input = tone(440.0, RATE as usize * seconds);
        for speed in [0.5f32, 0.75, 1.25, 1.5, 2.0] {
            let out = paced(&input, speed);
            let frames = out.len() / 2;
            let expected = (RATE as f32 * seconds as f32 / speed) as usize;
            assert!(
                frames.abs_diff(expected) < RATE as usize / 20,
                "at {speed}: {frames} frames for {expected}"
            );
            assert!(
                out.iter().all(|s| s.is_finite() && s.abs() <= 1.0),
                "at {speed}"
            );
            let there = level_at(&out, 440.0);
            assert!(
                (there - 0.5).abs() < 0.05,
                "at {speed}: the tone's level is {there}"
            );
            // What a change of pitch would give: the tone at 440 Hz times the speed.
            let moved = level_at(&out, 440.0 * speed);
            assert!(
                moved < 0.05,
                "at {speed}: {moved} of the tone moved in pitch"
            );
            // The whole of the sound is the tone: the RMS is that of a sine of 0.5.
            let left: Vec<f32> = out.as_chunks::<2>().0.iter().map(|f| f[0]).collect();
            let part = &left[left.len() / 4..left.len() * 3 / 4];
            let rms = (part.iter().map(|s| s * s).sum::<f32>() / part.len() as f32).sqrt();
            assert!((rms - 0.3536).abs() < 0.03, "at {speed}: RMS {rms}");
        }
    }

    /// Both channels are the sound they were: what is only on the left stays there.
    #[test]
    fn the_channels_stay_apart() {
        let input: Vec<f32> = tone(330.0, RATE as usize * 2)
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|f| [f[0], 0.0])
            .collect();
        let out = paced(&input, 1.5);
        assert!(out.as_chunks::<2>().0.iter().all(|f| f[1] == 0.0));
        assert!(level_at(&out, 330.0) > 0.4);
    }
}
