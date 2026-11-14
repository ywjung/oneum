//! The file dialogs of macOS and Windows (through `rfd`). They block until the user has
//! chosen, so `done` is called before `pick` returns.
use super::{PickKind, PickRequest};
use std::path::{Path, PathBuf};

/// Shows the dialog `request` describes and hands what was chosen to `done`. Nothing
/// chosen (the dialog was cancelled): `done` is not called. Main thread only.
pub fn pick(request: PickRequest, done: impl FnOnce(Vec<PathBuf>) + Send + 'static) {
    let mut dialog = rfd::FileDialog::new();
    if !request.title.is_empty() {
        dialog = dialog.set_title(&request.title);
    }
    for (name, extensions) in &request.filters {
        dialog = dialog.add_filter(name, extensions);
    }
    let picked = match request.kind {
        PickKind::Folder => dialog.pick_folder().map(|folder| vec![folder]),
        PickKind::Files => dialog.pick_files(),
        PickKind::File => dialog.pick_file().map(|file| vec![file]),
        PickKind::SaveAs(name) => dialog
            .set_file_name(name)
            .save_file()
            .map(|file| vec![file]),
    };
    if let Some(paths) = picked.filter(|paths| !paths.is_empty()) {
        done(paths);
    }
}

/// A file written for the user (`PickKind::SaveAs`) is where they asked for it already.
pub fn hand_out(_file: &Path) {}
