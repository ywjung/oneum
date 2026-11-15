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

/// Where [`touch`] is kept in the context's data.
const TOUCH: &str = "oneum-touch";

/// True on the phone screens: what is shown there is read at arm's length and hit with
/// a finger, so the widgets that both kinds of screen share draw themselves larger.
pub fn touch(ctx: &egui::Context) -> bool {
    ctx.data(|data| data.get_temp::<bool>(egui::Id::unique(TOUCH)))
        .unwrap_or(false)
}

/// Text colour for de-emphasised content (secondary lines, unavailable tracks).
pub fn dim(ui: &egui::Ui) -> Color32 {
    ui.visuals().weak_text_color()
}

pub fn apply_choice(ctx: &egui::Context, choice: ThemeChoice) {
    // What the system draws for the app (the status bar's text, the file picker)
    // follows the same choice.
    crate::platform::window::set_appearance(match choice {
        ThemeChoice::System => None,
        ThemeChoice::Light => Some(false),
        ThemeChoice::Dark => Some(true),
    });
    ctx.set_theme(match choice {
        ThemeChoice::System => ThemePreference::System,
        ThemeChoice::Light => ThemePreference::Light,
        ThemeChoice::Dark => ThemePreference::Dark,
    });
}

/// Family name of the semibold UI face (always defined, see [`install_fonts`]).
const SEMIBOLD: &str = "oneum-semibold";
/// The system UI font: San Francisco on macOS and iOS, Roboto on Android. A variable
/// font: one file, several weights. (Windows has its fonts in files of their own, see
/// `install_fonts`.)
const SYSTEM_UI_FONT: &str = if cfg!(target_os = "ios") {
    "/System/Library/Fonts/Core/SFUI.ttf"
} else if cfg!(target_os = "android") {
    "/system/fonts/Roboto-Regular.ttf"
} else {
    "/System/Library/Fonts/SFNS.ttf"
};

/// The system UI font's file, if it can be read. (In the iOS Simulator the system's
/// files are below the folder `SIMULATOR_ROOT` names.)
fn system_ui_font() -> Option<Vec<u8>> {
    let root = std::env::var_os("SIMULATOR_ROOT").filter(|_| cfg!(target_os = "ios"));
    let path = match root {
        Some(root) => std::path::PathBuf::from(root).join(SYSTEM_UI_FONT.trim_start_matches('/')),
        None => std::path::PathBuf::from(SYSTEM_UI_FONT),
    };
    std::fs::read(path).ok()
}
/// How far the system UI font is raised in its line (as a part of the font size), so
/// that it shares a baseline with the Korean fallback font.
const SYSTEM_UI_Y_OFFSET: f32 = if cfg!(target_os = "android") {
    0.0
} else {
    -0.06
};

/// A semibold font for titles and the names of things that are playing.
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

/// The face of a font file that has Korean: its number in the file (a file can hold
/// several fonts), or `None` if the file has none or is not a font.
#[cfg(any(target_os = "android", test))]
pub(crate) fn korean_face(file: &[u8]) -> Option<u32> {
    use skrifa::MetadataProvider;
    // In the system's CJK font the second face is the Korean one (the others have the
    // same letters, with the shapes another country gives the Chinese characters), so
    // that one is looked at first. A file with one font has only number 0.
    [1, 0, 2, 3, 4, 5, 6, 7, 8, 9].into_iter().find(|&index| {
        skrifa::FontRef::from_index(file, index).is_ok_and(|font| {
            let letters = font.charmap();
            // A syllable from each end of the block, and a consonant on its own.
            ['가', '힣', 'ㄱ']
                .into_iter()
                .all(|c| letters.map(c).is_some())
        })
    })
}

/// How likely a font file of the system is the one for Korean, by its name: lower is
/// tried sooner, `None` is never tried (a file is read whole to be looked into).
#[cfg(any(target_os = "android", test))]
fn korean_name_rank(name: &str) -> Option<u8> {
    let name = name.to_lowercase();
    let font = [".ttf", ".otf", ".ttc", ".otc"]
        .iter()
        .any(|e| name.ends_with(e));
    if !font || name.contains("emoji") {
        return None;
    }
    let has = |words: &[&str]| words.iter().any(|w| name.contains(w));
    let kind = if has(&["korean", "sanskr", "hangul", "kore"]) {
        0
    } else if name.contains("cjk") && !name.contains("serif") {
        1
    } else if has(&["cjk", "gothic", "nanum", "sec", "samsung", "oneui", "lg"]) {
        2
    } else {
        4
    };
    // The regular weight before the others of the same family.
    let weight = u8::from(has(&["bold", "light", "thin", "black", "medium", "italic"]));
    Some(kind * 2 + weight)
}

/// Android: a font of the system that has Korean, with the number of its face.
///
/// Korean (like every script the bundled fonts lack) normally comes from the UI
/// library's system-font fallback, which on Android goes by what `fonts.xml` says. It
/// only understands entries that name their font in a certain way, and some phones'
/// files do not (a maker's own, an older Android): there every Korean letter was an
/// empty box, on a Korean phone the whole app. So the app finds the font itself, by
/// looking into the system's font files for one that has the letters.
#[cfg(target_os = "android")]
fn android_korean_font() -> Option<(Vec<u8>, u32)> {
    let mut files: Vec<(u8, std::path::PathBuf)> =
        ["/system/fonts", "/product/fonts", "/system_ext/fonts"]
            .iter()
            .filter_map(|dir| std::fs::read_dir(dir).ok())
            .flat_map(|entries| entries.flatten().map(|entry| entry.path()))
            .filter_map(|path| {
                let rank = korean_name_rank(path.file_name()?.to_str()?)?;
                Some((rank, path))
            })
            .collect();
    files.sort();
    for (_, path) in files {
        // A font with eleven thousand syllables is large; the many small ones for
        // other scripts are not read at all.
        let large =
            std::fs::metadata(&path).is_ok_and(|meta| (500 << 10..=96 << 20).contains(&meta.len()));
        if !large {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if let Some(index) = korean_face(&bytes) {
            tracing::info!(font = %path.display(), index, "Korean is drawn with this font");
            return Some((bytes, index));
        }
    }
    tracing::warn!("no font of the system has Korean");
    None
}

/// How a font that is there for Korean is drawn: every glyph once, at whole pixels.
///
/// The UI library keeps each glyph in up to four copies, shifted by quarters of a
/// pixel, so that Latin text is spaced evenly. It leaves that out for Chinese
/// characters and kana, which are many and square, but not (yet) for Hangul, of which
/// a music library uses thousands: its picture of all glyphs then grows four times as
/// fast, and it is sent whole to the graphics device every time it doubles (on a phone
/// that is what the graphics memory ran out over, see `ui::memory`).
#[cfg(any(windows, target_os = "android"))]
fn korean_tweak() -> egui::epaint::text::FontTweak {
    egui::epaint::text::FontTweak {
        subpixel_binning: Some(false),
        ..Default::default()
    }
}

/// Uses the system UI font for Latin text (Korean and other scripts come from eframe's
/// system-font fallback; on Android the app finds the Korean font itself, see
/// `android_korean_font`). If the system font cannot be read the bundled egui fonts are
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
    if let Some(bytes) = system_ui_font() {
        let blob: egui::epaint::text::Blob = std::sync::Arc::new(bytes);
        let face = |weight: f32| {
            FontData::from_blob(blob.clone(), 0).tweak(FontTweak {
                coords: VariationCoords::new([("wght", weight)]),
                // San Francisco sits lower in its line box than the Korean fallback font;
                // raise it so mixed Korean/Latin text shares a baseline. (Roboto and
                // Android's Korean font agree as they are.)
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
                    let data = FontData::from_owned(bytes);
                    let data = if name.starts_with("win-korean") {
                        data.tweak(korean_tweak())
                    } else {
                        data
                    };
                    fonts
                        .font_data
                        .insert(name.into(), std::sync::Arc::new(data));
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
    // Android: Korean from the font the app found, after the fonts for Latin text.
    #[cfg(target_os = "android")]
    if let Some((bytes, index)) = android_korean_font() {
        let mut korean = FontData::from_owned(bytes).tweak(korean_tweak());
        korean.index = index;
        fonts
            .font_data
            .insert("android-korean".into(), std::sync::Arc::new(korean));
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts
                .families
                .entry(family)
                .or_default()
                .push("android-korean".into());
        }
        semibold_stack.push("android-korean".into());
    }
    fonts
        .families
        .insert(FontFamily::Name(SEMIBOLD.into()), semibold_stack);
    ctx.set_fonts(fonts);
}

/// One-time style setup for both themes. On a phone (`phone`) text and controls are
/// larger: they are read at arm's length and hit with a finger.
pub fn install(ctx: &egui::Context, phone: bool) {
    install_fonts(ctx);
    ctx.data_mut(|data| data.insert_temp(egui::Id::unique(TOUCH), phone));
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
            if phone {
                style.text_styles = [
                    (
                        TextStyle::Heading,
                        FontId::new(24.0, FontFamily::Proportional),
                    ),
                    (TextStyle::Body, FontId::new(15.0, FontFamily::Proportional)),
                    (
                        TextStyle::Button,
                        FontId::new(15.0, FontFamily::Proportional),
                    ),
                    (
                        TextStyle::Small,
                        FontId::new(12.5, FontFamily::Proportional),
                    ),
                    (
                        TextStyle::Monospace,
                        FontId::new(13.0, FontFamily::Monospace),
                    ),
                ]
                .into();
                style.spacing.button_padding = egui::vec2(14.0, 9.0);
                style.spacing.interact_size.y = 36.0;
                style.spacing.slider_rail_height = 5.0;
                // Lists are moved with a finger; the bar only shows where one is.
                style.spacing.scroll = egui::style::ScrollStyle::floating();
                // … and can be taken hold of at the right edge to move through a long
                // list quickly: its handle never gets smaller than a finger.
                style.spacing.scroll.handle_min_length = 44.0;
                style.visuals.handle_shape = egui::style::HandleShape::Circle;
                style.visuals.slider_trailing_fill = true;
            }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_likely_korean_fonts_are_looked_into_first() {
        let rank = |name: &str| korean_name_rank(name);
        // A phone maker's Korean font, the system's CJK font, its serif sister, the
        // font for Latin text: in that order.
        let order = [
            "SamsungKorean-Regular.ttf",
            "NotoSansCJK-Regular.ttc",
            "NotoSerifCJK-Regular.ttc",
            "Roboto-Regular.ttf",
        ];
        for pair in order.windows(2) {
            assert!(rank(pair[0]) < rank(pair[1]), "{pair:?}");
        }
        assert!(rank("NotoSansKR-Regular.otf") < rank("NotoSansKR-Bold.otf"));
        assert!(rank("NotoSansCJK-Regular.ttc").is_some());
        // Never read: the emoji font is large and has no letters, and what is not a
        // font is not one.
        assert_eq!(rank("NotoColorEmoji.ttf"), None);
        assert_eq!(rank("fonts.xml"), None);
    }

    /// With the fonts of the computer the tests run on, where they are there (a Mac).
    #[test]
    fn a_font_file_is_asked_whether_it_has_korean() {
        assert_eq!(korean_face(b"not a font at all"), None);
        assert_eq!(korean_face(&[]), None);
        let read = |path: &str| std::fs::read(path).ok();
        if let Some(korean) = read("/System/Library/Fonts/AppleSDGothicNeo.ttc") {
            assert!(korean_face(&korean).is_some(), "the Mac's Korean font");
        }
        if let Some(latin) = read("/System/Library/Fonts/Supplemental/Arial.ttf") {
            assert_eq!(korean_face(&latin), None, "a font for Latin text");
        }
    }
}
