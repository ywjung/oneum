//! Choosing files and folders, and the right to read them.
//!
//! Android closes shared storage to apps. With "all files access" (which the user
//! gives on a page of the system's settings) the app reads folders by their paths, as
//! on a desk: a folder that is chosen is used where it is. A single file that is chosen
//! (a playlist, a correction file) comes as an address of the system's; the Java side
//! copies it into the app's cache and hands over the path of the copy.
use crate::platform::{PickKind, PickRequest};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};

type Done = Box<dyn FnOnce(Vec<PathBuf>) + Send>;

/// Who waits for which picker.
static WAITING: Mutex<Vec<(i32, Done)>> = Mutex::new(Vec::new());
static NEXT: AtomicI32 = AtomicI32::new(1);

/// Shows the system's picker for `request`. `done` is called later (on Java's main
/// thread) with paths the app can read; not at all if nothing was chosen. Asked where
/// to save a file, it answers at once with a place in the app's cache: the file is
/// offered to the user once it is written (see [`hand_out`]).
pub fn pick(request: PickRequest, done: impl FnOnce(Vec<PathBuf>) + Send + 'static) {
    let kind = match request.kind {
        PickKind::SaveAs(name) => {
            let folder = std::env::temp_dir().join("OnEum-export");
            if let Err(error) = std::fs::create_dir_all(&folder) {
                tracing::warn!("cannot make the export folder: {error}");
                return;
            }
            done(vec![folder.join(clean_name(&name))]);
            return;
        }
        PickKind::Folder | PickKind::FolderInPlace => 0,
        PickKind::Files => 1,
        PickKind::File => 2,
    };
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut waiting) = WAITING.lock() {
        waiting.push((id, Box::new(done)));
    }
    super::java::pick(kind, id);
}

/// The picker asked for under `id` is done (see `natives`).
pub(super) fn picked(id: i32, paths: Vec<PathBuf>) {
    let done = WAITING.lock().ok().and_then(|mut waiting| {
        let at = waiting
            .iter()
            .position(|(waiting_id, _)| *waiting_id == id)?;
        Some(waiting.swap_remove(at).1)
    });
    if let Some(done) = done
        && !paths.is_empty()
    {
        done(paths);
    }
}

/// A file name without what a path would make of it.
fn clean_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | '\0') {
                '_'
            } else {
                c
            }
        })
        .collect();
    if cleaned.trim_matches('.').is_empty() {
        "file".to_string()
    } else {
        cleaned
    }
}

/// Offers a file the app wrote to the user: where to keep it.
pub fn hand_out(file: &Path) {
    let name = file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    super::java::hand_out(&file.to_string_lossy(), &name);
}

/// Whether the app may read all of shared storage.
pub fn has_storage_access() -> bool {
    super::java::has_all_files_access()
}

/// Shows the page of the system's settings where that right is given.
pub fn ask_storage_access() {
    super::java::ask_all_files_access();
}

/// Folders are read by their paths on Android: none has a right of its own to bring
/// back (that is an iPhone's way).
pub fn restore_linked_folders(_data_dir: &Path) -> Vec<(PathBuf, Option<PathBuf>)> {
    Vec::new()
}

pub fn reach_linked_folders() -> bool {
    false
}

pub fn forget_linked_folder(_folder: &Path) {}
