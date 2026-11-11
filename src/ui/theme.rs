//! Colours, spacing and text sizes for both light and dark appearance.
use crate::app::settings::ThemeChoice;
use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Theme, ThemePreference,
};

pub const ROW_HEIGHT: f32 = 26.0;
pub const PLAYER_BAR_HEIGHT: f32 = 76.0;
pub const TOP_BAR_HEIGHT: f32 = 44.0;

pub fn accent(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(0x9A, 0x97, 0xF5)
    } else {
        Color32::from_rgb(0x53, 0x4F, 0xD6)
    }
}

/// Text colour for de-emphasised content (secondary lines, unavailable tracks).
pub fn dim(ui: &egui::Ui) -> Color32 {
    ui.visuals().weak_text_color()
}

pub fn apply_choice(ctx: &egui::Context, choice: ThemeChoice) {
    ctx.set_theme(match choice {
        ThemeChoice::System => ThemePreference::System,
        ThemeChoice::Light => ThemePreference::Light,
        ThemeChoice::Dark => ThemePreference::Dark,
    });
}

/// One-time style setup for both themes.
pub fn install(ctx: &egui::Context) {
    for theme in [Theme::Dark, Theme::Light] {
        let dark = theme == Theme::Dark;
        ctx.style_mut_of(theme, |style| {
            style.text_styles = [
                (
                    TextStyle::Heading,
                    FontId::new(22.0, FontFamily::Proportional),
                ),
                (TextStyle::Body, FontId::new(13.5, FontFamily::Proportional)),
                (
                    TextStyle::Button,
                    FontId::new(13.5, FontFamily::Proportional),
                ),
                (
                    TextStyle::Small,
                    FontId::new(11.5, FontFamily::Proportional),
                ),
                (
                    TextStyle::Monospace,
                    FontId::new(12.5, FontFamily::Monospace),
                ),
            ]
            .into();
            style.spacing.item_spacing = egui::vec2(8.0, 6.0);
            style.spacing.button_padding = egui::vec2(10.0, 5.0);
            style.spacing.interact_size.y = 24.0;
            style.spacing.scroll.bar_width = 8.0;
            style.interaction.selectable_labels = false;

            let v = &mut style.visuals;
            let accent = accent(dark);
            let radius = CornerRadius::same(6);
            v.widgets.noninteractive.corner_radius = radius;
            v.widgets.inactive.corner_radius = radius;
            v.widgets.hovered.corner_radius = radius;
            v.widgets.active.corner_radius = radius;
            v.widgets.open.corner_radius = radius;
            v.window_corner_radius = CornerRadius::same(10);
            v.menu_corner_radius = CornerRadius::same(8);
            v.selection.bg_fill = accent.gamma_multiply(if dark { 0.45 } else { 0.28 });
            // Text of selected rows uses this colour: keep it neutral so that "selected"
            // (background) and "now playing" (accent text) remain distinguishable.
            let strong = if dark {
                Color32::from_rgb(0xF5, 0xF5, 0xF7)
            } else {
                Color32::from_rgb(0x12, 0x12, 0x16)
            };
            v.selection.stroke = Stroke::new(1.0, strong);
            v.hyperlink_color = accent;
            v.widgets.hovered.bg_stroke = Stroke::new(1.0, accent.gamma_multiply(0.6));
            v.widgets.active.bg_stroke = Stroke::new(1.0, accent);
            if dark {
                v.panel_fill = Color32::from_rgb(0x1C, 0x1C, 0x20);
                v.window_fill = Color32::from_rgb(0x26, 0x26, 0x2B);
                v.extreme_bg_color = Color32::from_rgb(0x13, 0x13, 0x16);
                v.faint_bg_color = Color32::from_rgb(0x21, 0x21, 0x26);
            } else {
                v.panel_fill = Color32::from_rgb(0xFA, 0xFA, 0xFB);
                v.window_fill = Color32::from_rgb(0xFF, 0xFF, 0xFF);
                v.extreme_bg_color = Color32::from_rgb(0xFF, 0xFF, 0xFF);
                v.faint_bg_color = Color32::from_rgb(0xF2, 0xF2, 0xF5);
            }
        });
    }
}

/// Background of the sidebar and the player bar (slightly set off from the content).
pub fn chrome_fill(ui: &egui::Ui) -> Color32 {
    if ui.visuals().dark_mode {
        Color32::from_rgb(0x16, 0x16, 0x1A)
    } else {
        Color32::from_rgb(0xF0, 0xF0, 0xF3)
    }
}
