//! The music library: folders, scanning, the in-memory index, playlists and artwork.
//!
//! See docs/ARCHITECTURE.md ("라이브러리와 저장"). The SQLite database is the source of
//! truth. [`Library`] keeps every track in memory and publishes immutable
//! [`LibrarySnapshot`]s that the UI reads without locking; all file and database work
//! happens on background threads or in the calling (non-UI) thread.
//!
//! Typical start-up: [`Library::open`], [`Library::set_notifier`], then
//! [`Library::rescan`] and [`Library::start_watching`].
pub mod apple;
mod art;
pub mod backup;
mod check;
pub mod cue;
pub mod details;
#[cfg(test)]
mod hostile;
mod ident;
mod legacy;
mod loudness;
pub mod lyrics;
mod m3u;
pub mod online_lyrics;
pub mod radio;
pub mod remote;
mod scan;
mod search;
mod snapshot;
pub mod stats;
mod tags;
mod watch;

pub use art::ArtImage;
pub use check::CheckStatus;
pub use ident::{album_key, disc_of, fnv1a64};
pub use loudness::LoudnessStatus;
pub use search::{PreparedQuery, has_hangul, normalize, typed_keys};
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
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use unicode_normalization::UnicodeNormalization;
use watch::WatchMsg;

/// How many of the tracks played last are kept in memory (see `Inner::recent_plays`).
const RECENT_PLAYS_KEPT: usize = 1000;

/// The `kv` key of the record of read CUE sheets (see `Inner::sheets_read`).
const SHEETS_READ_KEY: &str = "cue_sheets_read";

/// What is recorded of a CUE sheet whose tracks were read (see `Inner::sheets_read`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SheetRecord {
    /// The sheet's modification time when it was read.
    mtime: i64,
    /// The numbers of the tracks reading it gave: not every track of a sheet is one
    /// (those that start beyond the end of the audio are not). None: the file was taken
    /// in alone.
    tracks: Vec<u32>,
    /// The audio file's modification time and size then.
    audio_mtime: i64,
    audio_size: u64,
}

impl SheetRecord {
    /// Whether the record was made of the sheet and its audio file as they are now.
    fn is_of(&self, sheet_mtime: i64, audio_mtime: i64, audio_size: u64) -> bool {
        self.mtime == sheet_mtime
            && self.audio_mtime == audio_mtime
            && self.audio_size == audio_size
    }
}

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
    /// Albums that have another key than they had (old, new): those that are told
    /// apart by their folder, after the folder got another path. What is kept
    /// elsewhere under an album's key (a pin) follows by this.
    pub album_keys_changed: Vec<(u64, u64)>,
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

/// A playlist of another record (see `Library::take_over`), as it is to be made here.
struct ListIn {
    name: String,
    /// Its songs that are in the library, in its order.
    tracks: Vec<TrackId>,
    /// It had no songs in the record either: it is made all the same, empty.
    empty_there: bool,
}

/// What an Apple Music library import did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppleImport {
    /// Songs of the export found in the library (by their file).
    pub matched: usize,
    /// Songs of the export whose file is not in the library.
    pub unmatched: usize,
    /// Playlists made.
    pub playlists: usize,
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
    /// The track's artwork is an image file in its folder (not embedded in the file).
    pub(crate) folder_art: bool,
}

pub(crate) struct RootState {
    pub(crate) id: i64,
    /// The folder as the user added it; track paths start with this.
    pub(crate) path: PathBuf,
    /// `path` with symbolic links resolved and in its on-disk spelling (equal to `path`
    /// while the folder is offline and cannot be resolved).
    pub(crate) canonical: PathBuf,
    pub(crate) online: bool,
    pub(crate) last_scan_at: Option<i64>,
    /// Set while the folder's path exists but its known files were not found there:
    /// the highest track id from before that was noticed. Until a track up to that id
    /// is seen again, missing tracks of the folder are kept, not removed.
    pub(crate) unconfirmed_after: Option<TrackId>,
}

/// A symbolic link to a directory that a scan followed inside a music folder.
pub(crate) struct FollowedLink {
    pub(crate) root_id: i64,
    /// Path of the link itself, in library spelling.
    pub(crate) link: PathBuf,
    /// Resolved directory the link points to.
    pub(crate) target: PathBuf,
}

/// In-memory mirror of the `roots` and `tracks` tables.
#[derive(Default)]
pub(crate) struct State {
    pub(crate) tracks: HashMap<TrackId, Entry>,
    by_path: HashMap<PathBuf, TrackId>,
    pub(crate) roots: Vec<RootState>,
    /// Directory links seen by scans (this session); lets a resolved path be mapped
    /// back to the spelling under which the scanner stored the file.
    pub(crate) links: Vec<FollowedLink>,
    /// Bumped by every change to `tracks`; lets `publish` skip redundant rebuilds.
    revision: u64,
    /// Bumped whenever tracks leave the library (see [`Library::removals`]).
    removals: u64,
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

    pub(crate) fn id_of_path(&self, path: &Path) -> Option<TrackId> {
        self.by_path.get(path).copied()
    }

    /// Maps a resolved (canonical) file path to the spelling the library uses for it and
    /// the music folder it belongs to: a file under a music folder is spelled through
    /// that folder's path as the user added it, and a file reached by a scan through a
    /// directory link is spelled through that link. Other paths are returned unchanged.
    pub(crate) fn library_path(&self, canonical: &Path) -> (PathBuf, Option<i64>) {
        for root in &self.roots {
            if let Ok(relative) = canonical.strip_prefix(&root.canonical) {
                return (root.path.join(relative), Some(root.id));
            }
            if canonical.starts_with(&root.path) {
                return (canonical.to_path_buf(), Some(root.id));
            }
        }
        // The most specific link wins when links are nested.
        let link = self
            .links
            .iter()
            .filter(|link| canonical.starts_with(&link.target))
            .max_by_key(|link| link.target.as_os_str().len());
        if let Some(link) = link
            && let Ok(relative) = canonical.strip_prefix(&link.target)
        {
            return (link.link.join(relative), Some(link.root_id));
        }
        (canonical.to_path_buf(), None)
    }

    /// Adds a track or replaces the one with the same id.
    pub(crate) fn insert(&mut self, track: Track, folder_art: bool) -> Arc<Track> {
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
                folder_art,
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
        self.removals += 1;
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

    /// Sets a track's artwork and where it comes from.
    pub(crate) fn set_art(&mut self, id: TrackId, art_id: Option<String>, folder_art: bool) {
        if let Some(entry) = self.tracks.get_mut(&id) {
            entry.folder_art = folder_art;
        }
        self.update(id, |t| t.art_id = art_id);
    }
}

/// Artwork cache files waiting to be regenerated.
#[derive(Default)]
struct ArtJobs {
    /// Requested ids, oldest first. The newest request is served first: it is what the
    /// user is looking at now.
    pending: std::collections::VecDeque<String>,
    /// Ids in `pending` or being worked on.
    active: HashSet<String>,
    /// Ids that could not be regenerated; not retried until the next scan finishes.
    failed: HashSet<String>,
}

/// Upper bound of queued artwork regenerations; older requests are dropped beyond it.
const ART_QUEUE_LIMIT: usize = 64;

/// How long quitting waits for the library's threads together.
const SHUTDOWN_PATIENCE: std::time::Duration = std::time::Duration::from_secs(3);

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
    /// Tracks of media servers on the network that the play queue names.
    remote: Mutex<remote::Catalog>,
    remote_generation: AtomicU64,
    art: ArtCache,
    art_jobs: Mutex<ArtJobs>,
    art_wake: Condvar,
    /// Bumped whenever a regenerated cover becomes available.
    art_generation: AtomicU64,
    scan: ScanQueue,
    counters: Counters,
    /// The run that measures the loudness of untagged tracks.
    loudness: loudness::LoudnessJob,
    /// The run that checks files (integrity, dynamic range, true peak).
    checks: check::CheckJob,
    /// Files that could not be added, by path, with the size and mtime they had. Lets
    /// later scans count them again without reading them again (not persisted).
    skipped: Mutex<HashMap<PathBuf, scan::Skipped>>,
    /// Tracks that scans added recently, with the time they were added. A file that was
    /// moved shows up as "new" and as "missing" in two different scans when it changes
    /// music folder or when the watcher reports the move in two parts; this lets the
    /// later scan still connect the two.
    recently_added: Mutex<std::collections::VecDeque<(TrackId, std::time::Instant)>>,
    /// The tracks played last, most recent first, each once (at most
    /// `RECENT_PLAYS_KEPT`): the window asks for them in a frame, which must not wait
    /// for the database. Read from the history when the library is opened and kept up
    /// by `record_play`; a track that has left the library since is passed over.
    recent_plays: Mutex<Vec<TrackId>>,
    /// Files whose songs the user removed from the library while the files stayed on
    /// disk (a CUE sheet's track under its track path). A scan passes them by: without
    /// this a removed song was back as a new one when its folder was scanned again,
    /// which is at every start. A file leaves the list when it is opened by hand, when
    /// it is gone from the disk, or when its folder is no music folder any more. Kept
    /// in the database (`removed_files`).
    removed: Mutex<HashSet<PathBuf>>,
    /// CUE sheets whose tracks are in the library, by `scan::sheet_key`, with what was
    /// recorded when they were read: a sheet changed since (or missing a track it gave)
    /// is read again. Kept in the `kv` table (see [`Inner::save_sheet_records`]).
    sheets_read: Mutex<HashMap<String, SheetRecord>>,
    /// `sheets_read` changed since it was last saved.
    sheets_dirty: AtomicBool,
    /// Makes the next folder scan panic (tests of the scan thread's containment).
    #[cfg(test)]
    panic_in_next_scan: AtomicBool,
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

/// `path` with symbolic links resolved and in its on-disk spelling; `path` itself if it
/// cannot be resolved (e.g. the folder is offline).
pub(crate) fn resolve_dir(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Gives every track that names no album artist the album key of the path it has now
/// (see `ident::album_key`), and tells which keys became which.
fn rekey_albums(db: &mut Db) -> rusqlite::Result<Vec<(u64, u64)>> {
    let mut changed: Vec<(TrackId, u64)> = Vec::new();
    let mut keys: Vec<(u64, u64)> = Vec::new();
    for (id, path, album, compilation, before) in db.tracks_without_album_artist()? {
        let now = album_key(&album, "", compilation, &path);
        if now != before {
            changed.push((id, now));
            keys.push((before, now));
        }
    }
    if !changed.is_empty() {
        db.set_album_keys(&changed)?;
    }
    keys.sort_unstable();
    keys.dedup();
    Ok(keys)
}

/// How long the music folders are given, all together, to say whether they are there
/// when the library is opened. A disk answers in no time, one that was asleep in a
/// few seconds.
const FOLDERS_ANSWER_WITHIN: Duration = Duration::from_secs(5);

/// Asks `ask` about each of `paths`, all at the same time and on threads of their own,
/// and gives the answers that came within `wait` (`None` for the others).
///
/// The library is opened before there is a window. A music folder on a share whose
/// server is gone (the laptop on another network, the NAS switched off) answers a
/// question about it after a minute or more, or never: asked one after the other on
/// the thread that opens the window, such a folder kept the app from appearing at
/// all. A folder that does not answer in time is treated as one that is not there;
/// the scan that follows the opening looks again, on its own thread.
fn asked_together<T: Send + 'static>(
    paths: &[PathBuf],
    wait: Duration,
    ask: impl Fn(&Path) -> T + Send + Sync + 'static,
) -> Vec<Option<T>> {
    let ask = Arc::new(ask);
    let (answered, answers) = std::sync::mpsc::channel();
    for (at, path) in paths.iter().enumerate() {
        let (asking, telling, asked) = (Arc::clone(&ask), answered.clone(), path.clone());
        let on_its_own = std::thread::Builder::new()
            .name("oneum-folder-probe".into())
            .spawn(move || {
                let _ = telling.send((at, asking(&asked)));
            });
        if on_its_own.is_err() {
            // (No thread to be had: asked here, as it always was.)
            let _ = answered.send((at, ask(path)));
        }
    }
    drop(answered);
    let until = std::time::Instant::now() + wait;
    let mut given: Vec<Option<T>> = paths.iter().map(|_| None).collect();
    while let Some(left) = until.checked_duration_since(std::time::Instant::now()) {
        match answers.recv_timeout(left) {
            Ok((at, answer)) => given[at] = Some(answer),
            // Everyone has answered, or the time is over.
            Err(_) => break,
        }
    }
    given
}

/// Reads a playlist file, refusing files that are implausibly large (and what is not a
/// regular file: a pipe would never deliver).
fn read_playlist_file(path: &Path) -> Result<Vec<u8>, LibraryError> {
    crate::storage::files::read_bounded(path, m3u::MAX_FILE_BYTES)?.ok_or_else(|| {
        LibraryError::Io(std::io::Error::new(
            std::io::ErrorKind::FileTooLarge,
            "the file is too large to be a playlist, or not a regular file",
        ))
    })
}

/// A CUE sheet in a folder, as far as it says which file there is its audio file.
struct FolderSheet {
    path: PathBuf,
    file: SheetFile,
    /// The numbers of its tracks.
    numbers: Vec<u32>,
}

/// The audio file of a sheet in a folder.
enum SheetFile {
    /// The file the sheet names.
    Named(PathBuf),
    /// The sheet cannot be read just now: the file of this size, by the record of the
    /// sheet's last reading (as the scan knows it meanwhile, see `scan::hold_sheet`).
    OfSize(u64),
}

/// The CUE sheets in `folder` that are for one audio file there, in the order of their
/// names (the first one for a file has it, as for the scan).
fn sheets_in(inner: &Inner, folder: &Path) -> Vec<FolderSheet> {
    let Ok(entries) = fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut sheets: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("cue"))
        })
        .collect();
    sheets.sort();
    let mut found: Vec<FolderSheet> = Vec::new();
    for path in sheets {
        let named = cue::read_checked(&path).and_then(|sheet| match sheet {
            Some(sheet) => Ok(cue::audio_of_checked(&path, &sheet)?.map(|audio| {
                let numbers = sheet.tracks.iter().map(|t| t.number).collect();
                (SheetFile::Named(audio), numbers)
            })),
            None => Ok(None),
        });
        let sheet = match named {
            Ok(sheet) => sheet,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            // It (or its file) cannot be examined just now: what its last reading
            // recorded stands for it.
            Err(_) => lock(&inner.sheets_read)
                .get(&scan::sheet_key(&path))
                .map(|record| (SheetFile::OfSize(record.audio_size), record.tracks.clone())),
        };
        if let Some((file, numbers)) = sheet {
            found.push(FolderSheet {
                path,
                file,
                numbers,
            });
        }
    }
    found
}

fn to_stored(file: ScannedFile, now: i64) -> StoredTrack {
    let root_id = file.root_id;
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
            measured: None,
            unmeasurable: false,
            check: None,
            art_id: file.art_id,
            favorite: false,
            rating: 0,
            play_count: 0,
            last_played_at: None,
            added_at: now,
        },
        compilation: tags.compilation,
        art_from_folder: file.art_from_folder,
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

    /// Publishes changes to a track's play statistics, favourite flag and measured
    /// loudness without rebuilding the index.
    ///
    /// The values are read from the state here, under the publish lock, so that
    /// concurrent callers cannot publish in the wrong order. Only those fields are
    /// copied onto the track as it is currently published: if a scan has changed the
    /// track's tags in the meantime, the published track keeps matching its (old) index
    /// entries until the scan's own full publish replaces both together.
    fn publish_patch(&self, ids: &[TrackId]) {
        if ids.is_empty() {
            return;
        }
        {
            let _published = lock(&self.publish_lock);
            let snapshot = self.snapshot();
            let patched: Vec<Arc<Track>> = {
                let state = lock(&self.state);
                ids.iter()
                    .filter_map(|id| {
                        let current = &state.tracks.get(id)?.track;
                        let published = snapshot.get(*id)?;
                        if published.favorite == current.favorite
                            && published.rating == current.rating
                            && published.play_count == current.play_count
                            && published.last_played_at == current.last_played_at
                            && published.added_at == current.added_at
                            && published.measured == current.measured
                            && published.unmeasurable == current.unmeasurable
                            && published.check == current.check
                        {
                            return None;
                        }
                        let mut track = (**published).clone();
                        // What was measured belongs to one state of the file.
                        if published.mtime_ns == current.mtime_ns
                            && published.file_size == current.file_size
                        {
                            track.measured = current.measured;
                            track.unmeasurable = current.unmeasurable;
                            track.check = current.check;
                        }
                        track.favorite = current.favorite;
                        track.rating = current.rating;
                        track.play_count = current.play_count;
                        track.last_played_at = current.last_played_at;
                        track.added_at = current.added_at;
                        // Share the state's allocation when nothing else differs.
                        Some(if track == **current {
                            Arc::clone(current)
                        } else {
                            Arc::new(track)
                        })
                    })
                    .collect()
            };
            if patched.is_empty() {
                return;
            }
            self.replace_snapshot(snapshot.with_replaced(&patched));
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

    /// Writes scanned files to the database and the in-memory state, in one transaction.
    /// Returns `(track id, newly added)` per file, in order (empty if nothing was
    /// written). `touches` are unchanged files to mark as present and assign to
    /// `touch_root`. Nothing is written once `cancel` is set (checked under the database
    /// lock, see `Library::remove_folder`).
    /// Whether the user removed the song of this file from the library (see
    /// [`Inner::removed`]): a scan leaves such a file alone.
    pub(crate) fn is_removed(&self, path: &Path) -> bool {
        lock(&self.removed).contains(path)
    }

    /// Takes the files `gone` picks off the list of removed ones.
    fn forget_removed(&self, gone: impl Fn(&Path) -> bool) {
        let forgotten: Vec<PathBuf> = {
            let mut removed = lock(&self.removed);
            let forgotten: Vec<PathBuf> =
                removed.iter().filter(|path| gone(path)).cloned().collect();
            for path in &forgotten {
                removed.remove(path);
            }
            forgotten
        };
        if forgotten.is_empty() {
            return;
        }
        if let Err(err) = lock(&self.db).forget_removed_files(&forgotten) {
            // They are off the list for this run; the next run has them on it again.
            tracing::warn!(%err, "the list of removed files could not be written");
        }
    }

    /// After a folder was scanned in full: files that were removed from the library
    /// and are not on the disk any more are forgotten (a file that turns up under such
    /// a name later is a new one). For a CUE sheet's track that is the sheet.
    pub(crate) fn forget_removed_that_are_gone(&self, root: &Path) {
        let under_root: Vec<PathBuf> = lock(&self.removed)
            .iter()
            .filter(|path| path.starts_with(root))
            .cloned()
            .collect();
        let gone: HashSet<PathBuf> = under_root
            .into_iter()
            .filter(|path| {
                matches!(
                    std::fs::metadata(cue::backing_file(path)),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound
                )
            })
            .collect();
        if !gone.is_empty() {
            self.forget_removed(|path| gone.contains(path));
        }
    }

    fn commit_scanned(
        &self,
        mut files: Vec<ScannedFile>,
        touches: &[TrackId],
        touch_root: Option<i64>,
        cancel: Option<&AtomicBool>,
    ) -> Vec<(TrackId, bool)> {
        if touch_root.is_some() {
            // A scan: what the user removed from the library stays out (a CUE sheet
            // that is read again gives all of its tracks, the removed ones too).
            let removed = lock(&self.removed);
            if !removed.is_empty() {
                files.retain(|file| !removed.contains(&file.path));
            }
        } else {
            // Opened by hand: that takes a file back in, and it is removed no longer.
            let opened: HashSet<&Path> = files.iter().map(|file| file.path.as_path()).collect();
            self.forget_removed(|path| opened.contains(path));
        }
        if files.is_empty() && touches.is_empty() {
            return Vec::new();
        }
        let now = unix_now();
        let mut rows: Vec<StoredTrack> =
            files.into_iter().map(|file| to_stored(file, now)).collect();

        let mut db = lock(&self.db);
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return Vec::new();
        }
        {
            // A folder may have been removed since the file was read; such a file is
            // kept as an individually registered one instead of failing the batch.
            let state = lock(&self.state);
            for row in &mut rows {
                if row.track.root_id.is_some_and(|id| state.root(id).is_none()) {
                    row.track.root_id = None;
                }
            }
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
        let touched = match db.adopt_tracks(touches, touch_root) {
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
                    track.rating = old.track.rating;
                    track.play_count = old.track.play_count;
                    track.last_played_at = old.track.last_played_at;
                    track.added_at = old.track.added_at;
                    // What was measured and checked stays valid as long as the file is
                    // the same (read again for its names: new tags, a sheet's titles).
                    if old.track.mtime_ns == track.mtime_ns
                        && old.track.file_size == track.file_size
                    {
                        track.measured = old.track.measured;
                        track.unmeasurable = old.track.unmeasurable;
                        track.check = old.track.check;
                    }
                    false
                }
                None => true,
            };
            state.insert(track, row.art_from_folder);
            committed.push((id, is_new));
        }
        if touched {
            for &id in touches {
                state.update(id, |t| {
                    t.available = true;
                    t.root_id = touch_root;
                });
            }
        }
        committed
    }

    /// Marks tracks whose files are back as available.
    fn revive(&self, ids: &[TrackId]) {
        if ids.is_empty() {
            return;
        }
        let mut db = lock(&self.db);
        match db.set_tracks_available(ids, true) {
            Ok(()) => {
                let mut state = lock(&self.state);
                for &id in ids {
                    state.update(id, |t| t.available = true);
                }
            }
            Err(err) => tracing::warn!(%err, "could not mark tracks available"),
        }
    }

    /// Removes tracks that others have taken the place of (a CUE sheet's audio file,
    /// now taken in as the sheet's tracks), each `(track, those that take its place)`,
    /// handing its places in playlists and its favourite mark over to them (see
    /// `Db::hand_over_track`). Returns how many went. Stops early once `cancel` is set.
    fn hand_over(&self, handovers: &[(TrackId, Vec<TrackId>)], cancel: &AtomicBool) -> usize {
        let mut done = 0;
        for (old, heirs) in handovers {
            let mut db = lock(&self.db);
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            match db.hand_over_track(*old, heirs) {
                Ok((favorite, rating)) => {
                    let mut state = lock(&self.state);
                    for &heir in heirs {
                        state.update(heir, |t| {
                            t.favorite |= favorite;
                            if t.rating == 0 {
                                t.rating = rating;
                            }
                        });
                    }
                    state.remove(*old);
                    done += 1;
                }
                Err(err) => tracing::warn!(%err, "could not hand a replaced track over"),
            }
        }
        done
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

    /// Brings individually registered files under a music folder in line with the
    /// spelling the folder's scan uses. Such a file is stored under its resolved path;
    /// if the folder was added through a symbolic link (or any path that resolves
    /// differently), the scan would not recognise the row and add the file a second
    /// time. Also refreshes the folder's resolved path.
    fn adopt_respelled(&self, root_id: i64, root_path: &Path, cancel: &AtomicBool) {
        let canonical = resolve_dir(root_path);
        let moves: Vec<(TrackId, PathBuf)> = {
            let mut state = lock(&self.state);
            if let Some(root) = state.root_mut(root_id) {
                root.canonical.clone_from(&canonical);
            }
            if canonical == root_path {
                return;
            }
            state
                .tracks
                .values()
                .map(|entry| &entry.track)
                .filter(|t| t.root_id.is_none() && scan::is_walkable(&canonical, &t.path))
                .filter_map(|t| {
                    let relative = t.path.strip_prefix(&canonical).ok()?;
                    let respelled = root_path.join(relative);
                    state
                        .id_of_path(&respelled)
                        .is_none()
                        .then_some((t.id, respelled))
                })
                .collect()
        };
        for (id, path) in moves {
            let db = lock(&self.db);
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let row = match db.get_track(id) {
                Ok(Some(row)) => row,
                Ok(None) => continue,
                Err(err) => {
                    tracing::warn!(%err, "could not read a track to adopt");
                    continue;
                }
            };
            // The album identity of an album without album artist depends on the path.
            let key = album_key(
                &row.track.album,
                &row.track.album_artist,
                row.compilation,
                &path,
            );
            match db.move_track(id, &path, key) {
                Ok(true) => {
                    let mut track = row.track;
                    track.path = path;
                    track.album_key = key;
                    lock(&self.state).insert(track, row.art_from_folder);
                }
                Ok(false) => {}
                Err(err) => tracing::warn!(%err, "could not adopt a track"),
            }
        }
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

    /// Starts a library thread. False if it could not be started (`body` then never
    /// runs: whatever it was to release is the caller's to release).
    fn spawn(self: &Arc<Self>, name: &str, body: impl FnOnce(Arc<Inner>) + Send + 'static) -> bool {
        let inner = Arc::clone(self);
        match std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || body(inner))
        {
            Ok(handle) => {
                let mut threads = lock(&self.threads);
                threads.retain(|h| !h.is_finished());
                threads.push(handle);
                true
            }
            Err(err) => {
                tracing::error!(%err, thread = name, "could not start a library thread");
                false
            }
        }
    }

    /// Queues the regeneration of a missing artwork cache file. One worker thread
    /// serves the queue, so scrolling through many missing covers cannot start an
    /// unbounded number of threads; duplicate requests are ignored.
    fn request_art(&self, art_id: &str) {
        if self.shutdown.load(Ordering::Relaxed) {
            return;
        }
        let mut jobs = lock(&self.art_jobs);
        if jobs.active.contains(art_id) || jobs.failed.contains(art_id) {
            return;
        }
        if jobs.pending.len() >= ART_QUEUE_LIMIT
            && let Some(dropped) = jobs.pending.pop_front()
        {
            // The oldest request was scrolled past long ago; it can be asked for again.
            jobs.active.remove(&dropped);
        }
        jobs.pending.push_back(art_id.to_string());
        jobs.active.insert(art_id.to_string());
        drop(jobs);
        self.art_wake.notify_one();
    }

    /// Forgets the record of the CUE sheets under a music folder that is no longer one
    /// (saved at once: no scan of it comes to do it).
    fn forget_sheets_under(&self, root_path: &Path) {
        let forgot = {
            let mut read = lock(&self.sheets_read);
            let before = read.len();
            read.retain(|key, _| !Path::new(key).starts_with(root_path));
            read.len() != before
        };
        if forgot {
            self.sheets_dirty.store(true, Ordering::Release);
            self.save_sheet_records();
        }
    }

    /// Saves the record of read CUE sheets if it changed (see `sheets_read`).
    fn save_sheet_records(&self) {
        if !self.sheets_dirty.swap(false, Ordering::AcqRel) {
            return;
        }
        let records: Vec<(String, SheetRecord)> = lock(&self.sheets_read)
            .iter()
            .map(|(key, record)| (key.clone(), record.clone()))
            .collect();
        let saved = serde_json::to_string(&records)
            .map_err(|err| err.to_string())
            .and_then(|json| {
                lock(&self.db)
                    .kv_set(SHEETS_READ_KEY, &json)
                    .map_err(|err| err.to_string())
            });
        if let Err(err) = saved {
            tracing::warn!(%err, "could not save which CUE sheets were read");
            self.sheets_dirty.store(true, Ordering::Release);
        }
    }

    /// Recreates one artwork cache file from any track that uses it.
    fn regenerate_art(&self, art_id: &str) -> bool {
        let track = lock(&self.state)
            .tracks
            .values()
            .find(|e| e.track.available && e.track.art_id.as_deref() == Some(art_id))
            .map(|e| Arc::clone(&e.track));
        let Some(track) = track else {
            return false;
        };
        // Outside the lock: for a track of a CUE sheet this reads the sheet.
        let source = track.audio_file();
        let produced = tags::read_picture(&source)
            .and_then(|bytes| self.art.store_embedded(&bytes))
            .or_else(|| {
                track
                    .file()
                    .parent()
                    .and_then(art::find_folder_image)
                    .and_then(|image| self.art.store_folder_image(&image))
            });
        // If the file's artwork changed the id differs; the next rescan fixes the
        // track's reference.
        produced.as_deref() == Some(art_id)
    }
}

/// Body of the artwork worker thread.
fn run_art_worker(inner: Arc<Inner>) {
    loop {
        let art_id = {
            let mut jobs = lock(&inner.art_jobs);
            loop {
                if inner.shutdown.load(Ordering::Relaxed) {
                    return;
                }
                if let Some(id) = jobs.pending.pop_back() {
                    break id;
                }
                jobs = inner.art_wake.wait(jobs).unwrap_or_else(|e| e.into_inner());
            }
        };
        let regenerated = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            inner.regenerate_art(&art_id)
        }))
        .unwrap_or(false);
        {
            let mut jobs = lock(&inner.art_jobs);
            jobs.active.remove(&art_id);
            if !regenerated {
                jobs.failed.insert(art_id);
            }
        }
        if regenerated {
            inner.art_generation.fetch_add(1, Ordering::Release);
            inner.notify();
        } else {
            tracing::debug!("artwork could not be regenerated from its track");
        }
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
        Self::open_with_home(paths, None, &[])
    }

    /// [`Library::open`] where the music lies in a folder that moves (`home`: the
    /// container of an iPhone app, which is given another path when the app is updated
    /// or restored). The library's paths below the folder it was in last time are
    /// brought along to where it is now before anything is read.
    pub fn open_in_home(
        paths: LibraryPaths,
        home: &Path,
    ) -> Result<(Arc<Library>, OpenReport), LibraryError> {
        Self::open_with_home(paths, Some(home), &[])
    }

    /// [`Library::open_in_home`] where music folders outside `home` may have been moved
    /// too (`moved`: where each was, and where it is now). Those are folders the system
    /// can still tell after a move: the ones linked on an iPhone. What is known about
    /// their songs stays with them. A folder that cannot be followed (something is
    /// known at its new place already) is left as it was.
    pub fn open_following(
        paths: LibraryPaths,
        home: &Path,
        moved: &[(PathBuf, PathBuf)],
    ) -> Result<(Arc<Library>, OpenReport), LibraryError> {
        Self::open_with_home(paths, Some(home), moved)
    }

    fn open_with_home(
        paths: LibraryPaths,
        home: Option<&Path>,
        moved_folders: &[(PathBuf, PathBuf)],
    ) -> Result<(Arc<Library>, OpenReport), LibraryError> {
        fs::create_dir_all(&paths.data_dir)?;
        if let Err(err) = fs::create_dir_all(&paths.cache_dir) {
            tracing::warn!(%err, "could not create the cache directory; artwork is disabled");
        }
        let opened = Db::open(&paths.data_dir.join(DB_FILE_NAME))?;
        let mut db = opened.db;
        let mut paths_moved = false;
        if let Some(home) = home {
            let moved = db.follow_home(home, SHEETS_READ_KEY)?;
            if moved > 0 {
                paths_moved = true;
                tracing::info!(
                    moved,
                    "the library's folder has moved: its paths were brought along"
                );
            }
        }
        for (before, now) in moved_folders {
            match db.follow_folder(before, now, SHEETS_READ_KEY) {
                Ok(0) => {}
                Ok(moved) => {
                    paths_moved = true;
                    tracing::info!(
                        moved,
                        "a music folder was moved: its paths were brought along"
                    );
                }
                Err(error) => {
                    tracing::warn!("a music folder that was moved cannot be followed: {error}");
                }
            }
        }
        // An album without an album artist is told apart by its folder, and its key
        // is made of the folder's path. The paths have just become others: the keys
        // follow, all of them, or a song read after this (added to such a folder,
        // or its tags changed) would have the key of the new path and stand apart
        // from the album it belongs to.
        let album_keys_changed = if paths_moved {
            rekey_albums(&mut db).unwrap_or_else(|error| {
                tracing::warn!("the albums' keys could not follow the moved paths: {error}");
                Vec::new()
            })
        } else {
            Vec::new()
        };
        let report = OpenReport {
            recovered_corrupt_db: opened.recovered_corrupt_db,
            migrated_from: opened.migrated_from,
            album_keys_changed,
        };

        let mut state = State::default();
        let roots = db.roots()?;
        let folders: Vec<PathBuf> = roots.iter().map(|root| root.path.clone()).collect();
        let answers = asked_together(&folders, FOLDERS_ANSWER_WITHIN, |path| {
            (path.is_dir(), resolve_dir(path))
        });
        for (root, answer) in roots.into_iter().zip(answers) {
            let (online, canonical) = answer.unwrap_or_else(|| {
                tracing::warn!(
                    root_id = root.id,
                    "a music folder did not answer in time; it counts as not there for now"
                );
                (false, root.path.clone())
            });
            state.roots.push(RootState {
                id: root.id,
                path: root.path,
                canonical,
                online,
                last_scan_at: root.last_scan_at,
                unconfirmed_after: root.unconfirmed_after,
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
            state.insert(track, stored.art_from_folder);
        }
        for root_id in stale_offline {
            db.set_root_available(root_id, false)?;
        }

        let sheets_read: HashMap<String, SheetRecord> = db
            .kv_get(SHEETS_READ_KEY)
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str::<Vec<(String, SheetRecord)>>(&json).ok())
            .map(|records| records.into_iter().collect())
            .unwrap_or_default();
        let removed: HashSet<PathBuf> = db
            .removed_files()
            .unwrap_or_else(|err| {
                tracing::warn!(%err, "could not read the list of removed files");
                Vec::new()
            })
            .into_iter()
            .collect();
        let recent_plays = db.recent_plays(RECENT_PLAYS_KEPT).unwrap_or_else(|err| {
            tracing::warn!(%err, "could not read play history");
            Vec::new()
        });
        // Servers' tracks are kept for the saved queue only: what it does not name of
        // an earlier run goes now.
        if let Err(err) = db.prune_remote_tracks() {
            tracing::warn!(%err, "could not forget servers' tracks that are not queued");
        }
        let remote = remote::Catalog::from_rows(db.remote_tracks().unwrap_or_else(|err| {
            tracing::warn!(%err, "could not read the servers' tracks");
            Vec::new()
        }));
        let inner = Arc::new(Inner {
            db: Mutex::new(db),
            state: Mutex::new(state),
            snapshot: RwLock::new(LibrarySnapshot::empty()),
            publish_lock: Mutex::new(None),
            generation: AtomicU64::new(0),
            playlists: Mutex::new(Vec::new()),
            playlists_generation: AtomicU64::new(0),
            notifier: RwLock::new(None),
            remote: Mutex::new(remote),
            remote_generation: AtomicU64::new(0),
            art: ArtCache::new(&paths.cache_dir),
            art_jobs: Mutex::new(ArtJobs::default()),
            art_wake: Condvar::new(),
            art_generation: AtomicU64::new(0),
            scan: ScanQueue::default(),
            counters: Counters::default(),
            loudness: loudness::LoudnessJob::default(),
            checks: check::CheckJob::default(),
            skipped: Mutex::new(HashMap::new()),
            recently_added: Mutex::new(std::collections::VecDeque::new()),
            recent_plays: Mutex::new(recent_plays),
            removed: Mutex::new(removed),
            sheets_read: Mutex::new(sheets_read),
            sheets_dirty: AtomicBool::new(false),
            #[cfg(test)]
            panic_in_next_scan: AtomicBool::new(false),
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
        inner.spawn("oneum-art", run_art_worker);
        inner.spawn("oneum-art-cleanup", |inner| {
            let mut referenced: HashSet<String> = lock(&inner.state)
                .tracks
                .values()
                .filter_map(|e| e.track.art_id.clone())
                .collect();
            // The covers of servers' tracks in the queue are kept too.
            referenced.extend(lock(&inner.remote).art_ids().cloned());
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
        // symbolic link; the path is stored as the user gave it. Paths are resolved
        // here, before any lock is taken (the existing folders carry theirs).
        let resolved = resolve_dir(&path);
        let id = {
            let db = lock(&self.inner.db);
            let mut state = lock(&self.inner.state);
            for root in &state.roots {
                let existing = &root.canonical;
                if *existing == resolved || root.path == path {
                    return Err(LibraryError::AlreadyAdded);
                }
                if resolved.starts_with(existing) || path.starts_with(&root.path) {
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
                canonical: resolved,
                online: true,
                last_scan_at: None,
                unconfirmed_after: None,
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
        let (at, root, links) = {
            let _db = lock(&self.inner.db);
            let mut state = lock(&self.inner.state);
            let Some(at) = state.roots.iter().position(|root| root.id == root_id) else {
                return Err(LibraryError::NotFound);
            };
            let root = state.roots.remove(at);
            let (links, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut state.links)
                .into_iter()
                .partition(|link| link.root_id == root_id);
            state.links = kept;
            (at, root, links)
        };
        self.inner.scan.cancel_root(root_id);
        self.hand_to_twins(root_id);

        if let Err(error) = self.delete_folder_rows(root_id) {
            // The folder is still in the database with all of its tracks (the database
            // is locked, the disk full; nothing is deleted unless all of it is): it
            // stays a music folder, as before it was to be removed. The scan that was
            // stopped above is started again.
            tracing::warn!(root_id, %error, "a music folder could not be removed; it is kept");
            {
                let mut state = lock(&self.inner.state);
                let at = at.min(state.roots.len());
                state.roots.insert(at, root);
                state.links.extend(links);
            }
            self.inner
                .scan
                .request_root(&self.inner.counters, root_id, None);
            self.inner.reload_playlists();
            self.poke_watcher();
            self.inner.publish();
            return Err(error);
        }
        self.inner.forget_sheets_under(&root.path);
        // What was removed from this folder song by song: with the folder gone nothing
        // scans it, and added again it is taken in whole.
        self.inner
            .forget_removed(|path| path.starts_with(&root.path));
        tracing::info!(root_id, "music folder removed from the library");
        self.inner.reload_playlists();
        self.poke_watcher();
        self.inner.publish();
        Ok(())
    }

    /// Before a music folder goes: what the listener did with its songs (favourites,
    /// ratings, plays, places in playlists and in the queue) is handed to the same
    /// files in the other music folders, where a file there is plainly the same one
    /// (as a moved file is told, see `scan::match_relinks`). A collection that was
    /// copied to a new place, the new folder added and the old one taken away,
    /// would otherwise be the copies with nothing attached. The copy keeps its row
    /// (it may be the song that is playing) and takes the marks over; songs nothing
    /// was done with go with their folder.
    fn hand_to_twins(&self, root_id: i64) {
        let marked = match lock(&self.inner.db).marked_tracks_of_root(root_id) {
            Ok(ids) => ids,
            Err(err) => {
                tracing::warn!(%err, "could not look for the songs to hand over");
                return;
            }
        };
        if marked.is_empty() {
            return;
        }
        let (leaving, staying) = {
            let state = lock(&self.inner.state);
            let leaving: Vec<Arc<Track>> = marked
                .iter()
                .filter_map(|id| state.tracks.get(id).map(|entry| Arc::clone(&entry.track)))
                .collect();
            let staying: Vec<Arc<Track>> = state
                .tracks
                .values()
                .map(|entry| &entry.track)
                .filter(|track| track.available && track.root_id != Some(root_id))
                .cloned()
                .collect();
            (leaving, staying)
        };
        let mut handed = 0;
        for (gone, keep) in scan::match_relinks(&leaving, &staying) {
            let mut db = lock(&self.inner.db);
            match db.absorb_track(keep, gone) {
                Ok(Some(row)) => {
                    let mut state = lock(&self.inner.state);
                    state.remove(gone);
                    state.insert(row.track, row.art_from_folder);
                    handed += 1;
                }
                Ok(None) => {}
                Err(err) => tracing::warn!(%err, "could not hand a song over to its copy"),
            }
        }
        if handed > 0 {
            tracing::info!(
                handed,
                "songs of a folder that goes live on in their copies"
            );
        }
    }

    /// Deletes a music folder and its tracks from the database, all of it or nothing,
    /// and then the tracks from memory. It is one transaction on purpose, though a
    /// large folder keeps the database busy for it: deleted in parts, a failure half
    /// way would leave the first parts gone for good, and with them the favourites,
    /// play counts and playlist entries of those songs, which no scan brings back.
    /// How long that is, measured: a folder of 50,000 songs with 10,000 playlist
    /// entries and 25,000 plays is deleted in about a tenth of a second.
    fn delete_folder_rows(&self, root_id: i64) -> Result<(), LibraryError> {
        let mut db = lock(&self.inner.db);
        db.delete_root_with_tracks(root_id)?;
        let mut state = lock(&self.inner.state);
        let ids: Vec<TrackId> = state
            .tracks
            .values()
            .filter(|e| e.track.root_id == Some(root_id))
            .map(|e| e.track.id)
            .collect();
        for id in ids {
            state.remove(id);
        }
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
        self.inner.loudness.cancel();
        if let Some(tx) = lock(&self.inner.watch_tx).take() {
            let _ = tx.send(WatchMsg::Stop);
        }
        // Taking the lock first makes sure the worker is either before its check of the
        // shutdown flag or already waiting, so the wake-up cannot be missed.
        drop(lock(&self.inner.art_jobs));
        self.inner.art_wake.notify_all();
        let handles = std::mem::take(&mut *lock(&self.inner.threads));
        let current = std::thread::current().id();
        // Waited for, but not without end: a thread in the middle of reading a file on
        // a network volume that went away does not come back until the system gives
        // up on it, and quitting must not wait for that. Such a thread holds no lock
        // (none is held across reading a file) and ends with the process.
        let deadline = std::time::Instant::now() + SHUTDOWN_PATIENCE;
        for handle in handles {
            if handle.thread().id() == current {
                continue;
            }
            while !handle.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            if handle.is_finished() {
                let _ = handle.join();
            } else {
                tracing::warn!(
                    thread = handle.thread().name().unwrap_or("?"),
                    "a library thread did not stop; leaving it"
                );
            }
        }
        if let Err(err) = lock(&self.inner.db).checkpoint() {
            tracing::debug!(%err, "final checkpoint failed");
        }
    }

    // ---- tracks --------------------------------------------------------------------

    /// Registers individual audio files (not tied to a music folder) and returns their
    /// ids in input order, skipping files that are unreadable or unsupported. Files the
    /// library already knows are reused, also when they are named through a symbolic
    /// link, in different letter case on a case-insensitive volume, or in another
    /// Unicode form: a path is resolved to its on-disk spelling before it is compared
    /// or stored. Blocking: reads the files.
    ///
    /// The audio file of a CUE sheet in a music folder is in the library as the sheet's
    /// tracks: it stands for them (not added alone, which its scan would undo).
    pub fn import_files(&self, paths: &[PathBuf]) -> Vec<TrackId> {
        self.import_paths(paths).into_iter().flatten().collect()
    }

    /// The tracks each path stands for, in input order: one, the tracks of the CUE
    /// sheet a file is the audio of, or none (unreadable, unsupported, not there).
    pub fn import_paths(&self, paths: &[PathBuf]) -> Vec<Vec<TrackId>> {
        /// Files per database transaction.
        const BATCH: usize = 200;
        let inner = &self.inner;
        let lookup = |path: &Path| -> Option<(TrackId, bool)> {
            let state = lock(&inner.state);
            let id = state.id_of_path(path)?;
            state.tracks.get(&id).map(|e| (id, e.track.available))
        };

        // Pass 1: find out which paths the library already has. A path that is not
        // known as written is resolved to its on-disk spelling (symbolic links, letter
        // case on a case-insensitive volume, Unicode form) and looked up again under
        // the spelling the library uses for that location. No lock is held while the
        // file system is asked.
        let mut results: Vec<Vec<TrackId>> = vec![Vec::new(); paths.len()];
        // The CUE sheets of a folder, as (audio file, sheet, its track numbers).
        let mut sheets_of: HashMap<PathBuf, Vec<FolderSheet>> = HashMap::new();
        let mut revive: Vec<TrackId> = Vec::new();
        // New files: library path -> (folder it belongs to, result slots waiting for it).
        let mut fresh: Vec<(PathBuf, Option<i64>, Vec<usize>)> = Vec::new();
        let mut fresh_index: HashMap<PathBuf, usize> = HashMap::new();
        // Tracks of CUE sheets that are not in the library, by sheet: (the sheet, the
        // folder it belongs to, the track numbers asked for with their result slots).
        type SheetWanted = (PathBuf, Option<i64>, Vec<(u32, usize)>);
        let mut fresh_sheets: Vec<SheetWanted> = Vec::new();
        for (slot, path) in paths.iter().enumerate() {
            let Ok(absolute) = std::path::absolute(path) else {
                continue;
            };
            let absolute = m3u::lexical_normalize(&absolute);
            let mut known = lookup(&absolute);
            let mut location = None;
            // A track of a CUE sheet, as a playlist written by this app names it
            // (`…/Album.cue/03`): the sheet is the file to find.
            if known.is_none()
                && let (Some(sheet), Some(number)) =
                    (cue::sheet_of(&absolute), cue::track_number(&absolute))
            {
                let Ok(canonical) = fs::canonicalize(sheet) else {
                    continue; // the sheet does not exist
                };
                let (sheet_path, root_id) = lock(&inner.state).library_path(&canonical);
                known = lookup(&cue::track_path(&sheet_path, number));
                if known.is_none() {
                    match fresh_sheets
                        .iter_mut()
                        .find(|(path, _, _)| *path == sheet_path)
                    {
                        Some((_, _, wanted)) => wanted.push((number, slot)),
                        None => fresh_sheets.push((sheet_path, root_id, vec![(number, slot)])),
                    }
                    continue;
                }
            } else if known.is_none()
                && let Ok(canonical) = fs::canonicalize(&absolute)
            {
                let (library_path, root_id) = lock(&inner.state).library_path(&canonical);
                known = lookup(&library_path);
                location = Some((library_path, root_id));
            }
            if let Some((id, available)) = known {
                results[slot] = vec![id];
                if !available && absolute.is_file() && !revive.contains(&id) {
                    revive.push(id);
                }
                continue;
            }
            let Some((library_path, root_id)) = location else {
                continue; // the file does not exist
            };
            if !tags::has_audio_extension(&library_path) {
                continue;
            }
            // The audio file of a CUE sheet in a music folder: it is the sheet's tracks.
            if root_id.is_some()
                && let Some(folder) = library_path.parent()
            {
                let sheets = sheets_of
                    .entry(folder.to_path_buf())
                    .or_insert_with(|| sheets_in(inner, folder));
                let size = sheets
                    .iter()
                    .any(|sheet| matches!(sheet.file, SheetFile::OfSize(_)))
                    .then(|| fs::metadata(&library_path).map(|meta| meta.len()).ok())
                    .flatten();
                // The first sheet (by name) that is for the file has it.
                let sheet = sheets.iter().find(|sheet| match &sheet.file {
                    SheetFile::Named(audio) => *audio == library_path,
                    SheetFile::OfSize(of) => size == Some(*of),
                });
                if let Some(sheet) = sheet {
                    // Only the tracks its reading gave: one that starts beyond the end
                    // of the file may still be kept (a favourite), and is not one.
                    let given = lock(&inner.sheets_read)
                        .get(&scan::sheet_key(&sheet.path))
                        .map(|record| record.tracks.clone());
                    let state = lock(&inner.state);
                    let tracks: Vec<TrackId> = sheet
                        .numbers
                        .iter()
                        .filter(|n| given.as_ref().is_none_or(|given| given.contains(n)))
                        .filter_map(|n| state.id_of_path(&cue::track_path(&sheet.path, *n)))
                        .collect();
                    if !tracks.is_empty() {
                        // The file is there (the sheet was found for it): its tracks are
                        // available, like a known file opened again.
                        for &id in &tracks {
                            let unavailable =
                                state.tracks.get(&id).is_some_and(|e| !e.track.available);
                            if unavailable && !revive.contains(&id) {
                                revive.push(id);
                            }
                        }
                        results[slot] = tracks;
                        continue;
                    }
                }
            }
            match fresh_index.get(&library_path) {
                Some(&index) => fresh[index].2.push(slot),
                None => {
                    fresh_index.insert(library_path.clone(), fresh.len());
                    fresh.push((library_path, root_id, vec![slot]));
                }
            }
        }

        // Pass 2: read the new files and store them, one transaction per batch.
        let folder_art: FolderArt = Mutex::new(HashMap::new());
        let mut changed = !revive.is_empty();
        inner.revive(&revive);
        for batch in fresh.chunks(BATCH) {
            let mut files = Vec::with_capacity(batch.len());
            let mut slots: Vec<&[usize]> = Vec::with_capacity(batch.len());
            for (path, root_id, waiting) in batch {
                let Some(meta) = fs::metadata(path).ok().filter(|m| m.is_file()) else {
                    continue;
                };
                let Ok(mut file) =
                    scan::read_file(inner, path, meta.len(), scan::mtime_ns(&meta), &folder_art)
                else {
                    continue;
                };
                // A file the folder scan would not visit (inside a hidden directory)
                // stays an individually registered file; otherwise the scanner would
                // never confirm it.
                file.root_id = root_id.filter(|id| {
                    lock(&inner.state)
                        .root(*id)
                        .is_some_and(|root| scan::is_walkable(&root.path, path))
                });
                files.push(file);
                slots.push(waiting);
            }
            let committed = inner.commit_scanned(files, &[], None, None);
            for ((id, _), waiting) in committed.iter().zip(slots) {
                for &slot in waiting {
                    results[slot] = vec![*id];
                }
                changed = true;
            }
        }
        // The tracks of sheets: each sheet read once, the tracks asked for stored.
        for (sheet_path, root_id, wanted) in fresh_sheets {
            let Ok(sheet_meta) = fs::metadata(&sheet_path) else {
                continue;
            };
            let Some(audio) =
                cue::read(&sheet_path).and_then(|sheet| cue::audio_of(&sheet_path, &sheet))
            else {
                continue;
            };
            let Some(audio_meta) = fs::metadata(&audio).ok().filter(|m| m.is_file()) else {
                continue;
            };
            let (size, audio_mtime) = (audio_meta.len(), scan::mtime_ns(&audio_meta));
            let Ok(read) =
                scan::read_sheet(inner, &sheet_path, &audio, size, audio_mtime, &folder_art)
            else {
                continue;
            };
            let given: Vec<u32> = read
                .iter()
                .filter_map(|file| cue::track_number(&file.path))
                .collect();
            // In a music folder its scan takes the sheet in whole; elsewhere (or where
            // that scan does not go) these tracks are individually registered ones.
            let root_id = root_id.filter(|id| {
                lock(&inner.state)
                    .root(*id)
                    .is_some_and(|root| scan::is_walkable(&root.path, &sheet_path))
            });
            let mut files = Vec::new();
            let mut slots: Vec<Vec<usize>> = Vec::new();
            for mut file in read {
                let waiting: Vec<usize> = cue::track_number(&file.path)
                    .map(|number| {
                        wanted
                            .iter()
                            .filter(|(asked, _)| *asked == number)
                            .map(|(_, slot)| *slot)
                            .collect()
                    })
                    .unwrap_or_default();
                if waiting.is_empty() {
                    continue;
                }
                file.root_id = root_id;
                files.push(file);
                slots.push(waiting);
            }
            let states: Vec<(PathBuf, i64)> =
                files.iter().map(|f| (f.path.clone(), f.mtime_ns)).collect();
            let committed = inner.commit_scanned(files, &[], None, None);
            for ((id, _), waiting) in committed.iter().zip(&slots) {
                for &slot in waiting {
                    results[slot] = vec![*id];
                }
                changed = true;
            }
            // Individually registered: the check of such files goes by this record
            // (see `scan::check_loose_sheet`).
            if root_id.is_none() {
                let record = SheetRecord {
                    mtime: scan::mtime_ns(&sheet_meta),
                    tracks: given,
                    audio_mtime,
                    audio_size: size,
                };
                scan::record_sheets_read(
                    inner,
                    vec![(scan::sheet_key(&sheet_path), record, states)],
                );
            }
        }
        if changed {
            inner.publish();
        }
        inner.save_sheet_records();
        results
    }

    /// Removes tracks from the library. Their files are not touched, and are
    /// remembered as removed: scans of their folders pass them by from now on, so the
    /// songs do not come back by themselves (see [`Inner::removed`]). Opening such a
    /// file by hand takes it in again.
    pub fn remove_tracks(&self, ids: &[TrackId]) {
        if ids.is_empty() {
            return;
        }
        {
            let mut db = lock(&self.inner.db);
            // (The tracks' lock is not held while the database writes.)
            let tracks: Vec<(TrackId, PathBuf)> = {
                let state = lock(&self.inner.state);
                ids.iter()
                    .filter_map(|&id| Some((id, state.tracks.get(&id)?.track.path.clone())))
                    .collect()
            };
            if let Err(err) = db.delete_tracks_remembering(&tracks) {
                tracing::warn!(%err, "could not remove tracks");
                return;
            }
            let mut state = lock(&self.inner.state);
            let mut removed = lock(&self.inner.removed);
            for (id, path) in tracks {
                state.remove(id);
                removed.insert(path);
            }
        }
        self.inner.reload_playlists();
        self.inner.publish();
    }

    /// How many files are remembered as removed from the library (see
    /// [`Library::remove_tracks`]).
    pub fn removed_count(&self) -> usize {
        lock(&self.inner.removed).len()
    }

    /// Forgets every removal and scans the music folders: the songs that were removed
    /// one by one, and whose files are still there, are in the library again.
    pub fn restore_removed(&self) {
        self.inner.forget_removed(|_| true);
        self.rescan();
    }

    /// The current data of a track (may be newer than the published snapshot).
    /// Up to `count` songs to play after `seed` when the queue has run out (see
    /// [`radio::picks`]), none of them in `exclude`.
    pub fn radio_picks(&self, seed: TrackId, exclude: &[TrackId], count: usize) -> Vec<TrackId> {
        let Some(seed) = self.track(seed) else {
            return Vec::new();
        };
        let snapshot = self.snapshot();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        // A different roll every time: the clock's nanoseconds and the seed.
        let dice = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| u64::from(d.subsec_nanos()))
            ^ (seed.id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        radio::picks(
            &snapshot.tracks,
            &seed,
            &exclude.iter().copied().collect(),
            count,
            now,
            dice,
        )
    }

    /// A count that grows whenever tracks leave the library (removed by the user,
    /// their folder removed, their files gone). Most notifications are about other
    /// things (scan and measuring progress, tags, play counts); comparing this tells
    /// whether anything that refers to tracks by id has to be checked again.
    pub fn removals(&self) -> u64 {
        lock(&self.inner.state).removals
    }

    pub fn track(&self, id: TrackId) -> Option<Arc<Track>> {
        if Library::is_remote(id) {
            return self.remote_track(id);
        }
        lock(&self.inner.state)
            .tracks
            .get(&id)
            .map(|e| Arc::clone(&e.track))
    }

    /// Marks or unmarks tracks as favourites. On an error nothing changed.
    /// Sets the listener's rating of the tracks: 1 to 5 stars, 0 for none. Kept in the
    /// app's database; the files are not touched.
    pub fn set_rating(&self, ids: &[TrackId], rating: u8) -> Result<(), LibraryError> {
        let rating = rating.min(5);
        let changed: Vec<TrackId> = {
            let mut db = lock(&self.inner.db);
            let mut state = lock(&self.inner.state);
            let changed: Vec<TrackId> = ids
                .iter()
                .copied()
                .filter(|id| {
                    state
                        .tracks
                        .get(id)
                        .is_some_and(|e| e.track.rating != rating)
                })
                .collect();
            db.set_rating(&changed, rating)?;
            for &id in &changed {
                state.update(id, |t| t.rating = rating);
            }
            changed
        };
        self.inner.publish_patch(&changed);
        Ok(())
    }

    pub fn set_favorite(&self, ids: &[TrackId], favorite: bool) -> Result<(), LibraryError> {
        let changed: Vec<TrackId> = {
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
            db.set_favorite(&ids, favorite)?;
            let mut state = lock(&self.inner.state);
            for &id in &ids {
                state.update(id, |t| t.favorite = favorite);
            }
            ids
        };
        self.inner.publish_patch(&changed);
        Ok(())
    }

    // ---- measured loudness -----------------------------------------------------------

    /// Starts measuring the loudness of the tracks whose files have no ReplayGain tags,
    /// in the background. The results are kept in the app's database; the files are
    /// only read. Returns false if a run is in progress already.
    pub fn measure_loudness(&self) -> bool {
        loudness::start(&self.inner)
    }

    /// Stops the run after the files that are being read; what was stored stays.
    pub fn cancel_loudness(&self) {
        self.inner.loudness.cancel();
    }

    pub fn loudness_status(&self) -> LoudnessStatus {
        self.inner.loudness.status()
    }

    /// Forgets everything that was measured (not while a run is in progress).
    pub fn forget_loudness(&self) {
        if !self.inner.loudness.status().running {
            self.inner.forget_loudness();
        }
    }

    /// Blocks until no measuring run is in progress. Returns false if `timeout`
    /// elapsed first. Not for the UI thread; meant for tools and tests.
    pub fn wait_for_loudness(&self, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while self.inner.loudness.status().running {
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }

    // ---- file check ------------------------------------------------------------------

    /// Starts checking the files that have no result yet for their current state:
    /// integrity, dynamic range, true peak (see `audio::check`). The files are only
    /// read. Returns false if a run is in progress already.
    pub fn check_files(&self) -> bool {
        check::start(&self.inner)
    }

    /// Stops the run after the files that are being read; what was stored stays.
    pub fn cancel_check(&self) {
        self.inner.checks.cancel();
    }

    pub fn check_status(&self) -> CheckStatus {
        self.inner.checks.status()
    }

    /// Forgets every check result (not while a run is in progress).
    pub fn forget_checks(&self) {
        if !self.inner.checks.status().running {
            self.inner.forget_checks();
        }
    }

    /// Blocks until no check run is in progress. False if `timeout` elapsed first. Not
    /// for the UI thread.
    pub fn wait_for_checks(&self, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while self.inner.checks.status().running {
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }

    /// Records that a track was played: adds a history row and updates its play count
    /// and last-played time.
    pub fn record_play(&self, id: TrackId) {
        let now = unix_now();
        {
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
            });
            let mut recent = lock(&self.inner.recent_plays);
            recent.retain(|played| *played != id);
            recent.insert(0, id);
            recent.truncate(RECENT_PLAYS_KEPT);
        }
        self.inner.publish_patch(&[id]);
    }

    /// Every counted play at or after `since` (Unix seconds): `(track, when)`, oldest
    /// first. Blocking; not for the UI thread.
    pub fn plays_since(&self, since: i64) -> Vec<(TrackId, i64)> {
        lock(&self.inner.db)
            .plays_since(since)
            .unwrap_or_else(|err| {
                tracing::warn!(%err, "could not read play history");
                Vec::new()
            })
    }

    /// Recently played tracks, most recent first, each track once.
    pub fn recent_plays(&self, limit: usize) -> Vec<TrackId> {
        // From memory (see `Inner::recent_plays`): no waiting for the database.
        let recent = lock(&self.inner.recent_plays).clone();
        let state = lock(&self.inner.state);
        recent
            .into_iter()
            .filter(|id| state.tracks.contains_key(id))
            .take(limit)
            .collect()
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

    /// Creates a playlist with the given tracks in it, in one transaction (a failure
    /// leaves no empty playlist behind). The name is trimmed.
    pub fn create_playlist_with(
        &self,
        name: &str,
        tracks: &[TrackId],
    ) -> Result<i64, LibraryError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(LibraryError::InvalidName);
        }
        let (id, _) = lock(&self.inner.db).create_playlist_with_items(name, unix_now(), tracks)?;
        self.playlists_changed();
        Ok(id)
    }

    /// Gives a playlist exactly these tracks, in this order (undo of a removal). One
    /// transaction: if it fails, the playlist keeps what it had.
    pub fn replace_playlist_items(&self, id: i64, tracks: &[TrackId]) -> Result<(), LibraryError> {
        if !lock(&self.inner.db).replace_playlist_items(id, tracks)? {
            return Err(LibraryError::NotFound);
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
        let bytes = read_playlist_file(src)?;
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
        let mut resolved: Vec<Vec<TrackId>> = {
            let state = lock(&self.inner.state);
            let mut by_nfc: Option<HashMap<String, TrackId>> = None;
            entries
                .iter()
                .map(|entry| {
                    let path = entry.path.as_ref()?;
                    if let Some(id) = state.id_of_path(path) {
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
                .map(|id| id.into_iter().collect())
                .collect()
        };
        // Second pass: entries known under another spelling of their path, and files
        // that exist but are new to the library (read and stored in batches).
        let unknown: Vec<(usize, PathBuf)> = entries
            .iter()
            .enumerate()
            .filter(|(i, _)| resolved[*i].is_empty())
            .filter_map(|(i, entry)| Some((i, entry.path.clone()?)))
            .collect();
        let unknown_paths: Vec<PathBuf> = unknown.iter().map(|(_, p)| p.clone()).collect();
        // (An entry naming a CUE sheet's audio file stands for the sheet's tracks.)
        for ((i, _), ids) in unknown.iter().zip(self.import_paths(&unknown_paths)) {
            resolved[*i] = ids;
        }

        let matched: Vec<TrackId> = resolved.iter().flatten().copied().collect();
        let missing: Vec<String> = entries
            .iter()
            .zip(&resolved)
            .filter(|(_, ids)| ids.is_empty())
            .map(|(entry, _)| entry.raw.clone())
            .collect();

        // The playlist and its entries are written together or not at all.
        let (playlist_id, added) =
            lock(&self.inner.db).create_playlist_with_items(&name, unix_now(), &matched)?;
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

    /// Takes over what Apple Music knew from its library export (see `apple`): play
    /// counts, last plays, dates added, loved songs as favourites, and the user's
    /// playlists. Songs are matched by their file (also when the path differs only in
    /// Unicode normalisation); songs not in the library are counted, not added. Counts
    /// are merged, not added up: importing twice changes nothing more. Blocking.
    pub fn import_apple_music(&self, file: &Path) -> Result<AppleImport, LibraryError> {
        self.import_apple_music_from(file, false)
    }

    /// [`Self::import_apple_music`] for a library whose files may lie elsewhere than
    /// the export says. With `elsewhere`, a song that is not found at its place is
    /// looked for by the end of its path (the folders it lies in and its name, see
    /// [`apple::match_by_tail`]): on a phone the music is a copy of what the export,
    /// made on a computer, describes.
    pub fn import_apple_music_from(
        &self,
        file: &Path,
        elsewhere: bool,
    ) -> Result<AppleImport, LibraryError> {
        let not_export = || {
            LibraryError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "not an Apple Music library export",
            ))
        };
        let bytes = crate::storage::files::read_bounded(file, apple::MAX_FILE_BYTES)?
            .ok_or_else(not_export)?;
        let export = apple::parse(&String::from_utf8_lossy(&bytes)).ok_or_else(not_export)?;
        drop(bytes);

        // The export's songs that are in the library, by the export's id.
        let found: HashMap<i64, TrackId> = {
            let state = lock(&self.inner.state);
            let by_nfc: HashMap<String, TrackId> = state
                .by_path
                .iter()
                .map(|(p, &id)| (p.to_string_lossy().nfc().collect(), id))
                .collect();
            let mut found: HashMap<i64, TrackId> = export
                .tracks
                .iter()
                .filter_map(|t| {
                    let id = state.id_of_path(&t.path).or_else(|| {
                        let key: String = t.path.to_string_lossy().nfc().collect();
                        by_nfc.get(&key).copied()
                    })?;
                    Some((t.id, id))
                })
                .collect();
            if elsewhere {
                let taken: HashSet<TrackId> = found.values().copied().collect();
                let theirs: Vec<(i64, &Path)> = export
                    .tracks
                    .iter()
                    .filter(|t| !found.contains_key(&t.id))
                    .map(|t| (t.id, t.path.as_path()))
                    .collect();
                let ours: Vec<(TrackId, &Path)> = state
                    .by_path
                    .iter()
                    .filter(|(_, id)| !taken.contains(id))
                    .map(|(path, &id)| (id, path.as_path()))
                    .collect();
                found.extend(apple::match_by_tail(&theirs, &ours));
            }
            found
        };
        let rows: Vec<crate::storage::db::ListeningRow> = export
            .tracks
            .iter()
            .filter_map(|t| {
                let id = *found.get(&t.id)?;
                Some((id, t.play_count, t.last_played, t.added, t.loved, t.rating))
            })
            .collect();
        let playlists: Vec<ListIn> = export
            .playlists
            .iter()
            .map(|playlist| ListIn {
                name: playlist.name.clone(),
                tracks: playlist
                    .items
                    .iter()
                    .filter_map(|item| found.get(item).copied())
                    .collect(),
                // (The export's lists without songs are left out when it is read.)
                empty_there: false,
            })
            .collect();
        let made = self.take_over(&rows, &playlists, "Apple Music")?;
        let matched = found.len();
        tracing::info!(matched, playlists = made, "Apple Music library imported");
        Ok(AppleImport {
            matched,
            unmatched: export.tracks.len() - matched,
            playlists: made,
        })
    }

    /// Gives songs of the library what another record knew of them (an Apple Music
    /// library export, the app's own library data from another device). The listening
    /// rows are merged, not added up (see `Db::merge_listening`). The playlists are
    /// made, each with its songs in one step: one that is here already (same name,
    /// same songs: an earlier import) is not made again, one without a song that is
    /// here is left out unless it was empty in the record too, and a name taken by
    /// another list gets `mark` behind it. Returns how many playlists were made.
    fn take_over(
        &self,
        rows: &[crate::storage::db::ListeningRow],
        playlists: &[ListIn],
        mark: &str,
    ) -> Result<usize, LibraryError> {
        let changed: Vec<TrackId> = {
            let mut db = lock(&self.inner.db);
            db.merge_listening(rows)?;
            let mut state = lock(&self.inner.state);
            for &(id, plays, last, added, loved, rating) in rows {
                state.update(id, |t| {
                    if t.rating == 0 {
                        t.rating = rating.min(5);
                    }
                    t.play_count = t.play_count.max(plays);
                    if let Some(last) = last
                        && t.last_played_at.is_none_or(|at| last > at)
                    {
                        t.last_played_at = Some(last);
                    }
                    if let Some(added) = added
                        && added < t.added_at
                    {
                        t.added_at = added;
                    }
                    t.favorite |= loved;
                });
            }
            rows.iter().map(|r| r.0).collect()
        };
        self.inner.publish_patch(&changed);

        let mut existing: Vec<(String, Vec<TrackId>)> = self
            .playlists()
            .into_iter()
            .map(|p| {
                let tracks = self
                    .playlist_items(p.id)
                    .into_iter()
                    .map(|i| i.track_id)
                    .collect();
                (p.name, tracks)
            })
            .collect();
        let mut made = 0;
        for ListIn {
            name,
            tracks,
            empty_there,
        } in playlists
        {
            if tracks.is_empty() && !empty_there {
                continue;
            }
            // The list is here already, under its name or a marked one: passed over.
            let its_names = |known: &str| {
                known == name
                    || known
                        .strip_prefix(name.as_str())
                        .and_then(|rest| rest.strip_prefix(" ("))
                        .and_then(|rest| rest.strip_suffix(')'))
                        .is_some_and(|mark| !mark.is_empty())
            };
            if existing
                .iter()
                .any(|(known, items)| its_names(known) && items == tracks)
            {
                continue;
            }
            // A name that stands for another list here gets a mark, and where that
            // one is taken too, the next ("Drive (2)", "Drive (3)").
            let taken = |wanted: &str| existing.iter().any(|(known, _)| known == wanted);
            let name = if !taken(name) {
                name.clone()
            } else {
                // (A mark that is a number counts on; one that is a word gets a
                // number behind it.)
                let counted = mark.parse::<u32>().ok();
                let mut marked = format!("{name} ({mark})");
                let mut number = counted.unwrap_or(1);
                while taken(&marked) {
                    number += 1;
                    marked = match counted {
                        Some(_) => format!("{name} ({number})"),
                        None => format!("{name} ({mark} {number})"),
                    };
                }
                marked
            };
            lock(&self.inner.db).create_playlist_with_items(&name, unix_now(), tracks)?;
            existing.push((name, tracks.clone()));
            made += 1;
        }
        if made > 0 {
            self.playlists_changed();
        }
        Ok(made)
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

    /// Saves the playback session in one transaction: the play queue as
    /// `(queue item id, track id)` pairs in order, when given, and the rest of the
    /// session as `value` under `key`. Returns false if nothing could be saved (then
    /// nothing was: the stored session is still the previous one, whole).
    pub fn save_session(&self, queue: Option<&[(u64, TrackId)]>, key: &str, value: &str) -> bool {
        match lock(&self.inner.db).save_session(queue, key, value) {
            Ok(()) => true,
            Err(err) => {
                tracing::warn!(%err, "could not save the playback session");
                false
            }
        }
    }

    /// Where the user left off in a track, if that was remembered.
    pub fn resume_point(&self, track: TrackId) -> Option<u64> {
        lock(&self.inner.db)
            .resume_point(track)
            .unwrap_or_else(|err| {
                tracing::warn!(%err, "could not read a resume point");
                None
            })
    }

    /// Remembers where the user left off in a track, or forgets it (`None`).
    pub fn set_resume_point(&self, track: TrackId, position_ms: Option<u64>) {
        if let Err(err) = lock(&self.inner.db).set_resume_point(track, position_ms, unix_now()) {
            tracing::warn!(%err, "could not save a resume point");
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
    /// cache file — if it was evicted it is regenerated in the background; when it is
    /// available again [`Library::art_generation`] increases and the notifier is called.
    pub fn load_art_rgba(&self, art_id: &str, max_px: u32) -> Option<ArtImage> {
        if let Some(image) = self.inner.art.load_rgba(art_id, max_px) {
            return Some(image);
        }
        if self.inner.art.exists(art_id) {
            // Present but undecodable: discard it so it can be rebuilt.
            let _ = fs::remove_file(self.inner.art.path_for(art_id));
        }
        self.inner.request_art(art_id);
        None
    }

    /// Increases whenever a cover that [`Library::load_art_rgba`] could not deliver has
    /// been regenerated. A caller that got `None` can retry after this changed.
    pub fn art_generation(&self) -> u64 {
        self.inner.art_generation.load(Ordering::Acquire)
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

    /// Scans only `dirs` of the music folder they are in, as the folder watcher does
    /// when files in them change (lets tests take that path without waiting for the
    /// watcher). Folders that are in no music folder are ignored.
    #[doc(hidden)]
    pub fn debug_rescan_dirs(&self, dirs: &[PathBuf]) {
        let mut by_root: HashMap<i64, std::collections::BTreeSet<PathBuf>> = HashMap::new();
        {
            let state = lock(&self.inner.state);
            for dir in dirs {
                if let Some(root) = state.root_containing(dir) {
                    by_root.entry(root.id).or_default().insert(dir.clone());
                }
            }
        }
        for (root_id, dirs) in by_root {
            self.inner
                .scan
                .request_root(&self.inner.counters, root_id, Some(dirs));
        }
    }

    /// Forgets which files were added in the last minutes (lets tests stand where the
    /// library stands a long time after files were added).
    #[doc(hidden)]
    pub fn debug_forget_recently_added(&self) {
        lock(&self.inner.recently_added).clear();
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
    use crate::model::ReplayGainTags;

    /// A library in a temporary directory with one scanned, untagged WAV file named
    /// `old name.wav`.
    fn library_with_one_track() -> (tempfile::TempDir, Arc<Library>, TrackId) {
        let dir = tempfile::tempdir().unwrap();
        let music = dir.path().join("music");
        fs::create_dir_all(&music).unwrap();
        write_wav(&music.join("old name.wav"));
        let (library, _) = Library::open(LibraryPaths {
            data_dir: dir.path().join("data"),
            cache_dir: dir.path().join("cache"),
        })
        .unwrap();
        library.add_folder(&music).unwrap();
        assert!(library.wait_for_scan(Duration::from_secs(30)));
        let id = library.snapshot().tracks[0].id;
        (dir, library, id)
    }

    /// Minimal 16-bit mono PCM WAV, a quarter of a second long.
    pub(super) fn write_wav(path: &Path) {
        let data = vec![0u8; 4000];
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&8000u32.to_le_bytes());
        bytes.extend_from_slice(&16000u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&data);
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn stats_patch_never_publishes_unindexed_tag_changes() {
        let (_dir, library, id) = library_with_one_track();
        let inner = &library.inner;
        let track = library.track(id).unwrap();
        assert_eq!(track.title, "old name");

        // A scan batch has been committed but not published yet: the file was retagged.
        let retagged = ScannedFile {
            path: track.path.clone(),
            root_id: track.root_id,
            size: track.file_size + 1,
            mtime_ns: track.mtime_ns + 1,
            tags: tags::FileTags {
                title: "new name".into(),
                artist: "someone".into(),
                album_artist: String::new(),
                album: String::new(),
                compilation: false,
                disc_no: None,
                track_no: None,
                year: None,
                genre: String::new(),
                duration_ms: track.duration_ms,
                codec: "PCM",
                container: "WAV",
                sample_rate: track.sample_rate,
                channels: track.channels,
                bit_depth: track.bit_depth,
                bitrate_kbps: track.bitrate_kbps,
                replaygain: ReplayGainTags::default(),
                picture: None,
            },
            art_id: None,
            art_from_folder: false,
        };
        assert_eq!(
            inner.commit_scanned(vec![retagged], &[], None, None),
            [(id, false)]
        );
        assert_eq!(library.track(id).unwrap().title, "new name");
        assert_eq!(library.snapshot().get(id).unwrap().title, "old name");

        // The user marks the track as a favourite and plays it in that window. The
        // snapshot shows both, but its track still agrees with its search index.
        library.set_favorite(&[id], true).unwrap();
        library.record_play(id);
        let snapshot = library.snapshot();
        let published = snapshot.get(id).unwrap();
        assert!(published.favorite);
        assert_eq!(published.play_count, 1);
        assert_eq!(published.title, "old name");
        assert_eq!(snapshot.search("old").len(), 1);
        assert!(snapshot.search("new").is_empty());

        // The scan's own publish then brings the new tags and their index together,
        // with the statistics intact.
        inner.publish();
        let snapshot = library.snapshot();
        let published = snapshot.get(id).unwrap();
        assert_eq!(published.title, "new name");
        assert!(published.favorite);
        assert_eq!(published.play_count, 1);
        assert_eq!(snapshot.search("new").len(), 1);
        assert!(snapshot.search("old").is_empty());
    }

    #[test]
    fn stats_patch_reads_the_latest_state_whatever_the_caller_saw() {
        let (_dir, library, id) = library_with_one_track();
        let inner = &library.inner;
        // Two changes land in the state before either caller gets to publish.
        {
            let mut db = lock(&inner.db);
            db.set_favorite(&[id], true).unwrap();
            assert!(db.record_play(id, 77).unwrap());
            let mut state = lock(&inner.state);
            state.update(id, |t| t.favorite = true);
            state.update(id, |t| {
                t.play_count = 1;
                t.last_played_at = Some(77);
            });
        }
        // Whichever publishes first (or last) publishes everything.
        inner.publish_patch(&[id]);
        let after_first = library.snapshot();
        let published = after_first.get(id).unwrap();
        assert!(published.favorite);
        assert_eq!(
            (published.play_count, published.last_played_at),
            (1, Some(77))
        );
        let generation = library.generation();
        inner.publish_patch(&[id]);
        assert_eq!(library.generation(), generation, "nothing left to publish");
        // Unknown ids are ignored.
        inner.publish_patch(&[id + 1000]);
        assert_eq!(library.generation(), generation);
    }

    #[test]
    fn a_panicking_scan_does_not_leave_the_scanner_running() {
        let dir = tempfile::tempdir().unwrap();
        let music = dir.path().join("music");
        fs::create_dir_all(&music).unwrap();
        write_wav(&music.join("a.wav"));
        let (library, _) = Library::open(LibraryPaths {
            data_dir: dir.path().join("data"),
            cache_dir: dir.path().join("cache"),
        })
        .unwrap();
        library
            .inner
            .panic_in_next_scan
            .store(true, Ordering::Relaxed);
        library.add_folder(&music).unwrap();
        assert!(
            library.wait_for_scan(Duration::from_secs(30)),
            "the scanner must come to rest after a failed scan"
        );
        let status = library.scan_status();
        assert!(!status.running);
        assert_eq!(status.errors, 1);
        assert_eq!(status.current_root, None);
        assert!(library.snapshot().is_empty());

        // The scan thread survived and the next scan works.
        library.rescan();
        assert!(library.wait_for_scan(Duration::from_secs(30)));
        assert_eq!(library.snapshot().len(), 1);
        assert_eq!(library.scan_status().errors, 0);
    }

    #[test]
    fn resolved_paths_map_back_to_the_library_spelling() {
        let mut state = State::default();
        state.roots.push(RootState {
            id: 3,
            path: PathBuf::from("/Volumes/Link/Music"),
            canonical: PathBuf::from("/Volumes/Disk/Real Music"),
            online: true,
            last_scan_at: None,
            unconfirmed_after: None,
        });
        state.links.push(FollowedLink {
            root_id: 3,
            link: PathBuf::from("/Volumes/Link/Music/more"),
            target: PathBuf::from("/Volumes/Other"),
        });
        state.links.push(FollowedLink {
            root_id: 3,
            link: PathBuf::from("/Volumes/Link/Music/more/deep"),
            target: PathBuf::from("/Volumes/Other/Deep"),
        });
        let map = |path: &str| state.library_path(Path::new(path));
        assert_eq!(
            map("/Volumes/Disk/Real Music/a/b.flac"),
            (PathBuf::from("/Volumes/Link/Music/a/b.flac"), Some(3))
        );
        assert_eq!(
            map("/Volumes/Link/Music/a/b.flac"),
            (PathBuf::from("/Volumes/Link/Music/a/b.flac"), Some(3))
        );
        assert_eq!(
            map("/Volumes/Other/x.flac"),
            (PathBuf::from("/Volumes/Link/Music/more/x.flac"), Some(3))
        );
        assert_eq!(
            map("/Volumes/Other/Deep/y.flac"),
            (
                PathBuf::from("/Volumes/Link/Music/more/deep/y.flac"),
                Some(3)
            )
        );
        assert_eq!(
            map("/Users/me/Downloads/z.flac"),
            (PathBuf::from("/Users/me/Downloads/z.flac"), None)
        );
        // A sibling whose name merely starts the same is not inside the folder.
        assert_eq!(
            map("/Volumes/Disk/Real Music 2/z.flac"),
            (PathBuf::from("/Volumes/Disk/Real Music 2/z.flac"), None)
        );
    }

    #[test]
    fn shared_types_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Library>();
        assert_send_sync::<LibrarySnapshot>();
        assert_send_sync::<PreparedQuery>();
        assert_send_sync::<ScanStatus>();
        assert_send_sync::<LibraryError>();
    }

    /// A thread that does not come back (reading from a network volume that went away)
    /// does not keep the app from quitting.
    #[test]
    fn quitting_does_not_wait_for_ever_for_a_thread_that_is_stuck() {
        let (_dir, library, _) = library_with_one_track();
        let (release, stuck) = std::sync::mpsc::channel::<()>();
        library.inner.spawn("stuck", move |_| {
            let _ = stuck.recv();
        });
        let started = std::time::Instant::now();
        library.shutdown();
        let waited = started.elapsed();
        assert!(waited >= SHUTDOWN_PATIENCE, "it was waited for: {waited:?}");
        assert!(
            waited < SHUTDOWN_PATIENCE + Duration::from_secs(2),
            "{waited:?}"
        );
        drop(release);
    }
    /// The music folders are asked at the same time, and one that does not answer is
    /// not waited for beyond the time they have together.
    #[test]
    fn a_folder_that_does_not_answer_does_not_hold_up_the_others() {
        let paths: Vec<PathBuf> = ["a", "slow", "b"].iter().map(PathBuf::from).collect();
        let started = std::time::Instant::now();
        let answers = asked_together(&paths, Duration::from_millis(300), |path| {
            if path == Path::new("slow") {
                std::thread::sleep(Duration::from_secs(5));
            }
            path.to_string_lossy().to_uppercase()
        });
        assert_eq!(
            answers,
            [Some("A".to_string()), None, Some("B".to_string())]
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        // All of them answer: nothing is waited for.
        let started = std::time::Instant::now();
        let answers = asked_together(&paths, Duration::from_secs(30), Path::to_path_buf);
        assert_eq!(answers.len(), 3);
        assert!(answers.iter().all(Option::is_some));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(asked_together(&[], Duration::from_secs(30), |_| ()).is_empty());
    }
}
