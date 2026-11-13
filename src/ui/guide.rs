//! The user guide, shown inside the app.
//!
//! The text is `docs/USER_GUIDE.md`, compiled into the binary, so the guide is the same
//! document that is kept with the source and needs no file next to the app. It is
//! written in the little Markdown that `markdown.rs` understands and draws.
use super::icons::Icon;
use super::markdown::{self, Block, parse};
use super::{OneumApp, theme, widgets};
use eframe::egui::{self, Align, FontId, Layout, RichText, Sense};
use std::sync::OnceLock;

const GUIDE_MARKDOWN: &str = include_str!("../../docs/USER_GUIDE.md");
/// What is different on Windows; put in front of the second section there.
const WINDOWS_NOTES: &str = include_str!("../../docs/USER_GUIDE_WINDOWS.md");

/// The guide as Markdown for this system. The guide is written for the Mac; for Windows
/// the key symbols are spelled out for a PC keyboard, Finder becomes File Explorer, and
/// a section on what differs is added after "getting started".
pub(super) fn guide_markdown(windows: bool) -> String {
    if !windows {
        return GUIDE_MARKDOWN.to_string();
    }
    let text = crate::platform::pc_key_names(GUIDE_MARKDOWN).replace("Finder", "파일 탐색기");
    // After "getting started", i.e. in front of the third "## ".
    let third = text
        .match_indices("\n## ")
        .nth(2)
        .map_or(text.len(), |(at, _)| at + 1);
    format!("{}{}\n{}", &text[..third], WINDOWS_NOTES, &text[third..])
}

const CONTENTS_WIDTH: f32 = 168.0;
const SIDE_MARGIN: f32 = 20.0;
/// The guide is something to read: comfortable type, room between the sections.
const LOOK: markdown::Look = markdown::Look {
    body: 13.5,
    heading: 16.0,
    heading_gap: 18.0,
};

/// The guide, parsed once.
fn guide() -> &'static [Block] {
    static GUIDE: OnceLock<Vec<Block>> = OnceLock::new();
    GUIDE.get_or_init(|| parse(&guide_markdown(cfg!(windows))))
}

/// Section headings of the guide, in order (for the contents column and for tests).
pub(super) fn sections() -> Vec<&'static str> {
    guide()
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
                                    for (index, section) in sections().into_iter().enumerate() {
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
        markdown::show(ui, guide(), LOOK, |ui, section| {
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

    /// The Windows edition: PC key names, File Explorer, and the section on differences
    /// as the second one. It parses as cleanly as the Mac edition.
    #[test]
    fn the_windows_guide_speaks_of_a_pc() {
        let markdown = guide_markdown(true);
        assert!(!markdown.contains('⌘') && !markdown.contains('⇧') && !markdown.contains('⌥'));
        assert!(markdown.contains("Ctrl+F") && markdown.contains("Ctrl+Shift+U"));
        assert!(!markdown.contains("Finder") && markdown.contains("파일 탐색기"));
        let blocks = parse(&markdown);
        let headings: Vec<&str> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Heading(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(headings[0], "OnEum이라는 이름");
        assert_eq!(headings[1], "시작하기");
        assert_eq!(headings[2], "Windows에서 다른 점");
        assert_eq!(headings[3], "화면");
        let mac = parse(&guide_markdown(false));
        let mac_headings = mac
            .iter()
            .filter(|b| matches!(b, Block::Heading(_)))
            .count();
        assert_eq!(headings.len(), mac_headings + 1);
        // Same number of tables, none of them broken by the rewriting.
        let tables = |blocks: &[Block]| {
            blocks
                .iter()
                .filter(|b| {
                    matches!(b, Block::Table { head, rows }
                        if rows.iter().all(|r| r.len() == head.len()))
                })
                .count()
        };
        assert_eq!(tables(&blocks), 3);
        assert_eq!(tables(&mac), 3);
    }

    /// The guide that ships: everything in it is understood, nothing is left as markup.
    #[test]
    fn the_real_guide_is_fully_understood() {
        let blocks = guide();
        let titles = blocks
            .iter()
            .filter(|b| matches!(b, Block::Title(_)))
            .count();
        assert_eq!(titles, 1);
        let markdown = guide_markdown(cfg!(windows));
        let headings = markdown.lines().filter(|l| l.starts_with("## ")).count();
        assert!(headings >= 8);
        assert_eq!(sections().len(), headings);
        let tables = blocks
            .iter()
            .filter(|b| matches!(b, Block::Table { .. }))
            .count();
        assert_eq!(tables, 3, "shortcuts, output quality, troubleshooting");
        let mut texts: Vec<String> = Vec::new();
        for block in blocks {
            match block {
                Block::Title(t) | Block::Heading(t) => texts.push(t.clone()),
                Block::Paragraph(s) | Block::Item { text: s, .. } => texts.push(plain(s)),
                Block::Table { head, rows } => {
                    // Every row has as many cells as the head.
                    assert!(rows.iter().all(|r| r.len() == head.len()), "ragged table");
                    texts.extend(head.iter().chain(rows.iter().flatten()).map(|c| plain(c)));
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
        // Things a reader looks for are there.
        let all = texts.join("\n");
        for needle in [
            "온음",
            "시작하기",
            "단축키",
            "VU 미터",
            "출력 품질",
            "문제가 생겼을 때",
        ] {
            assert!(all.contains(needle), "{needle} is missing");
        }
    }
}
