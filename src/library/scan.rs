//! Background scanning of music folders.
//!
//! One scan thread takes requests from a coalescing queue. A scan of a folder runs
//! three stages connected by bounded channels: a directory walker, up to four tag
//! readers, and the scan thread itself as the single database writer (batched). Files
//! whose size and mtime are unchanged are not read again.
//!
//! At the end of a scan, tracks whose files were not seen are handled according to the
//! policy in docs/ARCHITECTURE.md ("누락·분리"): relink to a moved file if one matches,
//! keep as unavailable if the user refers to the track, otherwise remove from the
//! library. Files are never modified or deleted.
//!
//! Removing a track from the library is the one destructive thing a scan does, so it
//! only happens on positive evidence:
//! - the directory walk saw everything (any directory that could not be read makes the
//!   scan incomplete, and an incomplete scan removes and marks nothing);
//! - the file itself is confirmed absent ("not found", not "permission denied");
//! - the folder is demonstrably the folder we know: at least one known file is still
//!   there, or was found again under a new name. A folder path that exists but holds
//!   none of the known files (a stale mount point, a recreated directory) is treated
//!   like an offline folder: its tracks become unavailable and none is removed.
use super::tags::{self, FileTags, ReadError};
use super::{FollowedLink, Inner, art, lock, unix_now};
use crate::model::{Track, TrackId};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};
use walkdir::WalkDir;

/// Rows per database transaction.
const BATCH_ROWS: usize = 200;
/// A partial batch is written after this long without new results.
const BATCH_MAX_WAIT: Duration = Duration::from_millis(250);
/// Default minimum time between snapshot rebuilds while a scan is running.
pub(crate) const PUBLISH_INTERVAL: Duration = Duration::from_millis(500);
/// Capacity of the walker → reader and reader → writer channels.
const CHANNEL_CAPACITY: usize = 256;
const MAX_TAG_READERS: usize = 4;
/// Relinks (and artwork updates) written per database lock hold.
const SMALL_CHUNK: usize = 50;
/// `errno` for "too many levels of symbolic links" (`std::io::ErrorKind` has no stable
/// name for it): 40 on Linux, 62 on macOS and the BSDs.
const ELOOP: i32 = if cfg!(any(target_os = "linux", target_os = "android")) {
    40
} else {
    62
};
/// How long a newly added track remains a candidate for taking over the row of a file
/// that goes missing afterwards.
const RELINK_WINDOW: Duration = Duration::from_secs(120);
/// Upper bound of remembered recently added tracks.
const RELINK_MEMORY: usize = 20_000;
/// How many other tracks of a folder are looked at to tell whether the folder is alive.
const LIVENESS_SAMPLE: usize = 8;

/// Progress counters of the current scan session (reset when a session starts).
#[derive(Default)]
pub(crate) struct Counters {
    pub files_found: AtomicU64,
    pub files_done: AtomicU64,
    pub tracks_added: AtomicU64,
    pub tracks_updated: AtomicU64,
    pub unsupported: AtomicU64,
    pub errors: AtomicU64,
}

impl Counters {
    fn reset(&self) {
        for counter in [
            &self.files_found,
            &self.files_done,
            &self.tracks_added,
            &self.tracks_updated,
            &self.unsupported,
            &self.errors,
        ] {
            counter.store(0, Ordering::Relaxed);
        }
    }
}

/// A queued scan. `root_id == None` checks individually registered files.
struct Job {
    root_id: Option<i64>,
    /// Scan the whole folder (otherwise only `dirs`).
    full: bool,
    dirs: BTreeSet<PathBuf>,
}

#[derive(Default)]
struct QueueState {
    pending: VecDeque<Job>,
    /// Root of the running job and its cancel flag.
    current: Option<(Option<i64>, Arc<AtomicBool>)>,
    current_root: Option<PathBuf>,
    /// True from the first request until the queue runs empty.
    running: bool,
    stopped: bool,
}

/// Coalescing queue of scan requests, served by the scan thread.
#[derive(Default)]
pub(crate) struct ScanQueue {
    state: Mutex<QueueState>,
    changed: Condvar,
}

impl ScanQueue {
    /// Queues a scan of a music folder: of the whole folder when `dirs` is `None`,
    /// otherwise of the given directories. Requests for a folder that is already queued
    /// are merged; a request for the folder being scanned right now runs afterwards.
    pub(crate) fn request_root(
        &self,
        counters: &Counters,
        root_id: i64,
        dirs: Option<BTreeSet<PathBuf>>,
    ) {
        self.request(counters, Some(root_id), dirs);
    }

    /// Queues a check of the individually registered files.
    pub(crate) fn request_loose(&self, counters: &Counters) {
        self.request(counters, None, None);
    }

    fn request(&self, counters: &Counters, root_id: Option<i64>, dirs: Option<BTreeSet<PathBuf>>) {
        let mut state = lock(&self.state);
        if state.stopped {
            return;
        }
        if !state.running {
            state.running = true;
            counters.reset();
        }
        let full = dirs.is_none();
        let dirs = dirs.unwrap_or_default();
        match state.pending.iter_mut().find(|job| job.root_id == root_id) {
            Some(job) => {
                job.full |= full;
                job.dirs.extend(dirs);
            }
            None => state.pending.push_back(Job {
                root_id,
                full,
                dirs,
            }),
        }
        drop(state);
        self.changed.notify_all();
    }

    /// Drops queued scans of a folder and cancels the running one if it is for it.
    pub(crate) fn cancel_root(&self, root_id: i64) {
        let mut state = lock(&self.state);
        state.pending.retain(|job| job.root_id != Some(root_id));
        if let Some((current, cancel)) = &state.current
            && *current == Some(root_id)
        {
            cancel.store(true, Ordering::Relaxed);
        }
        drop(state);
        // Wake the scan thread so it can notice that the queue may now be empty.
        self.changed.notify_all();
    }

    /// Cancels everything and makes the scan thread exit.
    pub(crate) fn stop(&self) {
        let mut state = lock(&self.state);
        state.stopped = true;
        state.pending.clear();
        if let Some((_, cancel)) = &state.current {
            cancel.store(true, Ordering::Relaxed);
        }
        drop(state);
        self.changed.notify_all();
    }

    /// `(running, folder being scanned)`.
    pub(crate) fn status(&self) -> (bool, Option<PathBuf>) {
        let state = lock(&self.state);
        (state.running, state.current_root.clone())
    }

    /// Blocks until no scan is running or queued. Returns false on timeout.
    pub(crate) fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = lock(&self.state);
        while state.running && !state.stopped {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            state = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        true
    }
}

enum Next {
    Job(Job, Arc<AtomicBool>),
    Idle,
    Stop,
}

/// Body of the scan thread.
pub(crate) fn run_scan_thread(inner: Arc<Inner>) {
    loop {
        let next = {
            let mut state = lock(&inner.scan.state);
            loop {
                if state.stopped {
                    state.running = false;
                    break Next::Stop;
                }
                if let Some(job) = state.pending.pop_front() {
                    let cancel = Arc::new(AtomicBool::new(false));
                    state.current = Some((job.root_id, Arc::clone(&cancel)));
                    break Next::Job(job, cancel);
                }
                if state.running {
                    // The queue ran empty (or its last request was cancelled).
                    state.running = false;
                    break Next::Idle;
                }
                state = inner
                    .scan
                    .changed
                    .wait(state)
                    .unwrap_or_else(|e| e.into_inner());
            }
        };

        match next {
            Next::Stop => {
                inner.scan.changed.notify_all();
                return;
            }
            Next::Idle => {
                let c = &inner.counters;
                let (files, added, updated, unsupported, errors) = (
                    c.files_found.load(Ordering::Relaxed),
                    c.tracks_added.load(Ordering::Relaxed),
                    c.tracks_updated.load(Ordering::Relaxed),
                    c.unsupported.load(Ordering::Relaxed),
                    c.errors.load(Ordering::Relaxed),
                );
                // On a phone the folders are looked through again every half minute
                // (see `watch`): a look that brought nothing is not worth a line.
                if crate::platform::PHONE && added == 0 && updated == 0 {
                    tracing::debug!(files, unsupported, errors, "library scan finished");
                } else {
                    tracing::info!(
                        files,
                        added,
                        updated,
                        unsupported,
                        errors,
                        "library scan finished"
                    );
                }
                // Covers that could not be regenerated may be available now.
                lock(&inner.art_jobs).failed.clear();
                inner.scan.changed.notify_all();
                inner.notify();
            }
            Next::Job(job, cancel) => {
                // A panic in a scan must not leave the queue marked as running forever
                // (nor kill the thread): it is contained, logged and counted, and the
                // normal bookkeeping below runs either way.
                let outcome = catch_unwind(AssertUnwindSafe(|| match job.root_id {
                    Some(root_id) => {
                        let root_path = lock(&inner.state).root(root_id).map(|r| r.path.clone());
                        if let Some(root_path) = root_path {
                            lock(&inner.scan.state).current_root = Some(root_path.clone());
                            inner.notify();
                            scan_root(&inner, root_id, &root_path, job.full, job.dirs, &cancel);
                        }
                    }
                    None => scan_loose(&inner, &cancel),
                }));
                if outcome.is_err() {
                    tracing::error!("a library scan failed unexpectedly and was abandoned");
                    inner.counters.errors.fetch_add(1, Ordering::Relaxed);
                    // Show whatever the scan had committed before it failed.
                    inner.publish();
                }
                let mut state = lock(&inner.scan.state);
                state.current = None;
                state.current_root = None;
            }
        }
    }
}

/// A file the walker wants read.
struct Work {
    path: PathBuf,
    size: u64,
    mtime_ns: i64,
    /// For a CUE sheet (`path`): what its tracks are read from. They are read together.
    sheet: Option<SheetWork>,
}

/// A CUE sheet to read (see [`Work`]); `mtime_ns` of its `Work` is the audio file's.
struct SheetWork {
    audio: PathBuf,
    /// The sheet's own modification time, recorded once its tracks are in.
    sheet_mtime: i64,
}

/// The key a sheet is recorded under (see `Inner::sheets_read`).
pub(crate) fn sheet_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// A file that was read successfully.
pub(crate) struct ScannedFile {
    pub path: PathBuf,
    /// Music folder the file is stored under (`None`: individually registered).
    pub root_id: Option<i64>,
    pub size: u64,
    pub mtime_ns: i64,
    pub tags: FileTags,
    pub art_id: Option<String>,
    /// `art_id` is an image file in the folder, not a picture embedded in the file.
    pub art_from_folder: bool,
}

enum Outcome {
    Read(Box<ScannedFile>),
    /// An unchanged file whose row needs its availability or folder fixed.
    Touch(TrackId),
    /// A CUE sheet's tracks were read (sent after them): once they are in, the sheet is
    /// recorded as read at `mtime` (see `Inner::sheets_read`).
    SheetRead {
        key: String,
        record: super::SheetRecord,
        /// The path and state of each file read (the sheet's tracks, or the audio file
        /// alone), all of which have to be in for the record to be kept.
        tracks: Vec<(PathBuf, i64)>,
    },
}

/// What the library already knows about a file in the scanned area.
struct Known {
    id: TrackId,
    size: u64,
    mtime_ns: i64,
    needs_touch: bool,
    /// The tags would be read differently today (see `tags::would_read_differently`):
    /// the file is read again although it has not changed.
    reread: bool,
    art_id: Option<String>,
    folder_art: bool,
}

#[derive(Default)]
struct WalkResult {
    /// Known paths that still exist on disk.
    encountered: HashSet<PathBuf>,
    /// Known paths whose file changed and was sent to the tag readers.
    reread: HashSet<PathBuf>,
    /// Best cover image candidate per visited directory: `(priority, path)`.
    covers: HashMap<PathBuf, (usize, PathBuf)>,
    /// Directory links that were followed: `(link path, resolved target)`.
    links: Vec<(PathBuf, PathBuf)>,
    /// Number of directories (or entries) that could not be read. Whatever is below
    /// them was not seen, so nothing may be concluded from its absence.
    unreadable: usize,
    /// Audio files a CUE sheet is for, with the paths of that sheet's tracks: they are
    /// added as the sheet's tracks, not alone.
    covered: HashMap<PathBuf, Vec<PathBuf>>,
    /// Audio files that have their sheet (the first by name that is for them), with
    /// the paths of its tracks: another sheet for the same file is not used. Unlike
    /// `covered`, also a file that is taken in alone because no track of its sheet lies
    /// within it.
    claimed: HashMap<PathBuf, Vec<PathBuf>>,
    /// Known tracks that a sheet's tracks take the place of (a file from before it had
    /// a sheet, the tracks of a second sheet for the same file), with those tracks. They
    /// go once at least one of their replacements is in the library.
    superseded: HashMap<PathBuf, Vec<PathBuf>>,
    /// CUE sheets met by the walk (by `sheet_key`), readable or not.
    sheets: HashSet<String>,
    /// Audio files found that were not known, as (folder, size) keys (see
    /// [`folder_and_size`]): a sheet's file left alone when its sheet went is found again
    /// by its size.
    new_sizes: HashSet<u64>,
    /// The sheets whose audio file the walk examined (by `sheet_key`), as they were
    /// then: (the sheet's modification time, its file's, that file's size). A record
    /// that is of this state says which tracks the sheet has (see `finish_scan`).
    sheet_states: HashMap<String, (i64, i64, u64)>,
    /// The audio files of sheets that cannot be read just now, as (folder, size) keys
    /// (see `hold_sheet`): such a file is not taken in alone meanwhile.
    held: HashSet<u64>,
    /// Likewise, the files that such sheets have (also one that is taken in alone, no
    /// track of its sheet lying within it): another sheet for it is not used meanwhile.
    held_for_sheet: HashSet<u64>,
    /// The walk ran to its end (it was not cancelled).
    finished: bool,
}

impl WalkResult {
    /// The walk saw the whole scanned area.
    fn complete(&self) -> bool {
        self.finished && self.unreadable == 0
    }
}

/// A file that was looked at and not added.
#[derive(Clone, Copy)]
pub(crate) struct Skipped {
    size: u64,
    mtime_ns: i64,
    unsupported: bool,
}

/// Per-scan memo of the cover image found in each folder.
pub(crate) type FolderArt = Mutex<HashMap<PathBuf, Option<String>>>;

pub(crate) fn mtime_ns(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

/// True for a link inside a music folder that would take the scan out into far more
/// than music: one that leads to the folder itself or above it, to the home folder or
/// above it, or to the top of the disk. Such a link (they come in archives: `extras ->
/// /`) is not followed. Links to other folders, the usual way to keep part of a
/// collection on another drive, are.
pub(crate) fn link_leads_too_far(target: &Path, root: &Path, home: Option<&Path>) -> bool {
    target.parent().is_none()
        || root.starts_with(target)
        || home.is_some_and(|home| home.starts_with(target))
}

fn home_dir() -> Option<PathBuf> {
    let name = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .and_then(|home| fs::canonicalize(home).ok())
}

fn is_hidden(name: &std::ffi::OsStr) -> bool {
    name.as_encoded_bytes().first() == Some(&b'.')
}

/// True if a scan of the music folder `root` visits `path`: it lies under the folder and
/// no directory or file name on the way is hidden.
pub(crate) fn is_walkable(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root)
        .is_ok_and(|relative| !relative.components().any(|c| is_hidden(c.as_os_str())))
}

/// A key for "a file of this size in this folder".
fn folder_and_size(dir: &Path, size: u64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    dir.hash(&mut hasher);
    size.hash(&mut hasher);
    hasher.finish()
}

/// For a sheet's track that has gone: whether what it came from is still where it was —
/// the sheet itself (no longer usable, or without that track), or the audio file it was
/// cut from (now alone, found by its size, which the track recorded). Either shows that
/// the music folder is the one scanned before, not an empty or different drive.
fn sheet_left_traces(path: &Path, known: &Known, walk: &WalkResult) -> bool {
    let Some(sheet) = super::cue::sheet_of(path) else {
        return false;
    };
    sheet.is_file()
        || sheet
            .parent()
            .is_some_and(|dir| walk.new_sizes.contains(&folder_and_size(dir, known.size)))
}

/// True if the file is positively known to be absent. "Permission denied", I/O errors
/// and the like are not evidence of absence.
fn file_is_gone(path: &Path) -> bool {
    // A sheet's track is gone with its sheet, or when the sheet no longer has it (or
    // its audio file is gone).
    if let Some(sheet_path) = super::cue::sheet_of(path) {
        return match super::cue::read_checked(sheet_path) {
            Err(err) => matches!(
                err.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ),
            // There, but not a usable sheet any more.
            Ok(None) => true,
            Ok(Some(sheet)) => {
                let number = super::cue::track_number(path);
                if !sheet.tracks.iter().any(|t| Some(t.number) == number) {
                    return true;
                }
                match super::cue::audio_of_checked(sheet_path, &sheet) {
                    Ok(Some(audio)) => file_is_gone(&audio),
                    // The sheet names several files now, or its file is gone.
                    Ok(None) => true,
                    // Its file cannot be examined just now.
                    Err(_) => false,
                }
            }
        };
    }
    match fs::metadata(path) {
        Ok(meta) => !meta.is_file(),
        Err(err) => matches!(
            err.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
        ),
    }
}

/// Reads one file: tags, properties and artwork (cached as a side effect). Parser panics
/// are contained so that one bad file cannot take a scan down.
pub(crate) fn read_file(
    inner: &Inner,
    path: &Path,
    size: u64,
    mtime_ns: i64,
    folder_art: &FolderArt,
) -> Result<ScannedFile, ReadError> {
    inner.tag_reads.fetch_add(1, Ordering::Relaxed);
    let result = catch_unwind(AssertUnwindSafe(|| {
        let mut tags = match tags::read_tags(path, true) {
            Ok(tags) => tags,
            // An oversized or damaged embedded picture must not cost us the track:
            // try again without reading pictures, and if it is the tags themselves
            // that cannot be read, without them (the audio may be perfectly fine).
            Err(ReadError::Failed(_)) => match tags::read_tags(path, false) {
                Ok(tags) => tags,
                Err(ReadError::Failed(_)) => tags::read_without_tags(path)?,
                Err(unsupported) => return Err(unsupported),
            },
            Err(unsupported) => return Err(unsupported),
        };
        let embedded = tags
            .picture
            .take()
            .and_then(|bytes| inner.art.store_embedded(&bytes));
        let art_from_folder = embedded.is_none();
        let art_id = embedded.or_else(|| folder_art_id(inner, path, folder_art));
        Ok(ScannedFile {
            path: path.to_path_buf(),
            root_id: None,
            size,
            mtime_ns,
            tags,
            art_from_folder: art_from_folder && art_id.is_some(),
            art_id,
        })
    }));
    let result =
        result.unwrap_or_else(|_| Err(ReadError::Failed("the tag parser panicked".into())));
    match &result {
        Ok(_) => {
            lock(&inner.skipped).remove(path);
        }
        Err(err) => {
            tracing::debug!(path = %path.display(), %err, "file skipped");
            lock(&inner.skipped).insert(
                path.to_path_buf(),
                Skipped {
                    size,
                    mtime_ns,
                    unsupported: matches!(err, ReadError::Unsupported(_)),
                },
            );
        }
    }
    result
}

fn folder_art_id(inner: &Inner, path: &Path, folder_art: &FolderArt) -> Option<String> {
    let dir = path.parent()?;
    if let Some(known) = lock(folder_art).get(dir) {
        return known.clone();
    }
    let id = art::find_folder_image(dir).and_then(|image| inner.art.store_folder_image(&image));
    lock(folder_art).insert(dir.to_path_buf(), id.clone());
    id
}

/// True for a file with the `.cue` extension.
fn is_sheet(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cue"))
}

/// A CUE sheet whose tracks cannot be read just now (the sheet or its audio file cannot
/// be examined; that says nothing about what they hold): what is known of its tracks
/// stays as it is, and the audio file is not taken in alone, or by another sheet,
/// meanwhile. Not knowing which file that is, the walk knows it by its size: the one on
/// record from the sheet's last reading, or else the one its tracks recorded (see
/// `WalkResult::held`).
fn hold_sheet(
    inner: &Inner,
    sheet_path: &Path,
    key: String,
    known: &HashMap<PathBuf, Known>,
    result: &mut WalkResult,
) {
    let record = lock(&inner.sheets_read).get(&key).cloned();
    result.sheets.insert(key);
    let dir = sheet_path.parent();
    // The file stays the sheet's (no other sheet takes it). With no track of the sheet
    // within it, it is in the library alone: then there is nothing to keep out.
    if let (Some(record), Some(dir)) = (&record, dir) {
        let file = folder_and_size(dir, record.audio_size);
        result.held_for_sheet.insert(file);
        if !record.tracks.is_empty() {
            result.held.insert(file);
        }
    }
    for (path, track) in known {
        if super::cue::sheet_of(path) != Some(sheet_path) {
            continue;
        }
        // Only the tracks the reading gave: one that the library merely keeps (it
        // starts beyond the end of the file) says nothing about the file.
        let is_given = record.as_ref().is_none_or(|record| {
            super::cue::track_number(path).is_some_and(|number| record.tracks.contains(&number))
        });
        if !is_given {
            continue;
        }
        result.encountered.insert(path.clone());
        if let (None, Some(dir)) = (&record, dir) {
            let file = folder_and_size(dir, track.size);
            result.held.insert(file);
            result.held_for_sheet.insert(file);
        }
    }
}

/// A CUE sheet met by the walk (see `cue`). It comes before the other entries of its
/// folder, so the audio file it is for is marked as covered before the walk comes to
/// it. Its tracks are taken in (or found unchanged). False when the scan is ending (a
/// channel closed).
fn walk_sheet(
    inner: &Inner,
    sheet_path: &Path,
    known: &HashMap<PathBuf, Known>,
    result: &mut WalkResult,
    work_tx: &SyncSender<Work>,
    out_tx: &SyncSender<Outcome>,
) -> bool {
    let counters = &inner.counters;
    let key = sheet_key(sheet_path);
    let read =
        fs::metadata(sheet_path).and_then(|meta| Ok((meta, super::cue::read_checked(sheet_path)?)));
    let (sheet_meta, sheet) = match read {
        Ok((meta, Some(sheet))) => (meta, sheet),
        // Not a usable sheet (any more): its tracks from before go.
        Ok((_, None)) => return true,
        // Gone since the folder was listed: likewise.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return true,
        Err(err) => {
            tracing::debug!(%err, "a CUE sheet could not be read; its tracks are kept");
            hold_sheet(inner, sheet_path, key, known, result);
            return true;
        }
    };
    let audio = match super::cue::audio_of_checked(sheet_path, &sheet) {
        Ok(Some(audio)) => audio,
        // No file for it (any more): its tracks from before go.
        Ok(None) => return true,
        Err(err) => {
            tracing::debug!(%err, "a CUE sheet's file could not be examined; its tracks are kept");
            hold_sheet(inner, sheet_path, key, known, result);
            return true;
        }
    };
    let paths: Vec<PathBuf> = sheet
        .tracks
        .iter()
        .map(|t| super::cue::track_path(sheet_path, t.number))
        .collect();
    // Two sheets for one file: the first (by name) has it, and the other's tracks from
    // before go.
    if let Some(first) = result.claimed.get(&audio) {
        let first = first.clone();
        for path in paths {
            if known.contains_key(&path) {
                result.superseded.insert(path, first.clone());
            }
        }
        return true;
    }
    let audio_meta = match fs::metadata(&audio) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return true,
        Err(err) => {
            tracing::debug!(%err, "a CUE sheet's file could not be examined; its tracks are kept");
            hold_sheet(inner, sheet_path, key, known, result);
            return true;
        }
    };
    let size = audio_meta.len();
    // The file of a sheet that cannot be read just now (one that comes before this one:
    // its file is known by the size its tracks recorded, see `hold_sheet`): that sheet
    // has it, and this one waits like it, with nothing of its own changed.
    if sheet_path
        .parent()
        .is_some_and(|dir| result.held_for_sheet.contains(&folder_and_size(dir, size)))
    {
        result.sheets.insert(key);
        return true;
    }
    let audio_mtime = mtime_ns(&audio_meta);
    let sheet_mtime = mtime_ns(&sheet_meta);
    // Not every track of a sheet is one in the library: those that start beyond the end
    // of the audio are not. The record says how many reading it gave.
    result
        .sheet_states
        .insert(key.clone(), (sheet_mtime, audio_mtime, size));
    let record = lock(&inner.sheets_read).get(&key).cloned();
    let as_read = record.filter(|r| r.is_of(sheet_mtime, audio_mtime, size));
    // No track of it lies within its file (a sheet for a longer rip, or for another
    // disc): the file is taken in alone, as it was when the sheet was read.
    result.claimed.insert(audio.clone(), paths.clone());
    if as_read.as_ref().is_some_and(|r| r.tracks.is_empty()) {
        result.sheets.insert(key);
        return true;
    }
    result.sheets.insert(key.clone());
    result.covered.insert(audio.clone(), paths.clone());
    if known.contains_key(&audio) {
        result.superseded.insert(audio.clone(), paths.clone());
    }
    counters
        .files_found
        .fetch_add(paths.len() as u64, Ordering::Relaxed);
    // A track's state is its stretch of the file (`cue::stretch_stamp`): what was measured
    // of it stays valid when only names in the sheet change. The names are read again
    // when the sheet changed since its tracks were read.
    // Unchanged when the sheet and its file are as they were read, and every track the
    // reading gave is in the library as it was (none was removed meanwhile). A track the
    // reading did not give (it starts beyond the end of the file) is none, whatever the
    // library still holds of it: such a track, kept as unavailable (a favourite), is left
    // alone, neither seen nor brought back.
    let mut unchanged = as_read.is_some();
    let mut tracks = Vec::with_capacity(paths.len());
    for (i, path) in paths.iter().enumerate() {
        let number = sheet.tracks[i].number;
        if as_read
            .as_ref()
            .is_some_and(|record| !record.tracks.contains(&number))
        {
            continue;
        }
        let stamp = super::cue::stretch_stamp(
            audio_mtime,
            sheet.tracks[i].start,
            sheet.tracks.get(i + 1).map(|next| next.start),
        );
        match known.get(path) {
            Some(k) => {
                result.encountered.insert(path.clone());
                unchanged &= k.size == size && k.mtime_ns == stamp && !k.reread;
                tracks.push(k);
            }
            // Removed from the library by the user: not missing, and not a reason to
            // read the sheet again.
            None if inner.is_removed(path) => {}
            None => unchanged = false,
        }
    }
    if unchanged {
        counters
            .files_done
            .fetch_add(paths.len() as u64, Ordering::Relaxed);
        for k in tracks {
            if k.needs_touch && out_tx.send(Outcome::Touch(k.id)).is_err() {
                return false;
            }
        }
        return true;
    }
    result.reread.extend(
        paths
            .iter()
            .filter(|path| known.contains_key(*path))
            .cloned(),
    );
    // The tracks are counted as done when the sheet has been read.
    counters
        .files_done
        .fetch_add(paths.len().saturating_sub(1) as u64, Ordering::Relaxed);
    let work = Work {
        path: sheet_path.to_path_buf(),
        size,
        mtime_ns: audio_mtime,
        sheet: Some(SheetWork { audio, sheet_mtime }),
    };
    work_tx.send(work).is_ok()
}

/// Reads the tracks of a CUE sheet: the audio file's properties and artwork once, the
/// names and times from the sheet (the file's own tags fill what the sheet leaves out).
///
/// Only what lies within the file: a track that starts beyond its end (a sheet for a
/// longer rip, or for another disc) is none, and one runs at most to the end. A sheet
/// with no track within the file is no sheet for it: the file is read alone, as it is.
pub(crate) fn read_sheet(
    inner: &Inner,
    sheet_path: &Path,
    audio: &Path,
    size: u64,
    mtime_ns: i64,
    folder_art: &FolderArt,
) -> Result<Vec<ScannedFile>, ReadError> {
    let sheet = super::cue::read(sheet_path)
        .ok_or_else(|| ReadError::Failed("the CUE sheet cannot be read".into()))?;
    let image = read_file(inner, audio, size, mtime_ns, folder_art)?;
    let total_ms = image.tags.duration_ms;
    let pick = |a: &str, b: &str| {
        if a.is_empty() {
            b.to_string()
        } else {
            a.to_string()
        }
    };
    let mut tracks = Vec::with_capacity(sheet.tracks.len());
    for (i, track) in sheet.tracks.iter().enumerate() {
        let next = sheet.tracks.get(i + 1).map(|next| next.start);
        let Some(length_ms) = sheet_track_length(track.start, next, total_ms) else {
            continue; // starts beyond the end of the audio
        };
        let mut tags = image.tags.clone();
        tags.title = if track.title.is_empty() {
            format!("Track {:02}", track.number)
        } else {
            track.title.clone()
        };
        tags.artist = pick(
            &track.performer,
            &pick(&sheet.performer, &image.tags.artist),
        );
        tags.album_artist = pick(&sheet.performer, &image.tags.album_artist);
        tags.album = pick(&sheet.title, &image.tags.album);
        tags.track_no = Some(track.number);
        tags.disc_no = sheet.disc.or(image.tags.disc_no);
        tags.year = sheet.year.or(image.tags.year);
        tags.genre = pick(&sheet.genre, &image.tags.genre);
        tags.duration_ms = length_ms;
        // The file's own leveling tags are the whole album's.
        let file_gain = &image.tags.replaygain;
        tags.replaygain = crate::model::ReplayGainTags {
            track_gain_db: track.gain_db,
            track_peak: track.peak,
            album_gain_db: sheet
                .album_gain_db
                .or(file_gain.album_gain_db)
                .or(file_gain.track_gain_db),
            album_peak: sheet
                .album_peak
                .or(file_gain.album_peak)
                .or(file_gain.track_peak),
        };
        tracks.push(ScannedFile {
            path: super::cue::track_path(sheet_path, track.number),
            root_id: None,
            size,
            mtime_ns: super::cue::stretch_stamp(
                mtime_ns,
                track.start,
                sheet.tracks.get(i + 1).map(|next| next.start),
            ),
            tags,
            art_id: image.art_id.clone(),
            art_from_folder: image.art_from_folder,
        });
    }
    if tracks.is_empty() {
        return Ok(vec![image]);
    }
    Ok(tracks)
}

/// How long a sheet's track is, in milliseconds, from its start and the next track's
/// (CD frames) and the length of the audio (`0`: not known). `None` when it starts at or
/// beyond the end of the audio: it is no track. It runs at most to the end of the audio;
/// the last track runs to it, and is of unknown length (`0`, as for any file whose
/// length is not known) when that of the audio is.
pub(crate) fn sheet_track_length(start: u64, next: Option<u64>, total_ms: u64) -> Option<u64> {
    let start_ms = super::cue::frames_to_ms(start);
    let end_ms = match next {
        Some(next) if total_ms > 0 => super::cue::frames_to_ms(next).min(total_ms),
        Some(next) => super::cue::frames_to_ms(next),
        None if total_ms > 0 => total_ms,
        None => return Some(0),
    };
    (end_ms > start_ms).then(|| end_ms - start_ms)
}

/// Removes directories that are inside another directory of the set.
fn prune_nested(dirs: BTreeSet<PathBuf>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    // BTreeSet order puts a parent before its children.
    for dir in dirs {
        if !out.iter().any(|kept| dir.starts_with(kept)) {
            out.push(dir);
        }
    }
    out
}

fn scan_root(
    inner: &Inner,
    root_id: i64,
    root_path: &Path,
    full: bool,
    dirs: BTreeSet<PathBuf>,
    cancel: &AtomicBool,
) {
    if !root_path.is_dir() {
        inner.set_root_offline(root_id);
        return;
    }
    #[cfg(test)]
    if inner.panic_in_next_scan.swap(false, Ordering::Relaxed) {
        panic!("simulated scan failure");
    }
    inner.set_root_online(root_id);
    inner.adopt_respelled(root_id, root_path, cancel);

    let scan_dirs: Vec<PathBuf> = if full {
        vec![root_path.to_path_buf()]
    } else {
        prune_nested(
            dirs.into_iter()
                .filter(|d| d.starts_with(root_path))
                .collect(),
        )
    };
    if scan_dirs.is_empty() {
        return;
    }
    tracing::debug!(root = %root_path.display(), full, dirs = scan_dirs.len(), "scan started");

    // Tracks the library already has in the scanned area, including individually
    // registered files that live under this folder (they are adopted by it).
    let known: HashMap<PathBuf, Known> = {
        let state = lock(&inner.state);
        state
            .tracks
            .values()
            .filter(|entry| match entry.track.root_id {
                Some(id) => id == root_id,
                // Files the walk would not visit stay individually registered.
                None => is_walkable(root_path, &entry.track.path),
            })
            .filter(|entry| full || scan_dirs.iter().any(|d| entry.track.path.starts_with(d)))
            .map(|entry| {
                let t = &entry.track;
                (
                    t.path.clone(),
                    Known {
                        id: t.id,
                        size: t.file_size,
                        mtime_ns: t.mtime_ns,
                        needs_touch: !t.available || t.root_id != Some(root_id),
                        reread: [&t.title, &t.artist, &t.album_artist, &t.album, &t.genre]
                            .into_iter()
                            .any(|text| tags::would_read_differently(text)),
                        art_id: t.art_id.clone(),
                        folder_art: entry.folder_art,
                    },
                )
            })
            .collect()
    };

    let readers = std::thread::available_parallelism()
        .map_or(2, |n| n.get())
        .clamp(1, MAX_TAG_READERS);
    let (work_tx, work_rx) = sync_channel::<Work>(CHANNEL_CAPACITY);
    let (out_tx, out_rx) = sync_channel::<Outcome>(CHANNEL_CAPACITY);
    let work_rx = Mutex::new(work_rx);
    let folder_art: FolderArt = Mutex::new(HashMap::new());
    let mut new_ids: Vec<TrackId> = Vec::new();
    // Every track with a higher id than this was added by this scan.
    let newest_before = lock(&inner.state).tracks.keys().copied().max().unwrap_or(0);

    let walk = std::thread::scope(|scope| {
        let walker = {
            let out_tx = out_tx.clone();
            let known = &known;
            let scan_dirs = &scan_dirs;
            std::thread::Builder::new()
                .name("oneum-walk".into())
                .spawn_scoped(scope, move || {
                    walk(inner, scan_dirs, known, &work_tx, &out_tx, cancel)
                })
        };
        for n in 0..readers {
            let out_tx = out_tx.clone();
            let work_rx = &work_rx;
            let folder_art = &folder_art;
            let spawned = std::thread::Builder::new()
                .name(format!("oneum-tags-{n}"))
                .spawn_scoped(scope, move || {
                    read_worker(inner, work_rx, &out_tx, folder_art, cancel)
                });
            if let Err(err) = spawned {
                tracing::warn!(%err, "could not start a tag reader thread");
            }
        }
        drop(out_tx);

        // This thread is the single database writer.
        let mut batch: Vec<Outcome> = Vec::with_capacity(BATCH_ROWS);
        let mut last_publish = Instant::now();
        loop {
            let received = out_rx.recv_timeout(BATCH_MAX_WAIT);
            let disconnected = matches!(received, Err(RecvTimeoutError::Disconnected));
            let timed_out = matches!(received, Err(RecvTimeoutError::Timeout));
            if let Ok(outcome) = received {
                batch.push(outcome);
            }
            if batch.len() >= BATCH_ROWS || ((timed_out || disconnected) && !batch.is_empty()) {
                commit_batch(
                    inner,
                    root_id,
                    std::mem::take(&mut batch),
                    cancel,
                    &mut new_ids,
                );
                let interval =
                    Duration::from_millis(inner.publish_interval_ms.load(Ordering::Relaxed));
                if last_publish.elapsed() >= interval {
                    inner.publish();
                    last_publish = Instant::now();
                } else {
                    inner.notify();
                }
            }
            if disconnected {
                break;
            }
        }

        match walker {
            Ok(handle) => handle.join().unwrap_or_default(),
            Err(err) => {
                tracing::warn!(%err, "could not start the directory walker thread");
                WalkResult::default()
            }
        }
    });

    if walk.finished && !cancel.load(Ordering::Relaxed) {
        remember_links(inner, root_id, full, &scan_dirs, &walk);
        if walk.complete() {
            finish_scan(
                inner,
                root_id,
                root_path,
                full,
                &known,
                &walk,
                &new_ids,
                newest_before,
                cancel,
            );
            if full {
                inner.forget_removed_that_are_gone(root_path);
            }
        } else {
            // Part of the folder could not be read. New and changed files have been
            // taken in, but nothing is concluded from what was not seen: no track is
            // removed, marked missing or relinked, and the scan is not recorded.
            tracing::info!(
                unreadable = walk.unreadable,
                "scan incomplete: some directories could not be read; nothing was removed"
            );
        }
    }
    inner.reload_playlists();
    inner.publish();
    inner.save_sheet_records();
}

/// Records the directory links the walk followed, replacing what was known about the
/// scanned area.
fn remember_links(
    inner: &Inner,
    root_id: i64,
    full: bool,
    scan_dirs: &[PathBuf],
    walk: &WalkResult,
) {
    let mut state = lock(&inner.state);
    state.links.retain(|link| {
        link.root_id != root_id || !(full || scan_dirs.iter().any(|dir| link.link.starts_with(dir)))
    });
    for (link, target) in &walk.links {
        state.links.push(FollowedLink {
            root_id,
            link: link.clone(),
            target: target.clone(),
        });
    }
}

fn walk(
    inner: &Inner,
    dirs: &[PathBuf],
    known: &HashMap<PathBuf, Known>,
    work_tx: &SyncSender<Work>,
    out_tx: &SyncSender<Outcome>,
    cancel: &AtomicBool,
) -> WalkResult {
    let counters = &inner.counters;
    let mut result = WalkResult::default();
    for dir in dirs {
        match fs::symlink_metadata(dir) {
            Ok(_) => {}
            // The directory is gone: whatever was known below it is missing.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                result.unreadable += 1;
                continue;
            }
        }
        let home = home_dir();
        let canonical_root = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        // In each folder its CUE sheets come first: the files they cover are then known
        // when the walk comes to them.
        let entries = WalkDir::new(dir)
            .follow_links(true)
            .sort_by(|a, b| {
                let rank = |entry: &walkdir::DirEntry| !is_sheet(entry.path());
                rank(a)
                    .cmp(&rank(b))
                    .then_with(|| a.file_name().cmp(b.file_name()))
            })
            .into_iter()
            .filter_entry(|entry| {
                if entry.depth() == 0 {
                    return true;
                }
                if is_hidden(entry.file_name()) {
                    return false;
                }
                // A link out of the folder is followed, but not just anywhere.
                let too_far = entry.path_is_symlink()
                    && entry.file_type().is_dir()
                    && fs::canonicalize(entry.path()).is_ok_and(|target| {
                        link_leads_too_far(&target, &canonical_root, home.as_deref())
                    });
                if too_far {
                    tracing::info!("a link that leads out of the music folder to a parent folder was not followed");
                }
                !too_far
            });
        for entry in entries {
            if cancel.load(Ordering::Relaxed) {
                return result;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    let vanished = err
                        .io_error()
                        .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound);
                    // A link that points (directly or in a circle) at itself resolves to
                    // nothing, just like a dangling one.
                    let link_loop = err.loop_ancestor().is_some()
                        || err
                            .io_error()
                            .is_some_and(|io| io.raw_os_error() == Some(ELOOP));
                    if link_loop {
                        tracing::debug!("symbolic link loop skipped");
                    } else if !vanished {
                        // A directory that cannot be listed, a listing that broke off,
                        // a link that cannot be followed: with or without a path, what
                        // lies behind it was not seen.
                        tracing::debug!(%err, "part of the folder could not be read");
                        result.unreadable += 1;
                    }
                    continue;
                }
            };
            let path = entry.path();
            if entry.file_type().is_dir() {
                if entry.depth() > 0
                    && entry.path_is_symlink()
                    && let Ok(target) = fs::canonicalize(path)
                {
                    result.links.push((path.to_path_buf(), target));
                }
                continue;
            }
            if !entry.file_type().is_file() {
                continue;
            }
            if is_sheet(path) {
                if !walk_sheet(inner, path, known, &mut result, work_tx, out_tx) {
                    return result;
                }
                continue;
            }
            if !tags::has_audio_extension(path) {
                if let Some(priority) = art::cover_priority(path)
                    && let Some(dir) = path.parent()
                    && result
                        .covers
                        .get(dir)
                        .is_none_or(|(best, _)| priority < *best)
                {
                    result
                        .covers
                        .insert(dir.to_path_buf(), (priority, path.to_path_buf()));
                }
                continue;
            }
            // Taken in as the tracks of its sheet.
            if result.covered.contains_key(path) {
                continue;
            }
            let known_file = known.get(path);
            if known_file.is_some() {
                result.encountered.insert(path.to_path_buf());
            } else if inner.is_removed(path) {
                // Removed from the library by the user: it is not taken in again.
                continue;
            }
            counters.files_found.fetch_add(1, Ordering::Relaxed);
            let Ok(meta) = entry.metadata() else {
                counters.errors.fetch_add(1, Ordering::Relaxed);
                counters.files_done.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            let (size, mtime) = (meta.len(), mtime_ns(&meta));
            if known_file.is_none()
                && let Some(dir) = path.parent()
            {
                let key = folder_and_size(dir, size);
                // The file of a sheet that cannot be read just now: it is its tracks.
                if result.held.contains(&key) {
                    counters.files_done.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                result.new_sizes.insert(key);
            }
            if let Some(known_file) = known_file
                && known_file.size == size
                && known_file.mtime_ns == mtime
                && !known_file.reread
            {
                counters.files_done.fetch_add(1, Ordering::Relaxed);
                if known_file.needs_touch && out_tx.send(Outcome::Touch(known_file.id)).is_err() {
                    return result;
                }
                continue;
            }
            // A file that was already found unusable and has not changed since.
            let skipped = lock(&inner.skipped).get(path).copied();
            if let Some(skipped) = skipped
                && skipped.size == size
                && skipped.mtime_ns == mtime
            {
                let counter = if skipped.unsupported {
                    &counters.unsupported
                } else {
                    &counters.errors
                };
                counter.fetch_add(1, Ordering::Relaxed);
                counters.files_done.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            if known_file.is_some() {
                result.reread.insert(path.to_path_buf());
            }
            let work = Work {
                path: path.to_path_buf(),
                size,
                mtime_ns: mtime,
                sheet: None,
            };
            if work_tx.send(work).is_err() {
                return result;
            }
        }
    }
    result.finished = true;
    result
}

fn read_worker(
    inner: &Inner,
    work_rx: &Mutex<Receiver<Work>>,
    out_tx: &SyncSender<Outcome>,
    folder_art: &FolderArt,
    cancel: &AtomicBool,
) {
    loop {
        // The guard is dropped before the file is read, so readers work in parallel.
        let next = lock(work_rx).recv();
        let Ok(work) = next else { break };
        if cancel.load(Ordering::Relaxed) {
            continue; // drain so the walker is never blocked on a full channel
        }
        if let Some(sheet) = &work.sheet {
            let read = read_sheet(
                inner,
                &work.path,
                &sheet.audio,
                work.size,
                work.mtime_ns,
                folder_art,
            );
            inner.counters.files_done.fetch_add(1, Ordering::Relaxed);
            match read {
                Ok(tracks) => {
                    let states: Vec<(PathBuf, i64)> = tracks
                        .iter()
                        .map(|t| (t.path.clone(), t.mtime_ns))
                        .collect();
                    for track in tracks {
                        if out_tx.send(Outcome::Read(Box::new(track))).is_err() {
                            return;
                        }
                    }
                    let taken = states
                        .iter()
                        .filter_map(|(path, _)| super::cue::track_number(path))
                        .collect();
                    let done = Outcome::SheetRead {
                        key: sheet_key(&work.path),
                        record: super::SheetRecord {
                            mtime: sheet.sheet_mtime,
                            tracks: taken,
                            audio_mtime: work.mtime_ns,
                            audio_size: work.size,
                        },
                        tracks: states,
                    };
                    if out_tx.send(done).is_err() {
                        return;
                    }
                }
                Err(err) => {
                    tracing::debug!(%err, "CUE sheet skipped");
                    inner.counters.errors.fetch_add(1, Ordering::Relaxed);
                }
            }
            continue;
        }
        let result = read_file(inner, &work.path, work.size, work.mtime_ns, folder_art);
        inner.counters.files_done.fetch_add(1, Ordering::Relaxed);
        match result {
            Ok(file) => {
                if out_tx.send(Outcome::Read(Box::new(file))).is_err() {
                    break;
                }
            }
            Err(ReadError::Unsupported(_)) => {
                inner.counters.unsupported.fetch_add(1, Ordering::Relaxed);
            }
            Err(ReadError::Failed(_)) => {
                inner.counters.errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// A sheet that was read: its key, what to record of it, and the path and state of
/// each file read from it.
pub(crate) type SheetRead = (String, super::SheetRecord, Vec<(PathBuf, i64)>);

/// Records sheets as read. A sheet counts as read once all the files read from it are
/// in the library as they were read (they may have come in an earlier batch; a failed
/// commit leaves the sheet to be read again).
pub(crate) fn record_sheets_read(inner: &Inner, sheets: Vec<SheetRead>) {
    if sheets.is_empty() {
        return;
    }
    let done: Vec<(String, super::SheetRecord)> = {
        let state = lock(&inner.state);
        sheets
            .into_iter()
            .filter(|(_, _, tracks)| {
                tracks.iter().all(|(path, stamp)| {
                    // (A track the user removed is as good as in: it was read, and is
                    // kept out on purpose.)
                    inner.is_removed(path)
                        || state
                            .id_of_path(path)
                            .and_then(|id| state.tracks.get(&id))
                            .is_some_and(|entry| entry.track.mtime_ns == *stamp)
                })
            })
            .map(|(key, record, _)| (key, record))
            .collect()
    };
    if !done.is_empty() {
        lock(&inner.sheets_read).extend(done);
        inner.sheets_dirty.store(true, Ordering::Release);
    }
}

fn commit_batch(
    inner: &Inner,
    root_id: i64,
    batch: Vec<Outcome>,
    cancel: &AtomicBool,
    new_ids: &mut Vec<TrackId>,
) {
    let mut files = Vec::with_capacity(batch.len());
    let mut touches = Vec::new();
    let mut sheets = Vec::new();
    for outcome in batch {
        match outcome {
            Outcome::Read(file) => {
                let mut file = *file;
                file.root_id = Some(root_id);
                files.push(file);
            }
            Outcome::Touch(id) => touches.push(id),
            Outcome::SheetRead {
                key,
                record,
                tracks,
            } => sheets.push((key, record, tracks)),
        }
    }
    let committed = inner.commit_scanned(files, &touches, Some(root_id), Some(cancel));
    record_sheets_read(inner, sheets);
    let mut added = 0;
    let mut updated = 0;
    for (id, is_new) in committed {
        if is_new {
            added += 1;
            new_ids.push(id);
        } else {
            updated += 1;
        }
    }
    if added > 0 {
        let now = Instant::now();
        let mut recent = lock(&inner.recently_added);
        while recent.len() + added as usize > RELINK_MEMORY
            || recent
                .front()
                .is_some_and(|(_, at)| now.duration_since(*at) > RELINK_WINDOW)
        {
            if recent.pop_front().is_none() {
                break;
            }
        }
        let fresh = &new_ids[new_ids.len().saturating_sub(added as usize)..];
        recent.extend(fresh.iter().map(|id| (*id, now)));
    }
    let counters = &inner.counters;
    counters.tracks_added.fetch_add(added, Ordering::Relaxed);
    counters
        .tracks_updated
        .fetch_add(updated, Ordering::Relaxed);
}

/// What identifies "the same recording" when a file has moved: size, duration and tags.
/// A title that merely repeats the file name (untagged file) is ignored, since it
/// changes with a rename.
#[derive(PartialEq, Eq, Hash)]
struct RelinkKey {
    size: u64,
    duration_ms: u64,
    title: Option<String>,
    artist: String,
    album: String,
}

fn relink_key(track: &Track) -> RelinkKey {
    let stem = track.path.file_stem().map(|s| s.to_string_lossy());
    let untitled = stem.is_some_and(|stem| stem == track.title.as_str());
    RelinkKey {
        size: track.file_size,
        duration_ms: track.duration_ms,
        title: (!untitled).then(|| track.title.clone()),
        artist: track.artist.clone(),
        album: track.album.clone(),
    }
}

/// Pairs missing tracks with newly found files. A pair is made only when it is
/// unambiguous: one missing track and one new file share a key, or — among several —
/// exactly one of each has the same file name.
fn match_relinks(missing: &[Arc<Track>], new: &[Arc<Track>]) -> Vec<(TrackId, TrackId)> {
    // Per key: (missing tracks, new files).
    type Group<'a> = (Vec<&'a Arc<Track>>, Vec<&'a Arc<Track>>);
    let mut groups: HashMap<RelinkKey, Group<'_>> = HashMap::new();
    for track in missing {
        groups.entry(relink_key(track)).or_default().0.push(track);
    }
    for track in new {
        if let Some(group) = groups.get_mut(&relink_key(track)) {
            group.1.push(track);
        }
    }
    let mut pairs = Vec::new();
    for (gone, found) in groups.into_values() {
        if found.is_empty() {
            continue;
        }
        if gone.len() == 1 && found.len() == 1 {
            pairs.push((gone[0].id, found[0].id));
            continue;
        }
        for old in &gone {
            let name = old.path.file_name();
            let same_name_new: Vec<_> = found
                .iter()
                .filter(|t| t.path.file_name() == name)
                .collect();
            let same_name_old = gone.iter().filter(|t| t.path.file_name() == name).count();
            if same_name_new.len() == 1 && same_name_old == 1 {
                pairs.push((old.id, same_name_new[0].id));
            }
        }
    }
    pairs
}

/// True if the nearest existing ancestor of `path` (or `path` itself) is a symbolic link
/// whose target is gone — e.g. a link into an unplugged drive. Such files are treated
/// like an offline folder rather than as deleted.
fn behind_broken_symlink(path: &Path, root: &Path) -> bool {
    let mut current = Some(path);
    while let Some(candidate) = current {
        if let Ok(meta) = fs::symlink_metadata(candidate) {
            return meta.file_type().is_symlink() && fs::metadata(candidate).is_err();
        }
        if candidate == root {
            break;
        }
        current = candidate.parent();
    }
    false
}

/// Updates the artwork of unchanged tracks when the cover image in their folder was
/// added, replaced or removed. (Changed files get theirs when they are read.)
fn refresh_folder_art(
    inner: &Inner,
    known: &HashMap<PathBuf, Known>,
    walk: &WalkResult,
    cancel: &AtomicBool,
) {
    let mut by_dir: HashMap<&Path, Vec<&Known>> = HashMap::new();
    for (path, known) in known {
        if walk.encountered.contains(path)
            && !walk.reread.contains(path)
            && let Some(dir) = super::cue::backing_file(path).parent()
        {
            by_dir.entry(dir).or_default().push(known);
        }
    }
    let mut changes: Vec<(TrackId, Option<String>, bool)> = Vec::new();
    for (dir, tracks) in by_dir {
        let cover = walk.covers.get(dir).map(|(_, path)| path);
        if cover.is_none() && tracks.iter().all(|t| !t.folder_art) {
            continue; // no cover here and nobody is using one
        }
        // The id the current cover would have (cheap), and — only if some track needs
        // it — the id after actually caching it (`None` if it cannot be decoded).
        let current = cover.and_then(|path| art::ArtCache::folder_image_id(path));
        let mut cached: Option<Option<String>> = None;
        let mut usable = || {
            cached
                .get_or_insert_with(|| cover.and_then(|path| inner.art.store_folder_image(path)))
                .clone()
        };
        for track in tracks {
            if track.folder_art {
                if track.art_id != current {
                    let art_id = usable();
                    if art_id != track.art_id {
                        changes.push((track.id, art_id.clone(), art_id.is_some()));
                    }
                }
            } else if track.art_id.is_none() {
                if current.is_some()
                    && let Some(art_id) = usable()
                {
                    changes.push((track.id, Some(art_id), true));
                }
            } else if current.is_some() && track.art_id == current {
                // A row written before the source of the artwork was recorded.
                changes.push((track.id, current.clone(), true));
            }
        }
    }
    for chunk in changes.chunks(BATCH_ROWS) {
        let mut db = lock(&inner.db);
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        match db.set_tracks_art(chunk) {
            Ok(()) => {
                let mut state = lock(&inner.state);
                for (id, art_id, folder_art) in chunk {
                    state.set_art(*id, art_id.clone(), *folder_art);
                }
            }
            Err(err) => tracing::warn!(%err, "could not update folder artwork"),
        }
    }
}

/// Reconnects tracks whose files are gone with files that have just been found.
///
/// "Gone" are this scan's missing tracks and every track in the library that is
/// currently unavailable; "found" are the tracks this scan added and those other scans
/// added in the last couple of minutes. That covers a file moved to another music folder
/// (whichever folder is scanned first) and a move the watcher reported in two parts.
/// Returns the ids that were reconnected.
fn relink_moved(
    inner: &Inner,
    missing: &[TrackId],
    new_ids: &[TrackId],
    cancel: &AtomicBool,
) -> HashSet<TrackId> {
    let mut relinked = HashSet::new();
    if new_ids.is_empty() && lock(&inner.recently_added).is_empty() {
        return relinked; // nothing was found that a missing track could have moved to
    }
    let (gone, found) = {
        let state = lock(&inner.state);
        let listed: HashSet<TrackId> = missing.iter().chain(new_ids).copied().collect();
        let track = |id: &TrackId| state.tracks.get(id).map(|entry| Arc::clone(&entry.track));
        let mut gone: Vec<Arc<Track>> = missing.iter().filter_map(track).collect();
        gone.extend(
            state
                .tracks
                .values()
                .map(|entry| &entry.track)
                .filter(|t| !t.available && !listed.contains(&t.id))
                .cloned(),
        );
        let now = Instant::now();
        let mut found_ids: Vec<TrackId> = new_ids.to_vec();
        for (id, at) in lock(&inner.recently_added).iter() {
            if now.duration_since(*at) <= RELINK_WINDOW && !listed.contains(id) {
                found_ids.push(*id);
            }
        }
        let found: Vec<Arc<Track>> = found_ids
            .iter()
            .filter_map(track)
            .filter(|t| t.available)
            .collect();
        (gone, found)
    };
    if gone.is_empty() || found.is_empty() {
        return relinked;
    }
    let confirmed: HashSet<TrackId> = missing.iter().copied().collect();
    let paths: HashMap<TrackId, &Path> = gone.iter().map(|t| (t.id, t.path.as_path())).collect();
    // Tracks from elsewhere in the library are only taken if their file is really gone
    // (the "unavailable" flag may be stale); this scan's own were checked already.
    let pairs: Vec<(TrackId, TrackId)> = match_relinks(&gone, &found)
        .into_iter()
        .filter(|(keep, _)| {
            confirmed.contains(keep) || paths.get(keep).is_some_and(|path| file_is_gone(path))
        })
        .collect();
    for chunk in pairs.chunks(SMALL_CHUNK) {
        let mut db = lock(&inner.db);
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        for &(keep, take) in chunk {
            match db.relink_track(keep, take) {
                Ok(Some(row)) => {
                    let mut state = lock(&inner.state);
                    state.remove(take);
                    state.insert(row.track, row.art_from_folder);
                    relinked.insert(keep);
                }
                Ok(None) => {}
                Err(err) => tracing::warn!(%err, "could not relink a moved track"),
            }
        }
    }
    relinked
}

/// True if some track of the folder outside the scanned area — one that was in the
/// library before this scan and that `counts` as evidence — still has its file.
fn other_tracks_exist(
    inner: &Inner,
    root_id: i64,
    known: &HashMap<PathBuf, Known>,
    newest_before: TrackId,
    counts: impl Fn(TrackId) -> bool,
) -> bool {
    let sample: Vec<PathBuf> = lock(&inner.state)
        .tracks
        .values()
        .map(|entry| &entry.track)
        .filter(|t| {
            t.root_id == Some(root_id)
                && t.available
                && t.id <= newest_before
                && counts(t.id)
                && !known.contains_key(&t.path)
        })
        .take(LIVENESS_SAMPLE)
        .map(|t| t.path.clone())
        .collect();
    sample
        .iter()
        .any(|path| super::cue::backing_file(path).is_file())
}

/// Handles tracks whose files were not found, and records the scan time. Only called
/// after a walk that saw the whole scanned area.
#[allow(clippy::too_many_arguments)]
fn finish_scan(
    inner: &Inner,
    root_id: i64,
    root_path: &Path,
    full: bool,
    known: &HashMap<PathBuf, Known>,
    walk: &WalkResult,
    new_ids: &[TrackId],
    newest_before: TrackId,
    cancel: &AtomicBool,
) {
    refresh_folder_art(inner, known, walk, cancel);
    // Sheets of this folder that the whole walk did not meet are gone: so is their record.
    if full {
        let mut read = lock(&inner.sheets_read);
        let before = read.len();
        read.retain(|key, _| !Path::new(key).starts_with(root_path) || walk.sheets.contains(key));
        if read.len() != before {
            inner.sheets_dirty.store(true, Ordering::Release);
        }
    }

    // Which tracks each sheet has, going by the record of its reading, for the sheets
    // whose record is of the sheet and its file as the walk found them. A track of such
    // a sheet that the record does not list is none (it starts beyond the end of the
    // file, or the file is taken in alone), whatever the library still holds of it,
    // and whether the reading was in this scan or an earlier one.
    let given_by: HashMap<String, Vec<u32>> = {
        let records = lock(&inner.sheets_read);
        walk.sheet_states
            .iter()
            .filter_map(|(key, &(sheet_mtime, audio_mtime, size))| {
                let record = records.get(key)?;
                record
                    .is_of(sheet_mtime, audio_mtime, size)
                    .then(|| (key.clone(), record.tracks.clone()))
            })
            .collect()
    };
    // `Some(true)`: a track its sheet has; `Some(false)`: one it does not have; `None`:
    // not a sheet's track, or nothing is known (the sheet was not read as it is now).
    let given = |path: &Path| -> Option<bool> {
        let number = super::cue::track_number(path)?;
        let tracks = given_by.get(&sheet_key(super::cue::backing_file(path)))?;
        Some(tracks.contains(&number))
    };

    // What a sheet's tracks take the place of goes once they are really in (if the
    // sheet's file could not be read, or no track of the sheet lies within it, the old
    // track stays), and what the user gave it goes to them: the file to all of them, a
    // second sheet's track to the first sheet's track of its number.
    // A second sheet's track that has no such track to go to is simply not there any
    // more, once the first sheet was read as it is (whatever that gave: with no track
    // of it within the file, the file is there alone).
    let mut handovers: Vec<(TrackId, Vec<TrackId>)> = Vec::new();
    let mut outdone: HashSet<TrackId> = HashSet::new();
    {
        let state = lock(&inner.state);
        for (path, replacements) in &walk.superseded {
            let Some(old) = known.get(path) else {
                continue;
            };
            if walk.encountered.contains(path) {
                continue;
            }
            let of_sheet = super::cue::sheet_of(path).is_some();
            let takers: Vec<&PathBuf> = replacements
                .iter()
                .filter(|r| given(r) == Some(true))
                .filter(|r| !of_sheet || r.file_name() == path.file_name())
                .collect();
            let heirs: Vec<TrackId> = takers.iter().filter_map(|r| state.id_of_path(r)).collect();
            if !heirs.is_empty() {
                handovers.push((old.id, heirs));
            } else if of_sheet && replacements.first().is_some_and(|r| given(r).is_some()) {
                outdone.insert(old.id);
            } else if !takers.is_empty() && takers.iter().all(|r| inner.is_removed(r)) {
                // The sheet has its tracks for this, and the user removed every one of
                // them from the library: what they would take the place of goes all
                // the same, with nobody to hand its places to. (Kept as a track that
                // is merely missing, it would be back as soon as a scan met the file
                // without its sheet.)
                handovers.push((old.id, Vec::new()));
            }
        }
    }
    let handed_over = inner.hand_over(&handovers, cancel);
    let replaced: HashSet<TrackId> = handovers.iter().map(|(old, _)| *old).collect();

    // Missing: known, not seen by the walk, and confirmed absent by asking for the file
    // itself. A file that exists but was not visited (or cannot be examined) is left
    // exactly as it is. Also missing: the tracks a sheet does not have (see above).
    let mut missing: Vec<(&Path, TrackId)> = known
        .iter()
        .filter(|(path, known)| {
            !replaced.contains(&known.id)
                && (given(path) == Some(false)
                    || outdone.contains(&known.id)
                    || (!walk.encountered.contains(*path) && file_is_gone(path)))
        })
        .map(|(path, known)| (path.as_path(), known.id))
        .collect();

    let root_present = root_path.is_dir();
    let mut relinked = 0;
    if root_present {
        let ids: Vec<TrackId> = missing.iter().map(|(_, id)| *id).collect();
        let done = relink_moved(inner, &ids, new_ids, cancel);
        relinked = done.len();
        missing.retain(|(_, id)| !done.contains(id));
    }

    // Is this the folder we know? Evidence is a file that was already known and is still
    // there, or one found again under a new name. Without any, the path is treated like
    // an offline folder — and stays in doubt (`unconfirmed_after`) until a track from
    // before that moment is seen again: files that have appeared at the path since are
    // known too by then, but they prove nothing about the old ones.
    let unconfirmed = lock(&inner.state)
        .root(root_id)
        .and_then(|root| root.unconfirmed_after);
    let counts = |id: TrackId| unconfirmed.is_none_or(|limit| id <= limit);
    // What the walk itself shows of files that were known, among those `counts` holds
    // for: one still where it was, one that a sheet now covers (found where it was),
    // or what a sheet's track that has gone came from.
    let seen_by_walk = |counts: &dyn Fn(TrackId) -> bool| {
        walk.encountered
            .iter()
            .chain(walk.covered.keys())
            .filter_map(|path| known.get(path))
            .any(|known| counts(known.id))
            || missing.iter().any(|(path, id)| {
                counts(*id)
                    && known
                        .get(*path)
                        .is_some_and(|k| sheet_left_traces(path, k, walk))
            })
    };
    let alive = if missing.is_empty() {
        true
    } else {
        root_present
            && (relinked > 0
                || seen_by_walk(&counts)
                || other_tracks_exist(inner, root_id, known, newest_before, counts))
    };
    // A folder in doubt is so about the tracks from before the doubt began. Those added
    // since were found in the folder as it has been since: for them, a file known
    // since then that is still there shows that it is that folder, and one of them
    // that has gone is gone (not kept as unavailable for ever).
    let alive_since =
        root_present && unconfirmed.is_some_and(|limit| seen_by_walk(&|id: TrackId| id > limit));
    let in_doubt =
        |id: TrackId| !alive && !(alive_since && unconfirmed.is_some_and(|limit| id > limit));
    // A scoped scan that simply found nothing missing says nothing about the folder.
    let verdict = match (alive, unconfirmed) {
        (false, None) => Some(Some(newest_before)),
        (true, Some(_)) if !missing.is_empty() || full => Some(None),
        _ => None,
    };
    if let Some(mark) = verdict {
        let db = lock(&inner.db);
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        match db.set_root_unconfirmed(root_id, mark) {
            Ok(()) => {
                if let Some(root) = lock(&inner.state).root_mut(root_id) {
                    root.unconfirmed_after = mark;
                }
            }
            Err(err) => tracing::warn!(%err, "could not record the state of a music folder"),
        }
    }
    if !alive {
        tracing::info!(
            tracks = missing.len(),
            "none of the known files of a music folder were found; keeping its tracks as unavailable"
        );
    }
    let candidates: Vec<(TrackId, bool)> = missing
        .iter()
        .map(|(path, id)| (*id, in_doubt(*id) || behind_broken_symlink(path, root_path)))
        .collect();
    let (unavailable, removed) = inner.retire_missing(&candidates, cancel);
    if relinked + unavailable + removed + handed_over > 0 {
        tracing::info!(
            relinked,
            unavailable,
            removed,
            handed_over,
            "missing files handled"
        );
    }

    if full && alive {
        let db = lock(&inner.db);
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let now = unix_now();
        match db.set_root_scanned(root_id, now) {
            Ok(()) => {
                if let Some(root) = lock(&inner.state).root_mut(root_id) {
                    root.last_scan_at = Some(now);
                }
            }
            Err(err) => tracing::warn!(%err, "could not record the scan time"),
        }
    }
}

/// Checks files that were registered individually: those outside every music folder,
/// and those inside one that its scan does not visit (hidden directories).
fn scan_loose(inner: &Inner, cancel: &AtomicBool) {
    let loose: Vec<Arc<Track>> = {
        let state = lock(&inner.state);
        state
            .tracks
            .values()
            .map(|entry| &entry.track)
            .filter(|t| {
                t.root_id.is_none()
                    && state
                        .root_containing(&t.path)
                        .is_none_or(|root| !is_walkable(&root.path, &t.path))
            })
            .cloned()
            .collect()
    };
    if loose.is_empty() {
        return;
    }
    let folder_art: FolderArt = Mutex::new(HashMap::new());
    let mut files = Vec::new();
    let mut back = Vec::new();
    let mut gone: Vec<(TrackId, bool)> = Vec::new();
    // Tracks of CUE sheets (registered from a playlist that names them) are looked at
    // sheet by sheet.
    let mut of_sheets: BTreeMap<PathBuf, Vec<Arc<Track>>> = BTreeMap::new();
    let mut sheets_read = Vec::new();
    for track in loose {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        if let Some(sheet) = super::cue::sheet_of(&track.path) {
            of_sheets
                .entry(sheet.to_path_buf())
                .or_default()
                .push(track);
            continue;
        }
        match fs::metadata(&track.path) {
            Ok(meta) if meta.is_file() => {
                inner.counters.files_found.fetch_add(1, Ordering::Relaxed);
                let (size, mtime) = (meta.len(), mtime_ns(&meta));
                if size != track.file_size || mtime != track.mtime_ns {
                    match read_file(inner, &track.path, size, mtime, &folder_art) {
                        Ok(file) => files.push(file),
                        Err(ReadError::Unsupported(_)) => {
                            inner.counters.unsupported.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(ReadError::Failed(_)) => {
                            inner.counters.errors.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                } else if !track.available {
                    back.push(track.id);
                }
                inner.counters.files_done.fetch_add(1, Ordering::Relaxed);
            }
            _ if file_is_gone(&track.path) => {
                // A vanished folder may be an unplugged drive: keep the track. Only a
                // file missing from a folder that still exists is taken as deleted.
                let folder_exists = track.path.parent().is_some_and(Path::is_dir);
                gone.push((track.id, !folder_exists));
            }
            // The file cannot be examined right now (permissions, I/O error): that says
            // nothing about whether it exists, so the track is left as it is.
            _ => {}
        }
    }
    for (sheet_path, tracks) in &of_sheets {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        check_loose_sheet(
            inner,
            sheet_path,
            tracks,
            &folder_art,
            LooseOutcome {
                files: &mut files,
                back: &mut back,
                gone: &mut gone,
                sheets_read: &mut sheets_read,
            },
        );
    }
    let committed = inner.commit_scanned(files, &back, None, Some(cancel));
    record_sheets_read(inner, sheets_read);
    inner
        .counters
        .tracks_updated
        .fetch_add(committed.len() as u64, Ordering::Relaxed);
    inner.retire_missing(&gone, cancel);
    inner.reload_playlists();
    inner.publish();
    inner.save_sheet_records();
}

/// Where `check_loose_sheet` puts what it finds (see `scan_loose`).
struct LooseOutcome<'a> {
    /// Tracks read again.
    files: &'a mut Vec<ScannedFile>,
    /// Tracks that are there again.
    back: &'a mut Vec<TrackId>,
    /// Tracks that are gone: `(track, keep it)`.
    gone: &'a mut Vec<(TrackId, bool)>,
    /// Sheets read, to be recorded once their tracks are in.
    sheets_read: &'a mut Vec<SheetRead>,
}

/// Looks at the individually registered tracks of one CUE sheet: they are read again
/// when the sheet or its file changed since the sheet was read (by its record, as for
/// a music folder's sheets), are back when they were unavailable, and are gone when the
/// sheet, its file or the track in it is. What cannot be examined is left as it is.
fn check_loose_sheet(
    inner: &Inner,
    sheet_path: &Path,
    tracks: &[Arc<Track>],
    folder_art: &FolderArt,
    out: LooseOutcome<'_>,
) {
    let counters = &inner.counters;
    // A vanished folder may be an unplugged drive: its tracks are kept.
    let folder_exists = sheet_path.parent().is_some_and(Path::is_dir);
    let mut all_gone = |keep: bool| out.gone.extend(tracks.iter().map(|t| (t.id, keep)));
    let read =
        fs::metadata(sheet_path).and_then(|meta| Ok((meta, super::cue::read_checked(sheet_path)?)));
    let (sheet_meta, sheet) = match read {
        Ok((meta, Some(sheet))) => (meta, sheet),
        // Not a sheet any more.
        Ok((_, None)) => return all_gone(false),
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return all_gone(!folder_exists);
        }
        Err(_) => return,
    };
    let audio = match super::cue::audio_of_checked(sheet_path, &sheet) {
        Ok(Some(audio)) => audio,
        Ok(None) => return all_gone(false),
        Err(_) => return,
    };
    let audio_meta = match fs::metadata(&audio) {
        Ok(meta) if meta.is_file() => meta,
        Ok(_) => return all_gone(false),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return all_gone(false),
        Err(_) => return,
    };
    counters
        .files_found
        .fetch_add(tracks.len() as u64, Ordering::Relaxed);
    counters
        .files_done
        .fetch_add(tracks.len() as u64, Ordering::Relaxed);
    let (size, audio_mtime, sheet_mtime) = (
        audio_meta.len(),
        mtime_ns(&audio_meta),
        mtime_ns(&sheet_meta),
    );
    let key = sheet_key(sheet_path);
    let record = lock(&inner.sheets_read)
        .get(&key)
        .filter(|record| record.is_of(sheet_mtime, audio_mtime, size))
        .cloned();
    if let Some(record) = record {
        // As it was read: the tracks it gave are there, the others are none.
        for track in tracks {
            let given = super::cue::track_number(&track.path)
                .is_some_and(|number| record.tracks.contains(&number));
            if !given {
                out.gone.push((track.id, false));
            } else if !track.available {
                out.back.push(track.id);
            }
        }
        return;
    }
    let read = match read_sheet(inner, sheet_path, &audio, size, audio_mtime, folder_art) {
        Ok(read) => read,
        Err(ReadError::Unsupported(_)) => {
            counters.unsupported.fetch_add(1, Ordering::Relaxed);
            return;
        }
        Err(ReadError::Failed(_)) => {
            counters.errors.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };
    let record = super::SheetRecord {
        mtime: sheet_mtime,
        tracks: read
            .iter()
            .filter_map(|file| super::cue::track_number(&file.path))
            .collect(),
        audio_mtime,
        audio_size: size,
    };
    let mut read = read;
    let mut states = Vec::new();
    for track in tracks {
        match read.iter().position(|file| file.path == track.path) {
            Some(at) => {
                let file = read.swap_remove(at);
                states.push((file.path.clone(), file.mtime_ns));
                out.files.push(file);
            }
            // Not a track of the sheet as it is now (or not within its file).
            None => out.gone.push((track.id, false)),
        }
    }
    out.sheets_read.push((key, record, states));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ReplayGainTags;

    #[test]
    fn which_links_lead_too_far() {
        let root = Path::new("/Users/me/Music/Library");
        let home = Some(Path::new("/Users/me"));
        // Up and out: the folder itself, anything above it, the home folder and
        // above, the top of the disk.
        for target in [
            "/",
            "/Users",
            "/Users/me",
            "/Users/me/Music",
            "/Users/me/Music/Library",
        ] {
            assert!(
                link_leads_too_far(Path::new(target), root, home),
                "{target}"
            );
        }
        // Somewhere else: another folder, another drive, a folder beside it.
        for target in [
            "/Volumes/NAS/Music",
            "/Users/me/Music/Other",
            "/Users/me/Downloads/album",
            "/Users/me/Music/Library/sub",
            "/Users/other/Music",
        ] {
            assert!(
                !link_leads_too_far(Path::new(target), root, home),
                "{target}"
            );
        }
        // A music folder outside the home folder, and no home folder known.
        let external = Path::new("/Volumes/Disk/Music");
        assert!(link_leads_too_far(
            Path::new("/Volumes/Disk"),
            external,
            None
        ));
        assert!(!link_leads_too_far(
            Path::new("/Users/me/Music"),
            external,
            home
        ));
        assert!(link_leads_too_far(Path::new("/Users"), external, home));
    }

    fn track(id: TrackId, path: &str, title: &str, size: u64, duration_ms: u64) -> Arc<Track> {
        Arc::new(Track {
            id,
            path: PathBuf::from(path),
            root_id: Some(1),
            available: true,
            title: title.into(),
            artist: "Artist".into(),
            album_artist: String::new(),
            album: "Album".into(),
            album_key: 1,
            disc_no: None,
            track_no: None,
            year: None,
            genre: String::new(),
            duration_ms,
            codec: "MP3".into(),
            container: "MP3".into(),
            sample_rate: 44_100,
            channels: 2,
            bit_depth: None,
            bitrate_kbps: None,
            file_size: size,
            mtime_ns: 0,
            replaygain: ReplayGainTags::default(),
            measured: None,
            unmeasurable: false,
            check: None,
            art_id: None,
            favorite: false,
            rating: 0,
            play_count: 0,
            last_played_at: None,
            added_at: 0,
        })
    }

    #[test]
    fn relink_requires_same_size_and_tags() {
        let missing = [track(1, "/m/a/old.mp3", "Song", 100, 5000)];
        let same = [track(9, "/m/b/new.mp3", "Song", 100, 5000)];
        assert_eq!(match_relinks(&missing, &same), [(1, 9)]);
        let other_size = [track(9, "/m/b/new.mp3", "Song", 101, 5000)];
        assert!(match_relinks(&missing, &other_size).is_empty());
        let other_title = [track(9, "/m/b/new.mp3", "Other", 100, 5000)];
        assert!(match_relinks(&missing, &other_title).is_empty());
        let other_duration = [track(9, "/m/b/new.mp3", "Song", 100, 5001)];
        assert!(match_relinks(&missing, &other_duration).is_empty());
    }

    #[test]
    fn untagged_file_can_be_relinked_after_rename() {
        // The title of an untagged file is its file stem and changes with the rename.
        let missing = [track(1, "/m/demo take 1.wav", "demo take 1", 100, 5000)];
        let new = [track(9, "/m/demo final.wav", "demo final", 100, 5000)];
        assert_eq!(match_relinks(&missing, &new), [(1, 9)]);
    }

    #[test]
    fn ambiguous_matches_are_left_alone_unless_names_decide() {
        let missing = [
            track(1, "/m/a/one.mp3", "Song", 100, 5000),
            track(2, "/m/a/two.mp3", "Song", 100, 5000),
        ];
        // Two identical candidates with unrelated names: no safe pairing.
        let renamed = [
            track(8, "/m/b/x.mp3", "Song", 100, 5000),
            track(9, "/m/b/y.mp3", "Song", 100, 5000),
        ];
        assert!(match_relinks(&missing, &renamed).is_empty());
        // Moved to another folder with the names kept: paired by name.
        let moved = [
            track(8, "/m/b/two.mp3", "Song", 100, 5000),
            track(9, "/m/b/one.mp3", "Song", 100, 5000),
        ];
        let mut pairs = match_relinks(&missing, &moved);
        pairs.sort();
        assert_eq!(pairs, [(1, 9), (2, 8)]);
        // One missing track but two copies of it: ambiguous.
        let copies = [
            track(8, "/m/b/copy1.mp3", "Song", 100, 5000),
            track(9, "/m/b/copy2.mp3", "Song", 100, 5000),
        ];
        assert!(match_relinks(&missing[..1], &copies).is_empty());
    }

    #[test]
    fn nested_directories_are_pruned() {
        let dirs: BTreeSet<PathBuf> = ["/m/a/b", "/m/a", "/m/c", "/m/ab"]
            .into_iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(
            prune_nested(dirs),
            [
                PathBuf::from("/m/a"),
                PathBuf::from("/m/ab"),
                PathBuf::from("/m/c")
            ]
        );
    }

    /// A sheet's track whose audio file cannot be examined just now is not gone; one
    /// whose file is gone (a link to nothing), or no longer in the sheet, is.
    #[cfg(unix)]
    #[test]
    fn a_sheet_track_is_gone_only_when_that_is_known() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let sheet = dir.path().join("Album.cue");
        fs::write(
            &sheet,
            "FILE \"image.flac\" WAVE\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n",
        )
        .unwrap();
        let closed = dir.path().join("closed");
        fs::create_dir(&closed).unwrap();
        fs::write(closed.join("image.flac"), b"x").unwrap();
        std::os::unix::fs::symlink(closed.join("image.flac"), dir.path().join("image.flac"))
            .unwrap();
        let track = super::super::cue::track_path(&sheet, 1);
        assert!(!file_is_gone(&track));
        assert!(file_is_gone(&super::super::cue::track_path(&sheet, 2)));
        fs::set_permissions(&closed, fs::Permissions::from_mode(0o000)).unwrap();
        let entered = fs::metadata(closed.join("image.flac")).is_ok();
        let gone = file_is_gone(&track);
        fs::set_permissions(&closed, fs::Permissions::from_mode(0o755)).unwrap();
        if !entered {
            assert!(!gone, "not known to be gone");
        }
        fs::remove_file(closed.join("image.flac")).unwrap();
        assert!(file_is_gone(&track), "a link to nothing");
    }

    /// A sheet's track lasts to the next one's start, at most to the end of the audio;
    /// one beyond the end is none; the last, of audio of unknown length, is kept with
    /// its length unknown.
    #[test]
    fn how_long_a_sheets_track_is() {
        let s = |seconds: u64| seconds * 75; // CD frames
        assert_eq!(sheet_track_length(s(0), Some(s(60)), 300_000), Some(60_000));
        assert_eq!(sheet_track_length(s(240), None, 300_000), Some(60_000));
        assert_eq!(
            sheet_track_length(s(240), Some(s(400)), 300_000),
            Some(60_000),
            "to the end of the audio"
        );
        assert_eq!(sheet_track_length(s(300), Some(s(400)), 300_000), None);
        assert_eq!(sheet_track_length(s(400), None, 300_000), None);
        // Audio of unknown length: the next start is taken as written, the last runs
        // to the end with no length known.
        assert_eq!(sheet_track_length(s(240), Some(s(400)), 0), Some(160_000));
        assert_eq!(sheet_track_length(s(240), None, 0), Some(0));
    }

    #[test]
    fn hidden_names() {
        assert!(is_hidden(std::ffi::OsStr::new(".DS_Store")));
        assert!(is_hidden(std::ffi::OsStr::new("._track.mp3")));
        assert!(!is_hidden(std::ffi::OsStr::new("track.mp3")));
    }
}
