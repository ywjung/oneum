//! Small reusable widgets. Every icon-only control gets a tooltip and an accessibility name.
use super::icons::{self, Icon};
use super::theme;
use eframe::egui::{
    self, Align, Color32, Layout, Response, Role, Sense, Ui, UiBuilder, Vec2, WidgetInfo,
};

/// An icon-only button. `label` is used for the tooltip and for VoiceOver.
pub fn icon_button(ui: &mut Ui, icon: Icon, label: &str, size: f32, active: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    let label_owned = label.to_string();
    response.widget_info(move || WidgetInfo::labeled(Role::Button, true, &label_owned));
    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        let dark = visuals.dark_mode;
        if response.hovered() || response.has_focus() {
            ui.painter()
                .rect_filled(rect, 6.0, visuals.widgets.hovered.weak_bg_fill);
        }
        if response.has_focus() {
            ui.painter().rect_stroke(
                rect,
                6.0,
                visuals.selection.stroke,
                egui::StrokeKind::Inside,
            );
        }
        let color = if active {
            theme::accent(dark)
        } else if response.is_pointer_button_down_on() {
            visuals.strong_text_color()
        } else {
            visuals.text_color()
        };
        icons::paint(ui.painter(), rect.shrink(size * 0.2), icon, color);
    }
    response.on_hover_text(label)
}

/// A filled circular button for the primary transport control.
pub fn primary_icon_button(ui: &mut Ui, icon: Icon, label: &str, size: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    let label_owned = label.to_string();
    response.widget_info(move || WidgetInfo::labeled(Role::Button, true, &label_owned));
    if ui.is_rect_visible(rect) {
        let dark = ui.visuals().dark_mode;
        let mut fill = theme::accent(dark);
        if response.hovered() {
            fill = fill.gamma_multiply(0.88);
        }
        ui.painter().circle_filled(rect.center(), size / 2.0, fill);
        if response.has_focus() {
            ui.painter().circle_stroke(
                rect.center(),
                size / 2.0 + 2.0,
                ui.visuals().selection.stroke,
            );
        }
        let ink = if dark {
            Color32::from_rgb(0x14, 0x14, 0x18)
        } else {
            Color32::WHITE
        };
        icons::paint(ui.painter(), rect.shrink(size * 0.27), icon, ink);
    }
    response.on_hover_text(label)
}

/// Placeholder shown where a cover image would be.
pub fn art_placeholder(ui: &Ui, rect: egui::Rect, radius: f32) {
    let v = ui.visuals();
    ui.painter().rect_filled(rect, radius, v.faint_bg_color);
    ui.painter().rect_stroke(
        rect,
        radius,
        v.widgets.noninteractive.bg_stroke,
        egui::StrokeKind::Inside,
    );
    let side = rect.width().min(rect.height());
    icons::paint(
        ui.painter(),
        egui::Rect::from_center_size(rect.center(), Vec2::splat(side * 0.42)),
        Icon::Note,
        v.weak_text_color(),
    );
}

/// Paints a cover texture (or the placeholder) into `rect`.
pub fn art(ui: &Ui, rect: egui::Rect, texture: Option<egui::TextureId>, radius: f32) {
    match texture {
        Some(id) => {
            let image = egui::Image::new(egui::load::SizedTexture::new(id, rect.size()))
                .corner_radius(radius);
            image.paint_at(ui, rect);
        }
        None => art_placeholder(ui, rect, radius),
    }
}

/// Centered message for empty views.
pub fn empty_state(ui: &mut Ui, icon: Icon, title: &str, body: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.28).max(24.0));
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(44.0), Sense::hover());
        icons::paint(ui.painter(), rect, icon, ui.visuals().weak_text_color());
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(title)
                .font(theme::semibold(17.0))
                .strong(),
        );
        if !body.is_empty() {
            ui.label(egui::RichText::new(body).color(theme::dim(ui)));
        }
    });
}

/// A single-line label that truncates with an ellipsis (full text on hover).
pub fn truncated(ui: &mut Ui, text: impl Into<egui::WidgetText>) -> Response {
    ui.add(egui::Label::new(text).truncate())
}

// ----- settings building blocks ---------------------------------------------------------

/// A small heading above a group of settings.
pub fn section_title(ui: &mut Ui, text: &str) {
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(text)
                .font(theme::semibold(12.0))
                .color(theme::dim(ui)),
        );
    });
    ui.add_space(4.0);
}

/// Explanatory text below a group of settings.
pub fn section_note(ui: &mut Ui, text: &str) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        ui.add(egui::Label::new(egui::RichText::new(text).small().color(theme::dim(ui))).wrap());
    });
}

/// A rounded panel that groups related rows.
pub fn card<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    let visuals = ui.visuals();
    let fill = if visuals.dark_mode {
        Color32::from_rgb(0x26, 0x26, 0x2C)
    } else {
        Color32::WHITE
    };
    let border = if visuals.dark_mode {
        Color32::from_rgb(0x36, 0x36, 0x3E)
    } else {
        Color32::from_rgb(0xDD, 0xDD, 0xE3)
    };
    egui::Frame::new()
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, border))
        .corner_radius(10.0)
        .inner_margin(egui::Margin::symmetric(14, 2))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            add_contents(ui)
        })
        .inner
}

/// A hairline between two rows of a [`card`].
pub fn row_divider(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), Sense::hover());
    let color = divider_color(ui);
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        egui::Stroke::new(1.0, color),
    );
}

/// The colour of the thin lines between the rows of a card.
pub fn divider_color(ui: &Ui) -> Color32 {
    if ui.visuals().dark_mode {
        Color32::from_rgb(0x34, 0x34, 0x3C)
    } else {
        Color32::from_rgb(0xE4, 0xE4, 0xEA)
    }
}

/// One setting: what it is on the left (title, optional explanation), its control on the
/// right. The control is laid out right to left, so add the right-most widget first.
pub fn setting_row(
    ui: &mut Ui,
    title: &str,
    description: Option<&str>,
    enabled: bool,
    add_control: impl FnOnce(&mut Ui),
) {
    let height = if description.is_some() { 54.0 } else { 42.0 };
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::hover());
    let mut control_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::right_to_left(Align::Center)),
    );
    if !enabled {
        control_ui.disable();
    }
    add_control(&mut control_ui);
    let control_left = control_ui.min_rect().left().min(rect.right());

    // Text block, centred vertically as a whole and never running under the control.
    let title_height = 18.0;
    let desc_height = if description.is_some() { 16.0 } else { 0.0 };
    let block = title_height + desc_height;
    let text_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left(), rect.center().y - block / 2.0),
        egui::pos2((control_left - 12.0).max(rect.left() + 60.0), rect.bottom()),
    );
    let mut text_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(text_rect)
            .layout(Layout::top_down(Align::Min)),
    );
    text_ui.spacing_mut().item_spacing.y = 0.0;
    let title_color = if enabled {
        text_ui.visuals().text_color()
    } else {
        theme::dim(&text_ui)
    };
    text_ui.add(egui::Label::new(egui::RichText::new(title).color(title_color)).truncate());
    if let Some(description) = description {
        let dim = theme::dim(&text_ui);
        text_ui
            .add(egui::Label::new(egui::RichText::new(description).small().color(dim)).truncate());
    }
}

/// A row of mutually exclusive choices drawn as one control (like a macOS segmented
/// control). Returns true when the choice changed. Each segment is a radio button for
/// assistive technology and can be reached with Tab.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut Ui,
    id_salt: &str,
    current: &mut T,
    options: &[(T, &str)],
) -> bool {
    const HEIGHT: f32 = 28.0;
    const PAD: f32 = 13.0;
    let font = egui::TextStyle::Body.resolve(ui.style());
    let widths: Vec<f32> = options
        .iter()
        .map(|(_, label)| {
            let galley =
                ui.painter()
                    .layout_no_wrap((*label).to_string(), font.clone(), Color32::WHITE);
            (galley.size().x + PAD * 2.0).max(48.0)
        })
        .collect();
    let total: f32 = widths.iter().sum::<f32>() + 4.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(total, HEIGHT), Sense::hover());
    let visuals = ui.visuals().clone();
    let dark = visuals.dark_mode;
    let track = if dark {
        Color32::from_rgb(0x1A, 0x1A, 0x1F)
    } else {
        Color32::from_rgb(0xE8, 0xE8, 0xEE)
    };
    ui.painter().rect_filled(rect, 8.0, track);
    let mut changed = false;
    let mut x = rect.left() + 2.0;
    for (index, ((value, label), width)) in options.iter().zip(&widths).enumerate() {
        let seg = egui::Rect::from_min_size(
            egui::pos2(x, rect.top() + 2.0),
            egui::vec2(*width, HEIGHT - 4.0),
        );
        x += width;
        let response = ui.interact(seg, ui.scope_id().with((id_salt, index)), Sense::click());
        let selected = *current == *value;
        let enabled = ui.is_enabled();
        response.widget_info(|| WidgetInfo::selected(Role::RadioButton, enabled, selected, *label));
        if response.clicked() && !selected {
            *current = *value;
            changed = true;
        }
        if selected {
            let fill = if dark {
                Color32::from_rgb(0x4A, 0x48, 0x78)
            } else {
                Color32::WHITE
            };
            ui.painter().rect_filled(seg, 6.0, fill);
            if !dark {
                ui.painter().rect_stroke(
                    seg,
                    6.0,
                    egui::Stroke::new(1.0, Color32::from_rgb(0xCF, 0xCF, 0xD8)),
                    egui::StrokeKind::Inside,
                );
            }
        } else if response.hovered() && enabled {
            ui.painter()
                .rect_filled(seg, 6.0, visuals.widgets.hovered.weak_bg_fill);
        }
        if response.has_focus() {
            ui.painter()
                .rect_stroke(seg, 6.0, visuals.selection.stroke, egui::StrokeKind::Inside);
        }
        let color = if !enabled {
            visuals.weak_text_color()
        } else if selected {
            visuals.strong_text_color()
        } else {
            visuals.text_color()
        };
        ui.painter().text(
            seg.center(),
            egui::Align2::CENTER_CENTER,
            *label,
            font.clone(),
            color,
        );
    }
    changed
}

/// An on/off switch. `label` names it for assistive technology (the visible title is
/// drawn by the row it sits in).
pub fn toggle(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(egui::vec2(38.0, 22.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let enabled = ui.is_enabled();
    let state = *on;
    response.widget_info(|| WidgetInfo::selected(Role::Switch, enabled, state, label));
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(response.id, *on);
        let visuals = ui.visuals();
        let dark = visuals.dark_mode;
        let off = if dark {
            Color32::from_rgb(0x4A, 0x4A, 0x52)
        } else {
            Color32::from_rgb(0xC9, 0xC9, 0xD1)
        };
        let on_color = theme::accent(dark);
        let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t) as u8;
        let mut fill = Color32::from_rgb(
            mix(off.r(), on_color.r()),
            mix(off.g(), on_color.g()),
            mix(off.b(), on_color.b()),
        );
        if !enabled {
            fill = fill.gamma_multiply(0.5);
        }
        ui.painter().rect_filled(rect, 11.0, fill);
        if response.has_focus() {
            ui.painter().rect_stroke(
                rect.expand(2.0),
                13.0,
                visuals.selection.stroke,
                egui::StrokeKind::Outside,
            );
        }
        let knob_x = rect.left() + 11.0 + (rect.width() - 22.0) * t;
        ui.painter()
            .circle_filled(egui::pos2(knob_x, rect.center().y), 9.0, Color32::WHITE);
    }
    response
}

/// A tab of the settings window: icon and label, highlighted when selected.
pub fn tab_button(ui: &mut Ui, selected: bool, icon: Icon, label: &str) -> Response {
    // Measured with the heavier face so the tab keeps its width when it becomes selected.
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_string(), theme::semibold(13.0), Color32::WHITE);
    let font = if selected {
        theme::semibold(13.0)
    } else {
        egui::FontId::proportional(13.0)
    };
    let size = egui::vec2(galley.size().x + 14.0 + 8.0 + 24.0, 32.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::Tab, true, selected, label));
    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        let dark = visuals.dark_mode;
        if selected {
            ui.painter()
                .rect_filled(rect, 8.0, visuals.selection.bg_fill);
        } else if response.hovered() {
            ui.painter()
                .rect_filled(rect, 8.0, visuals.widgets.hovered.weak_bg_fill);
        }
        if response.has_focus() {
            ui.painter().rect_stroke(
                rect,
                8.0,
                visuals.selection.stroke,
                egui::StrokeKind::Inside,
            );
        }
        let (text, ink) = if selected {
            (visuals.strong_text_color(), theme::accent(dark))
        } else {
            (visuals.text_color(), visuals.weak_text_color())
        };
        let icon_rect = egui::Rect::from_center_size(
            egui::pos2(rect.left() + 12.0 + 7.0, rect.center().y),
            Vec2::splat(14.0),
        );
        icons::paint(ui.painter(), icon_rect, icon, ink);
        ui.painter().text(
            egui::pos2(icon_rect.right() + 8.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            font,
            text,
        );
    }
    response
}

/// A keyboard shortcut drawn as key caps: "⇧ ⌘ R" becomes three caps. Groups separated
/// by three spaces ("↑ ↓   ⇧ ↑ ↓") are alternatives, drawn with a gap between them.
/// Expects a right-to-left layout (the caps end at the right edge).
pub fn key_caps(ui: &mut Ui, keys: &str) {
    let dark = ui.visuals().dark_mode;
    let (fill, border) = if dark {
        (
            Color32::from_rgb(0x36, 0x36, 0x3E),
            Color32::from_rgb(0x4A, 0x4A, 0x54),
        )
    } else {
        (
            Color32::from_rgb(0xF2, 0xF2, 0xF6),
            Color32::from_rgb(0xD0, 0xD0, 0xD8),
        )
    };
    let text = ui.visuals().text_color();
    let font = egui::FontId::proportional(12.0);
    ui.spacing_mut().item_spacing.x = 3.0;
    let groups: Vec<&str> = keys.split("   ").collect();
    for (g, group) in groups.iter().enumerate().rev() {
        for key in group.split(' ').filter(|k| !k.is_empty()).rev() {
            // The Mac's modifier symbols are spelled out on other keyboards.
            let key = crate::platform::key_names(key);
            let galley = ui
                .painter()
                .layout_no_wrap(key.into_owned(), font.clone(), text);
            let width = (galley.size().x + 12.0).max(22.0);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 22.0), Sense::hover());
            ui.painter().rect_filled(rect, 5.0, fill);
            ui.painter().rect_stroke(
                rect,
                5.0,
                egui::Stroke::new(1.0, border),
                egui::StrokeKind::Inside,
            );
            ui.painter().galley(
                egui::pos2(
                    rect.center().x - galley.size().x / 2.0,
                    rect.center().y - galley.size().y / 2.0,
                ),
                galley,
                text,
            );
        }
        if g > 0 {
            ui.add_space(8.0);
        }
    }
}
