//! Headphone crossfeed.
//!
//! Loudspeakers are heard by both ears: the left speaker reaches the right ear too, a
//! little later, quieter, and with the highs shadowed by the head. Headphones give each
//! ear one channel only, which makes recordings mixed for speakers (a bass hard on one
//! side, as on many records of the 1960s) tiring to listen to. Crossfeed puts some of
//! that back: each channel gets the other one's low frequencies mixed in, and gives up
//! a little of its own so the bass does not pile up.
//!
//! This is the filter pair of Boris Mikhaylov's bs2b ("Bauer stereophonic-to-binaural"):
//! a one-pole low-pass on the way across, a matching high-shelf on the direct way. The
//! three strengths are its three customary settings.
//!
//! Switched off it is out of the path: the samples pass bit for bit. Switching it on,
//! off or to another strength is cross-faded, so it never clicks.
use serde::{Deserialize, Serialize};

/// How much crossfeed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, Serialize, Deserialize)]
pub enum Crossfeed {
    #[default]
    Off,
    /// The other channel 9.5 dB down, below 650 Hz (the setting known as Jan Meier's).
    Light,
    /// 6 dB down, below 700 Hz (Chu Moy's).
    Medium,
    /// 4.5 dB down, below 700 Hz: closest to speakers set up at 30° either side.
    Strong,
}

impl Crossfeed {
    /// `(cut-off of the low-pass in Hz, level of the other channel below the direct one
    /// at low frequencies in dB)`.
    fn design(self) -> Option<(f64, f64)> {
        match self {
            Crossfeed::Off => None,
            Crossfeed::Light => Some((650.0, 9.5)),
            Crossfeed::Medium => Some((700.0, 6.0)),
            Crossfeed::Strong => Some((700.0, 4.5)),
        }
    }

    pub fn to_u8(self) -> u8 {
        match self {
            Crossfeed::Off => 0,
            Crossfeed::Light => 1,
            Crossfeed::Medium => 2,
            Crossfeed::Strong => 3,
        }
    }

    pub fn from_u8(value: u8) -> Self {
        match value {
            1 => Crossfeed::Light,
            2 => Crossfeed::Medium,
            3 => Crossfeed::Strong,
            _ => Crossfeed::Off,
        }
    }
}

/// The filters of one strength at one sample rate.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Filters {
    /// Low-pass on the way across: `lo = a0_lo * x + b1_lo * lo`.
    a0_lo: f64,
    b1_lo: f64,
    /// High-shelf on the direct way: `hi = a0_hi * x + a1_hi * x_before + b1_hi * hi`.
    a0_hi: f64,
    a1_hi: f64,
    b1_hi: f64,
    /// Brings the sum back to unity at low frequencies.
    gain: f64,
}

impl Filters {
    fn new(rate: f64, cut_hz: f64, level_db: f64) -> Self {
        // How far down each way is at low frequencies. Their difference is `level_db`.
        let across_db = level_db * -5.0 / 6.0 - 3.0;
        let direct_db = level_db / 6.0 - 3.0;
        let across = 10f64.powf(across_db / 20.0);
        let given_up = 1.0 - 10f64.powf(direct_db / 20.0);
        // The shelf turns where the low-pass has fallen to what the direct way gave up.
        let shelf_hz = cut_hz * 2f64.powf((across_db - 20.0 * given_up.log10()) / 12.0);
        let x_lo = (-std::f64::consts::TAU * cut_hz / rate).exp();
        let x_hi = (-std::f64::consts::TAU * shelf_hz / rate).exp();
        Self {
            a0_lo: across * (1.0 - x_lo),
            b1_lo: x_lo,
            a0_hi: 1.0 - given_up * (1.0 - x_hi),
            a1_hi: -x_hi,
            b1_hi: x_hi,
            gain: 1.0 / (1.0 - given_up + across),
        }
    }
}

/// Length of the cross-fade when the setting changes.
const FADE_SECONDS: f64 = 0.04;

/// The running crossfeed of one feed (stereo, in place).
pub struct Crossfeeder {
    rate: f64,
    /// What it is set to.
    target: Crossfeed,
    /// What the filters are built for right now.
    current: Crossfeed,
    filters: Option<Filters>,
    /// Filter memory per channel: low-pass, high-shelf, the sample before.
    lo: [f64; 2],
    hi: [f64; 2],
    before: [f64; 2],
    /// How much of the output is the crossfed signal: 0 = none (the samples as they
    /// came), 1 = all.
    mix: f64,
    fade_step: f64,
}

impl Crossfeeder {
    pub fn new(sample_rate: u32, setting: Crossfeed) -> Self {
        let rate = f64::from(sample_rate.max(1));
        let mut feeder = Self {
            rate,
            target: setting,
            current: Crossfeed::Off,
            filters: None,
            lo: [0.0; 2],
            hi: [0.0; 2],
            before: [0.0; 2],
            // A feed starts at its setting: there is nothing before it to fade from.
            mix: if setting == Crossfeed::Off { 0.0 } else { 1.0 },
            fade_step: 1.0 / (FADE_SECONDS * rate),
        };
        feeder.build(setting);
        feeder
    }

    pub fn set(&mut self, setting: Crossfeed) {
        self.target = setting;
    }

    /// True while the samples pass through untouched.
    pub fn is_bypassed(&self) -> bool {
        self.current == Crossfeed::Off && self.target == Crossfeed::Off
    }

    fn build(&mut self, setting: Crossfeed) {
        self.current = setting;
        self.filters = setting
            .design()
            .map(|(cut, level)| Filters::new(self.rate, cut, level));
        self.lo = [0.0; 2];
        self.hi = [0.0; 2];
        self.before = [0.0; 2];
    }

    /// Applies the crossfeed to interleaved stereo `samples` in place.
    pub fn process(&mut self, samples: &mut [f32]) {
        if self.is_bypassed() {
            return;
        }
        for frame in samples.as_chunks_mut::<2>().0 {
            if self.current != self.target {
                // Fade out what is there, then build what is wanted and fade it in.
                self.mix -= self.fade_step;
                if self.mix <= 0.0 {
                    self.mix = 0.0;
                    self.build(self.target);
                }
            } else if self.mix < 1.0 {
                self.mix = (self.mix + self.fade_step).min(1.0);
            }
            let Some(f) = self.filters else {
                // Off, and faded all the way out: the samples as they are.
                continue;
            };
            let x = [f64::from(frame[0]), f64::from(frame[1])];
            for (ch, &input) in x.iter().enumerate() {
                self.lo[ch] = f.a0_lo * input + f.b1_lo * self.lo[ch];
                self.hi[ch] = f.a0_hi * input + f.a1_hi * self.before[ch] + f.b1_hi * self.hi[ch];
                self.before[ch] = input;
            }
            // Each ear: its own channel's shelf plus the other channel's low-pass.
            let wet = [
                (self.hi[0] + self.lo[1]) * f.gain,
                (self.hi[1] + self.lo[0]) * f.gain,
            ];
            for ((out, dry), wet) in frame.iter_mut().zip(x).zip(wet) {
                *out = (dry + (wet - dry) * self.mix) as f32;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 44_100;

    /// A tone on the left channel only (`right` = false) or on both.
    fn tone(frames: usize, hz: f64, both: bool) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let s =
                    ((i as f64 * hz * std::f64::consts::TAU / f64::from(RATE)).sin() * 0.5) as f32;
                [s, if both { s } else { 0.0 }]
            })
            .collect()
    }

    /// Level of the tone of `hz` in `channel` over the last half of `samples`, in dB
    /// relative to the 0.5 it went in with.
    fn level_db(samples: &[f32], channel: usize, hz: f64) -> f64 {
        let frames = samples.as_chunks::<2>().0;
        let half = &frames[frames.len() / 2..];
        let w = hz * std::f64::consts::TAU / f64::from(RATE);
        let (mut a, mut b) = (0.0, 0.0);
        for (k, frame) in half.iter().enumerate() {
            let (s, c) = (k as f64 * w).sin_cos();
            a += f64::from(frame[channel]) * s;
            b += f64::from(frame[channel]) * c;
        }
        20.0 * (2.0 * a.hypot(b) / half.len() as f64 / 0.5)
            .max(1e-12)
            .log10()
    }

    fn run(setting: Crossfeed, mut samples: Vec<f32>) -> Vec<f32> {
        let mut feeder = Crossfeeder::new(RATE, setting);
        for chunk in samples.chunks_mut(1000) {
            feeder.process(chunk);
        }
        samples
    }

    #[test]
    fn off_passes_bit_for_bit() {
        let input = tone(10_000, 440.0, false);
        let out = run(Crossfeed::Off, input.clone());
        assert!(
            out.iter()
                .zip(&input)
                .all(|(a, b)| a.to_bits() == b.to_bits())
        );
        assert!(Crossfeeder::new(RATE, Crossfeed::Off).is_bypassed());
    }

    #[test]
    fn low_notes_on_one_side_reach_the_other_ear_at_the_set_level() {
        for (setting, level) in [
            (Crossfeed::Light, 9.5),
            (Crossfeed::Medium, 6.0),
            (Crossfeed::Strong, 4.5),
        ] {
            // 60 Hz, far below the cut-off, on the left only.
            let out = run(setting, tone(RATE as usize, 60.0, false));
            let direct = level_db(&out, 0, 60.0);
            let across = level_db(&out, 1, 60.0);
            assert!(
                (direct - across - level).abs() < 0.2,
                "{setting:?}: direct {direct:.2} dB, across {across:.2} dB"
            );
            // The side it is on gives a little up, it does not get louder.
            assert!((-6.0..-0.5).contains(&direct), "{setting:?}: {direct:.2}");
        }
    }

    #[test]
    fn high_notes_stay_on_their_side() {
        // 8 kHz on the left: the head would shadow it, and so does the low-pass.
        for setting in [Crossfeed::Light, Crossfeed::Medium, Crossfeed::Strong] {
            let out = run(setting, tone(RATE as usize, 8_000.0, false));
            let direct = level_db(&out, 0, 8_000.0);
            let across = level_db(&out, 1, 8_000.0);
            assert!(
                direct - across > 20.0,
                "{setting:?}: only {:.1} dB apart",
                direct - across
            );
        }
    }

    #[test]
    fn what_is_in_the_middle_keeps_its_level_in_the_bass() {
        // The same on both channels (a voice, a bass in the centre): the two ways add
        // up to what went in at low frequencies; the highs come out a little lower,
        // by no more than 2 dB.
        for setting in [Crossfeed::Light, Crossfeed::Medium, Crossfeed::Strong] {
            let low = run(setting, tone(RATE as usize, 60.0, true));
            assert!(level_db(&low, 0, 60.0).abs() < 0.1, "{setting:?}");
            assert!((level_db(&low, 0, 60.0) - level_db(&low, 1, 60.0)).abs() < 1e-6);
            let high = run(setting, tone(RATE as usize, 10_000.0, true));
            let highs = level_db(&high, 0, 10_000.0);
            assert!(
                (-2.0..0.0).contains(&highs),
                "{setting:?}: highs at {highs:.2} dB"
            );
        }
    }

    #[test]
    fn nothing_gets_louder_than_it_was() {
        // Full scale on both channels, in phase and out of phase, at several pitches.
        for setting in [Crossfeed::Light, Crossfeed::Medium, Crossfeed::Strong] {
            for hz in [40.0, 300.0, 700.0, 2_000.0, 12_000.0] {
                for flip in [1.0, -1.0f32] {
                    let input: Vec<f32> = tone(RATE as usize / 2, hz, true)
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .flat_map(|f| [f[0] * 2.0, f[1] * 2.0 * flip])
                        .collect();
                    let out = run(setting, input);
                    let peak = out.iter().fold(0f32, |m, s| m.max(s.abs()));
                    assert!(peak <= 1.001, "{setting:?} at {hz} Hz: peak {peak}");
                }
            }
        }
    }

    #[test]
    fn switching_is_cross_faded_and_off_ends_in_the_untouched_signal() {
        let input = tone(RATE as usize, 100.0, false);
        let mut out = input.clone();
        let mut feeder = Crossfeeder::new(RATE, Crossfeed::Off);
        let quarter = out.len() / 4;
        let (a, rest) = out.split_at_mut(quarter);
        let (b, rest) = rest.split_at_mut(quarter);
        let (c, d) = rest.split_at_mut(quarter);
        feeder.process(a);
        feeder.set(Crossfeed::Strong);
        feeder.process(b);
        feeder.set(Crossfeed::Light);
        feeder.process(c);
        feeder.set(Crossfeed::Off);
        feeder.process(d);
        // No step between neighbouring samples beyond what the tone itself does (the
        // right channel is silent going in, so there every step is the crossfeed's).
        let steepest = 0.5 * std::f32::consts::TAU * 100.0 / RATE as f32;
        for channel in 0..2 {
            let largest = out
                .as_chunks::<2>()
                .0
                .windows(2)
                .fold(0f32, |m, w| m.max((w[1][channel] - w[0][channel]).abs()));
            assert!(
                largest <= steepest * 1.1,
                "channel {channel}: {largest} > {steepest}"
            );
        }
        // While it was on, the right channel had sound; at the end it is out of the
        // path and the samples are the input's own.
        let second_quarter = &out[quarter + quarter / 2..quarter * 2];
        assert!(
            second_quarter
                .iter()
                .skip(1)
                .step_by(2)
                .any(|s| s.abs() > 0.05)
        );
        assert!(feeder.is_bypassed());
        let tail = input.len() - 2_000;
        assert!(
            out[tail..]
                .iter()
                .zip(&input[tail..])
                .all(|(x, y)| x.to_bits() == y.to_bits())
        );
    }

    #[test]
    fn the_setting_survives_its_number() {
        for setting in [
            Crossfeed::Off,
            Crossfeed::Light,
            Crossfeed::Medium,
            Crossfeed::Strong,
        ] {
            assert_eq!(Crossfeed::from_u8(setting.to_u8()), setting);
        }
        assert_eq!(Crossfeed::from_u8(200), Crossfeed::Off);
    }
}
