//! The equalizer sheet: ten bands and a pre-amp, the presets that come with the app, and
//! the user's own.
//!
//! Which preset is "selected" is not stored anywhere: it is the one whose values are
//! the current ones. Move a slider and the selection reads "custom" by itself.
use super::icons::Icon;
use super::{OneumApp, theme, widgets};
use crate::audio::eq::{self, BAND_HZ, BANDS, BUILTIN_PRESETS, GAIN_RANGE_DB};
use crate::audio::{EqPreset, EqSettings};
use crate::i18n::Strings;
use crate::playback::Command;
use eframe::egui::{self, Align, Color32, Layout, Rect, RichText, Sense, pos2, vec2};

const WIDTH: f32 = 680.0;
const SIDE_MARGIN: f32 = 20.0;
/// Width of the pre-amp's column, to the left of the bands.
const PREAMP_WIDTH: f32 = 76.0;
const CURVE_HEIGHT: f32 = 118.0;
const SLIDER_HEIGHT: f32 = 156.0;
/// The curve shows this much above and below 0 dB.
const CURVE_RANGE_DB: f32 = 15.0;
/// A slider moves in steps of this many dB.
const STEP_DB: f32 = 0.5;
/// How many presets of the user's own are kept, and how long a name may be.
pub(super) const MAX_USER_PRESETS: usize = 40;
const MAX_NAME_CHARS: usize = 32;

/// One of the presets on offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PresetRef {
    /// Index into [`BUILTIN_PRESETS`].
    Builtin(usize),
    /// Index into the user's presets.
    User(usize),
}

fn same(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.01
}

fn same_values(settings: &EqSettings, preamp_db: f32, gains_db: &[f32; BANDS]) -> bool {
    same(settings.preamp_db, preamp_db)
        && settings
            .gains_db
            .iter()
            .zip(gains_db)
            .all(|(a, b)| same(*a, *b))
}

/// The preset whose values are the current ones, if there is one. The user's own come
/// first: a preset saved under a name of one's own is shown under that name.
pub(super) fn matching_preset(settings: &EqSettings, user: &[EqPreset]) -> Option<PresetRef> {
    user.iter()
        .position(|p| same_values(settings, p.preamp_db, &p.gains_db))
        .map(PresetRef::User)
        .or_else(|| {
            BUILTIN_PRESETS
                .iter()
                .position(|p| same_values(settings, p.preamp_db(), &p.gains_db))
                .map(PresetRef::Builtin)
        })
}

/// The name of a preset that comes with the app, in the app's language.
pub(super) fn builtin_name(s: &Strings, id: &str) -> &'static str {
    match id {
        "flat" => s.eq_preset_flat,
        "bass_boost" => s.eq_preset_bass_boost,
        "bass_cut" => s.eq_preset_bass_cut,
        "treble_boost" => s.eq_preset_treble_boost,
        "vocal" => s.eq_preset_vocal,
        "loudness" => s.eq_preset_loudness,
        "pop" => s.eq_preset_pop,
        "rock" => s.eq_preset_rock,
        "jazz" => s.eq_preset_jazz,
        "classical" => s.eq_preset_classical,
        "electronic" => s.eq_preset_electronic,
        "acoustic" => s.eq_preset_acoustic,
        _ => s.eq_custom,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SaveOutcome {
    Added,
    /// A preset of that name was there already and now holds the new values.
    Replaced,
    NoName,
    /// There are [`MAX_USER_PRESETS`] already.
    Full,
}

/// A preset name as it is kept: trimmed, without line breaks, not longer than fits.
fn clean_name(name: &str) -> String {
    let one_line: String = name
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    one_line
        .trim()
        .chars()
        .take(MAX_NAME_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Saves the current values under `name` among the user's presets.
pub(super) fn save_preset(
    presets: &mut Vec<EqPreset>,
    name: &str,
    settings: &EqSettings,
) -> SaveOutcome {
    let name = clean_name(name);
    if name.is_empty() {
        return SaveOutcome::NoName;
    }
    let values = settings.sanitized();
    if let Some(existing) = presets.iter_mut().find(|p| p.name == name) {
        existing.preamp_db = values.preamp_db;
        existing.gains_db = values.gains_db;
        return SaveOutcome::Replaced;
    }
    if presets.len() >= MAX_USER_PRESETS {
        return SaveOutcome::Full;
    }
    presets.push(EqPreset {
        name,
        preamp_db: values.preamp_db,
        gains_db: values.gains_db,
    });
    SaveOutcome::Added
}

/// "+3.5", "−2.0", "0": a gain as it is written above a slider.
fn gain_text(db: f32) -> String {
    if same(db, 0.0) {
        "0".to_string()
    } else if db > 0.0 {
        format!("+{db:.1}")
    } else {
        format!("−{:.1}", -db)
    }
}

/// "32", "125", "1k", "16k": a band as it is written below its slider.
fn band_text(hz: f32) -> String {
    if hz >= 1000.0 {
        format!("{}k", (hz / 1000.0).round() as u32)
    } else {
        format!("{}", hz.round() as u32)
    }
}

/// The slider position (0 at the bottom, 1 at the top) as a gain, in whole steps.
fn gain_at(position: f32) -> f32 {
    let db = (position.clamp(0.0, 1.0) * 2.0 - 1.0) * GAIN_RANGE_DB;
    (db / STEP_DB).round() * STEP_DB
}

/// Where a gain sits on a slider or on the curve: 0 at `-range`, 1 at `+range`.
fn position_of(db: f32, range: f32) -> f32 {
    ((db / range).clamp(-1.0, 1.0) + 1.0) / 2.0
}

/// A vertical slider for one gain. Drag or click to set, double-click for 0 dB, arrow
/// keys for single steps.
fn gain_slider(
    ui: &mut egui::Ui,
    rect: Rect,
    salt: usize,
    value: &mut f32,
    enabled: bool,
    label: &str,
) -> egui::Response {
    let id = ui.scope_id().with(("eq-slider", salt));
    let sense = if enabled {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let mut response = ui.interact(rect, id, sense);
    let track = Rect::from_min_max(
        pos2(rect.center().x, rect.top() + 9.0),
        pos2(rect.center().x, rect.bottom() - 9.0),
    );
    let before = *value;
    if enabled {
        if response.double_clicked() {
            *value = 0.0;
        } else if response.is_pointer_button_down_on()
            && let Some(pointer) = response.interact_pointer_pos()
        {
            *value = gain_at((track.bottom() - pointer.y) / track.height());
        }
        if response.has_focus() {
            // The arrow keys belong to the slider while it has the focus.
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    id,
                    egui::EventFilter {
                        vertical_arrows: true,
                        ..Default::default()
                    },
                );
            });
            let steps = ui.input(|i| {
                i.num_presses(egui::Key::ArrowUp) as f32
                    - i.num_presses(egui::Key::ArrowDown) as f32
            });
            if steps != 0.0 {
                *value = (*value + steps * STEP_DB).clamp(-GAIN_RANGE_DB, GAIN_RANGE_DB);
            }
        }
    }
    if !same(*value, before) {
        response.mark_changed();
    }
    let shown = *value;
    response.widget_info(|| egui::WidgetInfo::slider(enabled, f64::from(shown), label));

    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        let dark = visuals.dark_mode;
        let fade = if enabled { 1.0 } else { 0.45 };
        let groove = if dark {
            Color32::from_rgb(0x44, 0x44, 0x4E)
        } else {
            Color32::from_rgb(0xC8, 0xC8, 0xD2)
        }
        .gamma_multiply(fade);
        let accent = theme::accent(dark).gamma_multiply(fade);
        let painter = ui.painter();
        painter.line_segment([track.min, track.max], egui::Stroke::new(3.0, groove));
        let zero_y = track.center().y;
        painter.line_segment(
            [
                pos2(track.min.x - 6.0, zero_y),
                pos2(track.min.x + 6.0, zero_y),
            ],
            egui::Stroke::new(1.0, groove),
        );
        let knob_y = track.bottom() - position_of(shown, GAIN_RANGE_DB) * track.height();
        let knob = pos2(track.min.x, knob_y);
        // The part between 0 dB and the knob is what the band adds or takes away.
        painter.line_segment(
            [pos2(track.min.x, zero_y), knob],
            egui::Stroke::new(3.0, accent),
        );
        let lively = enabled && (response.hovered() || response.dragged());
        let radius = if lively { 8.0 } else { 7.0 };
        let knob_fill = if dark {
            Color32::from_rgb(0xF2, 0xF2, 0xF7)
        } else {
            Color32::WHITE
        }
        .gamma_multiply(if enabled { 1.0 } else { 0.6 });
        painter.circle(knob, radius, knob_fill, egui::Stroke::new(1.5, accent));
        if response.has_focus() {
            painter.circle_stroke(knob, radius + 3.0, visuals.selection.stroke);
        }
    }
    response
}

/// The frequency at horizontal position `t` (0..=1) of the curve: the bands' centres
/// are evenly spaced, half a band's width in from either edge, so that each sits right
/// above its slider.
fn curve_hz(t: f32) -> f32 {
    let lowest = BAND_HZ[0] / std::f32::consts::SQRT_2;
    let octaves = BANDS as f32;
    lowest * 2f32.powf(t * octaves)
}

/// The frequency response of the settings, drawn over the sliders.
fn paint_curve(ui: &egui::Ui, rect: Rect, settings: &EqSettings, rate: u32) {
    let visuals = ui.visuals();
    let dark = visuals.dark_mode;
    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, 8.0, visuals.extreme_bg_color);
    let grid = widgets::divider_color(ui);
    let dim = theme::dim(ui);
    let y_of = |db: f32| rect.bottom() - position_of(db, CURVE_RANGE_DB) * rect.height();
    for db in [-12.0, -6.0, 0.0, 6.0, 12.0f32] {
        let strong = db == 0.0;
        let stroke = egui::Stroke::new(
            1.0,
            if strong {
                dim.gamma_multiply(0.7)
            } else {
                grid
            },
        );
        painter.line_segment(
            [pos2(rect.left(), y_of(db)), pos2(rect.right(), y_of(db))],
            stroke,
        );
    }
    for (db, text) in [(12.0, "+12"), (0.0, "0 dB"), (-12.0, "−12")] {
        painter.text(
            pos2(rect.left() + 6.0, y_of(db) - 1.0),
            egui::Align2::LEFT_BOTTOM,
            text,
            egui::FontId::proportional(9.5),
            dim,
        );
    }
    // The curve is that of the values on the sliders, also while the equalizer is
    // switched off: it shows what switching it on would do, in a duller colour.
    let shown = EqSettings {
        enabled: true,
        ..*settings
    };
    let accent = theme::accent(dark);
    let color = if settings.enabled { accent } else { dim };
    const POINTS: usize = 160;
    let points: Vec<egui::Pos2> = (0..=POINTS)
        .map(|i| {
            let t = i as f32 / POINTS as f32;
            let db = eq::response_db(&shown, rate, curve_hz(t));
            pos2(rect.left() + t * rect.width(), y_of(db))
        })
        .collect();
    // Filled between the curve and 0 dB.
    let zero = y_of(0.0);
    let fill = color.gamma_multiply(if settings.enabled { 0.22 } else { 0.10 });
    let mut mesh = egui::Mesh::default();
    for (i, pair) in points.windows(2).enumerate() {
        let base = (i * 4) as u32;
        mesh.colored_vertex(pair[0], fill);
        mesh.colored_vertex(pair[1], fill);
        mesh.colored_vertex(pos2(pair[1].x, zero), fill);
        mesh.colored_vertex(pos2(pair[0].x, zero), fill);
        mesh.add_triangle(base, base + 1, base + 2);
        mesh.add_triangle(base, base + 2, base + 3);
    }
    painter.add(egui::Shape::mesh(mesh));
    painter.add(egui::Shape::line(points, egui::Stroke::new(2.0, color)));
}

impl OneumApp {
    /// Opens the equalizer (closing the other sheets, which are in the same place).
    pub(super) fn open_equalizer(&mut self) {
        self.settings_open = false;
        self.guide_open = false;
        self.eq_open = true;
    }

    pub(super) fn equalizer_window(&mut self, ctx: &egui::Context) {
        if !self.eq_open {
            return;
        }
        let s = self.s();
        let style = ctx.global_style();
        let dark = style.visuals.dark_mode;
        let body_fill = if dark {
            Color32::from_rgb(0x1E, 0x1E, 0x23)
        } else {
            Color32::from_rgb(0xF4, 0xF4, 0xF7)
        };
        let frame = egui::Frame::window(&style)
            .inner_margin(0)
            .corner_radius(12.0)
            .fill(body_fill);
        let mut close = false;
        // Dim what is behind and let a click there close the sheet, like Settings.
        let screen = ctx.content_rect();
        let backdrop_clicked = egui::Area::new(egui::Id::unique("eq-backdrop"))
            .order(egui::Order::Background)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let response = ui.allocate_rect(screen, Sense::click());
                let alpha = if dark { 120 } else { 70 };
                ui.painter()
                    .rect_filled(screen, 0.0, Color32::from_black_alpha(alpha));
                response.clicked()
            })
            .inner;
        if backdrop_clicked && self.dialog.is_none() {
            close = true;
        }
        let before = self.settings.eq;
        egui::Window::new(s.equalizer)
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .frame(frame)
            .fixed_size([WIDTH, 0.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -12.0])
            .show(ctx, |ui| {
                ui.set_width(WIDTH);
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.add_space(SIDE_MARGIN);
                    ui.label(
                        RichText::new(s.equalizer)
                            .font(theme::semibold(16.0))
                            .color(ui.visuals().strong_text_color()),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.add_space(12.0);
                        if widgets::icon_button(ui, Icon::Close, s.close, 26.0, false).clicked() {
                            close = true;
                        }
                        ui.add_space(10.0);
                        widgets::toggle(ui, &mut self.settings.eq.enabled, s.equalizer);
                        ui.add_space(8.0);
                        let state = if self.settings.eq.enabled {
                            s.eq_on
                        } else {
                            s.eq_off
                        };
                        ui.label(RichText::new(state).small().color(theme::dim(ui)));
                    });
                });
                ui.add_space(10.0);
                widgets::row_divider(ui);
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.add_space(SIDE_MARGIN);
                    ui.vertical(|ui| {
                        ui.set_width(WIDTH - SIDE_MARGIN * 2.0);
                        self.equalizer_body(ui);
                    });
                });
                ui.add_space(16.0);
            });
        self.settings.eq = self.settings.eq.sanitized();
        if self.settings.eq != before {
            self.core.player.send(Command::SetEq(self.settings.eq));
        }
        if close {
            self.eq_open = false;
        }
    }

    fn equalizer_body(&mut self, ui: &mut egui::Ui) {
        let s = self.s();
        let dim = theme::dim(ui);
        let current = matching_preset(&self.settings.eq, &self.settings.eq_presets);

        // Preset: what is chosen, and back to flat.
        let mut choose: Option<PresetRef> = None;
        let mut reset = false;
        ui.horizontal(|ui| {
            ui.label(RichText::new(s.eq_preset).color(dim));
            ui.add_space(8.0);
            let selected = match current {
                Some(PresetRef::Builtin(i)) => builtin_name(s, BUILTIN_PRESETS[i].id).to_string(),
                Some(PresetRef::User(i)) => self.settings.eq_presets[i].name.clone(),
                None => s.eq_custom.to_string(),
            };
            egui::ComboBox::from_id_salt("eq-preset")
                .width(230.0)
                .height(420.0)
                .selected_text(selected)
                .show_ui(ui, |ui| {
                    for (i, preset) in BUILTIN_PRESETS.iter().enumerate() {
                        let this = PresetRef::Builtin(i);
                        let name = builtin_name(s, preset.id);
                        if ui.selectable_label(current == Some(this), name).clicked() {
                            choose = Some(this);
                        }
                    }
                    if !self.settings.eq_presets.is_empty() {
                        ui.separator();
                        ui.label(RichText::new(s.eq_my_presets).small().color(dim));
                        for (i, preset) in self.settings.eq_presets.iter().enumerate() {
                            let this = PresetRef::User(i);
                            if ui
                                .selectable_label(current == Some(this), &preset.name)
                                .clicked()
                            {
                                choose = Some(this);
                            }
                        }
                    }
                });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let flat = same_values(&self.settings.eq, 0.0, &[0.0; BANDS]);
                let button = egui::Button::new(s.eq_reset).min_size(vec2(0.0, 26.0));
                reset = ui.add_enabled(!flat, button).clicked();
            });
        });
        if reset {
            choose = Some(PresetRef::Builtin(0));
        }
        if let Some(choice) = choose {
            let (preamp_db, gains_db) = match choice {
                PresetRef::Builtin(i) => {
                    (BUILTIN_PRESETS[i].preamp_db(), BUILTIN_PRESETS[i].gains_db)
                }
                PresetRef::User(i) => {
                    let preset = &self.settings.eq_presets[i];
                    (preset.preamp_db, preset.gains_db)
                }
            };
            self.settings.eq.preamp_db = preamp_db;
            self.settings.eq.gains_db = gains_db;
            // Choosing a preset is asking to hear it.
            if !reset {
                self.settings.eq.enabled = true;
            }
        }
        ui.add_space(12.0);

        // The curve, and under it the pre-amp and the ten bands.
        let enabled = self.settings.eq.enabled;
        let rate = self.player.output_sample_rate.unwrap_or(48_000);
        widgets::card(ui, |ui| {
            let value_row = 18.0;
            let name_row = 18.0;
            let height = CURVE_HEIGHT + 12.0 + value_row + SLIDER_HEIGHT + name_row;
            let (area, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
            let bands_left = area.left() + PREAMP_WIDTH;
            let band_width = (area.right() - bands_left) / BANDS as f32;
            let curve = Rect::from_min_max(
                pos2(bands_left, area.top()),
                pos2(area.right(), area.top() + CURVE_HEIGHT),
            );
            paint_curve(ui, curve, &self.settings.eq, rate);

            let sliders_top = curve.bottom() + 12.0 + value_row;
            let column = |left: f32, width: f32| {
                Rect::from_min_size(pos2(left, sliders_top), vec2(width, SLIDER_HEIGHT))
            };
            let text_color = if enabled {
                ui.visuals().text_color()
            } else {
                dim
            };
            let labelled_slider = |ui: &mut egui::Ui,
                                   rect: Rect,
                                   salt: usize,
                                   value: &mut f32,
                                   name: &str,
                                   spoken: &str| {
                gain_slider(ui, rect, salt, value, enabled, spoken);
                let painter = ui.painter();
                painter.text(
                    pos2(rect.center().x, rect.top() - 4.0),
                    egui::Align2::CENTER_BOTTOM,
                    gain_text(*value),
                    egui::FontId::proportional(10.5),
                    text_color,
                );
                painter.text(
                    pos2(rect.center().x, rect.bottom() + 4.0),
                    egui::Align2::CENTER_TOP,
                    name,
                    egui::FontId::proportional(11.0),
                    dim,
                );
            };
            let preamp = column(area.left(), PREAMP_WIDTH - 12.0);
            labelled_slider(
                ui,
                preamp,
                BANDS,
                &mut self.settings.eq.preamp_db,
                s.eq_preamp,
                s.eq_preamp,
            );
            // A line between the pre-amp and the bands.
            ui.painter().line_segment(
                [
                    pos2(bands_left - 8.0, sliders_top - value_row),
                    pos2(bands_left - 8.0, sliders_top + SLIDER_HEIGHT + name_row),
                ],
                egui::Stroke::new(1.0, widgets::divider_color(ui)),
            );
            let bands = BAND_HZ.iter().zip(self.settings.eq.gains_db.iter_mut());
            for (band, (hz, gain)) in bands.enumerate() {
                let rect = column(bands_left + band_width * band as f32, band_width);
                let name = band_text(*hz);
                let spoken = format!("{name} Hz");
                labelled_slider(ui, rect, band, gain, &name, &spoken);
            }
        });
        ui.add_space(12.0);

        // The user's own presets: save what is set under a name, delete the chosen one.
        let mut save = false;
        let mut delete = None;
        ui.horizontal(|ui| {
            let field = egui::TextEdit::singleline(&mut self.eq_name)
                .hint_text(s.eq_name_hint)
                .char_limit(MAX_NAME_CHARS)
                .desired_width(230.0);
            let typed = ui.add(field);
            let named = !clean_name(&self.eq_name).is_empty();
            if typed.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && named {
                save = true;
            }
            ui.add_space(6.0);
            let button = egui::Button::new(s.eq_save).min_size(vec2(0.0, 26.0));
            save |= ui.add_enabled(named, button).clicked();
            if let Some(PresetRef::User(i)) = current {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let button = egui::Button::new(s.eq_delete).min_size(vec2(0.0, 26.0));
                    if ui.add(button).clicked() {
                        delete = Some(i);
                    }
                });
            }
        });
        if save {
            let name = std::mem::take(&mut self.eq_name);
            let outcome = save_preset(&mut self.settings.eq_presets, &name, &self.settings.eq);
            match outcome {
                SaveOutcome::Added => self.info_toast(s.eq_saved),
                SaveOutcome::Replaced => self.info_toast(s.eq_replaced),
                SaveOutcome::Full => {
                    self.eq_name = name;
                    self.warn_toast(s.eq_full);
                }
                SaveOutcome::NoName => {}
            }
        }
        if let Some(index) = delete
            && index < self.settings.eq_presets.len()
        {
            // The values stay on the sliders: only the name goes.
            self.settings.eq_presets.remove(index);
            self.info_toast(s.eq_deleted);
        }
        ui.add_space(10.0);
        ui.add(egui::Label::new(RichText::new(s.eq_note).small().color(dim)).wrap());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{EN, KO};

    fn set(preamp_db: f32, gains_db: [f32; BANDS]) -> EqSettings {
        EqSettings {
            enabled: true,
            preamp_db,
            gains_db,
        }
    }

    #[test]
    fn the_selected_preset_is_the_one_whose_values_are_set() {
        let rock = BUILTIN_PRESETS
            .iter()
            .position(|p| p.id == "rock")
            .expect("rock is there");
        let mut now = set(
            BUILTIN_PRESETS[rock].preamp_db(),
            BUILTIN_PRESETS[rock].gains_db,
        );
        assert_eq!(matching_preset(&now, &[]), Some(PresetRef::Builtin(rock)));
        // Whether the equalizer is on does not come into it.
        now.enabled = false;
        assert_eq!(matching_preset(&now, &[]), Some(PresetRef::Builtin(rock)));
        // One slider moved: nobody's preset any more.
        now.gains_db[4] += 0.5;
        assert_eq!(matching_preset(&now, &[]), None);
        // Saved under a name, it is that preset; and a preset of one's own with the
        // values of one that comes with the app goes by its own name.
        let mut mine = Vec::new();
        assert_eq!(
            save_preset(&mut mine, "내 스피커", &now),
            SaveOutcome::Added
        );
        assert_eq!(matching_preset(&now, &mine), Some(PresetRef::User(0)));
        let flat = EqSettings::default();
        assert_eq!(matching_preset(&flat, &mine), Some(PresetRef::Builtin(0)));
        assert_eq!(
            save_preset(&mut mine, "아무것도 안 함", &flat),
            SaveOutcome::Added
        );
        assert_eq!(matching_preset(&flat, &mine), Some(PresetRef::User(1)));
    }

    #[test]
    fn saving_a_preset() {
        let mut mine = Vec::new();
        let first = set(-3.0, [3.0, 2.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 2.0]);
        assert_eq!(save_preset(&mut mine, "", &first), SaveOutcome::NoName);
        assert_eq!(save_preset(&mut mine, "  \n ", &first), SaveOutcome::NoName);
        assert_eq!(
            save_preset(&mut mine, "  거실\n스피커  ", &first),
            SaveOutcome::Added
        );
        assert_eq!(mine[0].name, "거실 스피커");
        assert_eq!((mine[0].preamp_db, mine[0].gains_db[0]), (-3.0, 3.0));
        // The same name again: the preset gets the new values, there is still one.
        let second = set(0.0, [0.0; BANDS]);
        assert_eq!(
            save_preset(&mut mine, "거실 스피커", &second),
            SaveOutcome::Replaced
        );
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].gains_db, [0.0; BANDS]);
        // A very long name is cut; values out of range are brought back.
        let long = "가".repeat(100);
        let wild = set(40.0, [-40.0; BANDS]);
        assert_eq!(save_preset(&mut mine, &long, &wild), SaveOutcome::Added);
        assert_eq!(mine[1].name.chars().count(), MAX_NAME_CHARS);
        assert_eq!((mine[1].preamp_db, mine[1].gains_db[9]), (12.0, -12.0));
        // There is room for so many and no more.
        for i in mine.len()..MAX_USER_PRESETS {
            assert_eq!(
                save_preset(&mut mine, &format!("p{i}"), &first),
                SaveOutcome::Added
            );
        }
        assert_eq!(
            save_preset(&mut mine, "one too many", &first),
            SaveOutcome::Full
        );
        assert_eq!(mine.len(), MAX_USER_PRESETS);
        // Replacing one of them still works when full.
        assert_eq!(
            save_preset(&mut mine, "거실 스피커", &first),
            SaveOutcome::Replaced
        );
    }

    #[test]
    fn every_preset_that_comes_with_the_app_has_a_name_in_both_languages() {
        let mut names = std::collections::HashSet::new();
        for preset in BUILTIN_PRESETS {
            for strings in [&KO, &EN] {
                let name = builtin_name(strings, preset.id);
                assert!(!name.is_empty());
                assert_ne!(name, strings.eq_custom, "{} has no name", preset.id);
                assert!(names.insert(name), "{name} twice");
            }
        }
    }

    #[test]
    fn sliders_move_in_half_decibel_steps_over_the_whole_range() {
        assert_eq!(gain_at(0.0), -12.0);
        assert_eq!(gain_at(0.5), 0.0);
        assert_eq!(gain_at(1.0), 12.0);
        assert_eq!(gain_at(2.0), 12.0);
        assert_eq!(gain_at(0.51), 0.0);
        assert_eq!(gain_at(0.53), 0.5);
        for db in [-12.0, -3.5, 0.0, 6.0, 12.0] {
            assert_eq!(gain_at(position_of(db, GAIN_RANGE_DB)), db);
        }
        assert_eq!(
            (gain_text(0.0), gain_text(3.5), gain_text(-2.0)),
            ("0".into(), "+3.5".into(), "−2.0".into())
        );
        let names: Vec<String> = BAND_HZ.iter().map(|hz| band_text(*hz)).collect();
        assert_eq!(
            names,
            [
                "32", "64", "125", "250", "500", "1k", "2k", "4k", "8k", "16k"
            ]
        );
    }

    #[test]
    fn each_band_sits_above_its_slider_in_the_curve() {
        // Band `i` of ten is centred at (i + 0.5) / 10 of the width.
        for (i, hz) in BAND_HZ.iter().enumerate() {
            let at = curve_hz((i as f32 + 0.5) / BANDS as f32);
            // The nominal centres are rounded numbers (125 for 128, 16 000 for 16 384).
            assert!((at / hz - 1.0).abs() < 0.03, "band {hz}: {at}");
        }
    }
}
