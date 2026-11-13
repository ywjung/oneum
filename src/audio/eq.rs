//! The equalizer: ten octave bands and a pre-amp.
//!
//! It runs in the decode thread at the output rate, after the rate conversion and in
//! front of the peak limiter, so a boost that pushes peaks over full scale is brought
//! down instead of being clipped. What it promises:
//!
//! - Switched off, or with everything at 0 dB, it does nothing at all: the samples pass
//!   bit for bit.
//! - A change of settings is glided to (the gains move a little every fraction of a
//!   millisecond), so moving a slider never clicks.
//! - The filters are computed in 64-bit floating point, which keeps the lowest band
//!   exact at 192 kHz, where a 32 Hz filter is a very fine thing.
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

pub const BANDS: usize = 10;
/// Centre frequencies of the bands, an octave apart.
pub const BAND_HZ: [f32; BANDS] = [
    32.0, 64.0, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0,
];
/// How far a band (and the pre-amp) can be raised or lowered, in dB.
pub const GAIN_RANGE_DB: f32 = 12.0;
/// Width of a band: with this Q neighbouring bands set alike add up to a nearly level
/// shelf, and one band alone reaches about an octave to either side.
const BAND_Q: f64 = std::f64::consts::SQRT_2;
/// A band this close to half the sample rate (or above it) cannot be built; it is left out.
const HIGHEST_BAND: f64 = 0.45;
/// Frames between two steps of a gliding gain.
const GLIDE_BLOCK: usize = 32;
/// How fast a gain glides, in dB per second: the whole range in a tenth of a second.
const GLIDE_DB_PER_SECOND: f64 = 240.0;
/// How long the filters keep running at 0 dB before they are taken out: what is left
/// ringing in them has died away by then.
const SETTLE_SECONDS: f64 = 0.5;

/// What the equalizer is set to.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqSettings {
    pub enabled: bool,
    /// Gain applied to everything, in dB: turned down to make room for boosted bands.
    pub preamp_db: f32,
    /// Gain of each band in dB, lowest band first.
    pub gains_db: [f32; BANDS],
}

impl Default for EqSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            preamp_db: 0.0,
            gains_db: [0.0; BANDS],
        }
    }
}

fn usable_db(db: f32) -> f32 {
    if db.is_finite() {
        db.clamp(-GAIN_RANGE_DB, GAIN_RANGE_DB)
    } else {
        0.0
    }
}

impl EqSettings {
    /// The same settings with every value inside the range (a damaged settings file
    /// must not turn into a filter that blows up).
    pub fn sanitized(mut self) -> Self {
        self.preamp_db = usable_db(self.preamp_db);
        for gain in &mut self.gains_db {
            *gain = usable_db(*gain);
        }
        self
    }

    /// True when the equalizer changes the sound: switched on and not all at 0 dB.
    pub fn is_active(&self) -> bool {
        let s = self.sanitized();
        s.enabled && (s.preamp_db != 0.0 || s.gains_db.iter().any(|g| *g != 0.0))
    }

    /// What the filters are to do: the settings, or all zero when switched off.
    fn target(&self) -> [f64; BANDS + 1] {
        let mut target = [0.0; BANDS + 1];
        if self.is_active() {
            let s = self.sanitized();
            for (slot, gain) in target.iter_mut().zip(s.gains_db) {
                *slot = f64::from(gain);
            }
            target[BANDS] = f64::from(s.preamp_db);
        }
        target
    }
}

/// A named setting of the bands and the pre-amp.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EqPreset {
    pub name: String,
    pub preamp_db: f32,
    pub gains_db: [f32; BANDS],
}

/// A preset that comes with the app. Its name is looked up by `id` in the app's language.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BuiltinPreset {
    pub id: &'static str,
    pub gains_db: [f32; BANDS],
}

impl BuiltinPreset {
    /// The pre-amp that goes with the preset: down by as much as the highest band is
    /// up, so that what was at full scale before still is, and nothing has to be
    /// limited.
    pub fn preamp_db(&self) -> f32 {
        -self.gains_db.iter().fold(0f32, |m, g| m.max(*g))
    }
}

/// The presets that come with the app: common shapes to start from.
///                                    32    64   125   250   500    1k    2k    4k    8k   16k
pub const BUILTIN_PRESETS: [BuiltinPreset; 12] = [
    preset("flat", [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
    preset(
        "bass_boost",
        [6.0, 5.0, 4.0, 2.0, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0],
    ),
    preset(
        "bass_cut",
        [-6.0, -5.0, -4.0, -2.0, -0.5, 0.0, 0.0, 0.0, 0.0, 0.0],
    ),
    preset(
        "treble_boost",
        [0.0, 0.0, 0.0, 0.0, 0.0, 0.5, 2.0, 3.5, 5.0, 6.0],
    ),
    preset(
        "vocal",
        [-2.0, -1.5, -1.0, 0.5, 2.0, 3.5, 3.5, 2.5, 1.0, 0.0],
    ),
    preset(
        "loudness",
        [6.0, 4.5, 2.0, 0.0, -1.0, -1.0, 0.0, 1.0, 3.0, 4.5],
    ),
    preset("pop", [-1.0, 0.0, 1.5, 3.0, 3.5, 2.5, 1.0, 0.0, -0.5, -1.0]),
    preset("rock", [4.5, 3.5, 2.0, 0.0, -1.5, -1.0, 1.0, 2.5, 3.5, 4.0]),
    preset("jazz", [3.0, 2.5, 1.0, 1.5, -1.0, -1.0, 0.0, 1.0, 2.5, 3.0]),
    preset(
        "classical",
        [3.5, 2.5, 1.5, 1.0, 0.0, 0.0, -0.5, 1.0, 2.0, 3.0],
    ),
    preset(
        "electronic",
        [5.0, 4.0, 1.5, 0.0, -1.5, 1.0, 0.5, 1.5, 3.5, 4.5],
    ),
    preset(
        "acoustic",
        [3.5, 3.0, 2.5, 1.0, 1.5, 1.5, 2.5, 3.0, 2.5, 1.5],
    ),
];

const fn preset(id: &'static str, gains_db: [f32; BANDS]) -> BuiltinPreset {
    BuiltinPreset { id, gains_db }
}

/// The settings as the decode threads read them: atomics only, no lock.
pub struct EqShared {
    enabled: AtomicBool,
    preamp_bits: AtomicU32,
    gain_bits: [AtomicU32; BANDS],
    /// Goes up with every change, so a reader knows when to look again.
    version: AtomicU64,
}

impl EqShared {
    pub fn new(settings: EqSettings) -> Arc<Self> {
        let shared = Arc::new(Self {
            enabled: AtomicBool::new(false),
            preamp_bits: AtomicU32::new(0),
            gain_bits: std::array::from_fn(|_| AtomicU32::new(0)),
            version: AtomicU64::new(0),
        });
        shared.set(settings);
        shared
    }

    pub fn set(&self, settings: EqSettings) {
        let settings = settings.sanitized();
        self.enabled.store(settings.enabled, Ordering::Relaxed);
        self.preamp_bits
            .store(settings.preamp_db.to_bits(), Ordering::Relaxed);
        for (slot, gain) in self.gain_bits.iter().zip(settings.gains_db) {
            slot.store(gain.to_bits(), Ordering::Relaxed);
        }
        self.version.fetch_add(1, Ordering::Release);
    }

    pub fn get(&self) -> EqSettings {
        EqSettings {
            enabled: self.enabled.load(Ordering::Relaxed),
            preamp_db: f32::from_bits(self.preamp_bits.load(Ordering::Relaxed)),
            gains_db: std::array::from_fn(|i| {
                f32::from_bits(self.gain_bits[i].load(Ordering::Relaxed))
            }),
        }
    }

    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }
}

/// One band: a peaking filter (the "Audio EQ Cookbook" form), normalised so that its
/// first feedback coefficient is 1.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
}

impl Biquad {
    /// `None` when the band cannot exist at this sample rate.
    fn peaking(rate: f64, hz: f64, gain_db: f64) -> Option<Self> {
        if hz >= rate * HIGHEST_BAND {
            return None;
        }
        let amp = 10f64.powf(gain_db / 40.0);
        let w = std::f64::consts::TAU * hz / rate;
        let alpha = w.sin() / (2.0 * BAND_Q);
        let cos = w.cos();
        // At 0 dB the numerator and the denominator are the same numbers: the filter
        // is then exactly "nothing", whatever is still ringing in it.
        let a0 = 1.0 + alpha / amp;
        Some(Self {
            b: [
                (1.0 + alpha * amp) / a0,
                -2.0 * cos / a0,
                (1.0 - alpha * amp) / a0,
            ],
            a: [-2.0 * cos / a0, (1.0 - alpha / amp) / a0],
        })
    }

    /// Gain of the filter at `hz`, in dB.
    fn response_db(&self, rate: f64, hz: f64) -> f64 {
        let w = std::f64::consts::TAU * hz / rate;
        let (sin1, cos1) = w.sin_cos();
        let (sin2, cos2) = (2.0 * w).sin_cos();
        let num_re = self.b[0] + self.b[1] * cos1 + self.b[2] * cos2;
        let num_im = -(self.b[1] * sin1 + self.b[2] * sin2);
        let den_re = 1.0 + self.a[0] * cos1 + self.a[1] * cos2;
        let den_im = -(self.a[0] * sin1 + self.a[1] * sin2);
        10.0 * ((num_re * num_re + num_im * num_im) / (den_re * den_re + den_im * den_im)).log10()
    }
}

/// What the equalizer does to a tone of `hz` at these settings, in dB (for the curve
/// that is drawn, and for the tests). 0 when it is switched off.
pub fn response_db(settings: &EqSettings, rate: u32, hz: f32) -> f32 {
    if !settings.is_active() {
        return 0.0;
    }
    let s = settings.sanitized();
    let rate = f64::from(rate.max(1));
    let bands: f64 = BAND_HZ
        .iter()
        .zip(s.gains_db)
        .filter_map(|(band, gain)| Biquad::peaking(rate, f64::from(*band), f64::from(gain)))
        .map(|filter| filter.response_db(rate, f64::from(hz)))
        .sum();
    (bands + f64::from(s.preamp_db)) as f32
}

/// The running equalizer of one feed (stereo, in place).
pub struct Equalizer {
    rate: f64,
    /// Where the gains are going: bands, then the pre-amp, in dB.
    target: [f64; BANDS + 1],
    /// Where they are now.
    current: [f64; BANDS + 1],
    filters: [Option<Biquad>; BANDS],
    /// Filter memory: per band, per channel, two values.
    state: [[[f64; 2]; 2]; BANDS],
    preamp: f64,
    /// The filters are in the path. Otherwise the samples pass untouched.
    engaged: bool,
    /// Frames processed since everything arrived at 0 dB.
    settled_frames: u64,
}

impl Equalizer {
    pub fn new(sample_rate: u32, settings: EqSettings) -> Self {
        let rate = f64::from(sample_rate.max(1));
        let target = settings.target();
        let mut eq = Self {
            rate,
            target,
            // A feed starts at its settings: there is nothing before it to glide from.
            current: target,
            filters: [None; BANDS],
            state: [[[0.0; 2]; 2]; BANDS],
            preamp: 1.0,
            engaged: target != [0.0; BANDS + 1],
            settled_frames: 0,
        };
        eq.update_filters();
        eq
    }

    /// New settings: the equalizer glides to them while it processes.
    pub fn set(&mut self, settings: EqSettings) {
        self.target = settings.target();
    }

    /// True while the samples pass through untouched.
    pub fn is_bypassed(&self) -> bool {
        !self.engaged && self.target == [0.0; BANDS + 1]
    }

    fn update_filters(&mut self) {
        for (band, filter) in self.filters.iter_mut().enumerate() {
            *filter = Biquad::peaking(self.rate, f64::from(BAND_HZ[band]), self.current[band]);
        }
        self.preamp = 10f64.powf(self.current[BANDS] / 20.0);
    }

    /// Moves the gains one step towards the target. Returns true if anything moved.
    fn glide(&mut self, frames: usize) -> bool {
        if self.current == self.target {
            return false;
        }
        let step = GLIDE_DB_PER_SECOND * frames as f64 / self.rate;
        for (now, target) in self.current.iter_mut().zip(self.target) {
            *now = if (target - *now).abs() <= step {
                target
            } else {
                *now + step.copysign(target - *now)
            };
        }
        self.update_filters();
        true
    }

    /// Equalizes interleaved stereo `samples` in place.
    pub fn process(&mut self, samples: &mut [f32]) {
        if !self.engaged {
            if self.target == [0.0; BANDS + 1] {
                return;
            }
            // Switched on: start from "nothing" with empty filters and glide in.
            self.engaged = true;
            self.settled_frames = 0;
            self.state = [[[0.0; 2]; 2]; BANDS];
        }
        for block in samples.chunks_mut(GLIDE_BLOCK * 2) {
            let frames = block.len() / 2;
            self.glide(frames);
            for frame in block.as_chunks_mut::<2>().0 {
                for (channel, sample) in frame.iter_mut().enumerate() {
                    let mut x = f64::from(*sample) * self.preamp;
                    for (filter, state) in self.filters.iter().zip(self.state.iter_mut()) {
                        let Some(f) = filter else { continue };
                        let s = &mut state[channel];
                        // Transposed direct form II.
                        let y = f.b[0] * x + s[0];
                        s[0] = f.b[1] * x - f.a[0] * y + s[1];
                        s[1] = f.b[2] * x - f.a[1] * y;
                        x = y;
                    }
                    *sample = x as f32;
                }
            }
            if self.current == [0.0; BANDS + 1] && self.target == self.current {
                self.settled_frames += frames as u64;
                if self.settled_frames as f64 >= SETTLE_SECONDS * self.rate {
                    // Back at 0 dB and rung out: out of the path. From the next
                    // block on the samples are the file's own again.
                    self.engaged = false;
                    return;
                }
            } else {
                self.settled_frames = 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, frames: usize, hz: f64, amplitude: f64) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let phase = i as f64 * hz * std::f64::consts::TAU / f64::from(rate);
                let s = (phase.sin() * amplitude) as f32;
                [s, s]
            })
            .collect()
    }

    /// Deterministic noise in −0.5..=0.5.
    fn noise(frames: usize, mut state: u32) -> Vec<f32> {
        (0..frames * 2)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state >> 8) as f32 / (1u32 << 24) as f32 - 0.5
            })
            .collect()
    }

    /// Amplitude of the tone of `hz` in the last half of `samples` (left channel).
    fn level(samples: &[f32], rate: u32, hz: f64) -> f64 {
        let left: Vec<f64> = samples
            .as_chunks::<2>()
            .0
            .iter()
            .skip(samples.len() / 4)
            .map(|f| f64::from(f[0]))
            .collect();
        let w = hz * std::f64::consts::TAU / f64::from(rate);
        let (mut a, mut b) = (0.0, 0.0);
        for (k, v) in left.iter().enumerate() {
            let (s, c) = (k as f64 * w).sin_cos();
            a += v * s;
            b += v * c;
        }
        2.0 * a.hypot(b) / left.len() as f64
    }

    fn settings(preamp_db: f32, gains_db: [f32; BANDS]) -> EqSettings {
        EqSettings {
            enabled: true,
            preamp_db,
            gains_db,
        }
    }

    fn one_band(band: usize, gain_db: f32) -> EqSettings {
        let mut gains = [0.0; BANDS];
        gains[band] = gain_db;
        settings(0.0, gains)
    }

    #[test]
    fn off_or_flat_passes_bit_for_bit() {
        let input = noise(20_000, 0x0E0E_0E0E);
        for flat in [
            EqSettings::default(),
            settings(0.0, [0.0; BANDS]),
            // Set, but switched off.
            EqSettings {
                enabled: false,
                ..settings(-3.0, [6.0; BANDS])
            },
        ] {
            assert!(!flat.is_active());
            let mut eq = Equalizer::new(44_100, flat);
            let mut out = input.clone();
            for chunk in out.chunks_mut(1000) {
                eq.process(chunk);
            }
            assert!(eq.is_bypassed());
            assert!(
                out.iter()
                    .zip(&input)
                    .all(|(a, b)| a.to_bits() == b.to_bits())
            );
        }
    }

    #[test]
    fn each_band_gives_its_gain_at_its_frequency() {
        for rate in [44_100, 96_000, 192_000] {
            for (band, hz) in BAND_HZ.iter().enumerate() {
                for gain in [-12.0, 6.0] {
                    let mut eq = Equalizer::new(rate, one_band(band, gain));
                    // Two seconds: time for the 32 Hz band to ring in.
                    let mut out = tone(rate, rate as usize * 2, f64::from(*hz), 0.1);
                    eq.process(&mut out);
                    let measured = 20.0 * (level(&out, rate, f64::from(*hz)) / 0.1).log10();
                    assert!(
                        (measured - f64::from(gain)).abs() < 0.05,
                        "{rate} Hz, band {hz} Hz at {gain} dB: measured {measured:.3} dB"
                    );
                }
            }
        }
    }

    #[test]
    fn a_band_leaves_what_is_far_from_it_alone() {
        // 1 kHz up by 12 dB: three octaves away it is within a quarter of a dB.
        let eq = one_band(5, 12.0);
        for far in [125.0, 8_000.0] {
            assert!(response_db(&eq, 44_100, far).abs() < 0.25, "{far} Hz");
        }
        // An octave away about a fifth is left.
        let near = response_db(&eq, 44_100, 2_000.0);
        assert!((1.5..4.0).contains(&near), "{near}");
    }

    #[test]
    fn the_drawn_curve_is_what_is_heard() {
        let rock = settings(-4.5, BUILTIN_PRESETS[7].gains_db);
        for rate in [44_100, 96_000] {
            for hz in [40.0, 100.0, 440.0, 1_500.0, 6_000.0, 12_000.0] {
                let mut eq = Equalizer::new(rate, rock);
                let mut out = tone(rate, rate as usize * 2, hz, 0.1);
                eq.process(&mut out);
                let measured = 20.0 * (level(&out, rate, hz) / 0.1).log10();
                let drawn = f64::from(response_db(&rock, rate, hz as f32));
                assert!(
                    (measured - drawn).abs() < 0.05,
                    "{rate} Hz at {hz} Hz: heard {measured:.3}, drawn {drawn:.3}"
                );
            }
        }
        assert_eq!(response_db(&EqSettings::default(), 44_100, 1_000.0), 0.0);
    }

    #[test]
    fn the_preamp_scales_everything() {
        let mut eq = Equalizer::new(48_000, settings(-6.0, [0.0; BANDS]));
        let mut out = tone(48_000, 48_000, 1_000.0, 0.5);
        eq.process(&mut out);
        let measured = 20.0 * (level(&out, 48_000, 1_000.0) / 0.5).log10();
        assert!((measured + 6.0).abs() < 0.01, "{measured}");
    }

    #[test]
    fn a_band_above_half_the_sample_rate_is_left_out() {
        // 16 kHz cannot be built at 32 kHz; the filter must not blow up trying.
        let mut eq = Equalizer::new(32_000, one_band(9, 12.0));
        let mut out = tone(32_000, 32_000, 1_000.0, 0.5);
        eq.process(&mut out);
        assert!(out.iter().all(|s| s.is_finite() && s.abs() <= 0.51));
        assert_eq!(response_db(&one_band(9, 12.0), 32_000, 1_000.0), 0.0);
    }

    #[test]
    fn changing_the_settings_never_jumps() {
        // A steady low tone while a band is thrown from one end to the other and the
        // equalizer is switched off: no step between neighbouring samples is larger
        // than the tone's own steepest step would be at the loudest setting.
        let rate = 44_100;
        let mut eq = Equalizer::new(rate, EqSettings::default());
        let mut out = tone(rate, rate as usize * 3, 64.0, 0.2);
        let quarter = out.len() / 6; // half a second
        let (a, rest) = out.split_at_mut(quarter);
        let (b, rest) = rest.split_at_mut(quarter);
        let (c, d) = rest.split_at_mut(quarter);
        eq.process(a);
        eq.set(one_band(1, 12.0));
        eq.process(b);
        eq.set(one_band(1, -12.0));
        eq.process(c);
        eq.set(EqSettings::default());
        eq.process(d);
        let steepest = 0.2 * 4.0 * std::f32::consts::TAU * 64.0 / rate as f32;
        let largest = out
            .as_chunks::<2>()
            .0
            .windows(2)
            .fold(0f32, |m, w| m.max((w[1][0] - w[0][0]).abs()));
        assert!(largest <= steepest * 1.05, "{largest} > {steepest}");
        // Once the gains are back at 0 dB and half a second has passed, it is out of
        // the path: from then on the samples are the input again, exactly.
        assert!(eq.is_bypassed());
        let input = tone(rate, rate as usize * 3, 64.0, 0.2);
        let tail = out.len() - rate as usize; // the last half second
        assert!(out[tail..].iter().zip(&input[tail..]).all(|(a, b)| a == b));
    }

    #[test]
    fn the_size_of_the_blocks_does_not_change_the_result() {
        let input = noise(30_000, 0xFACE_B00C);
        let run = |chunk: usize| {
            let mut eq = Equalizer::new(48_000, settings(-2.0, BUILTIN_PRESETS[9].gains_db));
            let mut out = input.clone();
            // Whole glide blocks, so that the gliding steps fall on the same frames.
            for part in out.chunks_mut(chunk * GLIDE_BLOCK * 2) {
                eq.process(part);
            }
            out
        };
        assert_eq!(run(1), run(40));
        assert_eq!(run(7), run(1000));
    }

    #[test]
    fn settings_out_of_range_are_brought_back() {
        let wild = EqSettings {
            enabled: true,
            preamp_db: f32::NAN,
            gains_db: [
                99.0,
                -99.0,
                f32::INFINITY,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
            ],
        }
        .sanitized();
        assert_eq!(wild.preamp_db, 0.0);
        assert_eq!(wild.gains_db[..3], [12.0, -12.0, 0.0]);
        let shared = EqShared::new(wild);
        let before = shared.version();
        shared.set(one_band(3, 40.0));
        assert_eq!(shared.get().gains_db[3], 12.0);
        assert!(shared.get().enabled);
        assert!(shared.version() > before);
    }

    #[test]
    fn the_presets_that_come_with_the_app_are_sane() {
        let mut ids = std::collections::HashSet::new();
        for preset in BUILTIN_PRESETS {
            assert!(ids.insert(preset.id), "{} twice", preset.id);
            assert!(
                preset.gains_db.iter().all(|g| g.abs() <= 6.0),
                "{}",
                preset.id
            );
            // With its pre-amp no frequency ends up above where it was.
            let s = settings(preset.preamp_db(), preset.gains_db);
            let highest = (0..200)
                .map(|i| 20.0 * 1000f32.powf(i as f32 / 199.0))
                .map(|hz| response_db(&s, 44_100, hz))
                .fold(f32::MIN, f32::max);
            assert!(highest <= 1.5, "{}: {highest} dB", preset.id);
        }
        assert_eq!(BUILTIN_PRESETS[0].id, "flat");
        assert!(!settings(BUILTIN_PRESETS[0].preamp_db(), BUILTIN_PRESETS[0].gains_db).is_active());
    }
}
