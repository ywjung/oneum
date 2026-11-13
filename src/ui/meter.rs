//! The analog VU meter: how the needle moves, how a dial is drawn, and how the two
//! dials are laid out in the meter deck.
//!
//! A VU meter shows the rectified average of the signal on a scale that is linear in
//! voltage (which is why the marks crowd together on the left), through a needle with
//! standardised sluggishness: it takes 300 ms to swing to a new steady level and
//! overshoots it by 1 to 1.5 percent. Both are reproduced here, so the needles move the
//! way they do on a tape deck rather than like a bar graph.
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Stroke, pos2, vec2};
use std::f32::consts::{FRAC_2_PI, PI};
use std::time::{Duration, Instant};

/// Peak amplitude of the sine wave that reads 0 VU: −8 dBFS, i.e. −11 dBFS RMS. Loud
/// modern masters then swing around 0 VU and quiet recordings stay well on the scale.
pub const ZERO_VU_SINE_PEAK: f32 = 0.398_107_17;
/// The scale ends at +3 VU; needle positions are fractions of that deflection.
const SCALE_TOP_VU: f32 = 3.0;
/// Where the needle is stopped mechanically, a little past the end of the scale.
const PIN: f32 = 1.06;
/// Natural frequency and damping of the movement. With this damping the overshoot is
/// 1.5 %, and the frequency puts the first arrival at the new level at 300 ms.
const OMEGA: f32 = 13.9;
const ZETA: f32 = 0.8;
/// A sample this close to full scale lights the peak lamp.
const PEAK_LAMP_LEVEL: f32 = 0.989;
const PEAK_LAMP_HOLD: Duration = Duration::from_millis(1200);
/// Width of a dial relative to its height.
pub const DIAL_ASPECT: f32 = 1.55;
/// The needle sweeps this far to either side of straight up.
const HALF_SWEEP: f32 = 47.0 * PI / 180.0;

/// Needle deflection (0 = rest, 1 = +3 VU) for a rectified-average level.
pub fn deflection(average: f32) -> f32 {
    if !average.is_finite() || average <= 0.0 {
        return 0.0;
    }
    // A sine of amplitude A has a rectified average of A · 2/π.
    let zero_vu = ZERO_VU_SINE_PEAK * FRAC_2_PI;
    average / zero_vu / vu_to_ratio(SCALE_TOP_VU)
}

fn vu_to_ratio(vu: f32) -> f32 {
    10f32.powf(vu / 20.0)
}

/// Position on the scale of a VU mark (−20 … +3).
fn mark_position(vu: f32) -> f32 {
    vu_to_ratio(vu - SCALE_TOP_VU)
}

/// The reading in VU that a needle position corresponds to; `None` at rest.
pub fn reading(position: f32) -> Option<f32> {
    (position > 0.001).then(|| 20.0 * position.log10() + SCALE_TOP_VU)
}

/// The moving part of one meter.
#[derive(Debug, Clone, Copy, Default)]
pub struct Needle {
    pub position: f32,
    velocity: f32,
}

impl Needle {
    /// Moves the needle for `dt` seconds towards `target` (a deflection).
    pub fn advance(&mut self, target: f32, dt: f32) {
        let target = if target.is_finite() {
            target.max(0.0)
        } else {
            0.0
        };
        // Long gaps (the window was hidden) are not simulated in full.
        let mut left = dt.clamp(0.0, 0.25);
        while left > 0.0 {
            let h = left.min(0.002);
            let pull = OMEGA * OMEGA * (target - self.position);
            let drag = 2.0 * ZETA * OMEGA * self.velocity;
            self.velocity += (pull - drag) * h;
            self.position += self.velocity * h;
            // The stops at both ends of the scale.
            if self.position > PIN {
                self.position = PIN;
                self.velocity = self.velocity.min(0.0);
            } else if self.position < 0.0 {
                self.position = 0.0;
                self.velocity = self.velocity.max(0.0);
            }
            left -= h;
        }
    }

    /// True when the needle has settled on `target` (nothing left to animate).
    pub fn settled(&self, target: f32) -> bool {
        (self.position - target.clamp(0.0, PIN)).abs() < 0.001 && self.velocity.abs() < 0.01
    }
}

/// Both needles and peak lamps, kept between frames.
#[derive(Debug, Default)]
pub struct MeterState {
    pub needles: [Needle; 2],
    peak_until: [Option<Instant>; 2],
    last_frame: Option<Instant>,
}

impl MeterState {
    /// Advances the needles to `now` towards the given levels (`None` = nothing is
    /// playing). Returns true while something is still moving or lit, i.e. while the
    /// meter needs to be drawn again soon.
    pub fn update(&mut self, levels: Option<crate::audio::Levels>, now: Instant) -> bool {
        let dt = self.last_frame.map_or(0.0, |last| {
            now.saturating_duration_since(last).as_secs_f32()
        });
        self.last_frame = Some(now);
        let mut busy = levels.is_some();
        for ch in 0..2 {
            let target = levels.map_or(0.0, |l| deflection(l.average[ch]));
            self.needles[ch].advance(target, dt);
            if levels.is_some_and(|l| l.peak[ch] >= PEAK_LAMP_LEVEL) {
                self.peak_until[ch] = Some(now + PEAK_LAMP_HOLD);
            }
            if self.peak_until[ch].is_some_and(|until| until <= now) {
                self.peak_until[ch] = None;
            }
            busy |= !self.needles[ch].settled(target) || self.peak_until[ch].is_some();
        }
        busy
    }

    pub fn peak_lit(&self, channel: usize) -> bool {
        self.peak_until[channel].is_some()
    }

    /// The meter is not being shown: forget the clock, so the first frame after it
    /// comes back does not count the whole absence as one step.
    pub fn suspend(&mut self) {
        self.last_frame = None;
    }
}

/// Height of the meter deck when it is first shown, and how far it can be resized.
pub const DECK_DEFAULT_HEIGHT: f32 = 172.0;
pub const DECK_HEIGHT_RANGE: std::ops::RangeInclusive<f32> = 112.0..=300.0;
/// Space between a dial and the text beside it, and the least width worth writing in.
pub const DECK_READOUT_GAP: f32 = 22.0;
pub const DECK_READOUT_MIN_WIDTH: f32 = 150.0;
/// Space between the two dials, as a fraction of a dial's width.
const DECK_GAP: f32 = 0.09;

/// Background of the meter deck: a shade darker than the bars around it, so the
/// meters read as set into the window.
pub fn deck_fill(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(0x10, 0x10, 0x13)
    } else {
        Color32::from_rgb(0xE2, 0xE2, 0xE7)
    }
}

/// The spectrum display between the dials: this wide compared to a dial at most, and
/// left out when less than the minimum remains for it.
const SPECTRUM_WIDTH: f32 = 1.3;
const SPECTRUM_MIN_WIDTH: f32 = 130.0;

/// Where things go in the meter deck.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeckLayout {
    /// The left and the right channel's dial.
    pub dials: [Rect; 2],
    /// The spectrum display between them, when the deck is wide enough.
    pub spectrum: Option<Rect>,
}

/// Lays the deck out inside `area`: the two dials as tall as the area allows, centred,
/// with the spectrum display between them if there is room for it; otherwise the dials
/// side by side, as large as fits in both directions.
pub fn deck_layout(area: Rect) -> DeckLayout {
    let height = area.height().max(1.0);
    let width = height * DIAL_ASPECT;
    let gap = width * DECK_GAP;
    let top = area.center().y - height / 2.0;
    let for_spectrum = (area.width() - 2.0 * width - 2.0 * gap).min(width * SPECTRUM_WIDTH);
    if for_spectrum >= SPECTRUM_MIN_WIDTH {
        let total = 2.0 * width + 2.0 * gap + for_spectrum;
        let left = area.center().x - total / 2.0;
        let size = vec2(width, height);
        return DeckLayout {
            dials: [
                Rect::from_min_size(pos2(left, top), size),
                Rect::from_min_size(pos2(left + width + 2.0 * gap + for_spectrum, top), size),
            ],
            spectrum: Some(Rect::from_min_size(
                pos2(left + width + gap, top),
                vec2(for_spectrum, height),
            )),
        };
    }
    let width = width.min(area.width() / (2.0 + DECK_GAP)).max(1.0);
    let size = vec2(width, width / DIAL_ASPECT);
    let gap = width * DECK_GAP;
    let left = area.center().x - gap / 2.0 - width;
    let top = area.center().y - size.y / 2.0;
    DeckLayout {
        dials: [
            Rect::from_min_size(pos2(left, top), size),
            Rect::from_min_size(pos2(left + width + gap, top), size),
        ],
        spectrum: None,
    }
}

const BEZEL: Color32 = Color32::from_rgb(0x19, 0x19, 0x1C);
const BEZEL_EDGE: Color32 = Color32::from_rgb(0x3C, 0x3C, 0x44);
const FACE: Color32 = Color32::from_rgb(0xF1, 0xE6, 0xC6);
const INK: Color32 = Color32::from_rgb(0x2A, 0x26, 0x22);
const RED: Color32 = Color32::from_rgb(0xC6, 0x2A, 0x1E);
const LAMP_OFF: Color32 = Color32::from_rgb(0x7A, 0x3A, 0x30);
const LAMP_ON: Color32 = Color32::from_rgb(0xFF, 0x3B, 0x26);
const COVER: Color32 = Color32::from_rgb(0x22, 0x22, 0x26);

/// VU marks on the scale: `(vu, is_major)`.
const MARKS: [(f32, bool); 11] = [
    (-20.0, true),
    (-10.0, true),
    (-7.0, true),
    (-5.0, true),
    (-3.0, true),
    (-2.0, false),
    (-1.0, false),
    (0.0, true),
    (1.0, false),
    (2.0, false),
    (3.0, true),
];

/// Draws one meter into `rect` (use [`DIAL_ASPECT`] for its shape): a dark bezel around
/// a lit cream face, the VU scale with its red zone above 0 and the percentage scale
/// below it, the needle at `position`, a peak lamp, and the channel letter in the
/// bottom left corner (bottom right with `letter_right`).
pub fn dial(
    painter: &egui::Painter,
    rect: Rect,
    position: f32,
    peak_lit: bool,
    channel: &str,
    letter_right: bool,
) {
    // Bezel: a dark frame with a lighter rim, as if the meter sat in a cut-out.
    let rim = (rect.height() * 0.05).clamp(4.0, 10.0);
    painter.rect_filled(rect, rim + 4.0, BEZEL);
    painter.rect_stroke(
        rect.shrink(0.5),
        rim + 4.0,
        Stroke::new(1.0, BEZEL_EDGE),
        egui::StrokeKind::Inside,
    );
    let face = rect.shrink(rim);
    let h = face.height();
    let unit = (h / 140.0).clamp(0.6, 2.2);
    let painter = painter.with_clip_rect(face.intersect(painter.clip_rect()));
    painter.rect_filled(face, 4.0, FACE);

    // The needle turns about a point just below the face; the lamp behind the scale
    // sits there too, so the face is brightest at the bottom centre.
    let pivot = pos2(face.center().x, face.top() + h * 1.06);
    for (radius, alpha) in [(1.30, 9), (1.02, 11), (0.76, 13), (0.50, 15)] {
        painter.circle_filled(
            pivot,
            h * radius,
            Color32::from_rgba_unmultiplied(0xFF, 0xF8, 0xDC, alpha),
        );
    }
    // The shadow the bezel casts on the top of the face.
    for (offset, alpha) in [(0.0, 46), (1.5, 26), (3.0, 12)] {
        painter.line_segment(
            [
                face.left_top() + vec2(0.0, offset * unit),
                face.right_top() + vec2(0.0, offset * unit),
            ],
            Stroke::new(1.5 * unit, Color32::from_black_alpha(alpha)),
        );
    }

    let scale_radius = h * 0.76;
    let at = |position: f32, radius: f32| -> Pos2 {
        let angle = -HALF_SWEEP + 2.0 * HALF_SWEEP * position;
        pivot + vec2(angle.sin(), -angle.cos()) * radius
    };
    let arc = |from: f32, to: f32, radius: f32, stroke: Stroke| {
        let steps = 48;
        let points: Vec<Pos2> = (0..=steps)
            .map(|i| at(from + (to - from) * i as f32 / steps as f32, radius))
            .collect();
        painter.add(egui::Shape::line(points, stroke));
    };
    let zero = mark_position(0.0);
    arc(
        mark_position(-20.0),
        zero,
        scale_radius,
        Stroke::new(1.4 * unit, INK),
    );
    // The red zone: a heavier band from 0 to +3.
    arc(
        zero,
        1.0,
        scale_radius + 1.7 * unit,
        Stroke::new(4.4 * unit, RED),
    );

    let font = FontId::proportional((h * 0.072).clamp(7.0, 16.0));
    for (vu, major) in MARKS {
        let position = mark_position(vu);
        let color = if vu >= 0.0 { RED } else { INK };
        let tick = if major { 9.0 } else { 5.5 } * unit;
        painter.line_segment(
            [
                at(position, scale_radius),
                at(position, scale_radius + tick),
            ],
            Stroke::new(if major { 1.4 } else { 1.0 } * unit, color),
        );
        painter.text(
            at(position, scale_radius + 9.0 * unit + font.size * 0.9),
            Align2::CENTER_CENTER,
            format!("{}", vu.abs() as i32),
            font.clone(),
            color,
        );
    }
    // Minus and plus at the two ends of the scale.
    let sign_radius = scale_radius + 4.5 * unit;
    painter.text(
        at(mark_position(-20.0) - 0.05, sign_radius),
        Align2::CENTER_CENTER,
        "−",
        font.clone(),
        INK,
    );
    painter.text(
        at(1.05, sign_radius),
        Align2::CENTER_CENTER,
        "+",
        font.clone(),
        RED,
    );

    // The percentage scale under the arc: 100 % is 0 VU. Left out on small dials.
    if h >= 96.0 {
        let small = FontId::proportional((h * 0.052).clamp(6.5, 11.0));
        let ink = INK.gamma_multiply(0.72);
        for percent in [0u8, 20, 40, 60, 80, 100] {
            let position = zero * f32::from(percent) / 100.0;
            painter.line_segment(
                [
                    at(position, scale_radius - 2.0 * unit),
                    at(position, scale_radius - 6.5 * unit),
                ],
                Stroke::new(0.9 * unit, ink),
            );
            painter.text(
                at(position, scale_radius - 7.5 * unit - small.size * 0.8),
                Align2::CENTER_CENTER,
                percent.to_string(),
                small.clone(),
                ink,
            );
        }
    }

    painter.text(
        pos2(face.center().x, pivot.y - scale_radius * 0.46),
        Align2::CENTER_CENTER,
        "VU",
        FontId::proportional((h * 0.125).clamp(10.0, 28.0)),
        INK,
    );
    let (corner, align, inset) = if letter_right {
        (face.right_bottom(), Align2::RIGHT_BOTTOM, vec2(-10.0, -7.0))
    } else {
        (face.left_bottom(), Align2::LEFT_BOTTOM, vec2(10.0, -7.0))
    };
    painter.text(
        corner + inset * unit,
        align,
        channel,
        FontId::proportional((h * 0.10).clamp(9.0, 20.0)),
        INK,
    );

    // Peak lamp, top right: an LED in a dark ring that glows when lit.
    let lamp = face.right_top() + vec2(-13.0, 13.0) * unit;
    if peak_lit {
        painter.circle_filled(lamp, 9.0 * unit, LAMP_ON.gamma_multiply(0.22));
        painter.circle_filled(lamp, 6.5 * unit, LAMP_ON.gamma_multiply(0.30));
    }
    painter.circle_filled(lamp, 4.6 * unit, INK);
    painter.circle_filled(lamp, 3.5 * unit, if peak_lit { LAMP_ON } else { LAMP_OFF });
    painter.text(
        lamp + vec2(-8.5 * unit, 0.0),
        Align2::RIGHT_CENTER,
        "PEAK",
        FontId::proportional((h * 0.05).clamp(6.5, 11.0)),
        INK.gamma_multiply(0.75),
    );

    // The needle: tapered, with a soft shadow, and the cover over its pivot.
    let position = position.clamp(0.0, PIN);
    let (base, tip) = (
        at(position, h * 0.12),
        at(position, scale_radius + 11.0 * unit),
    );
    let along = (tip - base).normalized();
    let across = vec2(-along.y, along.x);
    let needle = |offset: egui::Vec2, color: Color32| {
        egui::Shape::convex_polygon(
            vec![
                base + across * 1.25 * unit + offset,
                tip + across * 0.4 * unit + offset,
                tip - across * 0.4 * unit + offset,
                base - across * 1.25 * unit + offset,
            ],
            color,
            Stroke::NONE,
        )
    };
    painter.add(needle(vec2(1.8, 2.6) * unit, Color32::from_black_alpha(38)));
    painter.add(needle(vec2(0.0, 0.0), INK));
    painter.circle_filled(pivot, h * 0.215, COVER);
    painter.circle_stroke(pivot, h * 0.215, Stroke::new(1.0 * unit, BEZEL_EDGE));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs a needle from `from` towards `target` and returns its position after `at`
    /// seconds and the extreme it reached within one second.
    fn swing(from: f32, target: f32, at: f32) -> (f32, f32) {
        let mut needle = Needle {
            position: from,
            velocity: 0.0,
        };
        let (mut at_time, mut extreme) = (from, from);
        let frame = 1.0 / 60.0;
        let mut t = 0.0;
        while t < 1.0 {
            needle.advance(target, frame);
            t += frame;
            if target > from {
                extreme = extreme.max(needle.position);
            } else {
                extreme = extreme.min(needle.position);
            }
            if (t - at).abs() < frame / 2.0 {
                at_time = needle.position;
            }
        }
        (at_time, extreme)
    }

    #[test]
    fn calibration_puts_the_reference_sine_at_zero_vu() {
        let sine_average = |peak: f32| peak * FRAC_2_PI;
        let at_zero = deflection(sine_average(ZERO_VU_SINE_PEAK));
        assert!((reading(at_zero).expect("moving") - 0.0).abs() < 0.01);
        assert!((at_zero - mark_position(0.0)).abs() < 1e-4);
        // 3 dB more is the end of the scale, 20 dB less is the −20 mark.
        let top = deflection(sine_average(ZERO_VU_SINE_PEAK * vu_to_ratio(3.0)));
        assert!((top - 1.0).abs() < 1e-4);
        let low = deflection(sine_average(ZERO_VU_SINE_PEAK * vu_to_ratio(-20.0)));
        assert!((reading(low).expect("moving") + 20.0).abs() < 0.01);
        // Silence and nonsense leave the needle at rest.
        assert_eq!(deflection(0.0), 0.0);
        assert_eq!(deflection(f32::NAN), 0.0);
        assert_eq!(reading(0.0), None);
    }

    /// The VU standard: 99 % of the steady reading after 300 ms, with an overshoot of
    /// 1 to 1.5 %, and the same on the way back.
    #[test]
    fn the_needle_has_vu_ballistics() {
        let target = mark_position(0.0);
        let (at_300ms, highest) = swing(0.0, target, 0.3);
        assert!(
            (at_300ms / target - 1.0).abs() < 0.01,
            "at 300 ms: {:.4}",
            at_300ms / target
        );
        let overshoot = highest / target - 1.0;
        assert!(
            (0.010..=0.016).contains(&overshoot),
            "overshoot {overshoot:.4}"
        );
        let (back_at_300ms, _) = swing(target, 0.0, 0.3);
        assert!(back_at_300ms / target < 0.01, "{back_at_300ms}");
    }

    #[test]
    fn the_needle_stops_at_both_ends_and_does_not_depend_on_frame_rate() {
        // Driven far past the scale it rests on the pin; released, it comes back.
        let mut needle = Needle::default();
        for _ in 0..120 {
            needle.advance(5.0, 1.0 / 60.0);
        }
        assert_eq!(needle.position, PIN);
        for _ in 0..120 {
            needle.advance(0.0, 1.0 / 60.0);
        }
        assert!(needle.position < 0.001 && needle.settled(0.0));
        // 30 and 120 frames per second end up in the same place.
        let (mut slow, mut fast) = (Needle::default(), Needle::default());
        for _ in 0..6 {
            slow.advance(0.7, 1.0 / 30.0);
        }
        for _ in 0..24 {
            fast.advance(0.7, 1.0 / 120.0);
        }
        assert!((slow.position - fast.position).abs() < 0.003);
        // A stalled frame or garbage input cannot throw the needle off the dial.
        needle.advance(f32::NAN, 10.0);
        needle.advance(f32::INFINITY, f32::NAN);
        assert!((0.0..=PIN).contains(&needle.position));
    }

    #[test]
    fn the_deck_puts_the_dials_side_by_side_with_the_spectrum_between_when_it_fits() {
        // (width, height, spectrum expected)
        let cases = [
            (1148.0, 148.0, true),
            (860.0, 148.0, true),
            (728.0, 148.0, true),
            (560.0, 148.0, false),
            (1148.0, 276.0, true),
            (760.0, 276.0, false),
            (300.0, 148.0, false),
        ];
        for (w, h, with_spectrum) in cases {
            let area = Rect::from_min_size(pos2(16.0, 500.0), vec2(w, h));
            let layout = deck_layout(area);
            let [left, right] = layout.dials;
            assert!((left.size() - right.size()).length() < 0.01);
            assert!((left.width() / left.height() - DIAL_ASPECT).abs() < 1e-3);
            // Left of right, same height, not touching, both inside, centred as a pair.
            assert!(left.right() < right.left() && left.top() == right.top());
            let inside = area.expand(0.01);
            assert!(inside.contains_rect(left) && inside.contains_rect(right));
            let middle = (left.left() + right.right()) / 2.0;
            assert!((middle - area.center().x).abs() < 0.01);
            assert_eq!(layout.spectrum.is_some(), with_spectrum, "{w}x{h}");
            match layout.spectrum {
                Some(spectrum) => {
                    // Between the dials, as tall as they are, clear of both.
                    assert!(left.right() < spectrum.left() && spectrum.right() < right.left());
                    assert!((spectrum.height() - left.height()).abs() < 0.01);
                    assert!(spectrum.width() >= SPECTRUM_MIN_WIDTH);
                    assert!(spectrum.width() <= left.width() * SPECTRUM_WIDTH + 0.01);
                    assert!(
                        (left.height() - h).abs() < 0.01,
                        "dials use the full height"
                    );
                    let (a, b) = (
                        spectrum.left() - left.right(),
                        right.left() - spectrum.right(),
                    );
                    assert!((a - b).abs() < 0.01, "same gap on both sides");
                }
                None => {
                    // As large as the area allows in one direction or the other.
                    let fills_height = (left.height() - h).abs() < 0.01;
                    let fills_width = (right.right() - left.left() - w).abs() < 0.01;
                    assert!(fills_height || fills_width, "{w}x{h}: {left:?}");
                }
            }
        }
    }

    #[test]
    fn the_meter_rests_when_nothing_plays_and_holds_the_peak_lamp() {
        let mut meter = MeterState::default();
        let start = Instant::now();
        assert!(!meter.update(None, start), "at rest: nothing to animate");
        let loud = crate::audio::Levels {
            average: [0.2, 0.0],
            peak: [1.0, 0.1],
        };
        assert!(meter.update(Some(loud), start + Duration::from_millis(16)));
        assert!(meter.update(Some(loud), start + Duration::from_millis(200)));
        assert!(meter.needles[0].position > 0.3 && meter.needles[1].position == 0.0);
        assert!(meter.peak_lit(0) && !meter.peak_lit(1));
        // Playback stops: the needle falls back and the lamp goes out, then all is still.
        let mut now = start + Duration::from_millis(200);
        let mut frames = 0;
        while meter.update(None, now) {
            now += Duration::from_millis(16);
            frames += 1;
            assert!(frames < 400, "the meter never came to rest");
        }
        assert!(meter.needles[0].position < 0.002 && !meter.peak_lit(0));
        assert!(now.duration_since(start) >= PEAK_LAMP_HOLD);
    }
}
