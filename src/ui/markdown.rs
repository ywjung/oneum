//! The little Markdown that the app's own documents use, and how it is drawn.
//!
//! Two documents are compiled into the app and shown inside it: the user guide
//! (`guide.rs`) and the open source notices (Settings). Only what they use is
//! understood: a title, section headings, paragraphs, bulleted and numbered lists
//! (nested by indentation), tables, **bold**, `code`, and a web address alone in a table
//! cell, which becomes a link.
use super::{theme, widgets};
use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{self, Align, FontId, Layout, RichText, Sense};

/// A run of text with one style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Span {
    pub text: String,
    pub bold: bool,
    pub code: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Block {
    /// The document title (`# …`).
    Title(String),
    /// A section heading (`## …`).
    Heading(String),
    Paragraph(Vec<Span>),
    /// A list item. `number` is the marker of a numbered item ("1.").
    Item {
        depth: usize,
        number: Option<String>,
        text: Vec<Span>,
    },
    Table {
        head: Vec<Vec<Span>>,
        rows: Vec<Vec<Vec<Span>>>,
    },
}

/// Splits a line into styled runs: `**bold**` and `` `code` `` (no nesting inside code).
pub(super) fn spans(text: &str) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let (mut bold, mut code) = (false, false);
    let mut current = String::new();
    let mut flush = |current: &mut String, bold: bool, code: bool| {
        if !current.is_empty() {
            out.push(Span {
                text: std::mem::take(current),
                bold,
                code,
            });
        }
    };
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if c == '`' {
            flush(&mut current, bold, code);
            code = !code;
            rest = &rest[1..];
        } else if !code && rest.starts_with("**") {
            flush(&mut current, bold, code);
            bold = !bold;
            rest = &rest[2..];
        } else {
            current.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    flush(&mut current, bold, code);
    out
}

fn table_cells(line: &str) -> Vec<String> {
    line.trim()
        .trim_start_matches('|')
        .trim_end_matches('|')
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect()
}

/// The marker and the text of a list line (`- text`, `12. text`), if it is one.
fn list_item(line: &str) -> Option<(Option<String>, &str)> {
    if let Some(text) = line.strip_prefix("- ") {
        return Some((None, text));
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    let text = line[digits..].strip_prefix(". ")?;
    (digits > 0).then(|| (Some(format!("{}.", &line[..digits])), text))
}

pub(super) fn parse(markdown: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut paragraph = String::new();
    let end_paragraph = |paragraph: &mut String, blocks: &mut Vec<Block>| {
        if !paragraph.is_empty() {
            blocks.push(Block::Paragraph(spans(paragraph)));
            paragraph.clear();
        }
    };
    let mut lines = markdown.lines().peekable();
    // Whether the line before was blank (a blank line ends a list item).
    let mut after_blank = true;
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        let was_after_blank = std::mem::replace(&mut after_blank, trimmed.is_empty());
        if trimmed.is_empty() {
            end_paragraph(&mut paragraph, &mut blocks);
        } else if let Some(title) = trimmed.strip_prefix("# ") {
            end_paragraph(&mut paragraph, &mut blocks);
            blocks.push(Block::Title(title.trim().to_string()));
        } else if trimmed.starts_with("##") {
            end_paragraph(&mut paragraph, &mut blocks);
            blocks.push(Block::Heading(
                trimmed.trim_start_matches('#').trim().to_string(),
            ));
        } else if trimmed.starts_with('|') {
            end_paragraph(&mut paragraph, &mut blocks);
            let mut table = vec![table_cells(trimmed)];
            while let Some(next) = lines.next_if(|l| l.trim_start().starts_with('|')) {
                table.push(table_cells(next));
            }
            // The line of dashes between the head and the body carries no text.
            table.retain(|row| {
                !row.iter()
                    .all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':' | ' ')))
            });
            let mut rows = table
                .into_iter()
                .map(|row| row.iter().map(|c| spans(c)).collect::<Vec<_>>());
            if let Some(head) = rows.next() {
                blocks.push(Block::Table {
                    head,
                    rows: rows.collect(),
                });
            }
        } else if let Some((number, text)) = list_item(trimmed) {
            end_paragraph(&mut paragraph, &mut blocks);
            blocks.push(Block::Item {
                depth: indent / 2,
                number,
                text: spans(text),
            });
        } else if indent >= 2
            && !was_after_blank
            && paragraph.is_empty()
            && let Some(Block::Item { text, .. }) = blocks.last_mut()
        {
            // An indented line right after a list item continues that item.
            text.push(Span {
                text: " ".into(),
                bold: false,
                code: false,
            });
            text.extend(spans(trimmed));
        } else {
            if !paragraph.is_empty() {
                paragraph.push(' ');
            }
            paragraph.push_str(trimmed);
        }
    }
    end_paragraph(&mut paragraph, &mut blocks);
    blocks
}

/// How a document is set: the guide is something to read, the notices are small print.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Look {
    /// Size of the body text.
    pub body: f32,
    /// Size of the section headings.
    pub heading: f32,
    /// Space above every section heading but the first.
    pub heading_gap: f32,
}

/// Styled text ready to be laid out with wrapping.
fn job(ui: &egui::Ui, spans: &[Span], size: f32, strong: bool) -> LayoutJob {
    let mut job = LayoutJob::default();
    let text_color = ui.visuals().text_color();
    let strong_color = ui.visuals().strong_text_color();
    for span in spans {
        let format = if span.code {
            TextFormat {
                font_id: FontId::monospace(size - 1.0),
                color: strong_color,
                background: ui.visuals().code_bg_color,
                ..Default::default()
            }
        } else if span.bold || strong {
            TextFormat {
                font_id: theme::semibold(size),
                color: strong_color,
                ..Default::default()
            }
        } else {
            TextFormat {
                font_id: FontId::proportional(size),
                color: text_color,
                ..Default::default()
            }
        };
        job.append(&span.text, 0.0, format);
    }
    job
}

/// A table whose cells may run over several lines (the guide's tables).
fn table(ui: &mut egui::Ui, head: &[Vec<Span>], rows: &[Vec<Vec<Span>>], size: f32) {
    // The first column names the thing (a key, a setting, a symptom); the rest explain.
    let shares: &[f32] = match head.len() {
        0 | 1 => &[1.0],
        2 => &[0.31, 0.69],
        3 => &[0.22, 0.42, 0.36],
        _ => &[],
    };
    widgets::card(ui, |ui| {
        let gap = 14.0;
        let columns = head.len().max(1);
        let usable = ui.available_width() - gap * (columns - 1) as f32;
        let width = |i: usize| match shares.get(i) {
            Some(share) => usable * share,
            None => usable / columns as f32,
        };
        let row = |ui: &mut egui::Ui, cells: &[Vec<Span>], is_head: bool| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for (i, cell) in cells.iter().enumerate().take(columns) {
                    ui.allocate_ui_with_layout(
                        egui::vec2(width(i), 0.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.set_width(width(i));
                            // The naming column stands out; so does the head row.
                            let text = job(ui, cell, size - 0.5, is_head);
                            if is_head {
                                ui.label(text);
                            } else {
                                ui.add(egui::Label::new(text).wrap());
                            }
                        },
                    );
                }
            });
        };
        row(ui, head, true);
        for cells in rows {
            ui.add_space(3.0);
            widgets::row_divider(ui);
            ui.add_space(3.0);
            row(ui, cells, false);
        }
    });
}

/// From this many rows on (or with more than three columns) a table is a list to look
/// something up in, not text to read: see [`long_table`].
const LONG_TABLE_ROWS: usize = 40;
const LONG_TABLE_ROW_HEIGHT: f32 = 26.0;

fn is_long_table(columns: usize, rows: usize) -> bool {
    columns > 3 || rows >= LONG_TABLE_ROWS
}

/// The web address a table cell consists of, if that is all it holds.
pub(super) fn web_address(cell: &[Span]) -> Option<&str> {
    match cell {
        [only] if !only.code && crate::platform::is_web_address(&only.text) => Some(&only.text),
        _ => None,
    }
}

/// A web address as it is shown: without the scheme, which says nothing.
fn without_scheme(address: &str) -> &str {
    address
        .strip_prefix("https://")
        .or_else(|| address.strip_prefix("http://"))
        .unwrap_or(address)
        .trim_end_matches('/')
}

/// The rows of a long table that are inside the visible part `clip` (top and bottom)
/// of a table body that starts at `top`: only those are laid out, so a table of
/// hundreds of rows costs no more per frame than what is on screen.
fn visible_rows(
    top: f32,
    clip: std::ops::RangeInclusive<f32>,
    rows: usize,
) -> std::ops::Range<usize> {
    let row_at = |y: f32| ((y - top) / LONG_TABLE_ROW_HEIGHT).max(0.0);
    let first = (row_at(*clip.start()).floor() as usize).min(rows);
    let last = (row_at(*clip.end()).ceil() as usize).min(rows);
    first..last.max(first)
}

/// A table with many rows: one line per row, every row the same height, long cells cut
/// short (the pointer shows the rest), web addresses as links.
fn long_table(ui: &mut egui::Ui, head: &[Vec<Span>], rows: &[Vec<Vec<Span>>], size: f32) {
    // Name, version, license, where it comes from: the notices' table.
    let shares: &[f32] = match head.len() {
        4 => &[0.25, 0.14, 0.31, 0.30],
        _ => &[],
    };
    widgets::card(ui, |ui| {
        let gap = 12.0;
        let columns = head.len().max(1);
        let full = ui.available_width();
        let usable = full - gap * (columns - 1) as f32;
        let width = |i: usize| match shares.get(i) {
            Some(share) => usable * share,
            None => usable / columns as f32,
        };
        let cell_rect = |row: egui::Rect, i: usize| {
            let left = row.left() + (0..i).map(|k| width(k) + gap).sum::<f32>();
            egui::Rect::from_min_size(
                egui::pos2(left, row.top()),
                egui::vec2(width(i), row.height()),
            )
        };
        let cell_ui = |ui: &mut egui::Ui, rect: egui::Rect, salt: (usize, usize)| {
            ui.new_child(
                egui::UiBuilder::new()
                    .id_salt(salt)
                    .max_rect(rect)
                    .layout(Layout::left_to_right(Align::Center)),
            )
        };
        let row_size = egui::vec2(full, LONG_TABLE_ROW_HEIGHT);
        let (head_row, _) = ui.allocate_exact_size(row_size, Sense::hover());
        for (i, cell) in head.iter().enumerate() {
            let mut cell_ui = cell_ui(ui, cell_rect(head_row, i), (usize::MAX, i));
            let text = job(&cell_ui, cell, size - 0.5, true);
            cell_ui.add(egui::Label::new(text).truncate());
        }
        let body_size = egui::vec2(full, LONG_TABLE_ROW_HEIGHT * rows.len() as f32);
        let (body, _) = ui.allocate_exact_size(body_size, Sense::hover());
        let clip = ui.clip_rect();
        let line = egui::Stroke::new(1.0, widgets::divider_color(ui));
        for index in visible_rows(body.top(), clip.top()..=clip.bottom(), rows.len()) {
            let row = egui::Rect::from_min_size(
                egui::pos2(
                    body.left(),
                    body.top() + LONG_TABLE_ROW_HEIGHT * index as f32,
                ),
                row_size,
            );
            ui.painter().hline(row.x_range(), row.top(), line);
            for (i, cell) in rows[index].iter().enumerate().take(columns) {
                let mut cell_ui = cell_ui(ui, cell_rect(row, i), (index, i));
                if let Some(address) = web_address(cell) {
                    let text = RichText::new(without_scheme(address))
                        .size(size - 0.5)
                        .color(cell_ui.visuals().hyperlink_color);
                    let link = cell_ui
                        .add(egui::Label::new(text).truncate().sense(Sense::click()))
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text(address);
                    link.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Link, true, address));
                    if link.clicked() {
                        crate::platform::open_web_page(address);
                    }
                } else {
                    let text = job(&cell_ui, cell, size - 0.5, false);
                    cell_ui.add(egui::Label::new(text).truncate());
                }
            }
        }
    });
}

/// Draws a document. The title is left out (whoever shows the document names it);
/// `at_heading` is called where section `n` (1-based) is about to be drawn.
pub(super) fn show(
    ui: &mut egui::Ui,
    blocks: &[Block],
    look: Look,
    mut at_heading: impl FnMut(&mut egui::Ui, usize),
) {
    let dim = theme::dim(ui);
    let mut section = 0;
    let mut after_item = false;
    for block in blocks {
        let is_item = matches!(block, Block::Item { .. });
        if after_item && !is_item {
            ui.add_space(6.0);
        }
        after_item = is_item;
        match block {
            Block::Title(_) => {}
            Block::Heading(text) => {
                section += 1;
                if section > 1 {
                    ui.add_space(look.heading_gap);
                }
                at_heading(ui, section);
                ui.label(
                    RichText::new(text)
                        .font(theme::semibold(look.heading))
                        .color(ui.visuals().strong_text_color()),
                );
                ui.add_space(7.0);
            }
            Block::Paragraph(text) => {
                ui.add(egui::Label::new(job(ui, text, look.body, false)).wrap());
                ui.add_space(7.0);
            }
            Block::Item {
                depth,
                number,
                text,
            } => {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.add_space(4.0 + 18.0 * *depth as f32);
                    // A fixed column for the marker keeps wrapped lines aligned.
                    let marker = number.as_deref().unwrap_or("•");
                    ui.allocate_ui_with_layout(
                        egui::vec2(18.0, 0.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.set_width(18.0);
                            ui.label(RichText::new(marker).size(look.body).color(dim));
                        },
                    );
                    ui.vertical(|ui| {
                        ui.add(egui::Label::new(job(ui, text, look.body, false)).wrap());
                    });
                });
                ui.add_space(4.0);
            }
            Block::Table { head, rows } => {
                ui.add_space(2.0);
                if is_long_table(head.len(), rows.len()) {
                    long_table(ui, head, rows, look.body);
                } else {
                    table(ui, head, rows, look.body);
                }
                ui.add_space(8.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(spans: &[Span]) -> String {
        spans.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn inline_styles() {
        let s = spans("누르면 **다음에 재생**과 `이유` → 아이유");
        assert_eq!(plain(&s), "누르면 다음에 재생과 이유 → 아이유");
        let styles: Vec<(bool, bool)> = s.iter().map(|s| (s.bold, s.code)).collect();
        assert_eq!(
            styles,
            vec![
                (false, false),
                (true, false),
                (false, false),
                (false, true),
                (false, false)
            ]
        );
        // Stars inside code are text; an unclosed marker does not lose anything.
        assert_eq!(plain(&spans("`a**b`")), "a**b");
        assert_eq!(
            plain(&spans("끝이 **없는 굵은 글씨")),
            "끝이 없는 굵은 글씨"
        );
        assert!(spans("").is_empty());
    }

    #[test]
    fn blocks_of_a_small_document() {
        let doc = "# 제목\n\n## 첫 절\n첫 줄\n둘째 줄\n\n1. 하나\n2. **둘**\n- 점\n  - 안쪽 점\n\n| 키 | 동작 |\n|---|---|\n| Space | 재생 |\n| ⌘F | `검색` |\n\n## 둘째 절 (⌘,)\n끝.";
        let blocks = parse(doc);
        assert_eq!(blocks[0], Block::Title("제목".into()));
        assert_eq!(blocks[1], Block::Heading("첫 절".into()));
        // Lines of one paragraph are joined.
        assert!(matches!(&blocks[2], Block::Paragraph(p) if plain(p) == "첫 줄 둘째 줄"));
        let items: Vec<(usize, Option<&str>, String)> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Item {
                    depth,
                    number,
                    text,
                } => Some((*depth, number.as_deref(), plain(text))),
                _ => None,
            })
            .collect();
        assert_eq!(
            items,
            vec![
                (0, Some("1."), "하나".to_string()),
                (0, Some("2."), "둘".to_string()),
                (0, None, "점".to_string()),
                (1, None, "안쪽 점".to_string()),
            ]
        );
        let Some(Block::Table { head, rows }) =
            blocks.iter().find(|b| matches!(b, Block::Table { .. }))
        else {
            panic!("no table in {blocks:?}");
        };
        assert_eq!(
            head.iter().map(|c| plain(c)).collect::<Vec<_>>(),
            ["키", "동작"]
        );
        assert_eq!(rows.len(), 2, "the line of dashes is not a row");
        assert_eq!(plain(&rows[1][1]), "검색");
        assert!(rows[1][1][0].code);
        assert_eq!(blocks.last(), Some(&Block::Paragraph(spans("끝."))));
    }

    #[test]
    fn an_indented_line_continues_the_list_item_before_it() {
        let blocks = parse("- 첫 줄\n  이어지는 **줄**\n- 다음 항목\n\n  들여쓴 문단은 따로");
        let texts: Vec<String> = blocks
            .iter()
            .map(|b| match b {
                Block::Item { text, .. } | Block::Paragraph(text) => plain(text),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(
            texts,
            ["첫 줄 이어지는 줄", "다음 항목", "들여쓴 문단은 따로"]
        );
        assert!(matches!(blocks[2], Block::Paragraph(_)));
    }

    #[test]
    fn a_web_address_alone_in_a_cell_is_a_link() {
        let cell = spans("https://github.com/pdeljanov/Symphonia");
        assert_eq!(
            web_address(&cell),
            Some("https://github.com/pdeljanov/Symphonia")
        );
        assert_eq!(
            without_scheme("https://github.com/pdeljanov/Symphonia/"),
            "github.com/pdeljanov/Symphonia"
        );
        // Text around it, code, or something that is not a web address: plain text.
        assert_eq!(web_address(&spans("see https://example.org")), None);
        assert_eq!(web_address(&spans("`https://example.org`")), None);
        assert_eq!(web_address(&spans("file:///etc/passwd")), None);
        assert_eq!(web_address(&spans("MIT OR Apache-2.0")), None);
        assert_eq!(web_address(&[]), None);
    }

    #[test]
    fn long_tables_are_told_from_tables_to_read() {
        // The guide's tables: two or three columns, a handful of rows.
        assert!(!is_long_table(2, 25) && !is_long_table(3, 6));
        // The notices: four columns, hundreds of rows.
        assert!(is_long_table(4, 3) && is_long_table(2, 40) && is_long_table(4, 253));
    }

    #[test]
    fn only_the_rows_on_screen_are_laid_out() {
        let h = LONG_TABLE_ROW_HEIGHT;
        // The body starts at y = 100; 253 rows.
        assert_eq!(visible_rows(100.0, 100.0..=100.0 + 10.0 * h, 253), 0..10);
        // Scrolled: the top of the view is inside row 20, the bottom inside row 31.
        let view = 100.0 + 20.5 * h..=100.0 + 31.5 * h;
        assert_eq!(visible_rows(100.0, view, 253), 20..32);
        // The table is below the view, above it, or ends inside it.
        assert_eq!(visible_rows(900.0, 0.0..=500.0, 253), 0..0);
        assert!(visible_rows(-10_000.0, 0.0..=500.0, 253).is_empty());
        assert_eq!(visible_rows(100.0, 0.0..=10_000.0, 5), 0..5);
        assert!(visible_rows(100.0, 0.0..=500.0, 0).is_empty());
    }
}
