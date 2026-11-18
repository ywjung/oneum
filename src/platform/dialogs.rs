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
        PickKind::Folder | PickKind::FolderInPlace => {
            dialog.pick_folder().map(|folder| vec![folder])
        }
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
pub fn hand_out(_file: &Path, done: impl FnOnce(super::HandedOut) + Send + 'static) {
    done(super::HandedOut::Kept);
}

/// Folders outside the app's reach that were linked on an earlier run (a phone's way of
/// using a folder where it is): none on a desk, where every folder can be read.
pub fn restore_linked_folders(
    _data_dir: &std::path::Path,
) -> Vec<(std::path::PathBuf, Option<std::path::PathBuf>)> {
    Vec::new()
}

/// The sample music that comes with the app: only the iPhone app has it (see
/// `platform::ios::files::sample_music`).
pub fn sample_music() -> Option<std::path::PathBuf> {
    None
}

pub fn add_sample_music(_name: &str) -> std::io::Result<std::path::PathBuf> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "no sample music here",
    ))
}

/// Whether music folders can be read at all: always on a desk. (Android needs a right
/// the user gives in the system's settings.)
pub fn has_storage_access() -> bool {
    true
}

pub fn ask_storage_access() {}

/// Tries again to reach linked folders that could not be reached: nothing to do on a
/// desk.
pub fn reach_linked_folders() -> bool {
    false
}

/// Forgets the right to read a linked folder (see [`restore_linked_folders`]).
pub fn forget_linked_folder(_folder: &std::path::Path) {}
