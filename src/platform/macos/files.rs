//! Choosing files on a Mac, and keeping the right to read what was chosen.
//!
//! The app that is downloaded reads every folder with the user's own rights. The app of
//! the Mac App Store runs in the App Sandbox: it reads only its own container and what
//! the user handed it (in the open panel, by dropping it on the window, by opening it
//! from Finder), and that only until the app quits. A bookmark made while the right is
//! there brings it back on later runs. The bookmarks are kept in the app's data folder.
//!
//! Outside the sandbox none of this is done: nothing is kept, nothing has to be reached.
use crate::platform::{PickKind, PickRequest};
use objc2::rc::Retained;
use objc2_foundation::{
    NSData, NSString, NSURL, NSURLBookmarkCreationOptions, NSURLBookmarkResolutionOptions,
};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub use crate::platform::dialogs::{
    add_sample_music, ask_all_files_access, ask_storage_access, hand_out, has_all_files_access,
    has_storage_access, offers_all_files_access, sample_music,
};

/// Whether the app runs in the App Sandbox (the system sets this for a sandboxed
/// process, and nothing sets it for another).
pub fn in_sandbox() -> bool {
    static IN_SANDBOX: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *IN_SANDBOX.get_or_init(|| std::env::var_os("APP_SANDBOX_CONTAINER_ID").is_some())
}

/// Shows the dialog `request` describes and hands what was chosen to `done` (see
/// `platform::dialogs::pick`). A folder that was chosen is kept (see [`keep_access`]).
pub fn pick(request: PickRequest, done: impl FnOnce(Vec<PathBuf>) + Send + 'static) {
    let keeps = matches!(request.kind, PickKind::Folder | PickKind::FolderInPlace);
    crate::platform::dialogs::pick(request, move |paths| {
        if keeps {
            keep_access(&paths);
        }
        done(paths);
    });
}

/// A folder or a file outside the container that the user handed to the app: where it
/// was then, and the bookmark (as hexadecimal digits) that gives the right to read it.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct Link {
    path: String,
    bookmark: String,
    /// A single file (a song that was opened), not a folder.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    file: bool,
}

/// How many single files are kept at most: the oldest are let go. (Each link that is
/// reached holds something of the system's for as long as the app runs.)
const MOST_FILES: usize = 1000;

/// The app's data folder, told once at start-up (see [`restore_linked_folders`]).
static DATA_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// What was handed over in this run and could not be kept for later runs.
static NOT_KEPT: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// The links reached in this run: as they are spelled in the links file, and the URL
/// whose right to read is given back when the link is forgotten.
static REACHED: Mutex<Vec<(String, Retained<NSURL>)>> = Mutex::new(Vec::new());

/// Locks a mutex whether or not a thread panicked while holding it: what these hold
/// stays usable.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn links_file() -> Option<PathBuf> {
    Some(DATA_DIR.get()?.join("linked-folders.json"))
}

fn load_links() -> Vec<Link> {
    links_file()
        .and_then(|file| std::fs::read(file).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Writes the list of links. An error says the links are not kept for later runs.
fn save_links(links: &[Link]) -> std::io::Result<()> {
    let Some(file) = links_file() else {
        return Err(std::io::Error::other("the app's data folder is not known"));
    };
    let written = serde_json::to_vec_pretty(links)
        .map_err(std::io::Error::other)
        .and_then(|bytes| {
            if let Some(folder) = file.parent() {
                std::fs::create_dir_all(folder)?;
            }
            // Whole or not at all: a half-written list would lose every link.
            let scratch = file.with_extension("json.new");
            std::fs::write(&scratch, bytes)?;
            std::fs::rename(&scratch, &file)
        });
    if let Err(error) = &written {
        tracing::warn!("cannot keep the links to folders: {error}");
    }
    written
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
        .collect()
}

/// A path as the system resolves it (as it is, where it cannot be resolved).
fn resolved(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Whether the link's place is `path`, which resolves to `path_resolved`. Spelled alike
/// or, for what bears the same name, alike once resolved: the links are many, so the
/// system is only asked where the name already fits.
fn is_at(link: &Link, path: &Path, path_resolved: &Path) -> bool {
    let there = Path::new(&link.path);
    there == path
        || there == path_resolved
        || (there.file_name() == path.file_name() && resolved(there) == path_resolved)
}

/// Whether `inner` lies in `folder` (or is it), as spelled or as `inner` resolves.
fn lies_in(inner: &Path, inner_resolved: &Path, folder: &Path) -> bool {
    inner.starts_with(folder) || inner_resolved.starts_with(folder)
}

/// Whether `path` lies in `folder` (or is it), both as the system resolves them.
fn is_inside(path: &Path, folder: &Path) -> bool {
    resolved(path).starts_with(resolved(folder))
}

/// A bookmark that carries the right to read `path`, which the app may read just now.
fn bookmark_of(path: &Path) -> Option<String> {
    let url = NSURL::fileURLWithPath(&NSString::from_str(path.to_str()?));
    let made = url.bookmarkDataWithOptions_includingResourceValuesForKeys_relativeToURL_error(
        NSURLBookmarkCreationOptions::WithSecurityScope
            | NSURLBookmarkCreationOptions::SecurityScopeAllowOnlyReadAccess,
        None,
        None,
    );
    match made {
        Ok(data) => Some(to_hex(&data.to_vec())),
        Err(error) => {
            tracing::warn!(
                "cannot keep the right to read {}: {error}",
                crate::storage::files::for_log(path)
            );
            None
        }
    }
}

/// Puts `path` into `links` with a bookmark made now. Says whether one could be made.
/// A folder takes the place of the links to what lies in it; of single files only the
/// newest [`MOST_FILES`] stay.
fn put_link(links: &mut Vec<Link>, path: &Path) -> bool {
    let Some(bookmark) = bookmark_of(path) else {
        return false;
    };
    let file = !path.is_dir();
    let at = resolved(path);
    links.retain(|link| {
        let there = Path::new(&link.path);
        !is_at(link, path, &at) && (file || !(there.starts_with(path) || there.starts_with(&at)))
    });
    links.push(Link {
        path: path.to_string_lossy().into_owned(),
        bookmark,
        file,
    });
    let files = links.iter().filter(|link| link.file).count();
    let mut over = files.saturating_sub(MOST_FILES);
    links.retain(|link| {
        let goes = link.file && over > 0;
        over -= usize::from(goes);
        !goes
    });
    true
}

/// Keeps the right to read what the user just handed to the app (chosen in the open
/// panel, dropped on the window, opened from Finder) for later runs. Nothing to do
/// outside the sandbox, for what lies in the app's own container, and for what lies in
/// a folder that is kept already. What cannot be kept is read in this run only (see
/// [`links_not_kept`]).
pub fn keep_access(paths: &[PathBuf]) {
    if !in_sandbox() || paths.is_empty() {
        return;
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut links = load_links();
    let mut changed = false;
    for path in paths {
        if home.as_deref().is_some_and(|home| is_inside(path, home)) {
            continue;
        }
        let at = resolved(path);
        let covered = links.iter().any(|link| {
            if link.file {
                is_at(link, path, &at)
            } else {
                lies_in(path, &at, Path::new(&link.path))
            }
        });
        if covered {
            continue;
        }
        if put_link(&mut links, path) {
            changed = true;
            lock(&NOT_KEPT).retain(|earlier| earlier != path);
        } else {
            let mut not_kept = lock(&NOT_KEPT);
            if !not_kept.contains(path) {
                not_kept.push(path.clone());
            }
        }
    }
    if changed && save_links(&links).is_err() {
        let mut not_kept = lock(&NOT_KEPT);
        for path in paths {
            if !not_kept.contains(path) {
                not_kept.push(path.clone());
            }
        }
    }
}

/// How many folders and files are read in this run only (see [`NOT_KEPT`]).
pub fn links_not_kept() -> usize {
    lock(&NOT_KEPT).len()
}

/// Tries again to keep what is read in this run only. Says whether all of it is kept
/// now.
pub fn keep_links_again() -> bool {
    let paths: Vec<PathBuf> = std::mem::take(&mut *lock(&NOT_KEPT));
    keep_access(&paths);
    links_not_kept() == 0
}

/// Asks the system for the right to read a link again. Says where it is now and, if
/// its bookmark has aged, a fresh one; `None` while it cannot be reached (a drive that
/// is not plugged in, a folder that was deleted).
fn reach(link: &Link) -> Option<(PathBuf, Option<String>)> {
    let bytes = from_hex(&link.bookmark)?;
    let data = NSData::with_bytes(&bytes);
    let mut stale = objc2::runtime::Bool::NO;
    // SAFETY: `stale` is a valid place for the answer for the length of the call.
    let resolved = unsafe {
        NSURL::URLByResolvingBookmarkData_options_relativeToURL_bookmarkDataIsStale_error(
            &data,
            NSURLBookmarkResolutionOptions::WithSecurityScope,
            None,
            &mut stale,
        )
    };
    let url = match resolved {
        Ok(url) => url,
        Err(error) => {
            tracing::warn!(
                "{} cannot be reached: {error}",
                crate::storage::files::for_log(Path::new(&link.path))
            );
            return None;
        }
    };
    // SAFETY: a plain call; the right is kept until the link is forgotten or the app
    // quits.
    if !unsafe { url.startAccessingSecurityScopedResource() } {
        tracing::warn!(
            "the system did not let {} be read",
            crate::storage::files::for_log(Path::new(&link.path))
        );
    }
    let now = PathBuf::from(url.path()?.to_string());
    // A bookmark that has aged is made anew while the place can be reached.
    let fresh = stale.as_bool().then(|| bookmark_of(&now)).flatten();
    lock(&REACHED).push((link.path.clone(), url));
    Some((now, fresh))
}

/// Brings back the right to read what was handed to the app on earlier runs. Called
/// once, before the library looks into its folders; `data_dir` is the app's data
/// folder, where the links are kept. Says for each folder where it was and where it is
/// now (`None`: it cannot be reached, as a drive that is not plugged in). Nothing
/// outside the sandbox.
pub fn restore_linked_folders(data_dir: &Path) -> Vec<(PathBuf, Option<PathBuf>)> {
    let _ = DATA_DIR.set(data_dir.to_path_buf());
    if !in_sandbox() {
        return Vec::new();
    }
    let mut links = load_links();
    tracing::info!(links = links.len(), "running in the App Sandbox");
    let mut changed = false;
    let mut found = Vec::new();
    for link in &mut links {
        let was = PathBuf::from(&link.path);
        let Some((now, fresh)) = reach(link) else {
            if !link.file {
                found.push((was, None));
            }
            continue;
        };
        if let Some(fresh) = fresh {
            link.bookmark = fresh;
            changed = true;
        }
        // What was moved is kept under its new place (the library follows a folder).
        if now != was {
            let spelled = now.to_string_lossy().into_owned();
            for reached in lock(&REACHED).iter_mut() {
                if reached.0 == link.path {
                    reached.0.clone_from(&spelled);
                }
            }
            link.path = spelled;
            changed = true;
        }
        if !link.file {
            found.push((was, Some(now)));
        }
    }
    if changed {
        let _ = save_links(&links);
    }
    found
}

/// Tries again to reach what could not be reached so far in this run (a drive that was
/// plugged in after the app started). Says whether something was reached.
pub fn reach_linked_folders() -> bool {
    if !in_sandbox() {
        return false;
    }
    let mut any = false;
    for link in load_links() {
        if lock(&REACHED).iter().any(|(path, _)| *path == link.path) {
            continue;
        }
        any |= reach(&link).is_some();
    }
    any
}

/// Forgets the link to `folder` (it was taken out of the library) and gives the right
/// to read it back.
pub fn forget_linked_folder(folder: &Path) {
    if !in_sandbox() {
        return;
    }
    let at = resolved(folder);
    let same = |path: &Path| path == folder || path == at || resolved(path) == at;
    let mut links = load_links();
    let before = links.len();
    links.retain(|link| !same(Path::new(&link.path)));
    if links.len() != before {
        let _ = save_links(&links);
    }
    lock(&NOT_KEPT).retain(|path| !same(path));
    lock(&REACHED).retain(|(path, url)| {
        let goes = same(Path::new(path));
        if goes {
            // SAFETY: a plain call on the URL the right was asked for with.
            unsafe { url.stopAccessingSecurityScopedResource() };
        }
        !goes
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_come_back_as_the_bytes_they_were() {
        let bytes = [0u8, 1, 0x7f, 0x80, 0xff];
        assert_eq!(to_hex(&bytes), "00017f80ff");
        assert_eq!(from_hex("00017f80ff").as_deref(), Some(&bytes[..]));
        assert_eq!(from_hex("0"), None);
        assert_eq!(from_hex("zz"), None);
    }

    #[test]
    fn a_link_without_the_word_file_is_a_folder() {
        let links: Vec<Link> =
            serde_json::from_str(r#"[{"path":"/Volumes/Music","bookmark":"00"}]"#).unwrap();
        assert!(!links[0].file);
        let written = serde_json::to_string(&links).unwrap();
        assert!(!written.contains("file"), "{written}");
    }

    #[test]
    fn a_folder_takes_the_place_of_what_lies_in_it_and_old_files_are_let_go() {
        let dir = tempfile::tempdir().unwrap();
        let album = dir.path().join("album");
        std::fs::create_dir(&album).unwrap();
        let song = album.join("song.flac");
        std::fs::write(&song, b"x").unwrap();
        let other = dir.path().join("other.flac");
        std::fs::write(&other, b"x").unwrap();

        let mut links = Vec::new();
        // (Outside the sandbox a bookmark with the right in it may not be made: the
        // rest is only looked at where one was.)
        if !put_link(&mut links, &song) {
            eprintln!("no bookmark could be made here: the order of links is not looked at");
            return;
        }
        assert!(put_link(&mut links, &other));
        assert!(links.iter().all(|link| link.file));
        assert!(put_link(&mut links, &album));
        let kept: Vec<&str> = links.iter().map(|link| link.path.as_str()).collect();
        assert_eq!(kept, [other.to_str().unwrap(), album.to_str().unwrap()]);
        assert!(!links[1].file);

        // The same file again stands once, at the end.
        assert!(put_link(&mut links, &other));
        assert_eq!(links.len(), 2);
        assert_eq!(links[1].path, other.to_str().unwrap());
    }

    #[test]
    fn outside_the_sandbox_nothing_is_kept_and_nothing_is_reached() {
        if in_sandbox() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        keep_access(&[dir.path().to_path_buf()]);
        assert_eq!(links_not_kept(), 0);
        assert!(restore_linked_folders(dir.path()).is_empty());
        assert!(!reach_linked_folders());
        assert!(!dir.path().join("linked-folders.json").exists());
    }
}
