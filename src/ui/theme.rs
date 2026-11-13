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

/// Family name of the semibold UI face (always defined, see [`install_fonts`]).
const SEMIBOLD: &str = "oneum-semibold";
/// The macOS system UI font (San Francisco). A variable font: one file, several weights.
const SYSTEM_UI_FONT: &str = "/System/Library/Fonts/SFNS.ttf";
const SYSTEM_UI_Y_OFFSET: f32 = -0.06;

/// A semibold font for titles and the names of things that are playing.
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

/// Uses the system UI font for Latin text (Korean and other scripts come from eframe's
/// system-font fallback). If the system font cannot be read the bundled egui fonts are
/// used and "semibold" simply maps to the regular face.
fn install_fonts(ctx: &egui::Context) {
    use egui::epaint::text::{FontData, FontTweak, VariationCoords};
    let mut fonts = egui::FontDefinitions::default();
    let regular_stack = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let mut semibold_stack = regular_stack.clone();
    if let Ok(bytes) = std::fs::read(SYSTEM_UI_FONT) {
        let blob: egui::epaint::text::Blob = std::sync::Arc::new(bytes);
        let face = |weight: f32| {
            FontData::from_blob(blob.clone(), 0).tweak(FontTweak {
                coords: VariationCoords::new([("wght", weight)]),
                // San Francisco sits lower in its line box than the Korean fallback font;
                // raise it so mixed Korean/Latin text shares a baseline.
                y_offset_factor: SYSTEM_UI_Y_OFFSET,
                ..Default::default()
            })
        };
        fonts
            .font_data
            .insert("system-ui".into(), std::sync::Arc::new(face(430.0)));
        fonts.font_data.insert(
            "system-ui-semibold".into(),
            std::sync::Arc::new(face(620.0)),
        );
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "system-ui".into());
        semibold_stack.insert(0, "system-ui-semibold".into());
    }
    // Windows: Segoe UI for Latin text and Malgun Gothic for Korean, each with its bold
    // face for "semibold". Missing files are skipped (the bundled fonts remain).
    #[cfg(windows)]
    {
        let dir = std::env::var_os("WINDIR")
            .map_or_else(
                || std::path::PathBuf::from("C:\\Windows"),
                std::path::PathBuf::from,
            )
            .join("Fonts");
        let mut load = |name: &str, file: &str| -> bool {
            match std::fs::read(dir.join(file)) {
                Ok(bytes) => {
                    fonts.font_data.insert(
                        name.into(),
                        std::sync::Arc::new(FontData::from_owned(bytes)),
                    );
                    true
                }
                Err(_) => false,
            }
        };
        let latin = load("win-ui", "segoeui.ttf");
        let latin_bold = load("win-ui-semibold", "seguisb.ttf");
        let korean = load("win-korean", "malgun.ttf");
        let korean_bold = load("win-korean-bold", "malgunbd.ttf");
        let regular = fonts.families.entry(FontFamily::Proportional).or_default();
        if korean {
            regular.insert(0, "win-korean".into());
        }
        if latin {
            regular.insert(0, "win-ui".into());
        }
        if korean_bold {
            semibold_stack.insert(0, "win-korean-bold".into());
        } else if korean {
            semibold_stack.insert(0, "win-korean".into());
        }
        if latin_bold {
            semibold_stack.insert(0, "win-ui-semibold".into());
        } else if latin {
            semibold_stack.insert(0, "win-ui".into());
        }
    }
    fonts
        .families
        .insert(FontFamily::Name(SEMIBOLD.into()), semibold_stack);
    ctx.set_fonts(fonts);
}

/// One-time style setup for both themes.
pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    for theme in [Theme::Dark, Theme::Light] {
        let dark = theme == Theme::Dark;
        ctx.style_mut_of(theme, |style| {
            style.text_styles = [
                (
                    TextStyle::Heading,
                    FontId::new(22.0, FontFamily::Proportional),
                ),
                (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
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
