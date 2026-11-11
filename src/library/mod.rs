//! The music library: folders, scanning, the in-memory index, playlists and artwork.
//!
//! See docs/ARCHITECTURE.md ("라이브러리와 저장"). The SQLite database is the source of
//! truth. [`Library`] keeps every track in memory and publishes immutable
//! [`LibrarySnapshot`]s that the UI reads without locking; all file and database work
//! happens on background threads or in the calling (non-UI) thread.
//!
//! Typical start-up: [`Library::open`], [`Library::set_notifier`], then
//! [`Library::rescan`] and [`Library::start_watching`].
mod art;
mod ident;
mod m3u;
mod scan;
mod search;
mod snapshot;
mod tags;
mod watch;

pub use art::ArtImage;
pub use ident::{album_key, fnv1a64};
pub use search::{PreparedQuery, normalize};
pub use snapshot::{Album, Artist, LibrarySnapshot, SortKey};

use crate::model::{Track, TrackId};
use crate::storage::db::{DB_FILE_NAME, Db, DbError, StoredTrack};
use crate::storage::settings::write_atomic;
use art::ArtCache;
use scan::{Counters, FolderArt, ScanQueue, ScannedFile};
use snapshot::TrackIndex;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use unicode_normalization::UnicodeNormalization;
use watch::WatchMsg;

/// Where the library keeps its files.
#[derive(Debug, Clone)]
pub struct LibraryPaths {
    /// Durable data: `library.db` lives here.
    pub data_dir: PathBuf,
    /// Disposable data: the artwork cache lives in `<cache_dir>/art`.
    pub cache_dir: PathBuf,
}

/// Things that happened while opening the library which the user should be told about.
#[derive(Debug, Clone, Default)]
pub struct OpenReport {
    /// The database was unusable and was moved to this path; a new one was created.
    pub recovered_corrupt_db: Option<PathBuf>,
    /// The database was upgraded from this schema version.
    pub migrated_from: Option<u32>,
}

#[derive(Debug)]
pub enum LibraryError {
    /// The folder is already a music folder.
    AlreadyAdded,
    /// The folder lies inside this existing music folder.
    InsideExistingFolder(PathBuf),
    /// The folder contains this existing music folder.
    ContainsExistingFolder(PathBuf),
    NotADirectory,
    InvalidName,
    NotFound,
    Io(std::io::Error),
    Db(String),
}

impl fmt::Display for LibraryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LibraryError::AlreadyAdded => write!(f, "the folder is already in the library"),
            LibraryError::InsideExistingFolder(root) => write!(
                f,
                "the folder is inside a folder that is already in the library ({})",
                root.display()
            ),
            LibraryError::ContainsExistingFolder(root) => write!(
                f,
                "the folder contains a folder that is already in the library ({})",
                root.display()
            ),
            LibraryError::NotADirectory => write!(f, "not a folder"),
            LibraryError::InvalidName => write!(f, "the name must not be empty"),
            LibraryError::NotFound => write!(f, "not found"),
            LibraryError::Io(err) => write!(f, "file error: {err}"),
            LibraryError::Db(message) => write!(f, "library database error: {message}"),
        }
    }
}

impl std::error::Error for LibraryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LibraryError::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for LibraryError {
    fn from(err: std::io::Error) -> Self {
        LibraryError::Io(err)
    }
}

impl From<rusqlite::Error> for LibraryError {
    fn from(err: rusqlite::Error) -> Self {
        LibraryError::Db(err.to_string())
    }
}

impl From<DbError> for LibraryError {
    fn from(err: DbError) -> Self {
        match err {
            DbError::Io(err) => LibraryError::Io(err),
            other => LibraryError::Db(other.to_string()),
        }
    }
}

/// A music folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    pub id: i64,
    pub path: PathBuf,
    /// False while the folder does not exist (e.g. its drive is unplugged).
    pub online: bool,
    pub track_count: usize,
    /// Unix seconds of the last completed full scan.
    pub last_scan_at: Option<i64>,
}

/// Progress of background scanning. Counters cover the current scan session (from the
/// first request until the scanner is idle again) and keep their final values afterwards.
#[derive(Clone, Debug, Default)]
pub struct ScanStatus {
    pub running: bool,
    /// Audio files seen by the directory walk.
    pub files_found: u64,
    /// Files dealt with (unchanged, read, unsupported or failed).
    pub files_done: u64,
    pub tracks_added: u64,
    pub tracks_updated: u64,
    /// Files in a known container that the player cannot play (DRM, Opus, …).
    pub unsupported: u64,
    /// Files that could not be read or parsed.
    pub errors: u64,
    pub current_root: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistInfo {
    pub id: i64,
    pub name: String,
    pub item_count: usize,
}

/// One entry of a playlist. `item_id` identifies the entry itself, so the same track can
/// appear several times.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaylistItem {
    pub item_id: i64,
    pub track_id: TrackId,
}

/// Result of importing an M3U playlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct M3uImport {
    pub playlist_id: i64,
    pub name: String,
    /// Number of entries added to the playlist.
    pub matched: usize,
    /// Entries (as written in the file) that could not be found or read.
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlaylistData {
    id: i64,
    name: String,
    items: Vec<PlaylistItem>,
}

pub(crate) struct Entry {
    pub(crate) track: Arc<Track>,
    pub(crate) index: Arc<TrackIndex>,
}

pub(crate) struct RootState {
    pub(crate) id: i64,
    pub(crate) path: PathBuf,
    pub(crate) online: bool,
    pub(crate) last_scan_at: Option<i64>,
}

/// In-memory mirror of the `roots` and `tracks` tables.
#[derive(Default)]
pub(crate) struct State {
    pub(crate) tracks: HashMap<TrackId, Entry>,
    by_path: HashMap<PathBuf, TrackId>,
    pub(crate) roots: Vec<RootState>,
    /// Bumped by every change to `tracks`; lets `publish` skip redundant rebuilds.
    revision: u64,
}

impl State {
    pub(crate) fn root(&self, id: i64) -> Option<&RootState> {
        self.roots.iter().find(|root| root.id == id)
    }

    pub(crate) fn root_mut(&mut self, id: i64) -> Option<&mut RootState> {
        self.roots.iter_mut().find(|root| root.id == id)
    }

    /// The music folder that contains `path`, if any.
    pub(crate) fn root_containing(&self, path: &Path) -> Option<&RootState> {
        self.roots.iter().find(|root| path.starts_with(&root.path))
    }

    /// Adds a track or replaces the one with the same id.
    pub(crate) fn insert(&mut self, track: Track) -> Arc<Track> {
        let reused_index = match self.tracks.get(&track.id) {
            Some(old) => {
                if old.track.path != track.path {
                    self.by_path.remove(&old.track.path);
                }
                let same_text = old.track.title == track.title
                    && old.track.artist == track.artist
                    && old.track.album == track.album
                    && old.track.album_artist == track.album_artist;
                same_text.then(|| Arc::clone(&old.index))
            }
            None => None,
        };
        let index = reused_index.unwrap_or_else(|| Arc::new(TrackIndex::new(&track)));
        self.by_path.insert(track.path.clone(), track.id);
        let track = Arc::new(track);
        self.tracks.insert(
            track.id,
            Entry {
                track: Arc::clone(&track),
                index,
            },
        );
        self.revision += 1;
        track
    }

    pub(crate) fn remove(&mut self, id: TrackId) -> Option<Arc<Track>> {
        let entry = self.tracks.remove(&id)?;
        if self.by_path.get(&entry.track.path) == Some(&id) {
            self.by_path.remove(&entry.track.path);
        }
        self.revision += 1;
        Some(entry.track)
    }

    /// Changes fields that are not part of the search index or of a track's identity
    /// (availability, folder, favourite, play statistics).
    pub(crate) fn update(
        &mut self,
        id: TrackId,
        change: impl FnOnce(&mut Track),
    ) -> Option<Arc<Track>> {
        let entry = self.tracks.get_mut(&id)?;
        let mut track = (*entry.track).clone();
        change(&mut track);
        entry.track = Arc::new(track);
        self.revision += 1;
        Some(Arc::clone(&entry.track))
    }
}

type Notifier = Arc<dyn Fn() + Send + Sync>;

pub(crate) struct Inner {
    db: Mutex<Db>,
    state: Mutex<State>,
    snapshot: RwLock<Arc<LibrarySnapshot>>,
    /// Serialises snapshot replacement; holds the state revision of the last full build.
    publish_lock: Mutex<Option<u64>>,
    generation: AtomicU64,
    playlists: Mutex<Vec<PlaylistData>>,
    playlists_generation: AtomicU64,
    notifier: RwLock<Option<Notifier>>,
    art: ArtCache,
    /// Artwork ids being regenerated, or that could not be regenerated this session.
    art_regen: Mutex<HashSet<String>>,
    scan: ScanQueue,
    counters: Counters,
    /// Files that could not be added, by path, with the size and mtime they had. Lets
    /// later scans count them again without reading them again (not persisted).
    skipped: Mutex<HashMap<PathBuf, scan::Skipped>>,
    /// Minimum time between snapshot rebuilds during a scan, in milliseconds.
    publish_interval_ms: AtomicU64,
    tag_reads: AtomicU64,
    shutdown: AtomicBool,
    watch_tx: Mutex<Option<Sender<WatchMsg>>>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

/// Lock order: `db` before `state`; `publish_lock` before `state`. The notifier is never
/// called with a lock held. Tracks in `state` are only changed while `db` is held, so
/// holding `db` alone is enough to read a stable view; `state` itself is only ever held
/// for short in-memory work (never across database or file access), which keeps
/// `Library::track` and `Library::roots` fast at all times.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

fn to_stored(file: ScannedFile, root_id: Option<i64>, now: i64) -> StoredTrack {
    let tags = file.tags;
    let album_key = album_key(
        &tags.album,
        &tags.album_artist,
        tags.compilation,
        &file.path,
    );
    StoredTrack {
        track: Track {
            id: 0,
            path: file.path,
            root_id,
            available: true,
            title: tags.title,
            artist: tags.artist,
            album_artist: tags.album_artist,
            album: tags.album,
            album_key,
            disc_no: tags.disc_no,
            track_no: tags.track_no,
            year: tags.year,
            genre: tags.genre,
            duration_ms: tags.duration_ms,
            codec: tags.codec.to_string(),
            container: tags.container.to_string(),
            sample_rate: tags.sample_rate,
            channels: tags.channels,
            bit_depth: tags.bit_depth,
            bitrate_kbps: tags.bitrate_kbps,
            file_size: file.size,
            mtime_ns: file.mtime_ns,
            replaygain: tags.replaygain,
            art_id: file.art_id,
            favorite: false,
            play_count: 0,
            last_played_at: None,
            added_at: now,
        },
        compilation: tags.compilation,
    }
}

impl Inner {
    fn notify(&self) {
        let notifier = self
            .notifier
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(notifier) = notifier {
            notifier();
        }
    }

    fn snapshot(&self) -> Arc<LibrarySnapshot> {
        Arc::clone(&self.snapshot.read().unwrap_or_else(|e| e.into_inner()))
    }

    fn replace_snapshot(&self, snapshot: LibrarySnapshot) {
        *self.snapshot.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(snapshot);
        self.generation.fetch_add(1, Ordering::Release);
    }

    /// Rebuilds and publishes the snapshot if tracks changed since the last full build,
    /// then calls the notifier.
    fn publish(&self) {
        {
            let mut published = lock(&self.publish_lock);
            let (revision, entries) = {
                let state = lock(&self.state);
                if *published == Some(state.revision) {
                    (state.revision, None)
                } else {
                    let entries: Vec<_> = state
                        .tracks
                        .values()
                        .map(|e| (Arc::clone(&e.track), Arc::clone(&e.index)))
                        .collect();
                    (state.revision, Some(entries))
                }
            };
            if let Some(entries) = entries {
                self.replace_snapshot(LibrarySnapshot::build(entries));
                *published = Some(revision);
            }
        }
        self.notify();
    }

    /// Publishes a change to fields that are not indexed (favourite, play statistics)
    /// without rebuilding the index.
    fn publish_patch(&self, changed: &[Arc<Track>]) {
        if changed.is_empty() {
            return;
        }
        {
            let _published = lock(&self.publish_lock);
            let patched = self.snapshot().with_replaced(changed);
            self.replace_snapshot(patched);
        }
        self.notify();
    }

    /// Reloads the playlist cache from the database. Returns true if it changed.
    fn reload_playlists(&self) -> bool {
        let loaded = {
            let db = lock(&self.db);
            db.playlists()
                .and_then(|lists| Ok((lists, db.all_playlist_items()?)))
        };
        let (lists, items) = match loaded {
            Ok(loaded) => loaded,
            Err(err) => {
                tracing::warn!(%err, "could not load playlists");
                return false;
            }
        };
        let mut data: Vec<PlaylistData> = lists
            .into_iter()
            .map(|(id, name)| PlaylistData {
                id,
                name,
                items: Vec::new(),
            })
            .collect();
        let position: HashMap<i64, usize> =
            data.iter().enumerate().map(|(i, p)| (p.id, i)).collect();
        for (playlist_id, item_id, track_id) in items {
            if let Some(&i) = position.get(&playlist_id) {
                data[i].items.push(PlaylistItem { item_id, track_id });
            }
        }
        let mut current = lock(&self.playlists);
        if *current == data {
            return false;
        }
        *current = data;
        self.playlists_generation.fetch_add(1, Ordering::Release);
        true
    }

    /// Writes scanned files to the database and the in-memory state. Returns
    /// `(track id, newly added)` per file, in order. Nothing is written once `cancel` is
    /// set (checked under the database lock, see `Library::remove_folder`).
    fn commit_scanned(
        &self,
        root_id: Option<i64>,
        files: Vec<ScannedFile>,
        touches: &[TrackId],
        cancel: Option<&AtomicBool>,
    ) -> Vec<(TrackId, bool)> {
        if files.is_empty() && touches.is_empty() {
            return Vec::new();
        }
        let now = unix_now();
        let rows: Vec<StoredTrack> = files
            .into_iter()
            .map(|file| to_stored(file, root_id, now))
            .collect();

        let mut db = lock(&self.db);
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return Vec::new();
        }
        let ids = match db.upsert_tracks(&rows) {
            Ok(ids) => ids,
            Err(err) => {
                tracing::warn!(%err, count = rows.len(), "could not store scanned tracks");
                self.counters
                    .errors
                    .fetch_add(rows.len() as u64, Ordering::Relaxed);
                return Vec::new();
            }
        };
        let touched = match db.adopt_tracks(touches, root_id) {
            Ok(()) => true,
            Err(err) => {
                tracing::warn!(%err, "could not update unchanged tracks");
                false
            }
        };

        let mut state = lock(&self.state);
        let mut committed = Vec::with_capacity(ids.len());
        for (row, id) in rows.into_iter().zip(ids) {
            let mut track = row.track;
            track.id = id;
            let is_new = match state.tracks.get(&id) {
                Some(old) => {
                    track.favorite = old.track.favorite;
                    track.play_count = old.track.play_count;
                    track.last_played_at = old.track.last_played_at;
                    track.added_at = old.track.added_at;
                    false
                }
                None => true,
            };
            state.insert(track);
            committed.push((id, is_new));
        }
        if touched {
            for &id in touches {
                state.update(id, |t| {
                    t.available = true;
                    t.root_id = root_id;
                });
            }
        }
        committed
    }

    /// Applies the missing-file policy to tracks whose files are gone. Each candidate
    /// is `(track id, must keep)`: tracks that must be kept, are favourites or are in a
    /// playlist stay in the library as unavailable; the others are removed from it.
    /// Returns `(marked unavailable, removed)`. Stops early once `cancel` is set.
    fn retire_missing(
        &self,
        candidates: &[(TrackId, bool)],
        cancel: &AtomicBool,
    ) -> (usize, usize) {
        /// Rows per lock hold, so other threads are never kept waiting for long.
        const CHUNK: usize = 500;
        let mut keep: Vec<TrackId> = Vec::new();
        let mut removable: Vec<TrackId> = Vec::new();
        for &(id, must_keep) in candidates {
            if must_keep {
                keep.push(id);
            } else {
                removable.push(id);
            }
        }

        let mut removed = 0;
        for chunk in removable.chunks(CHUNK) {
            let mut db = lock(&self.db);
            if cancel.load(Ordering::Relaxed) {
                return (0, removed);
            }
            match db.delete_unreferenced_tracks(chunk) {
                Ok(deleted) => {
                    let deleted: HashSet<TrackId> = deleted.into_iter().collect();
                    let mut state = lock(&self.state);
                    for id in chunk {
                        if deleted.contains(id) {
                            state.remove(*id);
                        } else {
                            keep.push(*id);
                        }
                    }
                    removed += deleted.len();
                }
                Err(err) => tracing::warn!(%err, "could not remove missing tracks"),
            }
        }

        let mut unavailable = 0;
        for chunk in keep.chunks(CHUNK) {
            let mut db = lock(&self.db);
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let ids: Vec<TrackId> = {
                let state = lock(&self.state);
                chunk
                    .iter()
                    .copied()
                    .filter(|id| state.tracks.get(id).is_some_and(|e| e.track.available))
                    .collect()
            };
            match db.set_tracks_available(&ids, false) {
                Ok(()) => {
                    let mut state = lock(&self.state);
                    for &id in &ids {
                        state.update(id, |t| t.available = false);
                    }
                    unavailable += ids.len();
                }
                Err(err) => tracing::warn!(%err, "could not mark missing tracks"),
            }
        }
        (unavailable, removed)
    }

    /// Marks a folder as offline and its tracks as unavailable (they are kept).
    fn set_root_offline(&self, root_id: i64) {
        let changed = {
            let db = lock(&self.db);
            let (was_online, ids) = {
                let mut state = lock(&self.state);
                let Some(root) = state.root_mut(root_id) else {
                    return;
                };
                let was_online = std::mem::replace(&mut root.online, false);
                let ids: Vec<TrackId> = state
                    .tracks
                    .values()
                    .filter(|e| e.track.root_id == Some(root_id) && e.track.available)
                    .map(|e| e.track.id)
                    .collect();
                (was_online, ids)
            };
            if !ids.is_empty() {
                match db.set_root_available(root_id, false) {
                    Ok(()) => {
                        let mut state = lock(&self.state);
                        for id in &ids {
                            state.update(*id, |t| t.available = false);
                        }
                    }
                    Err(err) => tracing::warn!(%err, "could not mark tracks unavailable"),
                }
            }
            was_online || !ids.is_empty()
        };
        if changed {
            tracing::info!("music folder is offline; its tracks are kept as unavailable");
            self.publish();
        }
    }

    fn set_root_online(&self, root_id: i64) {
        let changed = lock(&self.state)
            .root_mut(root_id)
            .is_some_and(|root| !std::mem::replace(&mut root.online, true));
        if changed {
            self.notify();
        }
    }

    fn spawn(self: &Arc<Self>, name: &str, body: impl FnOnce(Arc<Inner>) + Send + 'static) {
        let inner = Arc::clone(self);
        match std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || body(inner))
        {
            Ok(handle) => {
                let mut threads = lock(&self.threads);
                threads.retain(|h| !h.is_finished());
                threads.push(handle);
            }
            Err(err) => tracing::error!(%err, thread = name, "could not start a library thread"),
        }
    }

    /// Recreates a missing artwork cache file from any track that uses it.
    fn regenerate_art(self: &Arc<Self>, art_id: &str) {
        if self.shutdown.load(Ordering::Relaxed)
            || !lock(&self.art_regen).insert(art_id.to_string())
        {
            return;
        }
        let source = lock(&self.state)
            .tracks
            .values()
            .find(|e| e.track.available && e.track.art_id.as_deref() == Some(art_id))
            .map(|e| e.track.path.clone());
        let Some(source) = source else {
            lock(&self.art_regen).remove(art_id);
            return;
        };
        let art_id = art_id.to_string();
        self.spawn("oneum-art", move |inner| {
            let produced = tags::read_picture(&source)
                .and_then(|bytes| inner.art.store_embedded(&bytes))
                .or_else(|| {
                    source
                        .parent()
                        .and_then(art::find_folder_image)
                        .and_then(|image| inner.art.store_folder_image(&image))
                });
            if produced.as_deref() == Some(art_id.as_str()) {
                lock(&inner.art_regen).remove(&art_id);
                inner.notify();
            } else {
                // The file's artwork changed or is unreadable; the next rescan fixes the
                // reference. Keeping the id in the set prevents retrying on every frame.
                tracing::debug!("artwork could not be regenerated from its track");
            }
        });
    }
}

/// The music library. Cheap to share (`Arc<Library>`); every method may be called from
/// any thread. Methods that touch the database or files are quick but not free — call
/// them from the UI thread only in response to user actions, not per frame. Per-frame
/// reads should use [`Library::snapshot`] and the generation counters.
pub struct Library {
    inner: Arc<Inner>,
}

impl Library {
    /// Opens (or creates) the library. Loads all tracks and builds the first snapshot
    /// before returning; no scan is started.
    pub fn open(paths: LibraryPaths) -> Result<(Arc<Library>, OpenReport), LibraryError> {
        fs::create_dir_all(&paths.data_dir)?;
        if let Err(err) = fs::create_dir_all(&paths.cache_dir) {
            tracing::warn!(%err, "could not create the cache directory; artwork is disabled");
        }
        let opened = Db::open(&paths.data_dir.join(DB_FILE_NAME))?;
        let db = opened.db;
        let report = OpenReport {
            recovered_corrupt_db: opened.recovered_corrupt_db,
            migrated_from: opened.migrated_from,
        };

        let mut state = State::default();
        for root in db.roots()? {
            let online = root.path.is_dir();
            state.roots.push(RootState {
                id: root.id,
                path: root.path,
                online,
                last_scan_at: root.last_scan_at,
            });
        }
        let offline: HashSet<i64> = state
            .roots
            .iter()
            .filter(|root| !root.online)
            .map(|root| root.id)
            .collect();
        let mut stale_offline: HashSet<i64> = HashSet::new();
        for stored in db.load_tracks()? {
            let mut track = stored.track;
            if track.available
                && let Some(root_id) = track.root_id
                && offline.contains(&root_id)
            {
                track.available = false;
                stale_offline.insert(root_id);
            }
            state.insert(track);
        }
        for root_id in stale_offline {
            db.set_root_available(root_id, false)?;
        }

        let inner = Arc::new(Inner {
            db: Mutex::new(db),
            state: Mutex::new(state),
            snapshot: RwLock::new(LibrarySnapshot::empty()),
            publish_lock: Mutex::new(None),
            generation: AtomicU64::new(0),
            playlists: Mutex::new(Vec::new()),
            playlists_generation: AtomicU64::new(0),
            notifier: RwLock::new(None),
            art: ArtCache::new(&paths.cache_dir),
            art_regen: Mutex::new(HashSet::new()),
            scan: ScanQueue::default(),
            counters: Counters::default(),
            skipped: Mutex::new(HashMap::new()),
            publish_interval_ms: AtomicU64::new(scan::PUBLISH_INTERVAL.as_millis() as u64),
            tag_reads: AtomicU64::new(0),
            shutdown: AtomicBool::new(false),
            watch_tx: Mutex::new(None),
            threads: Mutex::new(Vec::new()),
        });
        inner.publish();
        inner.reload_playlists();
        tracing::info!(
            tracks = inner.snapshot().len(),
            roots = lock(&inner.state).roots.len(),
            "library opened"
        );

        inner.spawn("oneum-scan", scan::run_scan_thread);
        inner.spawn("oneum-art-cleanup", |inner| {
            let referenced: HashSet<String> = lock(&inner.state)
                .tracks
                .values()
                .filter_map(|e| e.track.art_id.clone())
                .collect();
            inner.art.cleanup(&referenced, &inner.shutdown);
        });

        Ok((Arc::new(Library { inner }), report))
    }

    /// Registers a callback that is called from any thread whenever the snapshot, the
    /// scan status, the folders or the playlists change.
    pub fn set_notifier(&self, f: Box<dyn Fn() + Send + Sync>) {
        *self
            .inner
            .notifier
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(Arc::from(f));
    }

    /// The current snapshot. Never blocks on scanning or database work.
    pub fn snapshot(&self) -> Arc<LibrarySnapshot> {
        self.inner.snapshot()
    }

    /// Increases every time the snapshot is replaced.
    pub fn generation(&self) -> u64 {
        self.inner.generation.load(Ordering::Acquire)
    }

    /// Increases on any change to playlists or their items.
    pub fn playlists_generation(&self) -> u64 {
        self.inner.playlists_generation.load(Ordering::Acquire)
    }

    // ---- folders and scanning ------------------------------------------------------

    pub fn roots(&self) -> Vec<Root> {
        let snapshot = self.inner.snapshot();
        lock(&self.inner.state)
            .roots
            .iter()
            .map(|root| Root {
                id: root.id,
                path: root.path.clone(),
                online: root.online,
                track_count: snapshot.root_track_count(root.id),
                last_scan_at: root.last_scan_at,
            })
            .collect()
    }

    /// Adds a music folder and starts scanning it in the background.
    pub fn add_folder(&self, path: &Path) -> Result<i64, LibraryError> {
        let path = m3u::lexical_normalize(&std::path::absolute(path)?);
        if !path.is_dir() {
            return Err(LibraryError::NotADirectory);
        }
        // Compare resolved paths so that a folder cannot be added twice through a
        // symbolic link; the path is stored as the user gave it.
        let resolve = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let resolved = resolve(&path);
        let id = {
            let db = lock(&self.inner.db);
            let mut state = lock(&self.inner.state);
            for root in &state.roots {
                let existing = resolve(&root.path);
                if existing == resolved || root.path == path {
                    return Err(LibraryError::AlreadyAdded);
                }
                if resolved.starts_with(&existing) || path.starts_with(&root.path) {
                    return Err(LibraryError::InsideExistingFolder(root.path.clone()));
                }
                if existing.starts_with(&resolved) || root.path.starts_with(&path) {
                    return Err(LibraryError::ContainsExistingFolder(root.path.clone()));
                }
            }
            let id = db.insert_root(&path)?;
            state.roots.push(RootState {
                id,
                path,
                online: true,
                last_scan_at: None,
            });
            id
        };
        tracing::info!(root_id = id, "music folder added");
        self.inner.scan.request_root(&self.inner.counters, id, None);
        self.poke_watcher();
        self.inner.notify();
        Ok(id)
    }

    /// Removes a music folder and its tracks from the library. Files on disk are never
    /// touched. Playlist entries, history and queue entries of those tracks go with them.
    pub fn remove_folder(&self, root_id: i64) -> Result<(), LibraryError> {
        // Forget the folder first: from here on no new scan of it can start, and the
        // watcher stops caring about it. Then stop a scan that is already running;
        // its batches check the cancel flag under the database lock.
        {
            let _db = lock(&self.inner.db);
            let mut state = lock(&self.inner.state);
            if state.root(root_id).is_none() {
                return Err(LibraryError::NotFound);
            }
            state.roots.retain(|root| root.id != root_id);
        }
        self.inner.scan.cancel_root(root_id);

        // Delete the tracks in chunks so that other threads get their turn at the
        // database in between, then the folder row itself (which would also take any
        // remaining track rows with it).
        loop {
            let mut db = lock(&self.inner.db);
            let ids: Vec<TrackId> = lock(&self.inner.state)
                .tracks
                .values()
                .filter(|e| e.track.root_id == Some(root_id))
                .map(|e| e.track.id)
                .take(500)
                .collect();
            if ids.is_empty() {
                db.delete_root(root_id)?;
                break;
            }
            db.delete_tracks(&ids)?;
            let mut state = lock(&self.inner.state);
            for id in ids {
                state.remove(id);
            }
        }
        tracing::info!(root_id, "music folder removed from the library");
        self.inner.reload_playlists();
        self.poke_watcher();
        self.inner.publish();
        Ok(())
    }

    /// Starts an incremental background scan of every music folder (folders that are
    /// offline get their tracks marked unavailable) and re-checks individually added
    /// files. Requests made while a scan is running are queued and merged.
    pub fn rescan(&self) {
        let roots: Vec<i64> = lock(&self.inner.state).roots.iter().map(|r| r.id).collect();
        for id in roots {
            self.inner.scan.request_root(&self.inner.counters, id, None);
        }
        self.inner.scan.request_loose(&self.inner.counters);
        self.inner.notify();
    }

    pub fn scan_status(&self) -> ScanStatus {
        let (running, current_root) = self.inner.scan.status();
        let c = &self.inner.counters;
        ScanStatus {
            running,
            files_found: c.files_found.load(Ordering::Relaxed),
            files_done: c.files_done.load(Ordering::Relaxed),
            tracks_added: c.tracks_added.load(Ordering::Relaxed),
            tracks_updated: c.tracks_updated.load(Ordering::Relaxed),
            unsupported: c.unsupported.load(Ordering::Relaxed),
            errors: c.errors.load(Ordering::Relaxed),
            current_root,
        }
    }

    /// Blocks until no scan is running or queued. Returns false if `timeout` elapsed
    /// first. Not for the UI thread; meant for tools and tests.
    pub fn wait_for_scan(&self, timeout: Duration) -> bool {
        self.inner.scan.wait_idle(timeout)
    }

    /// Starts the file watcher and the periodic folder online/offline check. Calling it
    /// again has no effect.
    pub fn start_watching(&self) {
        if self.inner.shutdown.load(Ordering::Relaxed) {
            return;
        }
        let mut slot = lock(&self.inner.watch_tx);
        if slot.is_some() {
            return;
        }
        let (tx, rx) = channel();
        *slot = Some(tx.clone());
        drop(slot);
        self.inner.spawn("oneum-watch", move |inner| {
            watch::run_watch_thread(inner, rx, tx);
        });
    }

    fn poke_watcher(&self) {
        if let Some(tx) = lock(&self.inner.watch_tx).as_ref() {
            let _ = tx.send(WatchMsg::Reconcile);
        }
    }

    /// Stops all background threads and flushes the database. The library can still be
    /// read afterwards, but nothing is scanned or watched any more. Also runs on drop.
    pub fn shutdown(&self) {
        if self.inner.shutdown.swap(true, Ordering::SeqCst) {
            return;
        }
        self.inner.scan.stop();
        if let Some(tx) = lock(&self.inner.watch_tx).take() {
            let _ = tx.send(WatchMsg::Stop);
        }
        let handles = std::mem::take(&mut *lock(&self.inner.threads));
        let current = std::thread::current().id();
        for handle in handles {
            if handle.thread().id() != current {
                let _ = handle.join();
            }
        }
        if let Err(err) = lock(&self.inner.db).checkpoint() {
            tracing::debug!(%err, "final checkpoint failed");
        }
    }

    // ---- tracks --------------------------------------------------------------------

    /// Registers individual audio files (not tied to a music folder) and returns their
    /// ids in input order, skipping files that are unreadable or unsupported. Files the
    /// library already knows are reused. Blocking: reads the files.
    pub fn import_files(&self, paths: &[PathBuf]) -> Vec<TrackId> {
        self.import_paths(paths).into_iter().flatten().collect()
    }

    fn import_paths(&self, paths: &[PathBuf]) -> Vec<Option<TrackId>> {
        let folder_art: FolderArt = Mutex::new(HashMap::new());
        let mut changed = false;
        let ids = paths
            .iter()
            .map(|path| {
                let path = m3u::lexical_normalize(&std::path::absolute(path).ok()?);
                let existing = {
                    let state = lock(&self.inner.state);
                    state
                        .by_path
                        .get(&path)
                        .and_then(|id| state.tracks.get(id))
                        .map(|e| (e.track.id, e.track.available, e.track.root_id))
                };
                if let Some((id, available, root_id)) = existing {
                    if !available && path.is_file() {
                        self.inner.commit_scanned(root_id, Vec::new(), &[id], None);
                        changed = true;
                    }
                    return Some(id);
                }
                if !tags::has_audio_extension(&path) {
                    return None;
                }
                let meta = fs::metadata(&path).ok().filter(|m| m.is_file())?;
                let file = scan::read_file(
                    &self.inner,
                    &path,
                    meta.len(),
                    scan::mtime_ns(&meta),
                    &folder_art,
                )
                .ok()?;
                let root_id = lock(&self.inner.state)
                    .root_containing(&path)
                    .map(|root| root.id);
                let committed = self.inner.commit_scanned(root_id, vec![file], &[], None);
                changed = true;
                committed.first().map(|(id, _)| *id)
            })
            .collect();
        if changed {
            self.inner.publish();
        }
        ids
    }

    /// Removes tracks from the library (database only; files are not touched).
    pub fn remove_tracks(&self, ids: &[TrackId]) {
        if ids.is_empty() {
            return;
        }
        {
            let mut db = lock(&self.inner.db);
            if let Err(err) = db.delete_tracks(ids) {
                tracing::warn!(%err, "could not remove tracks");
                return;
            }
            let mut state = lock(&self.inner.state);
            for &id in ids {
                state.remove(id);
            }
        }
        self.inner.reload_playlists();
        self.inner.publish();
    }

    /// The current data of a track (may be newer than the published snapshot).
    pub fn track(&self, id: TrackId) -> Option<Arc<Track>> {
        lock(&self.inner.state)
            .tracks
            .get(&id)
            .map(|e| Arc::clone(&e.track))
    }

    pub fn set_favorite(&self, ids: &[TrackId], favorite: bool) {
        let changed: Vec<Arc<Track>> = {
            let mut db = lock(&self.inner.db);
            let ids: Vec<TrackId> = {
                let state = lock(&self.inner.state);
                ids.iter()
                    .copied()
                    .filter(|id| {
                        state
                            .tracks
                            .get(id)
                            .is_some_and(|e| e.track.favorite != favorite)
                    })
                    .collect()
            };
            if let Err(err) = db.set_favorite(&ids, favorite) {
                tracing::warn!(%err, "could not save favourites");
                return;
            }
            let mut state = lock(&self.inner.state);
            ids.iter()
                .filter_map(|&id| state.update(id, |t| t.favorite = favorite))
                .collect()
        };
        self.inner.publish_patch(&changed);
    }

    /// Records that a track was played: adds a history row and updates its play count
    /// and last-played time.
    pub fn record_play(&self, id: TrackId) {
        let now = unix_now();
        let changed = {
            let mut db = lock(&self.inner.db);
            match db.record_play(id, now) {
                Ok(true) => {}
                Ok(false) => return,
                Err(err) => {
                    tracing::warn!(%err, "could not record play");
                    return;
                }
            }
            lock(&self.inner.state).update(id, |t| {
                t.play_count = t.play_count.saturating_add(1);
                t.last_played_at = Some(now);
            })
        };
        if let Some(track) = changed {
            self.inner.publish_patch(&[track]);
        }
    }

    /// Recently played tracks, most recent first, each track once.
    pub fn recent_plays(&self, limit: usize) -> Vec<TrackId> {
        lock(&self.inner.db)
            .recent_plays(limit)
            .unwrap_or_else(|err| {
                tracing::warn!(%err, "could not read play history");
                Vec::new()
            })
    }

    // ---- playlists -----------------------------------------------------------------

    pub fn playlists(&self) -> Vec<PlaylistInfo> {
        lock(&self.inner.playlists)
            .iter()
            .map(|p| PlaylistInfo {
                id: p.id,
                name: p.name.clone(),
                item_count: p.items.len(),
            })
            .collect()
    }

    /// Creates a playlist. The name is trimmed; duplicates are allowed.
    pub fn create_playlist(&self, name: &str) -> Result<i64, LibraryError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(LibraryError::InvalidName);
        }
        let id = lock(&self.inner.db).create_playlist(name, unix_now())?;
        self.playlists_changed();
        Ok(id)
    }

    pub fn rename_playlist(&self, id: i64, name: &str) -> Result<(), LibraryError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(LibraryError::InvalidName);
        }
        if !lock(&self.inner.db).rename_playlist(id, name)? {
            return Err(LibraryError::NotFound);
        }
        self.playlists_changed();
        Ok(())
    }

    pub fn delete_playlist(&self, id: i64) -> Result<(), LibraryError> {
        if !lock(&self.inner.db).delete_playlist(id)? {
            return Err(LibraryError::NotFound);
        }
        self.playlists_changed();
        Ok(())
    }

    /// Items of a playlist in order (empty if the playlist does not exist).
    pub fn playlist_items(&self, id: i64) -> Vec<PlaylistItem> {
        lock(&self.inner.playlists)
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.items.clone())
            .unwrap_or_default()
    }

    /// Appends tracks in the given order. Ids of tracks that no longer exist are skipped.
    pub fn add_to_playlist(&self, id: i64, tracks: &[TrackId]) -> Result<(), LibraryError> {
        {
            let mut db = lock(&self.inner.db);
            if !db.playlist_exists(id)? {
                return Err(LibraryError::NotFound);
            }
            db.append_playlist_items(id, tracks)?;
        }
        self.playlists_changed();
        Ok(())
    }

    pub fn remove_playlist_items(&self, id: i64, item_ids: &[i64]) -> Result<(), LibraryError> {
        {
            let mut db = lock(&self.inner.db);
            if !db.playlist_exists(id)? {
                return Err(LibraryError::NotFound);
            }
            db.remove_playlist_items(id, item_ids)?;
        }
        self.playlists_changed();
        Ok(())
    }

    /// Moves the given items, keeping their relative order, so that they start at
    /// `to_index` of the list with those items removed (clamped to its length).
    pub fn move_playlist_items(
        &self,
        id: i64,
        item_ids: &[i64],
        to_index: usize,
    ) -> Result<(), LibraryError> {
        {
            let mut db = lock(&self.inner.db);
            if !db.playlist_exists(id)? {
                return Err(LibraryError::NotFound);
            }
            let current: Vec<i64> = db
                .all_playlist_items()?
                .into_iter()
                .filter(|(playlist_id, _, _)| *playlist_id == id)
                .map(|(_, item_id, _)| item_id)
                .collect();
            let selected: HashSet<i64> = item_ids.iter().copied().collect();
            let (moved, mut rest): (Vec<i64>, Vec<i64>) =
                current.iter().partition(|item| selected.contains(*item));
            if moved.is_empty() {
                return Ok(());
            }
            let at = to_index.min(rest.len());
            rest.splice(at..at, moved);
            if rest != current {
                db.set_playlist_order(id, &rest)?;
            }
        }
        self.playlists_changed();
        Ok(())
    }

    fn playlists_changed(&self) {
        if self.inner.reload_playlists() {
            self.inner.notify();
        }
    }

    /// Writes a playlist as extended M3U (UTF-8, absolute paths). Returns the number of
    /// entries written.
    pub fn export_m3u8(&self, id: i64, dest: &Path) -> Result<usize, LibraryError> {
        let items = lock(&self.inner.playlists)
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.items.clone())
            .ok_or(LibraryError::NotFound)?;
        let tracks: Vec<Arc<Track>> = {
            let state = lock(&self.inner.state);
            items
                .iter()
                .filter_map(|item| state.tracks.get(&item.track_id))
                .map(|e| Arc::clone(&e.track))
                .collect()
        };
        let (text, count) = m3u::render(tracks.iter().map(|t| &**t));
        write_atomic(dest, text.as_bytes())?;
        Ok(count)
    }

    /// Creates a playlist named after the file from an M3U/M3U8 file. Entries that exist
    /// on disk but are not in the library are imported like [`Library::import_files`];
    /// entries that cannot be found are reported in `missing`. Blocking.
    pub fn import_m3u8(&self, src: &Path) -> Result<M3uImport, LibraryError> {
        let bytes = fs::read(src)?;
        let src = std::path::absolute(src)?;
        let base = src.parent().unwrap_or(Path::new("/"));
        let entries = m3u::parse(&m3u::decode(&bytes), base);
        let name = src
            .file_stem()
            .map(|stem| stem.to_string_lossy().trim().to_string())
            .filter(|stem| !stem.is_empty())
            .unwrap_or_else(|| "Playlist".to_string());

        // First pass: entries the library already knows, by exact path or by a path that
        // differs only in Unicode normalisation (NFC playlist vs NFD file name).
        let mut resolved: Vec<Option<TrackId>> = {
            let state = lock(&self.inner.state);
            let mut by_nfc: Option<HashMap<String, TrackId>> = None;
            entries
                .iter()
                .map(|entry| {
                    let path = entry.path.as_ref()?;
                    if let Some(&id) = state.by_path.get(path) {
                        return Some(id);
                    }
                    let by_nfc = by_nfc.get_or_insert_with(|| {
                        state
                            .by_path
                            .iter()
                            .map(|(p, &id)| (p.to_string_lossy().nfc().collect(), id))
                            .collect()
                    });
                    let key: String = path.to_string_lossy().nfc().collect();
                    by_nfc.get(&key).copied()
                })
                .collect()
        };
        // Second pass: read files that exist but are new to the library.
        let unknown: Vec<(usize, PathBuf)> = entries
            .iter()
            .enumerate()
            .filter(|(i, _)| resolved[*i].is_none())
            .filter_map(|(i, entry)| Some((i, entry.path.clone()?)))
            .collect();
        let unknown_paths: Vec<PathBuf> = unknown.iter().map(|(_, p)| p.clone()).collect();
        for ((i, _), id) in unknown.iter().zip(self.import_paths(&unknown_paths)) {
            resolved[*i] = id;
        }

        let matched: Vec<TrackId> = resolved.iter().flatten().copied().collect();
        let missing: Vec<String> = entries
            .iter()
            .zip(&resolved)
            .filter(|(_, id)| id.is_none())
            .map(|(entry, _)| entry.raw.clone())
            .collect();

        let (playlist_id, added) = {
            let mut db = lock(&self.inner.db);
            let playlist_id = db.create_playlist(&name, unix_now())?;
            let added = db.append_playlist_items(playlist_id, &matched)?;
            (playlist_id, added)
        };
        self.playlists_changed();
        tracing::info!(
            matched = added,
            missing = missing.len(),
            "playlist imported"
        );
        Ok(M3uImport {
            playlist_id,
            name,
            matched: added,
            missing,
        })
    }

    // ---- session persistence for the playback module -------------------------------

    pub fn kv_get(&self, key: &str) -> Option<String> {
        lock(&self.inner.db).kv_get(key).unwrap_or_else(|err| {
            tracing::warn!(%err, "could not read a saved value");
            None
        })
    }

    pub fn kv_set(&self, key: &str, value: &str) {
        if let Err(err) = lock(&self.inner.db).kv_set(key, value) {
            tracing::warn!(%err, "could not save a value");
        }
    }

    /// Replaces the saved play queue with `(queue item id, track id)` pairs in order.
    pub fn save_queue(&self, items: &[(u64, TrackId)]) {
        if let Err(err) = lock(&self.inner.db).save_queue(items) {
            tracing::warn!(%err, "could not save the queue");
        }
    }

    /// The saved play queue; entries whose track no longer exists are dropped.
    pub fn load_queue(&self) -> Vec<(u64, TrackId)> {
        lock(&self.inner.db).load_queue().unwrap_or_else(|err| {
            tracing::warn!(%err, "could not load the queue");
            Vec::new()
        })
    }

    // ---- artwork -------------------------------------------------------------------

    /// Path of the cached cover JPEG for an artwork id (it may not exist).
    pub fn art_path(&self, art_id: &str) -> PathBuf {
        self.inner.art.path_for(art_id)
    }

    /// Decodes the cached cover and scales it down to at most `max_px` on its longest
    /// edge. Blocking; call from a worker thread. Returns `None` if there is no usable
    /// cache file — if it was evicted it is regenerated in the background and the
    /// notifier is called when it is available again.
    pub fn load_art_rgba(&self, art_id: &str, max_px: u32) -> Option<ArtImage> {
        if let Some(image) = self.inner.art.load_rgba(art_id, max_px) {
            return Some(image);
        }
        if self.inner.art.exists(art_id) {
            // Present but undecodable: discard it so it can be rebuilt.
            let _ = fs::remove_file(self.inner.art.path_for(art_id));
        }
        self.inner.regenerate_art(art_id);
        None
    }

    /// Changes how often the snapshot is rebuilt while a scan is running (default
    /// 500 ms). Lets tests observe progressive publishing deterministically.
    #[doc(hidden)]
    pub fn debug_set_publish_interval(&self, interval: Duration) {
        let millis = u64::try_from(interval.as_millis()).unwrap_or(u64::MAX);
        self.inner
            .publish_interval_ms
            .store(millis, Ordering::Relaxed);
    }

    /// Number of files whose tags were read since the library was opened (lets tests
    /// verify that unchanged files are not read again).
    #[doc(hidden)]
    pub fn debug_tag_reads(&self) -> u64 {
        self.inner.tag_reads.load(Ordering::Relaxed)
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_types_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Library>();
        assert_send_sync::<LibrarySnapshot>();
        assert_send_sync::<PreparedQuery>();
        assert_send_sync::<ScanStatus>();
        assert_send_sync::<LibraryError>();
    }
}
