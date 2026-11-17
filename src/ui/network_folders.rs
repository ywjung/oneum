//! Adding a network folder (a share of a NAS over WebDAV): the form that asks where it
//! is and who to be there, the try that follows, and what the listener is told when
//! the try fails. The password typed here goes to the system's store for passwords
//! and into no other place (see `platform::secrets`).
use super::network::Why;
use super::{OneumApp, theme};
use crate::net::folders::{self, Folder};
use crate::net::tls;
use crate::platform::secrets::Secret;
use eframe::egui::{self, RichText};
use zeroize::Zeroize;

pub(super) enum Status {
    Editing,
    /// The server is being asked.
    Trying,
    Failed(Why),
}

/// What is typed into the form.
pub(super) struct FolderForm {
    pub address: String,
    pub name: String,
    pub user: String,
    pub password: String,
    focus: bool,
    pub status: Status,
    /// The id of the folder that is being tried for this form: what a try brings is
    /// this form's only when it is the try of that folder (a try that was left
    /// behind by a form since closed comes back to no one).
    trying: Option<String>,
}

/// The password field, by a name of its own: what egui keeps of a field (see where
/// the field is drawn) is found by it.
const PASSWORD_FIELD: &str = "oneum-folder-password";

impl FolderForm {
    pub(super) fn new() -> Self {
        FolderForm {
            address: String::new(),
            name: String::new(),
            user: String::new(),
            password: String::new(),
            focus: true,
            status: Status::Editing,
            trying: None,
        }
    }
}

// The password does not stay in memory after the form is gone.
impl Drop for FolderForm {
    fn drop(&mut self) {
        self.password.zeroize();
    }
}

/// The address as it is asked for: with `https://` in front where no scheme was
/// typed, and ending with '/' (it names a folder). What a folder's name may hold and
/// an address may not is written as its escape: a space, and `#` and `?`, which in a
/// folder's address are letters of a name (it has no fragment and no query).
pub(super) fn folder_address(typed: &str) -> String {
    let typed = typed.trim();
    let address = if typed.contains("://") {
        typed.to_string()
    } else {
        format!("https://{typed}")
    };
    let path_at = address
        .find("://")
        .map(|at| at + 3)
        .and_then(|from| address[from..].find('/').map(|at| from + at))
        .unwrap_or(address.len());
    let (server, path) = address.split_at(path_at);
    let mut address = String::with_capacity(server.len() + path.len() + 8);
    address.push_str(server);
    for c in path.chars() {
        match c {
            ' ' => address.push_str("%20"),
            '#' => address.push_str("%23"),
            '?' => address.push_str("%3F"),
            c => address.push(c),
        }
    }
    if !address.ends_with('/') {
        address.push('/');
    }
    address
}

impl OneumApp {
    /// The folder the form describes, to be tried. `trusted`: the fingerprint of the
    /// server's certificate, when the listener has just said to trust it.
    pub(super) fn try_folder_form(&mut self, trusted: Option<String>) {
        let Some(form) = self.folder_form.as_mut() else {
            return;
        };
        let folder = Folder {
            id: folders::new_id(),
            name: form.name.clone(),
            url: folder_address(&form.address),
            user: form.user.trim().to_string(),
            pinned: trusted,
            sign_in: String::new(),
        }
        .sanitized();
        // (A folder that is on the list already is replaced, not added.)
        let full = self.settings.network_folders.len() >= folders::MAX_FOLDERS;
        match folder {
            Some(folder)
                if full
                    && !self
                        .settings
                        .network_folders
                        .iter()
                        .any(|known| known.url == folder.url) =>
            {
                form.status = Status::Failed(Why::Full);
            }
            Some(folder) => {
                form.status = Status::Trying;
                form.trying = Some(folder.id.clone());
                let password = Secret::new(form.password.as_str());
                self.network.try_folder(folder, password);
            }
            None => form.status = Status::Failed(Why::Address),
        }
    }

    /// The form, while it is open.
    pub(super) fn folder_form_window(&mut self, ctx: &egui::Context) {
        let Some(mut form) = self.folder_form.take() else {
            return;
        };
        let s = self.s();
        let touch = self.phone_mode;
        let modal = self.modal(ctx, egui::Id::unique("oneum-folder-form"));
        let width = 440.0_f32.min(ctx.content_rect().width() - 56.0);
        let (mut cancel, mut connect, mut trust) = (false, false, None::<String>);
        let trying = matches!(form.status, Status::Trying);
        let shown = modal.show(ctx, |ui| {
            ui.set_width(width);
            ui.label(
                RichText::new(s.folder_add_title)
                    .font(theme::semibold(if touch { 18.0 } else { 16.0 }))
                    .strong(),
            );
            ui.add_space(8.0);
            let field = |ui: &mut egui::Ui, text: &mut String, hint: &str, secret: bool| {
                let mut edit = egui::TextEdit::singleline(text)
                    .hint_text(hint)
                    .password(secret)
                    .desired_width(f32::INFINITY);
                if secret {
                    edit = edit.id(egui::Id::unique(PASSWORD_FIELD));
                }
                if touch {
                    edit = edit
                        .font(egui::FontId::proportional(17.0))
                        .margin(egui::Margin::symmetric(10, 9));
                }
                let response = ui.add_enabled(!trying, edit);
                response
                    .widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, hint));
                ui.add_space(6.0);
                response
            };
            let address = field(ui, &mut form.address, s.folder_address_hint, false);
            if form.focus {
                form.focus = false;
                address.request_focus();
            }
            field(ui, &mut form.name, s.folder_name_hint, false);
            let user = field(ui, &mut form.user, s.folder_user_hint, false);
            let password = field(ui, &mut form.password, s.folder_password_hint, true);
            // egui keeps what was typed into a field to take typing back: not of a
            // password (it would outlive the form, and come back with "undo").
            let field_id = egui::Id::unique(PASSWORD_FIELD);
            if let Some(mut state) = egui::text_edit::TextEditState::load(ui.ctx(), field_id) {
                state.clear_undoer();
                state.store(ui.ctx(), field_id);
            }
            // What the server said was said of what was typed then: of another
            // address, name or password nothing is known yet. (A certificate that
            // was shown is one server's: it is not to be trusted for another.)
            if (address.changed() || user.changed() || password.changed())
                && matches!(form.status, Status::Failed(_))
            {
                form.status = Status::Editing;
            }
            let dim = theme::dim(ui);
            ui.label(RichText::new(s.folder_password_note).small().color(dim));
            // A name and a password over a connection that is not encrypted.
            let plain = form
                .address
                .trim()
                .get(..7)
                .is_some_and(|start| start.eq_ignore_ascii_case("http://"));
            if plain && !form.user.trim().is_empty() {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(s.folder_plain_warning)
                        .small()
                        .color(ui.visuals().warn_fg_color),
                );
            }
            match &form.status {
                Status::Editing => {}
                Status::Trying => {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new(s.folder_trying).color(dim));
                    });
                }
                Status::Failed(why) => {
                    ui.add_space(6.0);
                    let warn = ui.visuals().warn_fg_color;
                    match why {
                        Why::Untrusted(fingerprint) => {
                            ui.label(RichText::new(s.folder_untrusted).color(warn));
                            ui.add_space(4.0);
                            ui.label(RichText::new(tls::spelled(fingerprint)).monospace().small());
                            ui.add_space(4.0);
                            ui.label(RichText::new(s.folder_untrusted_how).small().color(dim));
                        }
                        Why::Refused => {
                            ui.label(RichText::new(s.folder_refused).color(warn));
                        }
                        Why::Address => {
                            ui.label(RichText::new(s.folder_bad_address).color(warn));
                        }
                        Why::Full => {
                            ui.label(RichText::new(s.folder_full).color(warn));
                        }
                        Why::NoPassword => {
                            ui.label(RichText::new(s.folder_no_password).color(warn));
                        }
                        Why::Moved => {
                            ui.label(RichText::new(s.folder_moved).color(warn));
                        }
                        Why::NotFound => {
                            ui.label(RichText::new(s.folder_not_found).color(warn));
                        }
                        Why::NotAFolder => {
                            ui.label(RichText::new(s.folder_not_webdav).color(warn));
                        }
                        Why::NotTls => {
                            ui.label(RichText::new(s.folder_not_tls).color(warn));
                        }
                        Why::Other(reason) => {
                            ui.label(RichText::new(s.folder_unreachable).color(warn));
                            ui.label(RichText::new(reason).small().color(dim));
                        }
                    }
                }
            }
            let can_connect = !trying && !form.address.trim().is_empty();
            if password.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && can_connect
            {
                connect = true;
            }
            ui.add_space(12.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let dark = ui.visuals().dark_mode;
                let ink = if dark {
                    egui::Color32::from_rgb(0x14, 0x14, 0x18)
                } else {
                    egui::Color32::WHITE
                };
                let size = if touch {
                    egui::vec2((ui.available_width() - 8.0) / 2.0, 46.0)
                } else {
                    egui::vec2(76.0, 28.0)
                };
                // The server showed a certificate nobody vouches for: the button that
                // goes on says what going on means.
                let untrusted = match &form.status {
                    Status::Failed(Why::Untrusted(fingerprint)) => Some(fingerprint.clone()),
                    _ => None,
                };
                let label = if untrusted.is_some() {
                    s.folder_trust
                } else {
                    s.folder_connect
                };
                let go = egui::Button::new(RichText::new(label).strong().color(ink))
                    .fill(theme::accent(dark))
                    .min_size(size);
                if ui.add_enabled(can_connect, go).clicked() {
                    match untrusted {
                        Some(fingerprint) => trust = Some(fingerprint),
                        None => connect = true,
                    }
                }
                if ui.add(egui::Button::new(s.cancel).min_size(size)).clicked() {
                    cancel = true;
                }
            });
        });
        if cancel || shown.should_close() {
            // (A try that is under way comes back to no form, and is dropped.)
            return;
        }
        self.folder_form = Some(form);
        if let Some(fingerprint) = trust {
            self.try_folder_form(Some(fingerprint));
        } else if connect {
            self.try_folder_form(None);
        }
    }

    /// What a try of the form's folder brought: the folder is kept (its password in
    /// the system's store) and opened, or the form says why not.
    pub(super) fn take_tried_folder(&mut self) {
        let Some((folder, password, result)) = self.network.take_tried() else {
            return;
        };
        // The try of the form that is open, or of a script: one that was left behind
        // (its form was closed, or another address is being typed by now) is dropped
        // with its password.
        let of_the_form = self
            .folder_form
            .as_ref()
            .is_some_and(|form| form.trying.as_deref() == Some(folder.id.as_str()));
        if !of_the_form && !self.network.scripted_try {
            return;
        }
        self.network.scripted_try = false;
        let outcome = result.and_then(|()| {
            if folder.user.is_empty() {
                return Ok(());
            }
            folders::keep_password(&folder.id, &password).map_err(Why::Other)
        });
        match outcome {
            Ok(()) => {
                self.folder_form = None;
                // The same folder added again replaces what was there.
                let gone: Vec<String> = self
                    .settings
                    .network_folders
                    .iter()
                    .filter(|known| known.url == folder.url && known.id != folder.id)
                    .map(|known| known.id.clone())
                    .collect();
                for id in &gone {
                    folders::forget_password(id);
                }
                self.settings
                    .network_folders
                    .retain(|known| known.url != folder.url);
                self.settings.network_folders.push(folder.clone());
                folders::set_known(&self.settings.network_folders);
                self.network.open_network_folder(folder);
            }
            Err(why) => {
                self.network.last_try_failed = Some(why.clone());
                if let Some(form) = self.folder_form.as_mut() {
                    form.status = Status::Failed(why);
                }
            }
        }
    }

    /// Takes a network folder off the list and forgets its password.
    pub(super) fn forget_network_folder(&mut self, index: usize) {
        if index >= self.settings.network_folders.len() {
            return;
        }
        let folder = self.settings.network_folders.remove(index);
        folders::forget_password(&folder.id);
        folders::set_known(&self.settings.network_folders);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_is_completed_as_a_folders() {
        for (typed, asked) in [
            ("nas.local:5006/music", "https://nas.local:5006/music/"),
            (" http://10.0.0.5/share/ ", "http://10.0.0.5/share/"),
            ("https://nas.local/a b", "https://nas.local/a%20b/"),
            ("HTTP://nas/music", "HTTP://nas/music/"),
        ] {
            assert_eq!(folder_address(typed), asked);
        }
        // (What is no address after that is refused where the folder is made.)
        let folder = |url: &str| Folder {
            id: "folder-1".into(),
            url: url.into(),
            ..Folder::default()
        };
        assert!(
            folder(&folder_address("nas.local/music"))
                .sanitized()
                .is_some()
        );
        // A name with a space, a `#` or a `?` in it is a name still.
        assert_eq!(
            folder_address("nas.local:5006/music/My Albums"),
            "https://nas.local:5006/music/My%20Albums/"
        );
        assert_eq!(
            folder_address("https://nas.local/C#/Who?"),
            "https://nas.local/C%23/Who%3F/"
        );
        assert!(
            folder(&folder_address("https://nas.local/a b"))
                .sanitized()
                .is_some_and(|folder| folder.url == "https://nas.local/a%20b/")
        );
        assert!(
            folder(&folder_address("https://nas local/a"))
                .sanitized()
                .is_none()
        );
        assert!(
            folder(&folder_address("smb://nas/music"))
                .sanitized()
                .is_none()
        );
    }
}
