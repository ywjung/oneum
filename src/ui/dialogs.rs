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
            ui.set_width(380.0);
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
            ui.label(RichText::new(title).size(16.0).strong());
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
            }
            ui.add_space(12.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        can_confirm,
                        egui::Button::new(RichText::new(confirm).strong()),
                    )
                    .clicked()
                {
                    outcome = Outcome::Confirm;
                }
                if ui.button(s.cancel).clicked() {
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
            Dialog::RemoveFolder(id, _) => library.remove_folder(id),
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
