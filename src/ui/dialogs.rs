//! Modal dialogs. Only destructive actions and name entry use a modal (docs/UX.md).
use super::{Nav, OneumApp};
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
}

impl Dialog {
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
            };
            ui.label(
                RichText::new(title)
                    .font(super::theme::semibold(16.0))
                    .strong(),
            );
            ui.add_space(6.0);
            let mut can_confirm = true;
            if let Dialog::NewPlaylist { name, focus, .. } = &mut dialog {
                let edit = egui::TextEdit::singleline(name)
                    .hint_text(s.playlist_name)
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
                let destructive = !matches!(dialog, Dialog::NewPlaylist { .. });
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
                let spawned = std::thread::Builder::new()
                    .name("oneum-remove-folder".into())
                    .spawn(move || {
                        if let Err(error) = library.remove_folder(id) {
                            tracing::warn!("removing a music folder failed: {error}");
                        }
                    });
                if spawned.is_err() {
                    self.warn_toast(self.s().notice_generic_error);
                }
                return;
            }
            Dialog::DeletePlaylist(id, _) => library.delete_playlist(id),
            Dialog::RemoveTracks(tracks) => {
                library.remove_tracks(&tracks);
                Ok(())
            }
            Dialog::NewPlaylist { name, tracks, .. } => {
                library.create_playlist(&name).and_then(|id| {
                    if !tracks.is_empty() {
                        library.add_to_playlist(id, &tracks)?;
                    }
                    self.go(Nav::Playlist(id));
                    Ok(())
                })
            }
        };
        if let Err(error) = result {
            self.library_error_toast(&error);
        }
    }
}
