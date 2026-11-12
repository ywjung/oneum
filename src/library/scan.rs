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
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
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
const ELOOP: i32 = if cfg!(target_os = "linux") { 40 } else { 62 };
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
                tracing::info!(
                    files = c.files_found.load(Ordering::Relaxed),
                    added = c.tracks_added.load(Ordering::Relaxed),
                    updated = c.tracks_updated.load(Ordering::Relaxed),
                    unsupported = c.unsupported.load(Ordering::Relaxed),
                    errors = c.errors.load(Ordering::Relaxed),
                    "library scan finished"
                );
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
}

/// What the library already knows about a file in the scanned area.
struct Known {
    id: TrackId,
    size: u64,
    mtime_ns: i64,
    needs_touch: bool,
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

fn is_hidden(name: &std::ffi::OsStr) -> bool {
    name.as_encoded_bytes().first() == Some(&b'.')
}

/// True if a scan of the music folder `root` visits `path`: it lies under the folder and
/// no directory or file name on the way is hidden.
pub(crate) fn is_walkable(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root)
        .is_ok_and(|relative| !relative.components().any(|c| is_hidden(c.as_os_str())))
}

/// True if the file is positively known to be absent. "Permission denied", I/O errors
/// and the like are not evidence of absence.
fn file_is_gone(path: &Path) -> bool {
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
            // try again without reading pictures.
            Err(ReadError::Failed(_)) => tags::read_tags(path, false)?,
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
        let entries = WalkDir::new(dir)
            .follow_links(true)
            .into_iter()
            .filter_entry(|entry| entry.depth() == 0 || !is_hidden(entry.file_name()));
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
            let known_file = known.get(path);
            if known_file.is_some() {
                result.encountered.insert(path.to_path_buf());
            }
            counters.files_found.fetch_add(1, Ordering::Relaxed);
            let Ok(meta) = entry.metadata() else {
                counters.errors.fetch_add(1, Ordering::Relaxed);
                counters.files_done.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            let (size, mtime) = (meta.len(), mtime_ns(&meta));
            if let Some(known_file) = known_file
                && known_file.size == size
                && known_file.mtime_ns == mtime
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

fn commit_batch(
    inner: &Inner,
    root_id: i64,
    batch: Vec<Outcome>,
    cancel: &AtomicBool,
    new_ids: &mut Vec<TrackId>,
) {
    let mut files = Vec::with_capacity(batch.len());
    let mut touches = Vec::new();
    for outcome in batch {
        match outcome {
            Outcome::Read(file) => {
                let mut file = *file;
                file.root_id = Some(root_id);
                files.push(file);
            }
            Outcome::Touch(id) => touches.push(id),
        }
    }
    let committed = inner.commit_scanned(files, &touches, Some(root_id), Some(cancel));
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
            && let Some(dir) = path.parent()
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
    sample.iter().any(|path| path.is_file())
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

    // Missing: known, not seen by the walk, and confirmed absent by asking for the file
    // itself. A file that exists but was not visited (or cannot be examined) is left
    // exactly as it is.
    let mut missing: Vec<(&Path, TrackId)> = known
        .iter()
        .filter(|(path, _)| !walk.encountered.contains(*path) && file_is_gone(path))
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
    let alive = if missing.is_empty() {
        true
    } else {
        root_present
            && (relinked > 0
                || walk
                    .encountered
                    .iter()
                    .filter_map(|path| known.get(path))
                    .any(|known| counts(known.id))
                || other_tracks_exist(inner, root_id, known, newest_before, counts))
    };
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
        .map(|(path, id)| (*id, !alive || behind_broken_symlink(path, root_path)))
        .collect();
    let (unavailable, removed) = inner.retire_missing(&candidates, cancel);
    if relinked + unavailable + removed > 0 {
        tracing::info!(relinked, unavailable, removed, "missing files handled");
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
    for track in loose {
        if cancel.load(Ordering::Relaxed) {
            return;
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
    let committed = inner.commit_scanned(files, &back, None, Some(cancel));
    inner
        .counters
        .tracks_updated
        .fetch_add(committed.len() as u64, Ordering::Relaxed);
    inner.retire_missing(&gone, cancel);
    inner.reload_playlists();
    inner.publish();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ReplayGainTags;

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
            art_id: None,
            favorite: false,
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

    #[test]
    fn hidden_names() {
        assert!(is_hidden(std::ffi::OsStr::new(".DS_Store")));
        assert!(is_hidden(std::ffi::OsStr::new("._track.mp3")));
        assert!(!is_hidden(std::ffi::OsStr::new("track.mp3")));
    }
}
