//! The spectrum display in the middle of the meter deck: what is playing, split into
//! bands from bass to treble, as columns of light that rise at once and fall slowly.
//!
//! The samples come from the output callback's scope (the music before the app volume,
//! like the VU meters). They are windowed, transformed, and gathered into bands that are
//! equally wide to the ear (logarithmic in frequency).
use eframe::egui::{self, Align2, Color32, FontId, Rect, Stroke, pos2, vec2};
use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};
use std::sync::Arc;
use std::time::Instant;

/// Samples analysed per picture. At 44.1 kHz this resolves 21.5 Hz, enough to tell the
/// lowest bands apart.
pub const FFT_LEN: usize = 2048;
/// At high sample rates the same number of samples would resolve the bass too coarsely,
/// so several samples are averaged into one first (up to this many).
pub const MAX_DECIMATION: usize = 4;
/// The most samples [`Spectrum::analyse`] makes use of.
pub const MAX_SAMPLES: usize = FFT_LEN * MAX_DECIMATION;
pub const BANDS: usize = 36;
const LOW_HZ: f32 = 40.0;
const HIGH_HZ: f32 = 16_000.0;
/// Band levels are shown between these (after the tilt below).
const FLOOR_DB: f32 = -66.0;
const TOP_DB: f32 = -8.0;
/// Music has less energy the higher the frequency; without a tilt the right half of the
/// display would hardly move. 3 dB per octave around 1 kHz is the usual compensation.
const TILT_DB_PER_OCTAVE: f32 = 3.0;
/// How fast a column falls and how its cap behaves (in display heights per second).
const FALL_PER_SECOND: f32 = 1.9;
const CAP_HOLD_SECONDS: f32 = 0.45;
const CAP_FALL_PER_SECOND: f32 = 0.7;

/// Lower edge of band `index` (and upper edge of the one before) in Hz.
fn band_edge(index: usize) -> f32 {
    LOW_HZ * (HIGH_HZ / LOW_HZ).powf(index as f32 / BANDS as f32)
}

/// The frequency in the middle of band `index`.
pub fn band_centre(index: usize) -> f32 {
    (band_edge(index) * band_edge(index + 1)).sqrt()
}

pub struct Spectrum {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    buffer: Vec<Complex32>,
    scratch: Vec<Complex32>,
    /// Column heights and caps, 0 to 1.
    pub bars: [f32; BANDS],
    pub caps: [f32; BANDS],
    cap_held: [f32; BANDS],
    last_frame: Option<Instant>,
}

impl Default for Spectrum {
    fn default() -> Self {
        let fft = FftPlanner::<f32>::new().plan_fft_forward(FFT_LEN);
        let scratch = vec![Complex32::default(); fft.get_inplace_scratch_len()];
        // Hann window: no clicks at the edges of the slice that is analysed.
        let window = (0..FFT_LEN)
            .map(|n| 0.5 - 0.5 * (std::f32::consts::TAU * n as f32 / (FFT_LEN - 1) as f32).cos())
            .collect();
        Self {
            fft,
            window,
            buffer: vec![Complex32::default(); FFT_LEN],
            scratch,
            bars: [0.0; BANDS],
            caps: [0.0; BANDS],
            cap_held: [0.0; BANDS],
            last_frame: None,
        }
    }
}

impl Spectrum {
    /// The level of every band (0 to 1) for the latest samples at `rate` Hz. Pass up to
    /// [`MAX_SAMPLES`]; at least [`FFT_LEN`] are needed.
    pub fn analyse(&mut self, samples: &[f32], rate: u32) -> [f32; BANDS] {
        let mut levels = [0.0; BANDS];
        if samples.len() < FFT_LEN || rate == 0 {
            return levels;
        }
        // Bring high sample rates down to about 44.1 to 48 kHz by averaging neighbours
        // (as far as the samples at hand allow): the display ends at 16 kHz anyway.
        let decimation = ((rate / 44_100) as usize)
            .clamp(1, MAX_DECIMATION)
            .min(samples.len() / FFT_LEN);
        let latest = &samples[samples.len() - FFT_LEN * decimation..];
        for ((slot, group), window) in self
            .buffer
            .iter_mut()
            .zip(latest.chunks_exact(decimation))
            .zip(&self.window)
        {
            let mean = group
                .iter()
                .map(|s| if s.is_finite() { *s } else { 0.0 })
                .sum::<f32>()
                / decimation as f32;
            *slot = Complex32::new(mean * window, 0.0);
        }
        self.fft
            .process_with_scratch(&mut self.buffer, &mut self.scratch);
        let bin_hz = rate as f32 / decimation as f32 / FFT_LEN as f32;
        let nyquist_bin = FFT_LEN / 2;
        // A sine of amplitude A shows up as A · N/4 in its bin (Hann window).
        let to_amplitude = 4.0 / FFT_LEN as f32;
        for (band, level) in levels.iter_mut().enumerate() {
            let (low, high) = (band_edge(band), band_edge(band + 1));
            // Every bin whose centre lies in the band; at least the nearest one, since
            // the lowest bands are narrower than a bin.
            let first = ((low / bin_hz).ceil() as usize).clamp(1, nyquist_bin - 1);
            let last = ((high / bin_hz).ceil() as usize).clamp(first + 1, nyquist_bin);
            let nearest = ((band_centre(band) / bin_hz).round() as usize).clamp(1, nyquist_bin - 1);
            let range = if (first as f32) * bin_hz >= high {
                nearest..nearest + 1
            } else {
                first..last
            };
            let peak = self.buffer[range]
                .iter()
                .map(|c| c.norm())
                .fold(0.0f32, f32::max)
                * to_amplitude;
            if peak <= 0.0 {
                continue;
            }
            let tilt = TILT_DB_PER_OCTAVE * (band_centre(band) / 1000.0).log2();
            let db = 20.0 * peak.log10() + tilt;
            *level = ((db - FLOOR_DB) / (TOP_DB - FLOOR_DB)).clamp(0.0, 1.0);
        }
        levels
    }

    /// Moves the columns towards `levels` (`None`: nothing is playing, everything
    /// falls). Returns true while anything is still lit, i.e. while the display needs to
    /// be drawn again soon.
    pub fn update(&mut self, levels: Option<[f32; BANDS]>, now: Instant) -> bool {
        let dt = self
            .last_frame
            .map_or(0.0, |last| {
                now.saturating_duration_since(last).as_secs_f32()
            })
            .clamp(0.0, 0.25);
        self.last_frame = Some(now);
        let mut lit = levels.is_some();
        for band in 0..BANDS {
            let target = levels.map_or(0.0, |l| l[band]);
            // Up at once, down at a steady pace.
            self.bars[band] = target.max(self.bars[band] - FALL_PER_SECOND * dt);
            if self.bars[band] >= self.caps[band] {
                self.caps[band] = self.bars[band];
                self.cap_held[band] = 0.0;
            } else {
                self.cap_held[band] += dt;
                if self.cap_held[band] > CAP_HOLD_SECONDS {
                    self.caps[band] =
                        (self.caps[band] - CAP_FALL_PER_SECOND * dt).max(self.bars[band]);
                }
            }
            lit |= self.bars[band] > 0.001 || self.caps[band] > 0.001;
        }
        lit
    }

    /// The display is not shown: forget the clock (see `MeterState::suspend`).
    pub fn suspend(&mut self) {
        self.last_frame = None;
    }

    /// Centre frequency of the tallest column, if any is lit (for verification).
    pub fn loudest_band_hz(&self) -> Option<f32> {
        let (band, level) = self
            .bars
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))?;
        (*level > 0.02).then(|| band_centre(band))
    }
}

const BEZEL: Color32 = Color32::from_rgb(0x19, 0x19, 0x1C);
const BEZEL_EDGE: Color32 = Color32::from_rgb(0x3C, 0x3C, 0x44);
const GLASS: Color32 = Color32::from_rgb(0x0A, 0x0B, 0x10);
const GRID: Color32 = Color32::from_rgb(0x1E, 0x20, 0x2A);
const LABEL: Color32 = Color32::from_rgb(0x6C, 0x70, 0x82);
const CAP: Color32 = Color32::from_rgb(0xF2, 0xF3, 0xFA);
/// Colours of a column from its foot to its top.
const LOW: (f32, f32, f32) = (0x2E as f32, 0xD9 as f32, 0xC3 as f32);
const MID: (f32, f32, f32) = (0x8F as f32, 0x8C as f32, 0xFF as f32);
const HIGH: (f32, f32, f32) = (0xFF as f32, 0x5C as f32, 0xA8 as f32);

/// The colour of a column at `height` (0 = foot, 1 = top of the display).
fn column_colour(height: f32) -> Color32 {
    let mix = |a: (f32, f32, f32), b: (f32, f32, f32), t: f32| {
        Color32::from_rgb(
            (a.0 + (b.0 - a.0) * t) as u8,
            (a.1 + (b.1 - a.1) * t) as u8,
            (a.2 + (b.2 - a.2) * t) as u8,
        )
    };
    let height = height.clamp(0.0, 1.0);
    if height < 0.55 {
        mix(LOW, MID, height / 0.55)
    } else {
        mix(MID, HIGH, (height - 0.55) / 0.45)
    }
}

/// Draws the display into `rect`: a dark glass in the same bezel as the meters, columns
/// built from lit segments, a bright cap riding on each, and a frequency scale.
pub fn draw(painter: &egui::Painter, rect: Rect, bars: &[f32; BANDS], caps: &[f32; BANDS]) {
    let rim = (rect.height() * 0.05).clamp(4.0, 10.0);
    painter.rect_filled(rect, rim + 4.0, BEZEL);
    painter.rect_stroke(
        rect.shrink(0.5),
        rim + 4.0,
        Stroke::new(1.0, BEZEL_EDGE),
        egui::StrokeKind::Inside,
    );
    let glass = rect.shrink(rim);
    let painter = painter.with_clip_rect(glass.intersect(painter.clip_rect()));
    painter.rect_filled(glass, 4.0, GLASS);

    let label_height = (glass.height() * 0.13).clamp(9.0, 14.0);
    let field = Rect::from_min_max(
        glass.min + vec2(8.0, 7.0),
        glass.max - vec2(8.0, label_height + 4.0),
    );
    if field.width() < 40.0 || field.height() < 20.0 {
        return;
    }
    for quarter in 1..4 {
        let y = field.bottom() - field.height() * quarter as f32 / 4.0;
        painter.line_segment(
            [pos2(field.left(), y), pos2(field.right(), y)],
            Stroke::new(1.0, GRID),
        );
    }

    let pitch = field.width() / BANDS as f32;
    let gap = (pitch * 0.24).clamp(1.0, 4.0);
    // Segments of whole pixels, so the rows line up across all columns.
    let segment = (field.height() / 22.0).round().clamp(3.0, 9.0);
    let lit_part = (segment - 1.0).max(1.0);
    let rows = (field.height() / segment).floor().max(1.0);
    for band in 0..BANDS {
        let left = field.left() + pitch * band as f32 + gap / 2.0;
        let right = left + pitch - gap;
        let lit_rows = (bars[band].clamp(0.0, 1.0) * rows).round() as usize;
        for row in 0..lit_rows {
            let bottom = field.bottom() - segment * row as f32;
            painter.rect_filled(
                Rect::from_min_max(pos2(left, bottom - lit_part), pos2(right, bottom)),
                1.0,
                column_colour((row as f32 + 0.5) / rows),
            );
        }
        let cap = caps[band].clamp(0.0, 1.0);
        if cap > 0.01 {
            let y = field.bottom() - (cap * rows).round() * segment - 1.0;
            painter.rect_filled(
                Rect::from_min_max(pos2(left, y - 1.5), pos2(right, y)),
                0.5,
                CAP,
            );
        }
    }

    // The frequency scale: decades and the ends.
    let font = FontId::proportional((label_height - 1.0).clamp(8.0, 12.0));
    let span = (HIGH_HZ / LOW_HZ).ln();
    for (hz, text) in [(100.0f32, "100"), (1000.0, "1k"), (10_000.0, "10k")] {
        let x = field.left() + field.width() * (hz / LOW_HZ).ln() / span;
        painter.text(
            pos2(x, glass.bottom() - 3.0),
            Align2::CENTER_BOTTOM,
            text,
            font.clone(),
            LABEL,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn sine(hz: f32, rate: u32, amplitude: f32) -> Vec<f32> {
        (0..MAX_SAMPLES)
            .map(|n| amplitude * (std::f32::consts::TAU * hz * n as f32 / rate as f32).sin())
            .collect()
    }

    fn loudest(levels: &[f32; BANDS]) -> usize {
        levels
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i)
            .expect("bands")
    }

    #[test]
    fn bands_cover_the_range_evenly_on_a_logarithmic_scale() {
        assert!((band_edge(0) - LOW_HZ).abs() < 0.01);
        assert!((band_edge(BANDS) - HIGH_HZ).abs() < 1.0);
        let ratio = band_edge(1) / band_edge(0);
        for band in 0..BANDS {
            assert!((band_edge(band + 1) / band_edge(band) - ratio).abs() < 1e-3);
            assert!(band_centre(band) > band_edge(band) && band_centre(band) < band_edge(band + 1));
        }
    }

    #[test]
    fn a_tone_lights_the_band_it_is_in() {
        let mut spectrum = Spectrum::default();
        for rate in [44_100, 48_000, 96_000, 192_000] {
            for hz in [60.0, 110.0, 440.0, 1000.0, 3000.0, 9000.0, 14_000.0] {
                let levels = spectrum.analyse(&sine(hz, rate, 0.25), rate);
                let band = loudest(&levels);
                // The lowest bands are about as wide as the analysis resolves: there
                // the neighbour may win.
                let tolerance = if hz < 200.0 { 2 } else { 1 };
                let expected = (0..BANDS)
                    .find(|b| band_edge(*b) <= hz && hz < band_edge(b + 1))
                    .expect("in range");
                assert!(
                    band.abs_diff(expected) <= tolerance,
                    "{hz} Hz at {rate}: band {band}, expected {expected}"
                );
                assert!(levels[band] > 0.5, "{hz} Hz at {rate}: {}", levels[band]);
                // Far away from the tone the display stays dark.
                let far = if expected > BANDS / 2 { 2 } else { BANDS - 3 };
                assert!(
                    levels[far] < 0.15,
                    "{hz} Hz at {rate}: leak {}",
                    levels[far]
                );
            }
        }
    }

    #[test]
    fn level_follows_loudness_and_silence_is_dark() {
        let mut spectrum = Spectrum::default();
        let loud = spectrum.analyse(&sine(1000.0, 48_000, 0.5), 48_000);
        let soft = spectrum.analyse(&sine(1000.0, 48_000, 0.05), 48_000);
        let band = loudest(&loud);
        // 20 dB less is 20/58 of the display's range lower.
        let drop = loud[band] - soft[band];
        assert!((drop - 20.0 / (TOP_DB - FLOOR_DB)).abs() < 0.03, "{drop}");
        assert_eq!(spectrum.analyse(&[0.0; FFT_LEN], 48_000), [0.0; BANDS]);
        // With only the minimum of samples a high rate is analysed less finely, but it is.
        let short = &sine(1000.0, 192_000, 0.5)[..FFT_LEN];
        let coarse = spectrum.analyse(short, 192_000);
        assert!((band_centre(loudest(&coarse)) / 1000.0).log2().abs() < 0.5);
        // Too few samples, no rate, or garbage: dark, never a panic.
        assert_eq!(spectrum.analyse(&[0.5; 100], 48_000), [0.0; BANDS]);
        assert_eq!(
            spectrum.analyse(&sine(1000.0, 48_000, 0.5), 0),
            [0.0; BANDS]
        );
        let garbage = spectrum.analyse(&[f32::NAN; FFT_LEN], 48_000);
        assert!(garbage.iter().all(|l| l.is_finite()));
    }

    #[test]
    fn columns_rise_at_once_fall_slowly_and_come_to_rest() {
        let mut spectrum = Spectrum::default();
        let start = Instant::now();
        let mut levels = [0.0; BANDS];
        levels[10] = 0.8;
        assert!(spectrum.update(Some(levels), start));
        assert_eq!(spectrum.bars[10], 0.8, "up at once");
        assert_eq!(spectrum.caps[10], 0.8);
        // The music stops: the column falls, the cap waits and then follows.
        let step = Duration::from_millis(16);
        let mut now = start + step;
        assert!(spectrum.update(None, now));
        assert!(
            spectrum.bars[10] < 0.8 && spectrum.bars[10] > 0.7,
            "down slowly"
        );
        assert_eq!(spectrum.caps[10], 0.8, "the cap holds");
        let mut frames = 0;
        while spectrum.update(None, now) {
            now += step;
            frames += 1;
            assert!(spectrum.caps[10] >= spectrum.bars[10]);
            assert!(frames < 400, "the display never went dark");
        }
        assert!(
            spectrum
                .bars
                .iter()
                .chain(&spectrum.caps)
                .all(|v| *v <= 0.001)
        );
        assert!(spectrum.loudest_band_hz().is_none());
        assert!(
            !spectrum.update(None, now + step),
            "dark and still: nothing to draw"
        );
    }

    #[test]
    fn column_colours_run_from_teal_through_violet_to_pink() {
        let (foot, middle, top) = (column_colour(0.0), column_colour(0.55), column_colour(1.0));
        assert_eq!((foot.r(), foot.g(), foot.b()), (0x2E, 0xD9, 0xC3));
        assert_eq!((middle.r(), middle.g(), middle.b()), (0x8F, 0x8C, 0xFF));
        assert_eq!((top.r(), top.g(), top.b()), (0xFF, 0x5C, 0xA8));
        assert_eq!(column_colour(7.0), top);
        assert_eq!(column_colour(-1.0), foot);
    }
}
