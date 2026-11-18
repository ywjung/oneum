//! Choosing files and folders, and the right to read them.
//!
//! Android closes shared storage to apps. With "all files access" (which the user
//! gives on a page of the system's settings) the app reads folders by their paths, as
//! on a desk: a folder that is chosen is used where it is. A single file that is chosen
//! (a playlist, a correction file) comes as an address of the system's; the Java side
//! copies it into the app's cache and hands over the path of the copy.
use crate::platform::{HandedOut, PickKind, PickRequest};
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

/// Whoever waits for the picker asked for under `id`.
fn waiting_for(id: i32) -> Option<Box<dyn FnOnce(Vec<PathBuf>) + Send>> {
    WAITING.lock().ok().and_then(|mut waiting| {
        let at = waiting
            .iter()
            .position(|(waiting_id, _)| *waiting_id == id)?;
        Some(waiting.swap_remove(at).1)
    })
}

/// The picker asked for under `id` is done (see `natives`). Put away with nothing
/// chosen, nobody is told.
pub(super) fn picked(id: i32, paths: Vec<PathBuf>) {
    if let Some(done) = waiting_for(id)
        && !paths.is_empty()
    {
        done(paths);
    }
}

/// Something was chosen in the picker asked for under `id` that cannot be used:
/// whoever asked is told with no paths, and says so on the screen.
pub(super) fn pick_failed(id: i32) {
    if let Some(done) = waiting_for(id) {
        done(Vec::new());
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

type Told = Box<dyn FnOnce(HandedOut) + Send>;

/// Who waits to hear what became of which file that was offered.
static HANDING_OUT: Mutex<Vec<(i32, Told)>> = Mutex::new(Vec::new());

/// Offers a file the app wrote to the user: where to keep it. `done` is told, later
/// and on another thread, once it is kept there (it is copied), the question was put
/// away, or it could not be written.
pub fn hand_out(file: &Path, done: impl FnOnce(HandedOut) + Send + 'static) {
    let name = file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut waiting) = HANDING_OUT.lock() {
        waiting.push((id, Box::new(done)));
    }
    if !super::java::hand_out(&file.to_string_lossy(), &name, id) {
        handed_out(id, HandedOut::Failed);
    }
}

/// What became of the file offered under `id` (see `natives`).
pub(super) fn handed_out(id: i32, outcome: HandedOut) {
    tracing::info!(?outcome, "a file offered to the user");
    let done = HANDING_OUT.lock().ok().and_then(|mut waiting| {
        let at = waiting
            .iter()
            .position(|(waiting_id, _)| *waiting_id == id)?;
        Some(waiting.swap_remove(at).1)
    });
    if let Some(done) = done {
        done(outcome);
    }
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
