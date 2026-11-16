//! Parametric correction: a set of filters, each with its own frequency, gain and
//! width, as published for headphones (and measured for rooms).
//!
//! The ten-band equalizer is for taste; this is for making a particular headphone play
//! flat (or to a target), which takes filters placed where that headphone deviates. The
//! filters come from a file in the format everyone exchanges them in — Equalizer APO's,
//! which AutoEq writes as `ParametricEQ.txt`:
//!
//! ```text
//! Preamp: -6.2 dB
//! Filter 1: ON PK Fc 105 Hz Gain -3.5 dB Q 0.70
//! Filter 2: ON LSC Fc 105 Hz Gain 5.0 dB Q 0.70
//! Filter 3: ON HSC Fc 10000 Hz Gain -2.0 dB Q 0.70
//! ```
//!
//! `PK` is a peaking filter, `LSC` and `HSC` are low and high shelves around their centre
//! frequency; all three are the biquads of the Audio EQ Cookbook. Filter types this
//! module does not implement are refused when the file is read: a correction applied
//! in part would be a different correction.
//!
//! With no profile set the stage is out of the path (the samples pass bit for bit).
//! Changing the profile cross-fades from the old sound to the new.
use serde::{Deserialize, Serialize};
use std::f64::consts::PI;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Most filters a profile may have (published corrections use ten or fewer).
pub const MAX_FILTERS: usize = 32;
/// Longest name of a profile.
pub const MAX_NAME_CHARS: usize = 60;
/// Largest file that is read as a profile.
pub const MAX_FILE_BYTES: u64 = 64 * 1024;
const GAIN_LIMIT_DB: f32 = 30.0;
/// The most a profile may make any frequency louder, preamp and filters together. A
/// correction lowers more than it lifts (its preamp makes room for its boosts); this
/// leaves room for one that does not, and stops a file that is no correction at all
/// from driving everything into the limiter.
const MAX_NET_BOOST_DB: f32 = 12.0;
/// The sample rates the limit is worked out at (the response differs a little by rate).
const BOUND_RATES: [u32; 4] = [44_100, 48_000, 96_000, 192_000];
/// How much of an unknown filter type's name is kept for the message about it.
const MAX_KIND_CHARS: usize = 12;
const FADE_SECONDS: f64 = 0.04;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeqKind {
    Peak,
    LowShelf,
    HighShelf,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PeqFilter {
    pub kind: PeqKind,
    pub hz: f32,
    pub gain_db: f32,
    pub q: f32,
}

/// A correction: a gain for headroom and the filters, in the order of the file.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PeqProfile {
    pub name: String,
    pub preamp_db: f32,
    pub filters: Vec<PeqFilter>,
}

/// Why a file could not be read as a profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeqError {
    /// A filter of a type that is not implemented (its name as written).
    UnsupportedFilter(String),
    /// A `Filter` line that cannot be understood (the line).
    Malformed(String),
    /// More filters than a profile may have.
    TooManyFilters,
    /// Neither a preamp nor a filter in the file.
    Empty,
}

/// Corrections for well-known headphones that come with the app: AutoEq's results for
/// measurements by oratory1990, and one computed with AutoEq from a measurement by
/// Super* Review (see the head of the data file for sources and licence).
const BUILTIN_TEXT: &str = include_str!("../../assets/corrections/headphones.txt");

/// The profiles that come with the app, in the order of the data file.
pub fn builtin_profiles() -> &'static [PeqProfile] {
    static PROFILES: std::sync::OnceLock<Vec<PeqProfile>> = std::sync::OnceLock::new();
    PROFILES.get_or_init(|| {
        builtin_sections(BUILTIN_TEXT)
            .into_iter()
            .filter_map(|(name, text)| PeqProfile::parse(name, &text).ok())
            .collect()
    })
}

/// The data file cut into `(name, text of the profile)` at its "## name" lines.
fn builtin_sections(text: &str) -> Vec<(&str, String)> {
    let mut sections: Vec<(&str, String)> = Vec::new();
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("## ") {
            sections.push((name.trim(), String::new()));
        } else if let Some((_, body)) = sections.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    sections
}

/// The number that follows `label` on a line of words ("Fc 105 Hz": 105).
fn number_after(words: &[&str], label: &str) -> Option<f32> {
    let at = words.iter().position(|w| w.eq_ignore_ascii_case(label))?;
    words
        .get(at + 1)?
        .parse::<f32>()
        .ok()
        .filter(|v| v.is_finite())
}

impl PeqProfile {
    /// Reads the text of a profile file. Lines that are neither a preamp nor a filter
    /// (comments, blank lines) are passed over, and so are filters switched `OFF`.
    pub fn parse(name: &str, text: &str) -> Result<Self, PeqError> {
        let mut profile = PeqProfile {
            name: name.to_string(),
            ..Default::default()
        };
        let mut said_something = false;
        for line in text.lines() {
            let line = line.trim().trim_start_matches('\u{feff}');
            let Some((head, rest)) = line.split_once(':') else {
                continue;
            };
            let head = head.trim().to_ascii_lowercase();
            let words: Vec<&str> = rest.split_whitespace().collect();
            if head == "preamp" {
                let gain = words
                    .first()
                    .and_then(|w| w.parse::<f32>().ok())
                    .filter(|v| v.is_finite())
                    .ok_or_else(|| PeqError::Malformed(line.to_string()))?;
                profile.preamp_db = gain;
                said_something = true;
            } else if head == "filter" || head.starts_with("filter ") {
                let malformed = || PeqError::Malformed(line.to_string());
                match words.first() {
                    Some(state) if state.eq_ignore_ascii_case("on") => {}
                    Some(state) if state.eq_ignore_ascii_case("off") => continue,
                    _ => return Err(malformed()),
                }
                let kind = match words.get(1).map(|w| w.to_ascii_uppercase()).as_deref() {
                    Some("PK" | "PEQ") => PeqKind::Peak,
                    Some("LSC") => PeqKind::LowShelf,
                    Some("HSC") => PeqKind::HighShelf,
                    Some(other) => {
                        let shown = other.chars().take(MAX_KIND_CHARS).collect();
                        return Err(PeqError::UnsupportedFilter(shown));
                    }
                    None => return Err(malformed()),
                };
                let filter = PeqFilter {
                    kind,
                    hz: number_after(&words, "Fc").ok_or_else(malformed)?,
                    gain_db: number_after(&words, "Gain").ok_or_else(malformed)?,
                    // A shelf written with a slope instead of a Q is another filter.
                    q: number_after(&words, "Q").ok_or_else(malformed)?,
                };
                if !(filter.hz > 0.0 && filter.q > 0.0) {
                    return Err(malformed());
                }
                if profile.filters.len() == MAX_FILTERS {
                    return Err(PeqError::TooManyFilters);
                }
                profile.filters.push(filter);
                said_something = true;
            }
        }
        if !said_something {
            return Err(PeqError::Empty);
        }
        Ok(profile.sanitized())
    }

    /// The profile as it is used: values a hand-edited settings file can hold but a
    /// profile file cannot are brought into range.
    pub fn sanitized(mut self) -> Self {
        let finite = |v: f32, fallback: f32| if v.is_finite() { v } else { fallback };
        self.name = self
            .name
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_NAME_CHARS)
            .collect::<String>()
            .trim()
            .to_string();
        self.preamp_db = finite(self.preamp_db, 0.0).clamp(-GAIN_LIMIT_DB, MAX_NET_BOOST_DB);
        self.filters.truncate(MAX_FILTERS);
        for f in &mut self.filters {
            f.hz = finite(f.hz, 1_000.0).clamp(10.0, 24_000.0);
            f.gain_db = finite(f.gain_db, 0.0).clamp(-GAIN_LIMIT_DB, GAIN_LIMIT_DB);
            f.q = finite(f.q, 0.707).clamp(0.05, 30.0);
        }
        // Filters add up: each within its limit, thirty-two of them on one frequency
        // are hundreds of decibels. Whatever the filters do together, nothing comes out
        // more than the limit louder than it went in: the preamp gives way.
        let boost = BOUND_RATES
            .into_iter()
            .map(|rate| self.boost_db(rate))
            .fold(0.0, f64::max);
        self.preamp_db = self.preamp_db.min(MAX_NET_BOOST_DB - boost as f32);
        self
    }

    /// True if the profile changes nothing.
    pub fn is_flat(&self) -> bool {
        self.preamp_db == 0.0 && self.filters.iter().all(|f| f.gain_db == 0.0)
    }

    /// The largest boost anywhere in the response, preamp included, in dB (what the
    /// preamp has to make room for).
    pub fn peak_db(&self, rate: u32) -> f32 {
        (f64::from(self.preamp_db) + self.boost_db(rate)) as f32
    }

    /// The largest gain of the filters together (without the preamp) anywhere from
    /// 20 Hz to 20 kHz, in dB. Looked for on a fine grid and at every filter's own
    /// frequency, where a narrow filter has its peak.
    fn boost_db(&self, rate: u32) -> f64 {
        let rate = f64::from(rate.max(1));
        let biquads: Vec<Biquad> = self
            .filters
            .iter()
            .filter_map(|f| Biquad::design(f, rate))
            .collect();
        if biquads.is_empty() {
            return 0.0;
        }
        let grid = (0..=240).map(|i| 20.0 * 1_000f64.powf(f64::from(i) / 240.0));
        let centres = self.filters.iter().map(|f| f64::from(f.hz));
        grid.chain(centres)
            .filter(|hz| *hz < rate / 2.0)
            .map(|hz| {
                let w = 2.0 * PI * hz / rate;
                biquads.iter().map(|b| b.gain_db(w)).sum::<f64>()
            })
            .fold(f64::MIN, f64::max)
    }

    /// Gain of the whole profile at `hz`, in dB.
    pub fn response_db(&self, rate: u32, hz: f32) -> f32 {
        let rate = f64::from(rate.max(1));
        let w = 2.0 * PI * f64::from(hz) / rate;
        let mut db = f64::from(self.preamp_db);
        for filter in &self.filters {
            if let Some(biquad) = Biquad::design(filter, rate) {
                db += biquad.gain_db(w);
            }
        }
        db as f32
    }
}

#[derive(Debug, Clone, Copy)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
}

impl Biquad {
    /// The Audio EQ Cookbook's filter for `f` at `rate`. `None` for a filter that sits
    /// too close to (or above) half the sample rate to be realised sensibly, and for
    /// one that does nothing.
    fn design(f: &PeqFilter, rate: f64) -> Option<Self> {
        let hz = f64::from(f.hz);
        if f.gain_db == 0.0 || hz >= rate * 0.45 {
            return None;
        }
        let a = 10f64.powf(f64::from(f.gain_db) / 40.0);
        let w0 = 2.0 * PI * hz / rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * f64::from(f.q));
        let (b, a0, a1, a2) = match f.kind {
            PeqKind::Peak => (
                [1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a],
                1.0 + alpha / a,
                -2.0 * cos,
                1.0 - alpha / a,
            ),
            PeqKind::LowShelf => {
                let k = 2.0 * a.sqrt() * alpha;
                (
                    [
                        a * ((a + 1.0) - (a - 1.0) * cos + k),
                        2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                        a * ((a + 1.0) - (a - 1.0) * cos - k),
                    ],
                    (a + 1.0) + (a - 1.0) * cos + k,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                    (a + 1.0) + (a - 1.0) * cos - k,
                )
            }
            PeqKind::HighShelf => {
                let k = 2.0 * a.sqrt() * alpha;
                (
                    [
                        a * ((a + 1.0) + (a - 1.0) * cos + k),
                        -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                        a * ((a + 1.0) + (a - 1.0) * cos - k),
                    ],
                    (a + 1.0) - (a - 1.0) * cos + k,
                    2.0 * ((a - 1.0) - (a + 1.0) * cos),
                    (a + 1.0) - (a - 1.0) * cos - k,
                )
            }
        };
        Some(Biquad {
            b: [b[0] / a0, b[1] / a0, b[2] / a0],
            a: [a1 / a0, a2 / a0],
        })
    }

    /// Gain at the angular frequency `w` (radians per sample), in dB.
    fn gain_db(&self, w: f64) -> f64 {
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        let num_re = self.b[0] + self.b[1] * c1 + self.b[2] * c2;
        let num_im = -(self.b[1] * s1 + self.b[2] * s2);
        let den_re = 1.0 + self.a[0] * c1 + self.a[1] * c2;
        let den_im = -(self.a[0] * s1 + self.a[1] * s2);
        let power = (num_re * num_re + num_im * num_im) / (den_re * den_re + den_im * den_im);
        10.0 * power.max(1e-30).log10()
    }
}

/// A profile realised for one sample rate, with its filter memory.
struct Chain {
    gain: f64,
    filters: Vec<Biquad>,
    /// Per filter and channel (transposed direct form II).
    state: Vec<[[f64; 2]; 2]>,
}

impl Chain {
    fn new(profile: &PeqProfile, rate: f64) -> Self {
        let filters: Vec<Biquad> = profile
            .filters
            .iter()
            .filter_map(|f| Biquad::design(f, rate))
            .collect();
        Chain {
            gain: 10f64.powf(f64::from(profile.preamp_db) / 20.0),
            state: vec![[[0.0; 2]; 2]; filters.len()],
            filters,
        }
    }

    #[inline]
    fn run(&mut self, channel: usize, sample: f64) -> f64 {
        let mut x = sample * self.gain;
        for (biquad, state) in self.filters.iter().zip(&mut self.state) {
            let s = &mut state[channel];
            let y = biquad.b[0] * x + s[0];
            s[0] = biquad.b[1] * x - biquad.a[0] * y + s[1];
            s[1] = biquad.b[2] * x - biquad.a[1] * y;
            x = y;
        }
        x
    }
}

/// The profile that is set, shared between the thread that sets it and the decode
/// thread that applies it.
pub struct PeqShared {
    profile: Mutex<Option<Arc<PeqProfile>>>,
    version: AtomicU64,
}

impl PeqShared {
    pub fn new(profile: Option<PeqProfile>) -> Arc<Self> {
        Arc::new(PeqShared {
            profile: Mutex::new(profile.map(Arc::new)),
            version: AtomicU64::new(0),
        })
    }

    pub fn set(&self, profile: Option<PeqProfile>) {
        let profile = profile.map(|p| Arc::new(p.sanitized()));
        match self.profile.lock() {
            Ok(mut slot) => *slot = profile,
            Err(poisoned) => *poisoned.into_inner() = profile,
        }
        self.version.fetch_add(1, Ordering::Release);
    }

    pub fn get(&self) -> Option<Arc<PeqProfile>> {
        match self.profile.lock() {
            Ok(slot) => slot.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }
}

/// Applies a profile to interleaved stereo audio.
pub struct Corrector {
    rate: f64,
    /// What is set (`None`: nothing, the samples pass).
    current: Option<Chain>,
    /// What was set before, while the change is being cross-faded.
    previous: Option<Chain>,
    /// Share of `current` in the output during a cross-fade (1 = all of it).
    mix: f64,
    fade_step: f64,
    /// A change that came while another was being faded: made when that fade is over
    /// (`Some(None)`: switch off).
    pending: Option<Option<Chain>>,
}

impl Corrector {
    pub fn new(sample_rate: u32, profile: Option<&PeqProfile>) -> Self {
        let rate = f64::from(sample_rate.max(1));
        Corrector {
            rate,
            current: profile
                .filter(|p| !p.is_flat())
                .map(|p| Chain::new(p, rate)),
            previous: None,
            // A feed starts at its setting: there is nothing before it to fade from.
            mix: 1.0,
            fade_step: 1.0 / (FADE_SECONDS * rate),
            pending: None,
        }
    }

    /// Sets another profile (or none). What plays is cross-faded to it.
    pub fn set(&mut self, profile: Option<&PeqProfile>) {
        let next = profile
            .filter(|p| !p.is_flat())
            .map(|p| Chain::new(p, self.rate));
        if self.mix < 1.0 {
            // A fade started over from where this one has got to would jump: the new
            // setting waits for it to end (40 ms at most), the latest one winning.
            self.pending = Some(next);
            return;
        }
        self.fade_to(next);
    }

    fn fade_to(&mut self, next: Option<Chain>) {
        // Outside a fade there is no `previous`: nothing to fade from or to.
        if next.is_none() && self.current.is_none() {
            return;
        }
        self.previous = self.current.take();
        self.current = next;
        self.mix = 0.0;
    }

    /// True while the samples pass through untouched.
    pub fn is_bypassed(&self) -> bool {
        self.current.is_none() && self.previous.is_none()
    }

    pub fn process(&mut self, samples: &mut [f32]) {
        if self.is_bypassed() {
            return;
        }
        for frame in samples.as_chunks_mut::<2>().0 {
            let fading = self.mix < 1.0;
            for (channel, sample) in frame.iter_mut().enumerate() {
                let dry = f64::from(*sample);
                let new = match &mut self.current {
                    Some(chain) => chain.run(channel, dry),
                    None => dry,
                };
                let out = if fading {
                    let old = match &mut self.previous {
                        Some(chain) => chain.run(channel, dry),
                        None => dry,
                    };
                    old + (new - old) * self.mix
                } else {
                    new
                };
                *sample = out as f32;
            }
            if fading {
                self.mix = (self.mix + self.fade_step).min(1.0);
                if self.mix >= 1.0 {
                    self.previous = None;
                    if let Some(next) = self.pending.take() {
                        self.fade_to(next);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AUTOEQ: &str = "Preamp: -6.2 dB\n\
        Filter 1: ON LSC Fc 105 Hz Gain 5.5 dB Q 0.70\n\
        Filter 2: ON PK Fc 200 Hz Gain -3.4 dB Q 0.55\n\
        Filter 3: ON PK Fc 2900 Hz Gain 4.1 dB Q 2.10\n\
        Filter 4: ON HSC Fc 10000 Hz Gain -2.5 dB Q 0.70\n";

    fn sine(rate: u32, hz: f64, seconds: f64, amplitude: f64) -> Vec<f32> {
        let frames = (f64::from(rate) * seconds) as usize;
        (0..frames)
            .flat_map(|n| {
                let s = (amplitude * (2.0 * PI * hz * n as f64 / f64::from(rate)).sin()) as f32;
                [s, s]
            })
            .collect()
    }

    fn level_db(samples: &[f32]) -> f64 {
        let tail = &samples[samples.len() / 2..];
        let mean = tail.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / tail.len() as f64;
        10.0 * mean.log10()
    }

    #[test]
    fn a_published_profile_is_read() {
        let p = PeqProfile::parse("HD 600", AUTOEQ).expect("parses");
        assert_eq!(p.name, "HD 600");
        assert_eq!(p.preamp_db, -6.2);
        assert_eq!(p.filters.len(), 4);
        assert_eq!(
            p.filters[0],
            PeqFilter {
                kind: PeqKind::LowShelf,
                hz: 105.0,
                gain_db: 5.5,
                q: 0.70
            }
        );
        assert_eq!(p.filters[2].kind, PeqKind::Peak);
        assert_eq!(p.filters[3].kind, PeqKind::HighShelf);
        // Windows line ends, a byte order mark, comments, lower case, a filter that is
        // switched off, and Equalizer APO's unnumbered form.
        let other = "\u{feff}# my headphone\r\npreamp: -3 dB\r\n\r\nfilter: on pk fc 1000 hz gain 2 db q 1.41\r\nFilter 2: OFF PK Fc 50 Hz Gain 9 dB Q 1\r\n";
        let p = PeqProfile::parse("x", other).expect("parses");
        assert_eq!(p.preamp_db, -3.0);
        assert_eq!(p.filters.len(), 1);
        assert_eq!(p.filters[0].hz, 1_000.0);
        // A preamp alone is a profile; so is a filter alone.
        assert!(PeqProfile::parse("x", "Preamp: -2 dB").is_ok());
        assert_eq!(
            PeqProfile::parse("x", "Filter 1: ON PK Fc 100 Hz Gain 1 dB Q 1")
                .expect("parses")
                .preamp_db,
            0.0
        );
    }

    #[test]
    fn what_is_not_understood_is_refused_not_guessed() {
        // Filter types that are not implemented: the whole file is refused.
        for kind in ["LP", "HP", "LS", "HS", "NO", "AP", "BP", "LPQ"] {
            let text = format!("Filter 1: ON {kind} Fc 100 Hz Gain 3 dB Q 0.7");
            assert_eq!(
                PeqProfile::parse("x", &text),
                Err(PeqError::UnsupportedFilter(kind.to_string())),
                "{kind}"
            );
        }
        // A shelf given by its slope is another filter than one given by its Q.
        assert!(matches!(
            PeqProfile::parse("x", "Filter 1: ON LSC 12 dB Fc 100 Hz Gain 3 dB"),
            Err(PeqError::Malformed(_))
        ));
        for bad in [
            "Filter 1: ON PK Fc abc Hz Gain 3 dB Q 1",
            "Filter 1: ON PK Fc 100 Hz Q 1",
            "Filter 1: ON PK Fc 100 Hz Gain 3 dB Q 0",
            "Filter 1: ON PK Fc -5 Hz Gain 3 dB Q 1",
            "Filter 1: MAYBE PK Fc 100 Hz Gain 3 dB Q 1",
            "Filter 1: ON",
            "Preamp: loud",
            "Filter 1: ON PK Fc NaN Hz Gain 3 dB Q 1",
        ] {
            assert!(
                matches!(PeqProfile::parse("x", bad), Err(PeqError::Malformed(_))),
                "{bad}"
            );
        }
        assert_eq!(PeqProfile::parse("x", ""), Err(PeqError::Empty));
        assert_eq!(
            PeqProfile::parse("x", "just some text\nwithout filters"),
            Err(PeqError::Empty)
        );
        let many: String = (0..=MAX_FILTERS)
            .map(|i| format!("Filter {i}: ON PK Fc 100 Hz Gain 1 dB Q 1\n"))
            .collect();
        assert_eq!(PeqProfile::parse("x", &many), Err(PeqError::TooManyFilters));
    }

    #[test]
    fn values_out_of_range_are_brought_in() {
        let wild = PeqProfile {
            name: "  a\nname ".repeat(20),
            preamp_db: f32::NAN,
            filters: vec![
                PeqFilter {
                    kind: PeqKind::Peak,
                    hz: 1e9,
                    gain_db: 500.0,
                    q: 0.0,
                };
                MAX_FILTERS + 5
            ],
        }
        .sanitized();
        assert!(wild.name.chars().count() <= MAX_NAME_CHARS && !wild.name.contains('\n'));
        // Not a number became 0 dB, and then gave way to what the filters add up to.
        assert!(wild.preamp_db.is_finite() && wild.preamp_db < 0.0);
        assert!(wild.peak_db(192_000) <= MAX_NET_BOOST_DB + 0.01);
        assert_eq!(wild.filters.len(), MAX_FILTERS);
        assert_eq!(
            (
                wild.filters[0].hz,
                wild.filters[0].gain_db,
                wild.filters[0].q
            ),
            (24_000.0, 30.0, 0.05)
        );
    }

    /// The filters are what their numbers say: a peak has its gain at its frequency
    /// and half of it (in dB) at the edges of its width; a shelf has half its gain at
    /// its centre and all of it far beyond.
    #[test]
    fn the_filters_are_the_cookbooks() {
        for rate in [44_100, 48_000, 96_000, 192_000] {
            let peak = PeqProfile {
                filters: vec![PeqFilter {
                    kind: PeqKind::Peak,
                    hz: 1_000.0,
                    gain_db: 6.0,
                    q: 1.41,
                }],
                ..Default::default()
            };
            assert!(
                (peak.response_db(rate, 1_000.0) - 6.0).abs() < 0.01,
                "{rate}"
            );
            assert!(peak.response_db(rate, 50.0).abs() < 0.05);
            assert!(peak.response_db(rate, 15_000.0).abs() < 0.2);
            // Bandwidth: between the two half-gain points lies f0 / Q.
            // Walk outwards from the centre until the gain has fallen to half.
            let half = |step: f32| {
                let mut hz = 1_000.0;
                while peak.response_db(rate, hz) > 3.0 {
                    hz += step;
                }
                hz
            };
            let width = half(0.05) - half(-0.05);
            // (bilinear warping narrows it a little at the lower rates)
            assert!((width - 1_000.0 / 1.41).abs() < 25.0, "{rate}: {width}");

            let low = PeqProfile {
                filters: vec![PeqFilter {
                    kind: PeqKind::LowShelf,
                    hz: 105.0,
                    gain_db: 5.5,
                    q: 0.7,
                }],
                ..Default::default()
            };
            assert!((low.response_db(rate, 20.0) - 5.5).abs() < 0.25, "{rate}");
            assert!((low.response_db(rate, 105.0) - 2.75).abs() < 0.05, "{rate}");
            assert!(low.response_db(rate, 5_000.0).abs() < 0.02);

            let high = PeqProfile {
                filters: vec![PeqFilter {
                    kind: PeqKind::HighShelf,
                    hz: 8_000.0,
                    gain_db: -4.0,
                    q: 0.7,
                }],
                ..Default::default()
            };
            assert!(
                (high.response_db(rate, 8_000.0) + 2.0).abs() < 0.05,
                "{rate}"
            );
            assert!(high.response_db(rate, 100.0).abs() < 0.02);
            if rate >= 96_000 {
                assert!(
                    (high.response_db(rate, 40_000.0) + 4.0).abs() < 0.3,
                    "{rate}"
                );
            }
        }
    }

    /// What is computed is what is heard: tones through the stage come out at the
    /// level the response says, at every sample rate.
    #[test]
    fn the_sound_follows_the_response() {
        let profile = PeqProfile::parse("x", AUTOEQ).expect("parses");
        for rate in [44_100u32, 48_000, 96_000] {
            for hz in [40.0, 105.0, 200.0, 1_000.0, 2_900.0, 8_000.0, 14_000.0] {
                let mut audio = sine(rate, hz, 1.0, 0.1);
                let before = level_db(&audio);
                let mut corrector = Corrector::new(rate, Some(&profile));
                assert!(!corrector.is_bypassed());
                for block in audio.chunks_mut(2 * 517) {
                    corrector.process(block);
                }
                let change = level_db(&audio) - before;
                let expected = f64::from(profile.response_db(rate, hz as f32));
                assert!(
                    (change - expected).abs() < 0.05,
                    "{rate} Hz, {hz} Hz: {change:.3} dB, expected {expected:.3}"
                );
            }
        }
        // The preamp makes the room the boosts need: nothing in this profile ends up
        // above where it started.
        assert!(
            profile.peak_db(48_000) <= 0.0,
            "{}",
            profile.peak_db(48_000)
        );
    }

    #[test]
    fn without_a_profile_the_samples_pass_bit_for_bit() {
        let original = sine(44_100, 997.0, 0.2, 0.9);
        for profile in [None, Some(PeqProfile::default())] {
            let mut audio = original.clone();
            let mut corrector = Corrector::new(44_100, profile.as_ref());
            assert!(corrector.is_bypassed());
            corrector.process(&mut audio);
            assert_eq!(audio, original);
        }
        // A filter above what the sample rate can carry is left out, not bent.
        let high = PeqProfile {
            filters: vec![PeqFilter {
                kind: PeqKind::Peak,
                hz: 21_000.0,
                gain_db: 6.0,
                q: 1.0,
            }],
            ..Default::default()
        };
        assert_eq!(high.response_db(44_100, 10_000.0), 0.0);
        assert!(high.response_db(96_000, 21_000.0) > 5.9);
    }

    /// Switching a profile on, off or to another one never jumps: the output moves from
    /// the old sound to the new over a few hundredths of a second.
    #[test]
    fn changes_are_cross_faded() {
        let rate = 48_000;
        let loud = PeqProfile {
            preamp_db: -12.0,
            ..Default::default()
        };
        let mut audio = sine(rate, 100.0, 0.5, 0.5);
        let mut corrector = Corrector::new(rate, None);
        let (first, rest) = audio.split_at_mut(48_000 / 4 * 2);
        corrector.process(first);
        corrector.set(Some(&loud));
        assert!(!corrector.is_bypassed());
        corrector.process(rest);
        // No step larger than the tone's own slope (0.5·2π·100/48000 ≈ 0.0065).
        let max_step = audio
            .as_chunks::<2>()
            .0
            .windows(2)
            .fold(0f32, |m, w| m.max((w[1][0] - w[0][0]).abs()));
        assert!(max_step < 0.008, "{max_step}");
        // After the fade: 12 dB down.
        let end = &audio[audio.len() - 9_600..];
        let peak = end.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!(
            (peak - 0.5 * 10f32.powf(-12.0 / 20.0)).abs() < 0.002,
            "{peak}"
        );
        // Switched off again: back to the samples as they come.
        corrector.set(None);
        let mut more = sine(rate, 100.0, 0.2, 0.5);
        let untouched = more.clone();
        corrector.process(&mut more);
        assert!(corrector.is_bypassed());
        assert_eq!(more[more.len() - 100..], untouched[untouched.len() - 100..]);
    }

    #[test]
    fn the_shared_profile_is_versioned() {
        let shared = PeqShared::new(None);
        assert!(shared.get().is_none());
        let before = shared.version();
        shared.set(Some(PeqProfile::parse("x", AUTOEQ).expect("parses")));
        assert!(shared.version() > before);
        assert_eq!(shared.get().expect("set").filters.len(), 4);
        shared.set(None);
        assert!(shared.get().is_none());
    }

    /// The profiles that come with the app are what the source says: every one of them
    /// parses in full, and the count and sum of its numbers are those recorded from the
    /// source when the values were taken over (a mistyped digit would change them).
    #[test]
    fn the_builtin_profiles_are_what_the_source_says() {
        let sections = builtin_sections(BUILTIN_TEXT);
        assert_eq!(sections.len(), 35);
        assert_eq!(builtin_profiles().len(), sections.len(), "every one parses");
        let mut names = std::collections::HashSet::new();
        for ((name, text), profile) in sections.iter().zip(builtin_profiles()) {
            assert!(names.insert(*name), "{name} appears twice");
            assert_eq!(&profile.name, name);
            // The numbers of the profile's own lines, as in the source file.
            let mut numbers: Vec<f64> = Vec::new();
            let mut recorded = None;
            for line in text.lines() {
                if let Some(check) = line.strip_prefix("# check ") {
                    let mut parts = check.split_whitespace();
                    recorded = Some((
                        parts.next().and_then(|n| n.parse::<usize>().ok()),
                        parts.next().and_then(|n| n.parse::<f64>().ok()),
                    ));
                    continue;
                }
                if line.starts_with('#') {
                    continue;
                }
                let mut token = String::new();
                for c in line.chars().chain(std::iter::once(' ')) {
                    let continues = c.is_ascii_digit()
                        || (c == '.' && token.chars().any(|t| t.is_ascii_digit()))
                        || (c == '-' && token.is_empty());
                    if continues {
                        token.push(c);
                    } else {
                        if let Ok(value) = token.parse::<f64>() {
                            numbers.push(value);
                        }
                        token.clear();
                    }
                }
            }
            let (count, sum) = recorded.unwrap_or_else(|| panic!("{name}: no check line"));
            assert_eq!(Some(numbers.len()), count, "{name}: count of numbers");
            let total: f64 = numbers.iter().sum();
            assert!(
                (total - sum.expect("a sum")).abs() < 0.005,
                "{name}: sum of numbers {total}"
            );
            // Nothing had to be brought into range, and the preamp makes the room the
            // boosts need (AutoEq computes it that way): at most a fraction of a dB over.
            assert!(!profile.filters.is_empty() && profile.filters.len() <= 10);
            assert!(profile.preamp_db < 0.0, "{name}");
            for f in &profile.filters {
                assert!((20.0..=20_000.0).contains(&f.hz), "{name}: {f:?}");
                assert!(
                    f.gain_db.abs() <= 20.0 && (0.1..=6.0).contains(&f.q),
                    "{name}: {f:?}"
                );
            }
            let peak = profile.peak_db(48_000);
            assert!((-3.0..=0.8).contains(&peak), "{name}: peak {peak}");
        }
        // The one that was asked for by name.
        let hd800s = builtin_profiles()
            .iter()
            .find(|p| p.name == "Sennheiser HD 800 S")
            .expect("included");
        assert_eq!(hd800s.preamp_db, -6.2);
        assert_eq!(hd800s.filters.len(), 10);
        assert_eq!(
            hd800s.filters[0],
            PeqFilter {
                kind: PeqKind::LowShelf,
                hz: 105.0,
                gain_db: 6.6,
                q: 0.70
            }
        );
        assert_eq!(
            hd800s.filters[3],
            PeqFilter {
                kind: PeqKind::Peak,
                hz: 5_679.0,
                gain_db: -4.4,
                q: 4.36
            }
        );
        // The one computed with AutoEq from Super* Review's measurement: it stands with
        // its brand, after the Momentum 4 Wireless.
        let at = builtin_profiles()
            .iter()
            .position(|p| p.name == "Sennheiser Momentum 5 Wireless")
            .expect("included");
        assert_eq!(
            builtin_profiles()[at - 1].name,
            "Sennheiser Momentum 4 Wireless"
        );
        let momentum5 = &builtin_profiles()[at];
        assert_eq!(momentum5.preamp_db, -5.7);
        assert_eq!(momentum5.filters.len(), 10);
        assert_eq!(
            momentum5.filters[0],
            PeqFilter {
                kind: PeqKind::LowShelf,
                hz: 105.0,
                gain_db: -9.9,
                q: 0.70
            }
        );
        assert_eq!(
            momentum5.filters[5],
            PeqFilter {
                kind: PeqKind::HighShelf,
                hz: 10_000.0,
                gain_db: -7.5,
                q: 0.70
            }
        );
    }

    /// Each filter is within its limit; together they may not be. A file of boosts
    /// stacked on one frequency is read, and plays no louder than the limit.
    #[test]
    fn filters_that_add_up_are_held_to_the_limit_by_the_preamp() {
        let stacked: String = (1..=32)
            .map(|i| format!("Filter {i}: ON PK Fc 1000 Hz Gain 30 dB Q 30\n"))
            .collect();
        let profile = PeqProfile::parse("Stack", &format!("Preamp: 12 dB\n{stacked}")).unwrap();
        for rate in [44_100, 48_000, 96_000, 192_000] {
            let peak = profile.peak_db(rate);
            assert!(peak <= MAX_NET_BOOST_DB + 0.01, "{peak} dB at {rate}");
            // The peak of so narrow a stack lies between the points of any grid: it is
            // found at the filters' own frequency.
            assert!((profile.response_db(rate, 1_000.0) - peak).abs() < 0.01);
        }
        // What comes out of a full-scale tone at that frequency is no more than the
        // limit above full scale (the peak protection that follows takes that off).
        let mut corrector = Corrector::new(48_000, Some(&profile));
        let mut tone: Vec<f32> = (0..48_000)
            .flat_map(|i| {
                let s = (2.0 * PI * 1_000.0 * f64::from(i) / 48_000.0).sin() as f32;
                [s, s]
            })
            .collect();
        corrector.process(&mut tone);
        let loudest = tone.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!(loudest.is_finite() && loudest <= 4.1, "{loudest}");

        // Tidying again changes nothing, and a real correction is left as it is.
        assert_eq!(profile.clone().sanitized(), profile);
        let real = PeqProfile::parse(
            "Real",
            "Preamp: -6.2 dB\nFilter 1: ON LSC Fc 105 Hz Gain 6.6 dB Q 0.70\n",
        )
        .unwrap();
        assert_eq!(real.preamp_db, -6.2);
        // A boost without a preamp is allowed up to the limit.
        let bare = PeqProfile::parse("Bare", "Filter 1: ON PK Fc 1000 Hz Gain 9 dB Q 1\n").unwrap();
        assert_eq!(bare.preamp_db, 0.0);
    }

    #[test]
    fn the_name_of_an_unknown_filter_is_reported_short() {
        let text = format!(
            "Filter 1: ON {} Fc 100 Hz Gain 1 dB Q 1\n",
            "X".repeat(60_000)
        );
        match PeqProfile::parse("x", &text) {
            Err(PeqError::UnsupportedFilter(kind)) => assert_eq!(kind.len(), MAX_KIND_CHARS),
            other => panic!("{other:?}"),
        }
    }

    /// Profiles changed faster than a fade lasts: each change waits for the fade before
    /// it, so the sound glides from one to the next and never jumps.
    #[test]
    fn changes_quicker_than_the_fade_glide_and_the_last_one_wins() {
        let rate = 48_000;
        let a = PeqProfile::parse("A", "Preamp: -12 dB\n").unwrap();
        let b = PeqProfile::parse("B", "Preamp: 6 dB\n").unwrap();
        let mut corrector = Corrector::new(rate, Some(&a));
        let mut last = None::<f32>;
        let mut largest_step = 0f32;
        // A steady signal in blocks of 2 ms; the profile changes every 6 ms.
        for block in 0..200 {
            if block % 3 == 0 && block < 60 {
                let next = [Some(&b), None, Some(&a), Some(&b)][(block / 3) % 4];
                corrector.set(next);
            }
            let mut samples = vec![0.5f32; 2 * 96];
            corrector.process(&mut samples);
            for frame in samples.chunks(2) {
                if let Some(previous) = last {
                    largest_step = largest_step.max((frame[0] - previous).abs());
                }
                last = Some(frame[0]);
            }
        }
        // A 40 ms fade between -12 and +6 dB moves 0.5 x (2.0 - 0.25) over 1,920
        // frames: under 0.001 a frame. A fade started over would jump by far more.
        assert!(largest_step < 0.002, "{largest_step}");
        // The last change was to B: that is where it ends.
        let expected = 0.5 * 10f32.powf(6.0 / 20.0);
        assert!((last.unwrap() - expected).abs() < 1e-4, "{last:?}");
    }
}
