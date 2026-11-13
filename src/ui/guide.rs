//! The user guide, shown inside the app.
//!
//! The text is `docs/USER_GUIDE.md` (Korean) or `docs/USER_GUIDE.en.md` (English),
//! compiled into the binary, so the guide is the same document that is kept with the
//! source and needs no file next to the app. The one in the app's language is shown.
//! Both are written in the little Markdown that `markdown.rs` understands and draws,
//! and have the same sections in the same order.
use super::icons::Icon;
use super::markdown::{self, Block, parse};
use super::{OneumApp, theme, widgets};
use crate::i18n::Lang;
use eframe::egui::{self, Align, FontId, Layout, RichText, Sense};
use std::sync::OnceLock;

const GUIDE_KO: &str = include_str!("../../docs/USER_GUIDE.md");
const GUIDE_EN: &str = include_str!("../../docs/USER_GUIDE.en.md");
/// What is different on Windows; put in front of the second section there.
const WINDOWS_NOTES_KO: &str = include_str!("../../docs/USER_GUIDE_WINDOWS.md");
const WINDOWS_NOTES_EN: &str = include_str!("../../docs/USER_GUIDE_WINDOWS.en.md");

/// The guide as Markdown for this system, in `lang`. The guide is written for the Mac;
/// for Windows the key symbols are spelled out for a PC keyboard, Finder becomes File
/// Explorer, and a section on what differs is added after "getting started".
pub(super) fn guide_markdown(lang: Lang, windows: bool) -> String {
    let (guide, notes) = match lang {
        Lang::Ko => (GUIDE_KO, WINDOWS_NOTES_KO),
        Lang::En => (GUIDE_EN, WINDOWS_NOTES_EN),
    };
    if !windows {
        return guide.to_string();
    }
    let text = crate::platform::pc_key_names(guide);
    // The command keeps the name the menu gives it there; elsewhere it is the program.
    let text = match lang {
        Lang::Ko => text
            .replace("Finder에서 보기", "탐색기에서 보기")
            .replace("Finder", "파일 탐색기"),
        Lang::En => text
            .replace("Show in Finder", "Show in Explorer")
            .replace("Finder", "File Explorer"),
    };
    // After "getting started", i.e. in front of the third "## ".
    let third = text
        .match_indices("\n## ")
        .nth(2)
        .map_or(text.len(), |(at, _)| at + 1);
    format!("{}{}\n{}", &text[..third], notes, &text[third..])
}

const CONTENTS_WIDTH: f32 = 168.0;
const SIDE_MARGIN: f32 = 20.0;
/// The guide is something to read: comfortable type, room between the sections.
const LOOK: markdown::Look = markdown::Look {
    body: 13.5,
    heading: 16.0,
    heading_gap: 18.0,
};

/// The guide in `lang`, parsed once.
fn guide(lang: Lang) -> &'static [Block] {
    static KO: OnceLock<Vec<Block>> = OnceLock::new();
    static EN: OnceLock<Vec<Block>> = OnceLock::new();
    let cell = match lang {
        Lang::Ko => &KO,
        Lang::En => &EN,
    };
    cell.get_or_init(|| parse(&guide_markdown(lang, cfg!(windows))))
}

/// Section headings of the guide in `lang`, in order (for the contents column and for
/// tests).
pub(super) fn sections(lang: Lang) -> Vec<&'static str> {
    guide(lang)
        .iter()
        .filter_map(|b| match b {
            Block::Heading(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// One line of the contents column: the section name, left-aligned, lit under the pointer.
fn contents_entry(ui: &mut egui::Ui, section: &str) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(CONTENTS_WIDTH - 16.0, 26.0), Sense::click());
    let rect = rect.translate(egui::vec2(8.0, 0.0));
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, section));
    if ui.is_rect_visible(rect) {
        let color = if response.hovered() || response.has_focus() {
            ui.painter()
                .rect_filled(rect, 6.0, ui.visuals().widgets.hovered.weak_bg_fill);
            ui.visuals().strong_text_color()
        } else {
            ui.visuals().text_color()
        };
        ui.painter()
            .with_clip_rect(rect.shrink2(egui::vec2(6.0, 0.0)))
            .text(
                rect.left_center() + egui::vec2(10.0, 0.0),
                egui::Align2::LEFT_CENTER,
                section,
                FontId::proportional(13.0),
                color,
            );
    }
    response
}

impl OneumApp {
    /// Opens the guide (closing Settings, which is a sheet in the same place).
    pub(super) fn open_guide(&mut self) {
        self.settings_open = false;
        self.eq_open = false;
        self.guide_open = true;
        self.guide_jump = Some(0);
    }

    /// Opens Settings (closing the other sheets: only one is shown at a time).
    pub(super) fn open_settings(&mut self) {
        self.guide_open = false;
        self.eq_open = false;
        self.settings_open = true;
    }

    /// True while a sheet (Settings, the guide, the equalizer) covers the window: list keys and the
    /// like belong to the sheet then.
    pub(super) fn sheet_open(&self) -> bool {
        self.settings_open || self.guide_open || self.eq_open
    }

    pub(super) fn guide_window(&mut self, ctx: &egui::Context) {
        if !self.guide_open {
            return;
        }
        let s = self.s();
        let style = ctx.global_style();
        let dark = style.visuals.dark_mode;
        let body_fill = if dark {
            egui::Color32::from_rgb(0x1E, 0x1E, 0x23)
        } else {
            egui::Color32::from_rgb(0xF4, 0xF4, 0xF7)
        };
        let frame = egui::Frame::window(&style)
            .inner_margin(0)
            .corner_radius(12.0)
            .fill(body_fill);
        let mut close = false;
        // Dim what is behind and let a click there close the guide, like Settings.
        let screen = ctx.content_rect();
        let backdrop_clicked = egui::Area::new(egui::Id::unique("guide-backdrop"))
            .order(egui::Order::Background)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let response = ui.allocate_rect(screen, Sense::click());
                let alpha = if dark { 120 } else { 70 };
                ui.painter()
                    .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(alpha));
                response.clicked()
            })
            .inner;
        if backdrop_clicked && self.dialog.is_none() {
            close = true;
        }
        // As large as the window comfortably allows: this is something to read.
        let width = (screen.width() - 80.0).clamp(560.0, 820.0);
        let body_height = (screen.height() - 150.0).clamp(260.0, 620.0);
        let jump = self.guide_jump.take();
        let mut jump_to = None;
        egui::Window::new(s.menu_user_guide)
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .frame(frame)
            .fixed_size([width, 0.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -12.0])
            .show(ctx, |ui| {
                ui.set_width(width);
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.add_space(SIDE_MARGIN);
                    ui.label(
                        RichText::new(s.menu_user_guide)
                            .font(theme::semibold(16.0))
                            .color(ui.visuals().strong_text_color()),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.add_space(12.0);
                        if widgets::icon_button(ui, Icon::Close, s.close_guide, 26.0, false)
                            .clicked()
                        {
                            close = true;
                        }
                    });
                });
                ui.add_space(10.0);
                widgets::row_divider(ui);
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    // Contents: one entry per section; a click scrolls there.
                    ui.allocate_ui_with_layout(
                        egui::vec2(CONTENTS_WIDTH, body_height),
                        Layout::top_down(Align::Min),
                        |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("guide-contents")
                                .max_height(body_height)
                                .min_scrolled_height(body_height)
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    ui.add_space(12.0);
                                    ui.spacing_mut().item_spacing.y = 2.0;
                                    for (index, section) in
                                        sections(self.lang).into_iter().enumerate()
                                    {
                                        if contents_entry(ui, section).clicked() {
                                            jump_to = Some(index + 1);
                                        }
                                    }
                                    ui.add_space(12.0);
                                });
                        },
                    );
                    ui.add(egui::Separator::default().vertical().spacing(0.0));
                    let text_width = width - CONTENTS_WIDTH - 1.0;
                    ui.allocate_ui_with_layout(
                        egui::vec2(text_width, body_height),
                        Layout::top_down(Align::Min),
                        |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("guide-body")
                                .max_height(body_height)
                                .min_scrolled_height(body_height)
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    if jump == Some(0) {
                                        ui.scroll_to_cursor(Some(Align::TOP));
                                    }
                                    ui.add_space(16.0);
                                    ui.horizontal(|ui| {
                                        ui.add_space(SIDE_MARGIN);
                                        ui.vertical(|ui| {
                                            ui.set_width(text_width - SIDE_MARGIN * 2.0);
                                            self.guide_body(ui, jump);
                                        });
                                    });
                                    ui.add_space(18.0);
                                });
                        },
                    );
                });
            });
        if jump_to.is_some() {
            // Scrolling happens while the text is laid out, i.e. in the next frame.
            self.guide_jump = jump_to;
            ctx.request_repaint();
        }
        if close {
            self.guide_open = false;
        }
    }

    /// The text of the guide. `jump` scrolls to section `n` (1-based).
    fn guide_body(&self, ui: &mut egui::Ui, jump: Option<usize>) {
        markdown::show(ui, guide(self.lang), LOOK, |ui, section| {
            if jump == Some(section) {
                ui.scroll_to_cursor(Some(Align::TOP));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(spans: &[markdown::Span]) -> String {
        spans.iter().map(|s| s.text.as_str()).collect()
    }

    fn headings(blocks: &[Block]) -> Vec<&str> {
        blocks
            .iter()
            .filter_map(|b| match b {
                Block::Heading(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Tables whose rows all have as many cells as their head.
    fn tables(blocks: &[Block]) -> usize {
        blocks
            .iter()
            .filter(|b| {
                matches!(b, Block::Table { head, rows }
                    if rows.iter().all(|r| r.len() == head.len()))
            })
            .count()
    }

    /// The Windows edition: PC key names, File Explorer, and the section on differences
    /// as the third one. It parses as cleanly as the Mac edition, in both languages.
    #[test]
    fn the_windows_guide_speaks_of_a_pc() {
        for (lang, explorer, show_in, differences) in [
            (
                Lang::Ko,
                "파일 탐색기",
                "탐색기에서 보기",
                "Windows에서 다른 점",
            ),
            (
                Lang::En,
                "File Explorer",
                "Show in Explorer",
                "What Is Different on Windows",
            ),
        ] {
            let markdown = guide_markdown(lang, true);
            assert!(!markdown.contains('⌘') && !markdown.contains('⇧') && !markdown.contains('⌥'));
            assert!(markdown.contains("Ctrl+F") && markdown.contains("Ctrl+Shift+U"));
            assert!(!markdown.contains("Finder"), "{lang:?}");
            assert!(
                markdown.contains(explorer) && markdown.contains(show_in),
                "{lang:?}"
            );
            let blocks = parse(&markdown);
            let windows = headings(&blocks);
            assert_eq!(windows[2], differences);
            let mac = parse(&guide_markdown(lang, false));
            let mac_headings = headings(&mac);
            assert_eq!(windows.len(), mac_headings.len() + 1);
            // The other headings are the Mac ones, with the keys named for a PC.
            let pc: Vec<String> = mac_headings
                .iter()
                .map(|h| crate::platform::pc_key_names(h))
                .collect();
            assert_eq!(windows[..2], pc[..2]);
            assert_eq!(windows[3..], pc[2..]);
            // Same number of tables, none of them broken by the rewriting.
            assert_eq!(tables(&blocks), 3);
            assert_eq!(tables(&mac), 3);
        }
    }

    /// The guides that ship: everything in them is understood, nothing is left as
    /// markup, and what a reader looks for is there.
    #[test]
    fn the_real_guides_are_fully_understood() {
        for (lang, needles) in [
            (
                Lang::Ko,
                [
                    "온음",
                    "시작하기",
                    "단축키",
                    "VU 미터",
                    "출력 품질",
                    "문제가 생겼을 때",
                ],
            ),
            (
                Lang::En,
                [
                    "온음",
                    "Getting Started",
                    "Keyboard Shortcuts",
                    "VU Meter",
                    "Output Quality",
                    "Troubleshooting",
                ],
            ),
        ] {
            let blocks = guide(lang);
            let titles = blocks
                .iter()
                .filter(|b| matches!(b, Block::Title(_)))
                .count();
            assert_eq!(titles, 1);
            let markdown = guide_markdown(lang, cfg!(windows));
            let count = markdown.lines().filter(|l| l.starts_with("## ")).count();
            assert!(count >= 8);
            assert_eq!(sections(lang).len(), count);
            let all_tables = blocks
                .iter()
                .filter(|b| matches!(b, Block::Table { .. }))
                .count();
            assert_eq!(all_tables, 3, "shortcuts, output quality, troubleshooting");
            assert_eq!(tables(blocks), 3, "ragged table in {lang:?}");
            let mut texts: Vec<String> = Vec::new();
            for block in blocks {
                match block {
                    Block::Title(t) | Block::Heading(t) => texts.push(t.clone()),
                    Block::Paragraph(s) | Block::Item { text: s, .. } => texts.push(plain(s)),
                    Block::Table { head, rows } => {
                        texts.extend(head.iter().chain(rows.iter().flatten()).map(|c| plain(c)))
                    }
                }
            }
            for text in &texts {
                assert!(!text.trim().is_empty(), "empty block");
                assert!(
                    !text.contains("**") && !text.contains('`') && !text.starts_with('#'),
                    "markup left in: {text}"
                );
                assert!(!text.starts_with('|') && !text.starts_with("- "), "{text}");
            }
            let all = texts.join("\n");
            for needle in needles {
                assert!(all.contains(needle), "{needle} is missing in {lang:?}");
            }
        }
    }

    /// The two guides tell the same thing: the same sections in the same order, and
    /// as many points and table rows in each section.
    #[test]
    fn the_english_guide_follows_the_korean_one() {
        // Per section: (items, table rows).
        let outline = |lang: Lang| {
            let mut sections: Vec<(usize, usize)> = Vec::new();
            for block in guide(lang) {
                match block {
                    Block::Heading(_) => sections.push((0, 0)),
                    Block::Item { .. } => {
                        if let Some(last) = sections.last_mut() {
                            last.0 += 1;
                        }
                    }
                    Block::Table { rows, .. } => {
                        if let Some(last) = sections.last_mut() {
                            last.1 += rows.len();
                        }
                    }
                    _ => {}
                }
            }
            sections
        };
        assert_eq!(outline(Lang::En), outline(Lang::Ko));
        assert_eq!(sections(Lang::En).len(), sections(Lang::Ko).len());
        // Shortcuts are the same keys.
        let keys = |lang: Lang| -> Vec<String> {
            guide(lang)
                .iter()
                .filter_map(|b| match b {
                    Block::Table { rows, .. } => Some(rows),
                    _ => None,
                })
                .nth(1)
                .expect("the shortcuts table")
                .iter()
                .map(|row| plain(&row[0]))
                .collect()
        };
        assert_eq!(keys(Lang::En), keys(Lang::Ko));
    }

    /// Korean in the English guide only where it is meant: the name and what it means,
    /// a search by Korean initials, and the language's own name in the settings.
    #[test]
    fn the_english_guide_is_english() {
        let allowed = ["온음", "온", "ㅇㅇㅇ", "아이유", "한국어"];
        for markdown in [
            guide_markdown(Lang::En, false),
            guide_markdown(Lang::En, true),
        ] {
            let korean = |c: char| matches!(c, '\u{AC00}'..='\u{D7A3}' | '\u{3131}'..='\u{318E}');
            let mut word = String::new();
            for c in markdown.chars().chain([' ']) {
                if korean(c) {
                    word.push(c);
                } else if !word.is_empty() {
                    assert!(
                        allowed.contains(&word.as_str()),
                        "Korean in the English guide: {word}"
                    );
                    word.clear();
                }
            }
        }
    }
}
