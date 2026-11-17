//! Choosing files on an iPhone: the document picker.
//!
//! The app can only keep reading what lies in its own container, so what is chosen
//! elsewhere is brought in: music goes into the Documents folder (where the library
//! looks), anything else that is only read once goes into the temporary folder. What
//! already lies in the container is used where it is.
//!
//! A folder can also be linked: used where it is (in iCloud Drive, on a drive that is
//! plugged in), without a copy. The system lets the app read a folder the user chose
//! for as long as the app runs; a bookmark made then brings that right back on later
//! runs. The bookmarks are kept in the app's data folder.
use super::{run_on_main, scene};
use crate::platform::{PickKind, PickRequest};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class};
use objc2_foundation::{
    NSArray, NSData, NSObject, NSObjectProtocol, NSString, NSURL, NSURLBookmarkCreationOptions,
    NSURLBookmarkResolutionOptions,
};
use objc2_ui_kit::{UIDocumentPickerDelegate, UIDocumentPickerViewController};
use objc2_uniform_type_identifiers::{UTType, UTTypeFolder, UTTypeItem};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Where what was chosen is kept.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Keep {
    /// In the Documents folder: it is music, and stays.
    Music,
    /// In the temporary folder: it is read once (a playlist, a correction file).
    Scratch,
    /// Where it is: a folder of music that is linked, not copied.
    InPlace,
}

/// The folder inside Documents that single files are brought into.
const IMPORTED: &str = "Imported";

type Done = Box<dyn FnOnce(Vec<PathBuf>) + Send>;

/// The picker that is up: where its choice goes and who is told.
static PENDING: Mutex<Option<(Keep, Done)>> = Mutex::new(None);

thread_local! {
    /// The picker's delegate (the picker does not keep it alive itself).
    static DELEGATE: RefCell<Option<Retained<PickerDelegate>>> = const { RefCell::new(None) };
}

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements; the class has no instance
    // variables and does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "OneumPickerDelegate"]
    struct PickerDelegate;

    unsafe impl NSObjectProtocol for PickerDelegate {}

    unsafe impl UIDocumentPickerDelegate for PickerDelegate {
        #[unsafe(method(documentPicker:didPickDocumentsAtURLs:))]
        fn picked(&self, _picker: &UIDocumentPickerViewController, urls: &NSArray<NSURL>) {
            let urls: Vec<Retained<NSURL>> = urls.iter().collect();
            chosen(urls);
        }

        #[unsafe(method(documentPickerWasCancelled:))]
        fn cancelled(&self, _picker: &UIDocumentPickerViewController) {
            if let Ok(mut pending) = PENDING.lock() {
                pending.take();
            }
        }
    }
);

/// Shows the document picker for `request`. `done` is called later, on the main thread,
/// with paths inside the app's container; not at all if nothing was chosen. Asked where
/// to save a file, it answers at once with a place in the temporary folder: the file is
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
        other => other,
    };
    let done: Done = Box::new(done);
    let show = move || present(&kind, done);
    if MainThreadMarker::new().is_some() {
        show();
    } else {
        run_on_main(show);
    }
}

fn present(kind: &PickKind, done: Done) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let Some(root) = scene::root_view_controller() else {
        tracing::warn!("no view to show the document picker from");
        return;
    };
    let folder = matches!(kind, PickKind::Folder | PickKind::FolderInPlace);
    // SAFETY: constants the UniformTypeIdentifiers framework defines.
    let kind_of_thing: &UTType = unsafe { if folder { UTTypeFolder } else { UTTypeItem } };
    let types = NSArray::from_slice(&[kind_of_thing]);
    // Music files are handed over as copies (the system fetches them first if they are
    // still in iCloud). A folder cannot be: it is copied by us, with the access the
    // picker grants. A single file to read may lie in the app's own folder, where the
    // files it names (a playlist's songs) are beside it, so it is opened where it is.
    let as_copy = matches!(kind, PickKind::Files);
    let picker = UIDocumentPickerViewController::initForOpeningContentTypes_asCopy(
        mtm.alloc(),
        &types,
        as_copy,
    );
    picker.setAllowsMultipleSelection(matches!(kind, PickKind::Files));
    let delegate = DELEGATE.with(|slot| {
        slot.borrow_mut()
            .get_or_insert_with(|| {
                // SAFETY: a class without instance variables: `init` is all it needs.
                unsafe { objc2::msg_send![PickerDelegate::alloc(mtm), init] }
            })
            .clone()
    });
    picker.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    let keep = match kind {
        PickKind::File => Keep::Scratch,
        PickKind::FolderInPlace => Keep::InPlace,
        _ => Keep::Music,
    };
    if let Ok(mut pending) = PENDING.lock() {
        *pending = Some((keep, done));
    }
    root.presentViewController_animated_completion(&picker, true, None);
}

/// The user chose `urls`: they are brought in off the main thread (a folder of music
/// takes a while to copy), and whoever asked is told on the main thread.
fn chosen(urls: Vec<Retained<NSURL>>) {
    let Some((keep, done)) = PENDING.lock().ok().and_then(|mut pending| pending.take()) else {
        return;
    };
    let bring_in = move || {
        let paths: Vec<PathBuf> = urls
            .iter()
            .filter_map(|url| match keep {
                Keep::InPlace => link(url),
                _ => bring(url, keep),
            })
            .collect();
        run_on_main(move || done(paths));
    };
    // The closure is moved into the thread; without one the work is not done at all
    // (a phone that cannot start a thread has other worries).
    if let Err(error) = std::thread::Builder::new()
        .name("oneum-import".into())
        .spawn(bring_in)
    {
        tracing::warn!("cannot start the import: {error}");
    }
}

/// Offers a file the app wrote to the user: where to keep it (the Files app's places).
pub fn hand_out(file: &Path) {
    let file = file.to_path_buf();
    run_on_main(move || {
        let (Some(mtm), Some(root), Some(path)) = (
            MainThreadMarker::new(),
            scene::root_view_controller(),
            file.to_str(),
        ) else {
            return;
        };
        let url = NSURL::fileURLWithPath(&NSString::from_str(path));
        let urls = NSArray::from_retained_slice(&[url]);
        let picker =
            UIDocumentPickerViewController::initForExportingURLs_asCopy(mtm.alloc(), &urls, true);
        root.presentViewController_animated_completion(&picker, true, None);
    });
}

/// A file or folder the system handed over ("Open in OnEum"), as a path the app can
/// read for good: itself if it lies in the app's container, otherwise a copy in the
/// Documents folder.
pub(super) fn take_in(url: &NSURL) -> Option<PathBuf> {
    bring(url, Keep::Music)
}

/// `url` as a path inside the app's container, copying it there if need be.
fn bring(url: &NSURL, keep: Keep) -> Option<PathBuf> {
    let path = PathBuf::from(url.path()?.to_string());
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let documents = home.join("Documents");
    let temporary = std::env::temp_dir();
    if is_inside(&path, &home) {
        // Spelled below the container as the app knows it (the system also reaches it
        // through `/private`): the library knows its files by their paths.
        let path = spelled_below(&path, &home);
        // The picker's copies of music files lie in the temporary folder: they move to
        // where music is kept. Everything else of ours is used where it is.
        return if keep == Keep::Music && is_inside(&path, &temporary) {
            // (A file that is there already with the same bytes is that file: the
            // same song chosen again is not there twice afterwards.)
            let imported = documents.join(IMPORTED);
            std::fs::create_dir_all(&imported)
                .inspect_err(|error| tracing::warn!("cannot make {}: {error}", imported.display()))
                .ok()?;
            let to = crate::storage::files::free_name(&imported, path.file_name()?, Some(&path))
                .inspect_err(|error| tracing::warn!("cannot keep an imported file: {error}"))
                .ok()?;
            if to.exists() {
                let _ = std::fs::remove_file(&path);
                return Some(to);
            }
            std::fs::rename(&path, &to)
                .or_else(|_| std::fs::copy(&path, &to).map(|_| ()))
                .inspect_err(|error| tracing::warn!("cannot keep an imported file: {error}"))
                .ok()?;
            Some(to)
        } else {
            Some(path)
        };
    }
    // Somewhere else: read with the access the system granted for this URL.
    // SAFETY: balanced with the stop below; the URL is alive across both.
    let scoped = unsafe { url.startAccessingSecurityScopedResource() };
    let into = match keep {
        Keep::Scratch => temporary.join("OnEum-import"),
        Keep::Music | Keep::InPlace if path.is_dir() => documents.clone(),
        Keep::Music | Keep::InPlace => documents.join(IMPORTED),
    };
    // What is brought in again is not there twice afterwards: a file with the same
    // bytes is the one that is there, a folder takes in what it does not have yet
    // (see `take_in`). Only what is read once and thrown away is copied as it comes.
    let copied = match keep {
        Keep::Scratch => free_name(&into, path.file_name()?).and_then(|to| {
            copy_tree(&path, &to)
                .inspect_err(|error| tracing::warn!("cannot bring in {}: {error}", path.display()))
                .ok()
                .map(|()| to)
        }),
        Keep::Music | Keep::InPlace => crate::storage::files::take_in(&path, &into)
            .inspect_err(|error| tracing::warn!("cannot bring in {}: {error}", path.display()))
            .ok(),
    };
    if scoped {
        // SAFETY: see above.
        unsafe { url.stopAccessingSecurityScopedResource() };
    }
    copied
}

// ----- linked folders -------------------------------------------------------------------

/// A folder outside the container that is used where it is: where it was when it was
/// linked, and the bookmark (as hexadecimal digits) that gives the right to read it.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct LinkedFolder {
    path: String,
    bookmark: String,
}

/// The app's data folder, told once at start-up (see [`restore_linked_folders`]).
static DATA_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// The file the links are kept in.
fn links_file() -> Option<PathBuf> {
    Some(DATA_DIR.get()?.join("linked-folders.json"))
}

/// Whether two paths name one folder: spelled alike, or alike once the system has
/// resolved them (`/var` is `/private/var` on a phone).
fn same_place(one: &Path, other: &Path) -> bool {
    one == other
        || matches!(
            (std::fs::canonicalize(one), std::fs::canonicalize(other)),
            (Ok(one), Ok(other)) if one == other
        )
}

fn load_links() -> Vec<LinkedFolder> {
    links_file()
        .and_then(|file| std::fs::read(file).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_links(links: &[LinkedFolder]) {
    let Some(file) = links_file() else {
        return;
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
    if let Err(error) = written {
        tracing::warn!("cannot keep the linked folders: {error}");
    }
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

/// A bookmark for `url`, which the app has the right to read just now.
fn bookmark_of(url: &NSURL) -> Option<String> {
    let made = url.bookmarkDataWithOptions_includingResourceValuesForKeys_relativeToURL_error(
        NSURLBookmarkCreationOptions::MinimalBookmark,
        None,
        None,
    );
    match made {
        Ok(data) => Some(to_hex(&data.to_vec())),
        Err(error) => {
            tracing::warn!("cannot keep the right to read a folder: {error}");
            None
        }
    }
}

/// Links the folder at `url`: the app goes on reading it where it is, now and (by the
/// bookmark kept for it) on later runs. A folder of the app's own needs no link.
fn link(url: &NSURL) -> Option<PathBuf> {
    let path = PathBuf::from(url.path()?.to_string());
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    if is_inside(&path, &home) {
        return Some(spelled_below(&path, &home));
    }
    // The right to read it lasts while the app runs: it is not given back, since the
    // library reads the folder from now on.
    // SAFETY: a plain call on a URL the picker handed over.
    if !unsafe { url.startAccessingSecurityScopedResource() } {
        tracing::warn!(
            "the system did not let the folder be read: {}",
            path.display()
        );
    }
    let bookmark = bookmark_of(url)?;
    let mut links = load_links();
    links.retain(|link| !same_place(Path::new(&link.path), &path));
    let spelled = path.to_string_lossy().into_owned();
    lock(&REACHED).push(spelled.clone());
    links.push(LinkedFolder {
        path: spelled,
        bookmark,
    });
    save_links(&links);
    Some(path)
}

/// The linked folders the app has been given the right to read in this run (as they
/// are spelled in the links file).
static REACHED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Locks a mutex whether or not a thread panicked while holding it: what these hold
/// stays usable.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Asks the system for the right to read a linked folder again. Says where the folder
/// is now and, if its bookmark has aged, a fresh one; `None` while it cannot be reached
/// (a drive that is not plugged in, a folder that was deleted).
fn reach(link: &LinkedFolder) -> Option<(PathBuf, Option<String>)> {
    let bytes = from_hex(&link.bookmark)?;
    let data = NSData::with_bytes(&bytes);
    let mut stale = objc2::runtime::Bool::NO;
    // SAFETY: `stale` is a valid place for the answer for the length of the call.
    let resolved = unsafe {
        NSURL::URLByResolvingBookmarkData_options_relativeToURL_bookmarkDataIsStale_error(
            &data,
            NSURLBookmarkResolutionOptions(0),
            None,
            &mut stale,
        )
    };
    let url = match resolved {
        Ok(url) => url,
        Err(error) => {
            tracing::warn!("a linked folder cannot be reached: {error}");
            return None;
        }
    };
    // SAFETY: a plain call; the right is kept for as long as the app runs.
    if !unsafe { url.startAccessingSecurityScopedResource() } {
        tracing::warn!("the system did not let a linked folder be read");
    }
    let now = PathBuf::from(url.path()?.to_string());
    // A bookmark that has aged is made anew while the folder can be reached.
    let fresh = stale.as_bool().then(|| bookmark_of(&url)).flatten();
    lock(&REACHED).push(link.path.clone());
    Some((now, fresh))
}

/// Brings back the right to read the folders that were linked on earlier runs. Called
/// once, before the library looks into its folders; `data_dir` is the app's data
/// folder, where the links are kept. Says for each where it was and where it is now
/// (`None`: it cannot be reached, as a drive that is not plugged in).
pub fn restore_linked_folders(data_dir: &Path) -> Vec<(PathBuf, Option<PathBuf>)> {
    let _ = DATA_DIR.set(data_dir.to_path_buf());
    let mut links = load_links();
    let mut changed = false;
    let mut found = Vec::new();
    for link in &mut links {
        let was = PathBuf::from(&link.path);
        let Some((now, fresh)) = reach(link) else {
            found.push((was, None));
            continue;
        };
        if let Some(fresh) = fresh {
            link.bookmark = fresh;
            changed = true;
        }
        // A folder that was moved is kept under its new place (the library follows).
        if now != was {
            let mut reached = lock(&REACHED);
            reached.retain(|path| *path != link.path);
            link.path = now.to_string_lossy().into_owned();
            reached.push(link.path.clone());
            changed = true;
        }
        found.push((was, Some(now)));
    }
    if changed {
        save_links(&links);
    }
    found
}

/// Tries again to reach the linked folders that could not be reached so far in this
/// run (a drive that was plugged in after the app started). Says whether one was
/// reached. One that was moved meanwhile is followed the next time the app starts.
pub fn reach_linked_folders() -> bool {
    let mut any = false;
    for link in load_links() {
        if lock(&REACHED).contains(&link.path) {
            continue;
        }
        any |= reach(&link).is_some();
    }
    any
}

/// Whether music folders can be read at all: the app's own folder always can. (Android
/// needs a right the user gives in the system's settings.)
pub fn has_storage_access() -> bool {
    true
}

pub fn ask_storage_access() {}

/// Forgets the link to `folder` (it was taken out of the library).
pub fn forget_linked_folder(folder: &Path) {
    let mut links = load_links();
    let before = links.len();
    links.retain(|link| !same_place(Path::new(&link.path), folder));
    if links.len() != before {
        save_links(&links);
    }
}

/// Whether `path` lies in `folder` (both as the system resolves them: the container is
/// reached through `/var` and through `/private/var`).
fn is_inside(path: &Path, folder: &Path) -> bool {
    let resolve = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    resolve(path).starts_with(resolve(folder))
}

/// `path`, which lies in `folder`, as a path that begins the way `folder` is written.
fn spelled_below(path: &Path, folder: &Path) -> PathBuf {
    let resolve = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    match resolve(path).strip_prefix(resolve(folder)) {
        Ok(rest) => folder.join(rest),
        Err(_) => path.to_path_buf(),
    }
}

/// A file name without what a path would make of it.
fn clean_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if matches!(c, '/' | ':' | '\0') {
                '-'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').to_string();
    if cleaned.is_empty() {
        "OnEum".to_string()
    } else {
        cleaned
    }
}

/// A path in `folder` (which is made) for something named `name` that nothing has yet:
/// "Album", "Album 2", "Album 3" …
fn free_name(folder: &Path, name: &std::ffi::OsStr) -> Option<PathBuf> {
    std::fs::create_dir_all(folder)
        .inspect_err(|error| tracing::warn!("cannot make {}: {error}", folder.display()))
        .ok()?;
    let first = folder.join(name);
    if !first.exists() {
        return Some(first);
    }
    let as_path = Path::new(name);
    let stem = as_path.file_stem()?.to_string_lossy().into_owned();
    let extension = as_path
        .extension()
        .map(|e| e.to_string_lossy().into_owned());
    (2..10_000).find_map(|n| {
        let candidate = match &extension {
            Some(extension) => folder.join(format!("{stem} {n}.{extension}")),
            None => folder.join(format!("{stem} {n}")),
        };
        (!candidate.exists()).then_some(candidate)
    })
}

/// Copies a file, or a folder with all that is in it (links are not followed). A file
/// inside a folder that cannot be copied is left out; the rest still comes.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    let kind = std::fs::symlink_metadata(from)?.file_type();
    if kind.is_file() {
        return std::fs::copy(from, to).map(|_| ());
    }
    if !kind.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        if let Err(error) = copy_tree(&entry.path(), &to.join(entry.file_name())) {
            tracing::warn!("left out {}: {error}", entry.path().display());
        }
    }
    Ok(())
}
