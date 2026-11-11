//! Small reusable widgets. Every icon-only control gets a tooltip and an accessibility name.
use super::icons::{self, Icon};
use super::theme;
use eframe::egui::{self, Color32, Response, Role, Sense, Ui, Vec2, WidgetInfo};

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
        ui.label(egui::RichText::new(title).size(17.0).strong());
        if !body.is_empty() {
            ui.label(egui::RichText::new(body).color(theme::dim(ui)));
        }
    });
}

/// A single-line label that truncates with an ellipsis (full text on hover).
pub fn truncated(ui: &mut Ui, text: impl Into<egui::WidgetText>) -> Response {
    ui.add(egui::Label::new(text).truncate())
}
