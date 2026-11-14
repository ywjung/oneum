//! Choosing files on an iPhone: the document picker.
//!
//! The app can only keep reading what lies in its own container, so what is chosen
//! elsewhere is brought in: music goes into the Documents folder (where the library
//! looks), anything else that is only read once goes into the temporary folder. What
//! already lies in the container is used where it is.
use super::{run_on_main, scene};
use crate::platform::{PickKind, PickRequest};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class};
use objc2_foundation::{NSArray, NSObject, NSObjectProtocol, NSString, NSURL};
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
    let folder = matches!(kind, PickKind::Folder);
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
    let keep = if matches!(kind, PickKind::File) {
        Keep::Scratch
    } else {
        Keep::Music
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
        let paths: Vec<PathBuf> = urls.iter().filter_map(|url| bring(url, keep)).collect();
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
            let to = free_name(&documents.join(IMPORTED), path.file_name()?)?;
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
        Keep::Music if path.is_dir() => documents.clone(),
        Keep::Music => documents.join(IMPORTED),
    };
    let copied = free_name(&into, path.file_name()?).and_then(|to| {
        copy_tree(&path, &to)
            .inspect_err(|error| tracing::warn!("cannot bring in {}: {error}", path.display()))
            .ok()
            .map(|()| to)
    });
    if scoped {
        // SAFETY: see above.
        unsafe { url.stopAccessingSecurityScopedResource() };
    }
    copied
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
