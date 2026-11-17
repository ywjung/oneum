//! Modal dialogs. Only destructive actions and name entry use a modal (docs/UX.md).
use super::{AppEvent, Nav, OneumApp};
use crate::model::TrackId;
use eframe::egui::{self, RichText};

pub(super) enum Dialog {
    /// Remove a music folder (root id, displayed path).
    RemoveFolder(i64, String),
    DeletePlaylist(i64, String),
    /// Delete a correction profile of the listener's own (its name).
    DeleteCorrection(String),
    /// Remove these songs from the library; `own_copies` if copies the app made of some
    /// of them are deleted with them.
    RemoveTracks {
        tracks: Vec<TrackId>,
        own_copies: bool,
    },
    NewPlaylist {
        name: String,
        tracks: Vec<TrackId>,
        focus: bool,
    },
    /// Give a playlist another name.
    RenamePlaylist {
        id: i64,
        name: String,
        focus: bool,
    },
    /// The address of a media server to add (in `name`, like the other text entries).
    AddServer {
        name: String,
        focus: bool,
    },
    /// Name a smart playlist: a new one for `query`, or `original` under a new name.
    SmartPlaylist {
        name: String,
        query: String,
        original: Option<String>,
        focus: bool,
    },
}

impl Dialog {
    pub(super) fn smart_playlist(name: &str, query: String, original: Option<String>) -> Self {
        Self::SmartPlaylist {
            name: crate::app::settings::tidy_text(name, crate::app::settings::MAX_SMART_NAME_CHARS),
            query,
            original,
            focus: true,
        }
    }

    pub(super) fn new_playlist(default_name: &str, tracks: Vec<TrackId>) -> Self {
        Self::NewPlaylist {
            name: default_name.to_string(),
            tracks,
            focus: true,
        }
    }
}

enum Outcome {
    Open,
    Cancel,
    Confirm,
}

/// How far below the top of the screen's content a dialog stands on a phone: a little
/// way down, or right at the top where the keyboard leaves so little of the screen
/// (a phone on its side) that a dialog lower down would lie under it.
pub(super) fn phone_dialog_gap(ctx: &egui::Context) -> f32 {
    if phone_dialog_room(ctx, 72.0) < 200.0 {
        8.0
    } else {
        72.0
    }
}

/// How much height a phone's dialog has from `gap` below the top of the content down
/// to the keyboard (or to the end of the content, where no keyboard is up).
pub(super) fn phone_dialog_room(ctx: &egui::Context, gap: f32) -> f32 {
    let keyboard = crate::platform::window::keyboard_height();
    let content = ctx.content_rect();
    let bottom = if keyboard > 0.0 {
        (ctx.viewport_rect().bottom() - keyboard).min(content.bottom())
    } else {
        content.bottom()
    };
    bottom - content.top() - gap
}

impl OneumApp {
    /// The shell of a dialog: on a phone in the upper part of the screen, with the
    /// strips around the content dimmed like the rest.
    pub(super) fn modal(&self, ctx: &egui::Context, id: egui::Id) -> egui::Modal {
        let mut modal = egui::Modal::new(id);
        if self.phone_mode {
            // In the upper part of the screen: the keyboard comes up over the lower.
            let top = ctx.content_rect().top() + phone_dialog_gap(ctx) - ctx.viewport_rect().top();
            modal = modal
                .area(egui::Modal::default_area(id).anchor(egui::Align2::CENTER_TOP, [0.0, top]));
            // The modal dims what the system leaves to the app's content; the strips
            // around that (behind the status bar and the home indicator, beside a
            // camera) are dimmed here, or they would stand out as lighter bands.
            let (full, safe) = (ctx.viewport_rect(), ctx.content_rect());
            let mut painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Middle,
                egui::Id::unique("oneum-dialog-edges"),
            ));
            // (A layer's painter is cut off at the content's edges, where these begin.)
            painter.set_clip_rect(full);
            let dim = modal.backdrop_color;
            for strip in [
                egui::Rect::from_min_max(full.min, egui::pos2(full.right(), safe.top())),
                egui::Rect::from_min_max(egui::pos2(full.left(), safe.bottom()), full.max),
                egui::Rect::from_min_max(
                    egui::pos2(full.left(), safe.top()),
                    egui::pos2(safe.left(), safe.bottom()),
                ),
                egui::Rect::from_min_max(
                    egui::pos2(safe.right(), safe.top()),
                    egui::pos2(full.right(), safe.bottom()),
                ),
            ] {
                if strip.width() > 0.0 && strip.height() > 0.0 {
                    painter.rect_filled(strip, 0.0, dim);
                }
            }
            // Room around what it says, and corners like the phone's own alerts.
            modal = modal.frame(
                egui::Frame::popup(&ctx.global_style())
                    .inner_margin(16)
                    .corner_radius(16),
            );
        }
        modal
    }

    pub(super) fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.dialog.take() else {
            return;
        };
        let s = self.s();
        let mut outcome = Outcome::Open;
        let modal = self.modal(ctx, egui::Id::unique("oneum-dialog"));
        let width = 400.0_f32.min(ctx.content_rect().width() - 56.0);
        let modal = modal.show(ctx, |ui| {
            ui.set_width(width);
            let (title, body, confirm) = match &dialog {
                Dialog::RemoveFolder(_, path) => {
                    // On a phone the path is a long one inside the system's folders:
                    // the folder's name says more.
                    let shown = std::path::Path::new(path)
                        .file_name()
                        .filter(|_| self.phone_mode)
                        .map_or_else(|| path.clone(), |name| name.to_string_lossy().into_owned());
                    (
                        s.confirm_remove_folder_title,
                        format!("{shown}\n\n{}", s.confirm_remove_folder_body),
                        s.remove_folder,
                    )
                }
                Dialog::DeletePlaylist(_, name) => (
                    s.confirm_delete_playlist_title,
                    format!("“{name}”\n\n{}", s.confirm_delete_playlist_body),
                    s.delete,
                ),
                Dialog::DeleteCorrection(name) => (
                    s.confirm_delete_correction_title,
                    format!("“{name}”\n\n{}", s.confirm_delete_correction_body),
                    s.delete,
                ),
                Dialog::RemoveTracks { tracks, own_copies } => {
                    let mut body = format!(
                        "{}\n\n{}",
                        self.lang.song_count(tracks.len()),
                        s.confirm_remove_tracks_body
                    );
                    // Songs the app keeps a copy of (Android: music it could not read
                    // where it was): the copy is the app's, and goes with the song.
                    if *own_copies {
                        body.push_str("\n\n");
                        body.push_str(s.confirm_remove_copies);
                    }
                    (s.confirm_remove_tracks_title, body, s.remove_from_library)
                }
                Dialog::NewPlaylist { .. } => (s.new_playlist, String::new(), s.ok),
                Dialog::RenamePlaylist { .. } => (s.rename, String::new(), s.ok),
                Dialog::AddServer { .. } => (s.network_add_title, String::new(), s.ok),
                Dialog::SmartPlaylist { query, .. } => (s.smart_playlist, query.clone(), s.ok),
            };
            // On a phone: larger text, and two buttons a finger cannot miss, side by side.
            let touch = self.phone_mode;
            ui.label(
                RichText::new(title)
                    .font(super::theme::semibold(if touch { 18.0 } else { 16.0 }))
                    .strong(),
            );
            ui.add_space(6.0);
            let mut can_confirm = true;
            if let Dialog::SmartPlaylist { .. } = &dialog {
                // What is being named: the search itself.
                ui.label(RichText::new(&body).monospace().weak());
                ui.add_space(4.0);
            }
            let hint = match dialog {
                Dialog::SmartPlaylist { .. } => s.smart_name,
                Dialog::AddServer { .. } => s.network_add_hint,
                _ => s.playlist_name,
            };
            if let Dialog::NewPlaylist { name, focus, .. }
            | Dialog::RenamePlaylist { name, focus, .. }
            | Dialog::AddServer { name, focus }
            | Dialog::SmartPlaylist { name, focus, .. } = &mut dialog
            {
                let mut edit = egui::TextEdit::singleline(name)
                    .hint_text(hint)
                    .desired_width(f32::INFINITY);
                if touch {
                    edit = edit
                        .font(egui::FontId::proportional(17.0))
                        .margin(egui::Margin::symmetric(10, 9));
                }
                let response = ui.add(edit);
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::Role::TextInput, true, s.playlist_name)
                });
                if *focus {
                    *focus = false;
                    response.request_focus();
                }
                can_confirm = !name.trim().is_empty();
                if response.lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    && can_confirm
                {
                    outcome = Outcome::Confirm;
                }
            } else {
                ui.label(body);
                // Return confirms, as the emphasised button suggests; Escape cancels.
                if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    outcome = Outcome::Confirm;
                }
            }
            ui.add_space(12.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // The button that acts is filled: red when it takes something away.
                let dark = ui.visuals().dark_mode;
                let destructive = !matches!(
                    dialog,
                    Dialog::NewPlaylist { .. }
                        | Dialog::RenamePlaylist { .. }
                        | Dialog::AddServer { .. }
                        | Dialog::SmartPlaylist { .. }
                );
                let (fill, ink) = if destructive {
                    (
                        egui::Color32::from_rgb(0xD6, 0x45, 0x45),
                        egui::Color32::WHITE,
                    )
                } else if dark {
                    (
                        super::theme::accent(dark),
                        egui::Color32::from_rgb(0x14, 0x14, 0x18),
                    )
                } else {
                    (super::theme::accent(dark), egui::Color32::WHITE)
                };
                let button_size = if touch {
                    egui::vec2((ui.available_width() - 8.0) / 2.0, 46.0)
                } else {
                    egui::vec2(76.0, 28.0)
                };
                let confirm_button = egui::Button::new(RichText::new(confirm).strong().color(ink))
                    .fill(fill)
                    .min_size(button_size);
                if ui.add_enabled(can_confirm, confirm_button).clicked() {
                    outcome = Outcome::Confirm;
                }
                let cancel_button = egui::Button::new(s.cancel).min_size(button_size);
                if ui.add(cancel_button).clicked() {
                    outcome = Outcome::Cancel;
                }
            });
        });
        if modal.should_close() && matches!(outcome, Outcome::Open) {
            outcome = Outcome::Cancel;
        }
        match outcome {
            Outcome::Open => self.dialog = Some(dialog),
            Outcome::Cancel => {}
            Outcome::Confirm => self.confirm(dialog),
        }
    }

    fn confirm(&mut self, dialog: Dialog) {
        let library = self.core.library.clone();
        let result = match dialog {
            Dialog::RemoveFolder(id, path) => {
                // Removing a large folder deletes many rows: keep it off the UI thread.
                // If it cannot be removed (the folder then stays), the window says so.
                let (events, core) = (self.events_tx.clone(), self.core.clone());
                let spawned = std::thread::Builder::new()
                    .name("oneum-remove-folder".into())
                    .spawn(move || match library.remove_folder(id) {
                        // A folder that was linked on a phone: the right to read it
                        // is not needed any more.
                        Ok(_) => crate::platform::files::forget_linked_folder(
                            std::path::Path::new(&path),
                        ),
                        Err(error) => {
                            tracing::warn!("removing a music folder failed: {error}");
                            let _ = events.send(AppEvent::EditFailed(error));
                            core.request_repaint();
                        }
                    });
                if spawned.is_err() {
                    self.warn_toast(self.s().notice_generic_error);
                }
                return;
            }
            Dialog::DeletePlaylist(id, _) => {
                self.edit(move |library| {
                    library.delete_playlist(id).err().map(AppEvent::EditFailed)
                });
                Ok(())
            }
            Dialog::DeleteCorrection(name) => {
                self.delete_correction(&name);
                Ok(())
            }
            Dialog::RemoveTracks { tracks, .. } => {
                // Where the app keeps copies of its own (Android: music it could not
                // read where it was), the copies of these songs go with them.
                let copies = crate::app::paths::AppPaths::imported_copies_dir();
                let files: Vec<(TrackId, std::path::PathBuf)> = match &copies {
                    Some(_) => tracks
                        .iter()
                        .filter_map(|&id| Some((id, self.snapshot.get(id)?.path.clone())))
                        .collect(),
                    None => Vec::new(),
                };
                self.edit(move |library| {
                    library.remove_tracks(&tracks);
                    if let Some(copies) = copies {
                        // Only of the songs that are gone (removing can fail).
                        let gone: Vec<std::path::PathBuf> = files
                            .into_iter()
                            .filter(|(id, _)| library.track(*id).is_none())
                            .map(|(_, file)| file)
                            .collect();
                        crate::app::delete_own_copies(&copies, &gone);
                    }
                    None
                });
                Ok(())
            }
            Dialog::RenamePlaylist { id, name, .. } => {
                self.edit(move |library| {
                    library
                        .rename_playlist(id, &name)
                        .err()
                        .map(AppEvent::EditFailed)
                });
                Ok(())
            }
            Dialog::SmartPlaylist {
                name,
                query,
                original,
                ..
            } => {
                use crate::app::settings::{MAX_SMART_NAME_CHARS, MAX_SMART_PLAYLISTS, tidy_text};
                let name = tidy_text(&name, MAX_SMART_NAME_CHARS);
                if name.is_empty() {
                    return;
                }
                let list = &mut self.settings.smart_playlists;
                match original {
                    // Renamed: it keeps its place; another one of that name gives way.
                    Some(old) => {
                        list.retain(|p| p.name != name || p.name == old);
                        if let Some(smart) = list.iter_mut().find(|p| p.name == old) {
                            smart.name = name.clone();
                        }
                        if old != name {
                            // A pin follows the playlist to its new name.
                            super::repoint_pin(
                                &mut self.settings.pinned,
                                &crate::app::settings::Pin::Smart(old.clone()),
                                crate::app::settings::Pin::Smart(name.clone()),
                            );
                            self.views.remove(&Nav::Smart(old));
                        }
                    }
                    None => {
                        list.retain(|p| p.name != name);
                        if list.len() < MAX_SMART_PLAYLISTS {
                            list.push(crate::app::settings::SmartPlaylist {
                                name: name.clone(),
                                query,
                            });
                        }
                    }
                }
                self.go(Nav::Smart(name));
                return;
            }
            Dialog::AddServer { name, .. } => {
                // Looked for in the background; the network view says how it went.
                self.network.add_by_address(&name);
                Ok(())
            }
            Dialog::NewPlaylist { name, tracks, .. } => {
                // Created with its songs in one step: a failure leaves no empty
                // playlist behind. It opens once it exists.
                self.edit(move |library| {
                    Some(match library.create_playlist_with(&name, &tracks) {
                        Ok(id) => AppEvent::PlaylistCreated(id),
                        Err(error) => AppEvent::EditFailed(error),
                    })
                });
                Ok(())
            }
        };
        if let Err(error) = result {
            self.library_error_toast(&error);
        }
    }
}
