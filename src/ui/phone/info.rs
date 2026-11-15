//! What is known about one song, on a phone: the sheet that "Song Info" brings up.
//!
//! The same facts as the info panel on a desk (they come from the same place), laid out
//! as lines one below the other; what leads somewhere is tapped.
use super::super::icons::{self, Icon};
use super::super::panels::{kilohertz, known_artist, role_label, signal_path_block};
use super::super::{Action, Nav, OneumApp, lists, theme};
use super::{SIDE, Sheet, font, held};
use crate::model::Track;
use eframe::egui::{self, Align2, Rect, RichText, Sense, Ui, pos2, vec2};
use std::sync::Arc;

/// A line of the sheet: what it is and, at the right, what it says (on as many lines
/// as that needs). With `leads_on` it can be tapped; true when it was.
fn info_line(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    name: &str,
    value: &str,
    leads_on: bool,
) -> bool {
    let width = ui.available_width();
    let dim = theme::dim(ui);
    let color = if leads_on {
        theme::accent(ui.visuals().dark_mode)
    } else {
        ui.visuals().strong_text_color()
    };
    let name_galley = ui
        .painter()
        .layout_no_wrap(name.to_string(), font(14.0), dim);
    let name_width = if name.is_empty() {
        0.0
    } else {
        name_galley.size().x + 14.0
    };
    let arrow = if leads_on { 20.0 } else { 0.0 };
    let room = (width - 2.0 * SIDE - 4.0 - name_width - arrow).max(80.0);
    let galley = ui
        .painter()
        .layout(value.to_string(), font(15.0), color, room);
    let height = (galley.size().y + 18.0).max(40.0);
    let sense = if leads_on {
        Sense::click()
    } else {
        Sense::hover()
    };
    let rect = ui.allocate_space(vec2(width, height)).1;
    let response = ui.interact(rect, ui.scope_id().with(("info-line", id_salt)), sense);
    if leads_on {
        let label = format!("{name} {value}");
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
        if held(ui, &response) {
            ui.painter()
                .rect_filled(rect, 0.0, ui.visuals().widgets.hovered.weak_bg_fill);
        }
    }
    ui.painter().galley(
        pos2(
            rect.left() + SIDE + 4.0,
            rect.center().y - name_galley.size().y / 2.0,
        ),
        name_galley,
        dim,
    );
    // With a name before it, the value stands at the right; alone, at the left.
    let right = rect.right() - SIDE - arrow;
    let x = if name.is_empty() {
        rect.left() + SIDE + 4.0
    } else {
        right - galley.size().x
    };
    ui.painter().galley(
        pos2(x, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    if leads_on {
        icons::paint(
            ui.painter(),
            Rect::from_center_size(
                pos2(rect.right() - SIDE - 6.0, rect.center().y),
                egui::Vec2::splat(14.0),
            ),
            Icon::Forward,
            ui.visuals().weak_text_color(),
        );
    }
    response.clicked()
}

/// What the lines below are about.
fn info_heading(ui: &mut Ui, text: &str) {
    ui.add_space(16.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
    ui.painter().text(
        pos2(rect.left() + SIDE + 4.0, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        theme::semibold(13.0),
        theme::dim(ui),
    );
}

impl OneumApp {
    /// The lines of the "Song Info" sheet: where the song belongs (ways on to its
    /// album, artist and genres), what the library and the file say about it, who made
    /// it, the same song elsewhere in the library, and what is done to its sound while
    /// it plays. True when a line led somewhere and the sheet is done with.
    pub(super) fn phone_info(
        &mut self,
        ui: &mut Ui,
        track: &Arc<Track>,
        actions: &mut Vec<Action>,
    ) -> bool {
        let s = self.s();
        // What the file says beyond what the library keeps is read in the background:
        // those lines join the others a moment later.
        let details = self.extras_of(track);
        let versions = self.other_versions_of(track.id);
        let artists = self.artist_names();
        let mut close = false;
        if !track.available {
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
            ui.painter().text(
                pos2(rect.left() + SIDE + 4.0, rect.center().y),
                Align2::LEFT_CENTER,
                s.info_missing,
                font(15.0),
                ui.visuals().warn_fg_color,
            );
        }
        if !track.album.is_empty() && info_line(ui, "album", s.col_album, &track.album, true) {
            actions.push(Action::Go(Nav::Album(track.album_key)));
            close = true;
        }
        if !track.artist.is_empty() {
            // Only a name the library has as an artist leads somewhere.
            let known = known_artist(&artists, &track.artist)
                .or_else(|| known_artist(&artists, &track.album_artist));
            let leads_on = known.is_some();
            if info_line(ui, "artist", s.col_artist, &track.artist, leads_on)
                && let Some(known) = known
            {
                actions.push(Action::Go(Nav::Artist(known)));
                close = true;
            }
        }
        let rows = self.info_rows(track, details.as_deref());
        for (index, (name, value)) in rows.into_iter().enumerate() {
            if name == s.info_genre {
                // Every genre is a way to the other songs of its kind.
                for genre in lists::genres(&value) {
                    if info_line(ui, ("genre", genre), name, genre, true) {
                        actions.push(Action::Go(Nav::Genre(genre.to_string())));
                        close = true;
                    }
                }
            } else {
                info_line(ui, ("row", index), name, &value, false);
            }
        }
        // Who wrote, played and produced it.
        if let Some(details) = details.as_ref().filter(|d| !d.credits.is_empty()) {
            info_heading(ui, s.info_credits);
            for (index, credit) in details.credits.iter().enumerate() {
                let role = role_label(s, credit.role);
                let known = match credit.names.as_slice() {
                    [only] => known_artist(&artists, only),
                    _ => None,
                };
                let names = credit.names.join(", ");
                if info_line(ui, ("credit", index), role, &names, known.is_some())
                    && let Some(known) = known
                {
                    actions.push(Action::Go(Nav::Artist(known)));
                    close = true;
                }
            }
        }
        // The same song elsewhere in the library: another album, another format.
        if !versions.is_empty() {
            info_heading(ui, s.info_versions);
            for other in &versions {
                let album = if other.album.is_empty() {
                    s.unknown_album
                } else {
                    other.album.as_str()
                };
                let mut what = format!(
                    "{album} · {} · {}",
                    other.codec,
                    kilohertz(other.sample_rate)
                );
                if let Some(bits) = other.bit_depth {
                    what = format!("{what} · {bits} bit");
                }
                if info_line(ui, ("version", other.id), "", &what, true) {
                    self.phone.sheet = Some(Sheet::Info(other.id));
                }
            }
        }
        // What the engine does with the song while it is the one playing.
        if let Some(path) = self.signal_path().filter(|path| path.track_id == track.id) {
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.add_space(SIDE + 4.0);
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width() - SIDE);
                    ui.spacing_mut().item_spacing.y = 3.0;
                    signal_path_block(ui, s, &path);
                });
            });
        }
        info_heading(ui, s.info_file);
        let file = track
            .file()
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        ui.horizontal(|ui| {
            ui.add_space(SIDE + 4.0);
            ui.add(
                egui::Label::new(
                    RichText::new(file)
                        .font(font(14.0))
                        .color(ui.visuals().text_color()),
                )
                .wrap(),
            );
        });
        // Where a file manager can show it: the folder it is in, and the way there.
        if crate::platform::PHONE_FILE_MANAGER
            && let Some(folder) = track.file().parent()
        {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.add_space(SIDE + 4.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(folder.display().to_string())
                            .font(font(12.0))
                            .color(theme::dim(ui)),
                    )
                    .wrap(),
                );
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(SIDE + 4.0);
                if ui.button(s.show_in_finder).clicked() {
                    actions.push(Action::Reveal(track.id));
                }
            });
        }
        ui.add_space(8.0);
        close
    }
}
