//! Modal dialogs. Only destructive actions and name entry use a modal (docs/UX.md).
use super::{AppEvent, Nav, OneumApp};
use crate::model::TrackId;
use eframe::egui::{self, RichText};

pub(super) enum Dialog {
    /// Remove a music folder (root id, displayed path).
    RemoveFolder(i64, String),
    DeletePlaylist(i64, String),
    RemoveTracks(Vec<TrackId>),
    NewPlaylist {
        name: String,
        tracks: Vec<TrackId>,
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

impl OneumApp {
    pub(super) fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.dialog.take() else {
            return;
        };
        let s = self.s();
        let mut outcome = Outcome::Open;
        let modal = egui::Modal::new(egui::Id::unique("oneum-dialog")).show(ctx, |ui| {
            ui.set_width(400.0);
            let (title, body, confirm) = match &dialog {
                Dialog::RemoveFolder(_, path) => (
                    s.confirm_remove_folder_title,
                    format!("{path}\n\n{}", s.confirm_remove_folder_body),
                    s.remove_folder,
                ),
                Dialog::DeletePlaylist(_, name) => (
                    s.confirm_delete_playlist_title,
                    format!("“{name}”\n\n{}", s.confirm_delete_playlist_body),
                    s.delete,
                ),
                Dialog::RemoveTracks(tracks) => (
                    s.confirm_remove_tracks_title,
                    format!(
                        "{}\n\n{}",
                        self.lang.song_count(tracks.len()),
                        s.confirm_remove_tracks_body
                    ),
                    s.remove_from_library,
                ),
                Dialog::NewPlaylist { .. } => (s.new_playlist, String::new(), s.ok),
                Dialog::SmartPlaylist { query, .. } => (s.smart_playlist, query.clone(), s.ok),
            };
            ui.label(
                RichText::new(title)
                    .font(super::theme::semibold(16.0))
                    .strong(),
            );
            ui.add_space(6.0);
            let mut can_confirm = true;
            if let Dialog::SmartPlaylist { .. } = &dialog {
                // What is being named: the search itself.
                ui.label(RichText::new(&body).monospace().weak());
                ui.add_space(4.0);
            }
            let hint = if matches!(dialog, Dialog::SmartPlaylist { .. }) {
                s.smart_name
            } else {
                s.playlist_name
            };
            if let Dialog::NewPlaylist { name, focus, .. }
            | Dialog::SmartPlaylist { name, focus, .. } = &mut dialog
            {
                let edit = egui::TextEdit::singleline(name)
                    .hint_text(hint)
                    .desired_width(f32::INFINITY);
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
                    Dialog::NewPlaylist { .. } | Dialog::SmartPlaylist { .. }
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
                let confirm_button = egui::Button::new(RichText::new(confirm).strong().color(ink))
                    .fill(fill)
                    .min_size(egui::vec2(76.0, 28.0));
                if ui.add_enabled(can_confirm, confirm_button).clicked() {
                    outcome = Outcome::Confirm;
                }
                let cancel_button = egui::Button::new(s.cancel).min_size(egui::vec2(76.0, 28.0));
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
            Dialog::RemoveFolder(id, _) => {
                // Removing a large folder deletes many rows: keep it off the UI thread.
                // If it cannot be removed (the folder then stays), the window says so.
                let (events, core) = (self.events_tx.clone(), self.core.clone());
                let spawned = std::thread::Builder::new()
                    .name("oneum-remove-folder".into())
                    .spawn(move || {
                        if let Err(error) = library.remove_folder(id) {
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
            Dialog::RemoveTracks(tracks) => {
                self.edit(move |library| {
                    library.remove_tracks(&tracks);
                    None
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
