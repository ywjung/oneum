//! SQLite layer of the library: schema, migrations, corruption recovery and every query.
//!
//! The database is the source of truth; `crate::library` keeps an in-memory copy of the
//! track table and talks to this type through a mutex. All user-provided values are bound
//! as parameters. SQL text is only ever assembled from the constant column lists below.
use crate::model::{CheckVerdict, FileCheck, ReplayGainTags, Track, TrackId};
use rusqlite::{Connection, ErrorCode, OptionalExtension, Row, params};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// File name of the library database inside the data directory.
pub const DB_FILE_NAME: &str = "library.db";

/// Number of history rows kept; older plays are trimmed when new ones are recorded.
const HISTORY_KEEP_ROWS: i64 = 20_000;

/// Ordered migration list. Entry `i` upgrades the schema from version `i` to `i + 1`;
/// the current version is stored in `PRAGMA user_version`.
const MIGRATIONS: &[&str] = &[
    SCHEMA_V1, SCHEMA_V2, SCHEMA_V3, SCHEMA_V4, SCHEMA_V5, SCHEMA_V6, SCHEMA_V7,
];

const SCHEMA_V1: &str = "
CREATE TABLE roots (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    path          BLOB NOT NULL UNIQUE,
    last_scan_at  INTEGER
);
CREATE TABLE tracks (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    path           BLOB NOT NULL UNIQUE,
    root_id        INTEGER REFERENCES roots(id) ON DELETE CASCADE,
    available      INTEGER NOT NULL DEFAULT 1,
    title          TEXT NOT NULL,
    artist         TEXT NOT NULL,
    album_artist   TEXT NOT NULL,
    album          TEXT NOT NULL,
    album_key      INTEGER NOT NULL,
    compilation    INTEGER NOT NULL DEFAULT 0,
    disc_no        INTEGER,
    track_no       INTEGER,
    year           INTEGER,
    genre          TEXT NOT NULL,
    duration_ms    INTEGER NOT NULL,
    codec          TEXT NOT NULL,
    container      TEXT NOT NULL,
    sample_rate    INTEGER NOT NULL,
    channels       INTEGER NOT NULL,
    bit_depth      INTEGER,
    bitrate_kbps   INTEGER,
    file_size      INTEGER NOT NULL,
    mtime_ns       INTEGER NOT NULL,
    rg_track_gain  REAL,
    rg_track_peak  REAL,
    rg_album_gain  REAL,
    rg_album_peak  REAL,
    art_id         TEXT,
    favorite       INTEGER NOT NULL DEFAULT 0,
    play_count     INTEGER NOT NULL DEFAULT 0,
    last_played_at INTEGER,
    added_at       INTEGER NOT NULL
);
CREATE INDEX tracks_root ON tracks(root_id);
CREATE TABLE playlists (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TABLE playlist_items (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    track_id    INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    position    INTEGER NOT NULL
);
CREATE INDEX playlist_items_order ON playlist_items(playlist_id, position);
CREATE INDEX playlist_items_track ON playlist_items(track_id);
CREATE TABLE history (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    track_id  INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    played_at INTEGER NOT NULL
);
CREATE INDEX history_track ON history(track_id);
CREATE TABLE queue (
    position INTEGER PRIMARY KEY,
    item_id  INTEGER NOT NULL,
    track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE
);
CREATE INDEX queue_track ON queue(track_id);
CREATE TABLE kv (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;
";

/// v2:
/// - `tracks.art_from_folder` remembers whether a track's artwork came from an image
///   file in its folder (rather than from the audio file itself), so that adding,
///   replacing or removing that image can update the track without re-reading the
///   audio file.
/// - `roots.unconfirmed_after` is set while a folder's path exists but none of its known
///   files were found there (a stale mount point, a recreated directory). It holds the
///   highest track id from before that was noticed: only tracks up to that id can later
///   prove that the path leads to the known folder again.
const SCHEMA_V2: &str = "
ALTER TABLE tracks ADD COLUMN art_from_folder INTEGER NOT NULL DEFAULT 0;
ALTER TABLE roots ADD COLUMN unconfirmed_after INTEGER;
";

/// Version 3: leveling values the app measured itself for files without ReplayGain tags.
/// A row belongs to one state of the file (`of_mtime_ns`, `of_file_size`): when the file
/// changes, the row no longer matches and is not read. The column names differ from
/// those of `tracks` so that the two can be joined without qualifying anything.
const SCHEMA_V3: &str = "
CREATE TABLE loudness (
    track_id      INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
    of_mtime_ns   INTEGER NOT NULL,
    of_file_size  INTEGER NOT NULL,
    m_track_gain  REAL NOT NULL,
    m_track_peak  REAL NOT NULL,
    m_album_gain  REAL,
    m_album_peak  REAL
);
";

/// What another player knew about a song: `(track, plays, last played, added, loved)`
/// (times in Unix seconds).
pub type ListeningRow = (TrackId, u32, Option<i64>, Option<i64>, bool, u8);

const KV_UPSERT_SQL: &str = "INSERT INTO kv (key, value) VALUES (?1, ?2) \
                             ON CONFLICT(key) DO UPDATE SET value = excluded.value";

/// Version 4:
/// - `resume_points`: where the user left off in a long track (see the player's
///   "resume long tracks").
/// - `checks`: the results of the file check (integrity, dynamic range, true peak),
///   like `loudness` tied to one state of the file.
const SCHEMA_V4: &str = "
CREATE TABLE resume_points (
    track_id     INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
    position_ms  INTEGER NOT NULL,
    saved_at     INTEGER NOT NULL
);
CREATE TABLE checks (
    track_id     INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
    of_mtime_ns  INTEGER NOT NULL,
    of_file_size INTEGER NOT NULL,
    c_verdict    INTEGER NOT NULL,
    c_dr         REAL,
    c_true_peak  REAL,
    checked_at   INTEGER NOT NULL
);
";

/// Files whose songs the user removed from the library while the files stayed where
/// they are: a scan of their folder passes them by, so that what was removed stays
/// removed (see `Library::remove_tracks`). A track of a CUE sheet is here under the
/// path the library gives it.
const SCHEMA_V6: &str = "
CREATE TABLE removed_files (
    path BLOB PRIMARY KEY
) WITHOUT ROWID;
";

/// The listener's rating of a song: 1 to 5 stars, 0 for none.
const SCHEMA_V7: &str = "
ALTER TABLE tracks ADD COLUMN rating INTEGER NOT NULL DEFAULT 0;
";

/// Tracks of media servers on the network (see `library::remote`). They are not part of
/// the library: they are kept so that the play queue can name them and be restored.
/// Their queue entries have a table of their own, so that every entry still refers to
/// a row that exists (`queue.track_id` refers to `tracks`).
const SCHEMA_V5: &str = "
CREATE TABLE remote_tracks (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    url          TEXT NOT NULL UNIQUE,
    server       TEXT NOT NULL,
    title        TEXT NOT NULL,
    artist       TEXT NOT NULL,
    album_artist TEXT NOT NULL,
    album        TEXT NOT NULL,
    track_no     INTEGER,
    genre        TEXT NOT NULL,
    duration_ms  INTEGER NOT NULL,
    container    TEXT NOT NULL,
    sample_rate  INTEGER NOT NULL,
    channels     INTEGER NOT NULL,
    bit_depth    INTEGER,
    file_size    INTEGER NOT NULL,
    art_url      TEXT,
    art_id       TEXT,
    added_at     INTEGER NOT NULL
);
CREATE TABLE remote_queue (
    position  INTEGER PRIMARY KEY,
    item_id   INTEGER NOT NULL,
    remote_id INTEGER NOT NULL REFERENCES remote_tracks(id) ON DELETE CASCADE
);
CREATE INDEX remote_queue_track ON remote_queue(remote_id);
";

/// A track of a media server on the network, as stored (see `library::remote`).
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteRow {
    /// The row; the track's id in the app is its negative.
    pub id: i64,
    pub url: String,
    /// The server's name, for showing where the track is.
    pub server: String,
    pub title: String,
    pub artist: String,
    pub album_artist: String,
    pub album: String,
    pub track_no: Option<u32>,
    pub genre: String,
    pub duration_ms: u64,
    pub container: String,
    pub sample_rate: u32,
    pub channels: u8,
    pub bit_depth: Option<u8>,
    pub file_size: u64,
    pub art_url: Option<String>,
    pub art_id: Option<String>,
    pub added_at: i64,
}

/// Columns filled from the file on disk (re-written on every rescan of a changed file).
const SCAN_COLS: [&str; 27] = [
    "root_id",
    "available",
    "title",
    "artist",
    "album_artist",
    "album",
    "album_key",
    "compilation",
    "disc_no",
    "track_no",
    "year",
    "genre",
    "duration_ms",
    "codec",
    "container",
    "sample_rate",
    "channels",
    "bit_depth",
    "bitrate_kbps",
    "file_size",
    "mtime_ns",
    "rg_track_gain",
    "rg_track_peak",
    "rg_album_gain",
    "rg_album_peak",
    "art_id",
    "art_from_folder",
];

/// Columns owned by the user's activity; only set when a row is first inserted.
const USER_COLS: [&str; 4] = ["favorite", "play_count", "last_played_at", "added_at"];

static SELECT_TRACKS_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "SELECT id, path, {}, {}, m_track_gain, m_track_peak, m_album_gain, m_album_peak, \
         c_verdict, c_dr, c_true_peak, rating \
         FROM tracks \
         LEFT JOIN loudness ON loudness.track_id = tracks.id \
         AND loudness.of_mtime_ns = tracks.mtime_ns AND loudness.of_file_size = tracks.file_size \
         LEFT JOIN checks ON checks.track_id = tracks.id \
         AND checks.of_mtime_ns = tracks.mtime_ns AND checks.of_file_size = tracks.file_size",
        SCAN_COLS.join(", "),
        USER_COLS.join(", ")
    )
});

static UPSERT_TRACK_SQL: LazyLock<String> = LazyLock::new(|| {
    let placeholders: Vec<String> = (1..=1 + SCAN_COLS.len() + USER_COLS.len())
        .map(|i| format!("?{i}"))
        .collect();
    let updates: Vec<String> = SCAN_COLS
        .iter()
        .map(|c| format!("{c} = excluded.{c}"))
        .collect();
    format!(
        "INSERT INTO tracks (path, {}, {}) VALUES ({}) \
         ON CONFLICT(path) DO UPDATE SET {} RETURNING id",
        SCAN_COLS.join(", "),
        USER_COLS.join(", "),
        placeholders.join(", "),
        updates.join(", ")
    )
});

static RELINK_TRACK_SQL: LazyLock<String> = LazyLock::new(|| {
    // ?1 = id of the row to update, ?2 = new path, ?3.. = file-derived columns.
    let cols = SCAN_COLS.join(", ");
    format!(
        "UPDATE tracks SET (path, {cols}) = (?2, {}) WHERE id = ?1",
        SCAN_COLS
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 3))
            .collect::<Vec<_>>()
            .join(", ")
    )
});

/// Errors from the storage layer.
#[derive(Debug)]
pub enum DbError {
    Sqlite(rusqlite::Error),
    Io(io::Error),
    /// The file was written by a newer version of the app; it is left untouched.
    NewerSchema {
        found: u32,
        supported: u32,
    },
    /// A migration failed. `restored` tells whether the upgrade was rolled back, leaving
    /// the database exactly as it was before (a backup copy is kept either way).
    MigrationFailed {
        from: u32,
        to: u32,
        message: String,
        restored: bool,
    },
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbError::Sqlite(e) => write!(f, "database error: {e}"),
            DbError::Io(e) => write!(f, "database file error: {e}"),
            DbError::NewerSchema { found, supported } => write!(
                f,
                "the library database (schema v{found}) was created by a newer version of the app (this version supports up to v{supported})"
            ),
            DbError::MigrationFailed {
                from,
                to,
                message,
                restored,
            } => write!(
                f,
                "upgrading the library database from v{from} to v{to} failed ({message}); {}",
                if *restored {
                    "the database was left as it was"
                } else {
                    "the upgrade could not be rolled back; a copy of the previous database is kept next to it"
                }
            ),
        }
    }
}

impl std::error::Error for DbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DbError::Sqlite(e) => Some(e),
            DbError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for DbError {
    fn from(e: rusqlite::Error) -> Self {
        DbError::Sqlite(e)
    }
}

impl From<io::Error> for DbError {
    fn from(e: io::Error) -> Self {
        DbError::Io(e)
    }
}

/// A row of the `roots` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootRow {
    pub id: i64,
    pub path: PathBuf,
    pub last_scan_at: Option<i64>,
    /// See `SCHEMA_V2`: set while the folder's identity is in doubt.
    pub unconfirmed_after: Option<TrackId>,
}

/// A track row: the shared model type plus columns that are not part of it.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredTrack {
    pub track: Track,
    /// The file's compilation flag (input of the album identity; kept so album keys can be
    /// recomputed without re-reading files).
    pub compilation: bool,
    /// `track.art_id` refers to an image file in the track's folder, not to a picture
    /// embedded in the audio file.
    pub art_from_folder: bool,
}

/// Result of [`Db::open`].
pub struct Opened {
    pub db: Db,
    /// Where an unusable database file was moved to, if that happened.
    pub recovered_corrupt_db: Option<PathBuf>,
    /// Schema version the existing database was upgraded from, if it was upgraded.
    pub migrated_from: Option<u32>,
}

/// Connection to the library database.
pub struct Db {
    conn: Connection,
}

type SqlResult<T> = rusqlite::Result<T>;

/// A track whose album key is made of its path: its id, its path, the album's title,
/// whether it is of a compilation, and the key it has.
pub type FolderKeyed = (TrackId, PathBuf, String, bool, u64);

impl Db {
    /// Opens (creating or upgrading as needed) the database at `path`.
    pub fn open(path: &Path) -> Result<Opened, DbError> {
        Self::open_with_migrations(path, MIGRATIONS)
    }

    /// Like [`Db::open`] with an explicit migration list (used by tests to simulate
    /// future schema versions and failing upgrades).
    pub fn open_with_migrations(path: &Path, migrations: &[&str]) -> Result<Opened, DbError> {
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            fs::create_dir_all(dir)?;
        }

        let mut recovered_corrupt_db = None;
        let conn = if path.exists() {
            match open_checked(path) {
                Ok(conn) => conn,
                // Only a file that SQLite itself calls damaged is replaced. Anything else
                // (disk full, I/O error, permissions, a lock) says nothing about the
                // data: the file is left exactly as it is and the error is reported.
                Err(err) if is_corruption(&err) => {
                    tracing::warn!(error = %err, "library database is corrupt; moving it aside");
                    recovered_corrupt_db = Some(move_aside(path)?);
                    open_checked(path)?
                }
                Err(err) => return Err(DbError::Sqlite(err)),
            }
        } else {
            open_checked(path)?
        };

        let version: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let latest = u32::try_from(migrations.len()).unwrap_or(u32::MAX);
        if version > latest {
            return Err(DbError::NewerSchema {
                found: version,
                supported: latest,
            });
        }

        let mut migrated_from = None;
        if version < latest {
            let fresh = version == 0 && !has_user_tables(&conn)?;
            // The upgrade runs as one transaction that holds the write lock from its
            // first statement: no other connection (a second process, the library tool)
            // can write while it runs, and if any step fails the file is exactly as it
            // was. Nothing is ever restored by replacing files, which would pull the
            // database out from under a connection that has it open.
            // (A dropped connection rolls back whatever is open, so the early returns
            // below leave the file as it was too.)
            if fresh {
                conn.execute_batch("BEGIN IMMEDIATE")?;
            } else {
                backup_and_lock(&conn, path, version)?;
            }
            // Re-read under the lock: another process may have upgraded meanwhile.
            let locked_version: u32 =
                conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
            if locked_version > latest {
                return Err(DbError::NewerSchema {
                    found: locked_version,
                    supported: latest,
                });
            }
            for (index, sql) in migrations.iter().enumerate().skip(locked_version as usize) {
                let target = u32::try_from(index + 1).unwrap_or(u32::MAX);
                if let Err(err) = apply_migration(&conn, sql, target) {
                    // Some errors (a full disk, an I/O error) make SQLite roll back by
                    // itself; then there is nothing left to roll back.
                    let restored = conn.is_autocommit()
                        || {
                            match conn.execute_batch("ROLLBACK") {
                                Ok(()) => true,
                                Err(rollback_err) => {
                                    tracing::error!(error = %rollback_err, "could not roll back the database upgrade");
                                    false
                                }
                            }
                        };
                    return Err(DbError::MigrationFailed {
                        from: locked_version,
                        to: target,
                        message: err.to_string(),
                        restored,
                    });
                }
            }
            conn.execute_batch("COMMIT")?;
            if !fresh && locked_version < latest {
                migrated_from = Some(locked_version);
                tracing::info!(
                    from = locked_version,
                    to = latest,
                    "library database upgraded"
                );
            }
        }

        // (Not when the database had to be replaced: its copies are then all there
        // is of the library.)
        if recovered_corrupt_db.is_none() {
            prune_backups(path);
        }
        Ok(Opened {
            db: Db { conn },
            recovered_corrupt_db,
            migrated_from,
        })
    }

    /// Current schema version.
    pub fn schema_version(&self) -> SqlResult<u32> {
        self.conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
    }

    /// Flushes the write-ahead log into the main database file.
    pub fn checkpoint(&self) -> SqlResult<()> {
        self.conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
    }

    // ---- roots ---------------------------------------------------------------------

    pub fn roots(&self) -> SqlResult<Vec<RootRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, path, last_scan_at, unconfirmed_after FROM roots ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            Ok(RootRow {
                id: row.get(0)?,
                path: blob_to_path(row.get(1)?),
                last_scan_at: row.get(2)?,
                unconfirmed_after: row.get(3)?,
            })
        })?;
        rows.collect()
    }

    pub fn insert_root(&self, path: &Path) -> SqlResult<i64> {
        self.conn.execute(
            "INSERT INTO roots (path) VALUES (?1)",
            params![path_to_blob(path)],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Deletes a root and (by cascade) its tracks. Returns false if it did not exist.
    /// Deletes a music folder together with its tracks (and, with them, what refers to
    /// them: playlist entries, history, measurements). All of it or nothing: if any
    /// part fails, the folder and every one of its tracks are still there, favourites
    /// and playlist entries included.
    pub fn delete_root_with_tracks(&mut self, id: i64) -> SqlResult<bool> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM tracks WHERE root_id = ?1", params![id])?;
        let n = tx.execute("DELETE FROM roots WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(n > 0)
    }

    pub fn set_root_unconfirmed(&self, id: i64, after: Option<TrackId>) -> SqlResult<()> {
        self.conn.execute(
            "UPDATE roots SET unconfirmed_after = ?2 WHERE id = ?1",
            params![id, after],
        )?;
        Ok(())
    }

    pub fn set_root_scanned(&self, id: i64, unix_secs: i64) -> SqlResult<()> {
        self.conn.execute(
            "UPDATE roots SET last_scan_at = ?2 WHERE id = ?1",
            params![id, unix_secs],
        )?;
        Ok(())
    }

    // ---- tracks --------------------------------------------------------------------

    pub fn load_tracks(&self) -> SqlResult<Vec<StoredTrack>> {
        let mut stmt = self.conn.prepare(&SELECT_TRACKS_SQL)?;
        let rows = stmt.query_map([], row_to_track)?;
        rows.collect()
    }

    pub fn get_track(&self, id: TrackId) -> SqlResult<Option<StoredTrack>> {
        let sql = format!("{} WHERE id = ?1", *SELECT_TRACKS_SQL);
        self.conn
            .query_row(&sql, params![id], row_to_track)
            .optional()
    }

    /// Inserts the given tracks, or refreshes the file-derived columns of existing rows
    /// with the same path (keeping id, favourite, play counts and date added). `track.id`
    /// is ignored. Returns the row ids in input order. All rows go in one transaction.
    pub fn upsert_tracks(&mut self, rows: &[StoredTrack]) -> SqlResult<Vec<TrackId>> {
        let tx = self.conn.transaction()?;
        let mut ids = Vec::with_capacity(rows.len());
        {
            let mut stmt = tx.prepare_cached(&UPSERT_TRACK_SQL)?;
            for row in rows {
                let t = &row.track;
                let id: TrackId = stmt.query_row(
                    params![
                        path_to_blob(&t.path),
                        t.root_id,
                        t.available,
                        t.title,
                        t.artist,
                        t.album_artist,
                        t.album,
                        t.album_key.cast_signed(),
                        row.compilation,
                        t.disc_no,
                        t.track_no,
                        t.year,
                        t.genre,
                        to_i64(t.duration_ms),
                        t.codec,
                        t.container,
                        t.sample_rate,
                        t.channels,
                        t.bit_depth,
                        t.bitrate_kbps,
                        to_i64(t.file_size),
                        t.mtime_ns,
                        t.replaygain.track_gain_db.map(f64::from),
                        t.replaygain.track_peak.map(f64::from),
                        t.replaygain.album_gain_db.map(f64::from),
                        t.replaygain.album_peak.map(f64::from),
                        t.art_id,
                        row.art_from_folder,
                        t.favorite,
                        t.play_count,
                        t.last_played_at,
                        t.added_at,
                    ],
                    |r| r.get(0),
                )?;
                ids.push(id);
            }
        }
        tx.commit()?;
        Ok(ids)
    }

    /// Deletes track rows; playlist items, history and queue rows follow by cascade.
    pub fn delete_tracks(&mut self, ids: &[TrackId]) -> SqlResult<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached("DELETE FROM tracks WHERE id = ?1")?;
            for id in ids {
                stmt.execute(params![id])?;
            }
        }
        tx.commit()
    }

    /// Deletes track rows as [`Self::delete_tracks`] does and writes down the files
    /// they were of (`removed_files`), all or nothing: a song that was removed and not
    /// written down would be back with the next scan of its folder.
    pub fn delete_tracks_remembering(&mut self, tracks: &[(TrackId, PathBuf)]) -> SqlResult<()> {
        if tracks.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        {
            let mut remember =
                tx.prepare_cached("INSERT OR IGNORE INTO removed_files (path) VALUES (?1)")?;
            let mut delete = tx.prepare_cached("DELETE FROM tracks WHERE id = ?1")?;
            for (id, path) in tracks {
                remember.execute(params![path_to_blob(path).as_ref()])?;
                delete.execute(params![id])?;
            }
        }
        tx.commit()
    }

    /// The files whose songs were removed from the library by the user.
    pub fn removed_files(&self) -> SqlResult<Vec<PathBuf>> {
        let mut stmt = self.conn.prepare("SELECT path FROM removed_files")?;
        let rows = stmt.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
        rows.map(|blob| blob.map(blob_to_path)).collect()
    }

    /// Takes files off the list of removed ones (they were added again, are gone, or
    /// their folder is no music folder any more).
    pub fn forget_removed_files(&mut self, paths: &[PathBuf]) -> SqlResult<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached("DELETE FROM removed_files WHERE path = ?1")?;
            for path in paths {
                stmt.execute(params![path_to_blob(path).as_ref()])?;
            }
        }
        tx.commit()
    }

    /// Deletes those of the given tracks that the user does not refer to: not a
    /// favourite and not in any playlist. Returns the ids that were deleted. The check
    /// and the delete are one statement, so a track that became referenced in the
    /// meantime is never removed.
    pub fn delete_unreferenced_tracks(&mut self, ids: &[TrackId]) -> SqlResult<Vec<TrackId>> {
        let mut deleted = Vec::new();
        if ids.is_empty() {
            return Ok(deleted);
        }
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "DELETE FROM tracks WHERE id = ?1 AND favorite = 0 AND rating = 0 \
                 AND NOT EXISTS (SELECT 1 FROM playlist_items WHERE track_id = ?1)",
            )?;
            for &id in ids {
                if stmt.execute(params![id])? > 0 {
                    deleted.push(id);
                }
            }
        }
        tx.commit()?;
        Ok(deleted)
    }

    pub fn set_tracks_available(&mut self, ids: &[TrackId], available: bool) -> SqlResult<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached("UPDATE tracks SET available = ?2 WHERE id = ?1")?;
            for id in ids {
                stmt.execute(params![id, available])?;
            }
        }
        tx.commit()
    }

    pub fn set_root_available(&self, root_id: i64, available: bool) -> SqlResult<()> {
        self.conn.execute(
            "UPDATE tracks SET available = ?2 WHERE root_id = ?1",
            params![root_id, available],
        )?;
        Ok(())
    }

    /// Marks unchanged files as present and assigns them to `root_id` (used when a file
    /// that was registered individually turns out to live under a music folder, and when
    /// a previously missing file is back).
    pub fn adopt_tracks(&mut self, ids: &[TrackId], root_id: Option<i64>) -> SqlResult<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        {
            let mut stmt =
                tx.prepare_cached("UPDATE tracks SET available = 1, root_id = ?2 WHERE id = ?1")?;
            for id in ids {
                stmt.execute(params![id, root_id])?;
            }
        }
        tx.commit()
    }

    /// Changes the stored path (and the album identity that depends on it) of a track
    /// whose file is the same, e.g. when an individually registered file turns out to
    /// live under a music folder that spells the path differently. Returns false if the
    /// track does not exist or another row already has that path.
    pub fn move_track(&self, id: TrackId, path: &Path, album_key: u64) -> SqlResult<bool> {
        let n = self.conn.execute(
            "UPDATE OR IGNORE tracks SET path = ?2, album_key = ?3 WHERE id = ?1",
            params![id, path_to_blob(path), album_key.cast_signed()],
        )?;
        Ok(n > 0)
    }

    /// Sets the artwork of tracks: `(track id, artwork id, artwork is a folder image)`.
    pub fn set_tracks_art(&mut self, changes: &[(TrackId, Option<String>, bool)]) -> SqlResult<()> {
        if changes.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "UPDATE tracks SET art_id = ?2, art_from_folder = ?3 WHERE id = ?1",
            )?;
            for (id, art_id, from_folder) in changes {
                stmt.execute(params![id, art_id, from_folder])?;
            }
        }
        tx.commit()
    }

    /// The tracks of a music folder that hold something of the listener's: a
    /// favourite, a rating, plays, a place in a playlist or in the queue.
    pub fn marked_tracks_of_root(&self, root_id: i64) -> SqlResult<Vec<TrackId>> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM tracks WHERE root_id = ?1 AND (favorite != 0 OR rating != 0                  OR play_count > 0                  OR EXISTS (SELECT 1 FROM playlist_items WHERE track_id = tracks.id)                  OR EXISTS (SELECT 1 FROM queue WHERE track_id = tracks.id))",
        )?;
        let ids = stmt.query_map(params![root_id], |row| row.get(0))?;
        ids.collect()
    }

    /// Makes row `keep` point at the file described by row `take` (path and every
    /// file-derived column) and deletes row `take`. Id, favourite, play counts, history
    /// and playlist membership of `keep` are preserved, and the playlist entries, queue
    /// entries and history of `take` are moved over to `keep` instead of being lost with
    /// it. Returns the updated row.
    pub fn relink_track(&mut self, keep: TrackId, take: TrackId) -> SqlResult<Option<StoredTrack>> {
        let Some(source) = self.get_track(take)? else {
            return Ok(None);
        };
        let Some(target) = self.get_track(keep)? else {
            return Ok(None);
        };
        let tx = self.conn.transaction()?;
        for table in ["playlist_items", "queue", "history"] {
            tx.execute(
                &format!("UPDATE {table} SET track_id = ?1 WHERE track_id = ?2"),
                params![keep, take],
            )?;
        }
        tx.execute("DELETE FROM tracks WHERE id = ?1", params![take])?;
        let s = &source.track;
        tx.execute(
            &RELINK_TRACK_SQL,
            params![
                keep,
                path_to_blob(&s.path),
                s.root_id,
                s.available,
                s.title,
                s.artist,
                s.album_artist,
                s.album,
                s.album_key.cast_signed(),
                source.compilation,
                s.disc_no,
                s.track_no,
                s.year,
                s.genre,
                to_i64(s.duration_ms),
                s.codec,
                s.container,
                s.sample_rate,
                s.channels,
                s.bit_depth,
                s.bitrate_kbps,
                to_i64(s.file_size),
                s.mtime_ns,
                s.replaygain.track_gain_db.map(f64::from),
                s.replaygain.track_peak.map(f64::from),
                s.replaygain.album_gain_db.map(f64::from),
                s.replaygain.album_peak.map(f64::from),
                s.art_id,
                source.art_from_folder,
            ],
        )?;
        // Whatever the user did with the short-lived row carries over.
        let last_played = match (target.track.last_played_at, s.last_played_at) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        tx.execute(
            "UPDATE tracks SET favorite = ?2, play_count = ?3, last_played_at = ?4, \
             rating = CASE WHEN rating = 0 THEN ?5 ELSE rating END WHERE id = ?1",
            params![
                keep,
                target.track.favorite || s.favorite,
                target.track.play_count.saturating_add(s.play_count),
                last_played,
                s.rating,
            ],
        )?;
        tx.commit()?;
        self.get_track(keep)
    }

    /// Row `keep` takes over what the listener did with row `gone` (favourite, rating,
    /// plays, the earlier of the two dates added, its places in playlists, in the
    /// queue and in the history), and row `gone` is deleted. `keep` stays the file it
    /// is, under its own id: for a track whose file is gone while the same file is in
    /// the library as another track, which may be the one that is playing. Returns
    /// the row that is kept.
    pub fn absorb_track(&mut self, keep: TrackId, gone: TrackId) -> SqlResult<Option<StoredTrack>> {
        let Some(source) = self.get_track(gone)? else {
            return Ok(None);
        };
        let Some(target) = self.get_track(keep)? else {
            return Ok(None);
        };
        let tx = self.conn.transaction()?;
        for table in ["playlist_items", "queue", "history"] {
            tx.execute(
                &format!("UPDATE {table} SET track_id = ?1 WHERE track_id = ?2"),
                params![keep, gone],
            )?;
        }
        tx.execute("DELETE FROM tracks WHERE id = ?1", params![gone])?;
        let s = &source.track;
        let last_played = match (target.track.last_played_at, s.last_played_at) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        tx.execute(
            "UPDATE tracks SET favorite = ?2, play_count = ?3, last_played_at = ?4, \
             rating = CASE WHEN rating = 0 THEN ?5 ELSE rating END, \
             added_at = MIN(added_at, ?6) WHERE id = ?1",
            params![
                keep,
                target.track.favorite || s.favorite,
                target.track.play_count.saturating_add(s.play_count),
                last_played,
                s.rating,
                s.added_at,
            ],
        )?;
        tx.commit()?;
        self.get_track(keep)
    }

    /// Stores what was measured for tracks, replacing earlier measurements. Each entry
    /// names the state of the file it was measured from. Tracks that no longer exist
    /// are skipped.
    pub fn set_loudness(&mut self, rows: &[(TrackId, i64, u64, ReplayGainTags)]) -> SqlResult<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR REPLACE INTO loudness \
                 (track_id, of_mtime_ns, of_file_size, m_track_gain, m_track_peak, \
                  m_album_gain, m_album_peak) \
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7 WHERE EXISTS (SELECT 1 FROM tracks WHERE id = ?1)",
            )?;
            for (id, mtime_ns, file_size, values) in rows {
                stmt.execute(params![
                    id,
                    mtime_ns,
                    file_size.cast_signed(),
                    f64::from(values.track_gain_db.unwrap_or(0.0)),
                    f64::from(values.track_peak.unwrap_or(0.0)),
                    values.album_gain_db.map(f64::from),
                    values.album_peak.map(f64::from),
                ])?;
            }
        }
        tx.commit()
    }

    /// Forgets everything that was measured.
    /// Stores file check results, each for the state of the file it was made of. Rows of
    /// tracks that no longer exist are skipped.
    pub fn set_checks(
        &mut self,
        rows: &[(TrackId, i64, u64, FileCheck)],
        unix_secs: i64,
    ) -> SqlResult<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR REPLACE INTO checks \
                 (track_id, of_mtime_ns, of_file_size, c_verdict, c_dr, c_true_peak, checked_at) \
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7 WHERE EXISTS (SELECT 1 FROM tracks WHERE id = ?1)",
            )?;
            for (id, mtime_ns, file_size, check) in rows {
                stmt.execute(params![
                    id,
                    mtime_ns,
                    file_size.cast_signed(),
                    check.verdict.code(),
                    check.dr.map(f64::from),
                    check.true_peak.map(f64::from),
                    unix_secs,
                ])?;
            }
        }
        tx.commit()
    }

    pub fn clear_checks(&self) -> SqlResult<usize> {
        self.conn.execute("DELETE FROM checks", [])
    }

    pub fn clear_loudness(&self) -> SqlResult<usize> {
        self.conn.execute("DELETE FROM loudness", [])
    }

    /// Merges what another player knew about songs, in one transaction: the higher
    /// play count, the later last play, the earlier date added, loved songs become
    /// favourites, and a rating is taken for a song that has none here. Rows are
    /// `(track, plays, last played, added, loved, rating)`.
    pub fn merge_listening(&mut self, rows: &[ListeningRow]) -> SqlResult<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "UPDATE tracks SET \
                 play_count = MAX(play_count, ?2), \
                 last_played_at = CASE WHEN ?3 IS NULL THEN last_played_at \
                     WHEN last_played_at IS NULL OR ?3 > last_played_at THEN ?3 \
                     ELSE last_played_at END, \
                 added_at = CASE WHEN ?4 IS NOT NULL AND ?4 < added_at THEN ?4 ELSE added_at END, \
                 favorite = favorite OR ?5, \
                 rating = CASE WHEN rating = 0 THEN ?6 ELSE rating END \
                 WHERE id = ?1",
            )?;
            for (id, plays, last, added, loved, rating) in rows {
                stmt.execute(params![id, plays, last, added, loved, rating.min(&5)])?;
            }
        }
        tx.commit()
    }

    /// Sets the rating of the tracks: 1 to 5 stars, 0 for none.
    pub fn set_rating(&mut self, ids: &[TrackId], rating: u8) -> SqlResult<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached("UPDATE tracks SET rating = ?2 WHERE id = ?1")?;
            for id in ids {
                stmt.execute(params![id, rating.min(5)])?;
            }
        }
        tx.commit()
    }

    pub fn set_favorite(&mut self, ids: &[TrackId], favorite: bool) -> SqlResult<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached("UPDATE tracks SET favorite = ?2 WHERE id = ?1")?;
            for id in ids {
                stmt.execute(params![id, favorite])?;
            }
        }
        tx.commit()
    }

    /// Adds a history row and bumps the play counters. Returns false if the track does
    /// not exist.
    pub fn record_play(&mut self, id: TrackId, unix_secs: i64) -> SqlResult<bool> {
        let tx = self.conn.transaction()?;
        let updated = tx.execute(
            "UPDATE tracks SET play_count = play_count + 1, last_played_at = ?2 WHERE id = ?1",
            params![id, unix_secs],
        )?;
        if updated == 0 {
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO history (track_id, played_at) VALUES (?1, ?2)",
            params![id, unix_secs],
        )?;
        tx.execute(
            "DELETE FROM history WHERE id <= (SELECT MAX(id) FROM history) - ?1",
            params![HISTORY_KEEP_ROWS],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Adds plays that another record knew of (`(track, unix seconds)`; the app's own
    /// library data from another device) to the history, each only if it is not there
    /// yet. The rows are then in the order of time again: what was played last is told
    /// by the order of the rows ([`Self::recent_plays`]), and a play of last year that
    /// came in today must not count as the latest. The oldest rows beyond what the
    /// history keeps are dropped, as [`Self::record_play`] drops them. Returns how many
    /// plays were added.
    pub fn merge_plays(&mut self, plays: &[(TrackId, i64)]) -> SqlResult<usize> {
        let tx = self.conn.transaction()?;
        let mut all: Vec<(TrackId, i64)> = {
            let mut stmt = tx.prepare("SELECT track_id, played_at FROM history ORDER BY id")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<SqlResult<_>>()?
        };
        let mut known: std::collections::HashSet<(TrackId, i64)> = all.iter().copied().collect();
        let before = all.len();
        all.extend(plays.iter().copied().filter(|play| known.insert(*play)));
        let added = all.len() - before;
        if added == 0 {
            return Ok(0);
        }
        // (Stable: plays of the same second stay in the order they were in.)
        all.sort_by_key(|&(_, at)| at);
        let keep = usize::try_from(HISTORY_KEEP_ROWS).unwrap_or(usize::MAX);
        let dropped = all.len().saturating_sub(keep);
        tx.execute("DELETE FROM history", [])?;
        {
            // (A song that was taken out of the library since the plays were matched
            // has no plays any more: a scan may run beside this.)
            let mut stmt = tx.prepare(
                "INSERT INTO history (track_id, played_at)
                 SELECT ?1, ?2 WHERE EXISTS (SELECT 1 FROM tracks WHERE id = ?1)",
            )?;
            for (id, at) in &all[dropped..] {
                stmt.execute(params![id, at])?;
            }
        }
        tx.commit()?;
        Ok(added)
    }

    /// `(track, unix seconds)` of every play at or after `since`, oldest first.
    pub fn plays_since(&self, since: i64) -> SqlResult<Vec<(TrackId, i64)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT track_id, played_at FROM history WHERE played_at >= ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![since], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    }

    /// Distinct recently played tracks, most recent first.
    pub fn recent_plays(&self, limit: usize) -> SqlResult<Vec<TrackId>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT track_id FROM history GROUP BY track_id \
             ORDER BY MAX(id) DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![to_i64(limit as u64)], |r| r.get(0))?;
        rows.collect()
    }

    // ---- playlists -----------------------------------------------------------------

    /// `(id, name)` of every playlist in creation order.
    pub fn playlists(&self) -> SqlResult<Vec<(i64, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name FROM playlists ORDER BY id")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    }

    /// `(playlist id, item id, track id)` of every playlist item, grouped by playlist and
    /// ordered by position.
    pub fn all_playlist_items(&self) -> SqlResult<Vec<(i64, i64, TrackId)>> {
        let mut stmt = self.conn.prepare(
            "SELECT playlist_id, id, track_id FROM playlist_items \
             ORDER BY playlist_id, position, id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect()
    }

    pub fn create_playlist(&self, name: &str, unix_secs: i64) -> SqlResult<i64> {
        self.conn.execute(
            "INSERT INTO playlists (name, created_at) VALUES (?1, ?2)",
            params![name, unix_secs],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn rename_playlist(&self, id: i64, name: &str) -> SqlResult<bool> {
        let n = self.conn.execute(
            "UPDATE playlists SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
        Ok(n > 0)
    }

    pub fn delete_playlist(&self, id: i64) -> SqlResult<bool> {
        let n = self
            .conn
            .execute("DELETE FROM playlists WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }

    pub fn playlist_exists(&self, id: i64) -> SqlResult<bool> {
        self.conn
            .query_row("SELECT 1 FROM playlists WHERE id = ?1", params![id], |_| {
                Ok(())
            })
            .optional()
            .map(|found| found.is_some())
    }

    /// Appends tracks to the end of a playlist in the given order. Ids of tracks that do
    /// not exist are skipped. Returns the number of items added.
    pub fn append_playlist_items(&mut self, id: i64, tracks: &[TrackId]) -> SqlResult<usize> {
        let tx = self.conn.transaction()?;
        let added = append_items(&tx, id, tracks)?;
        tx.commit()?;
        Ok(added)
    }

    /// Creates a playlist and fills it in one transaction, so a failure cannot leave an
    /// empty playlist behind. Returns `(playlist id, items added)`.
    pub fn create_playlist_with_items(
        &mut self,
        name: &str,
        unix_secs: i64,
        tracks: &[TrackId],
    ) -> SqlResult<(i64, usize)> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO playlists (name, created_at) VALUES (?1, ?2)",
            params![name, unix_secs],
        )?;
        let id = tx.last_insert_rowid();
        let added = append_items(&tx, id, tracks)?;
        tx.commit()?;
        Ok((id, added))
    }

    /// Gives the tracks that take the place of `old` (a CUE sheet's tracks, for the file
    /// they are cut from) what the user gave it, and deletes it, in one transaction: each
    /// of its places in a playlist holds them instead, in their order; if it was a
    /// favourite, they are, and its rating is theirs where they have none. Returns
    /// whether it was a favourite, and its rating.
    pub fn hand_over_track(&mut self, old: TrackId, heirs: &[TrackId]) -> SqlResult<(bool, u8)> {
        let tx = self.conn.transaction()?;
        let (favorite, rating): (bool, i64) = tx
            .query_row(
                "SELECT favorite, rating FROM tracks WHERE id = ?1",
                params![old],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .unwrap_or((false, 0));
        let rating = rating.clamp(0, 5) as u8;
        if rating > 0 {
            let mut stmt =
                tx.prepare_cached("UPDATE tracks SET rating = ?2 WHERE id = ?1 AND rating = 0")?;
            for heir in heirs {
                stmt.execute(params![heir, rating])?;
            }
        }
        if favorite {
            let mut stmt = tx.prepare_cached("UPDATE tracks SET favorite = 1 WHERE id = ?1")?;
            for heir in heirs {
                stmt.execute(params![heir])?;
            }
        }
        let playlists: Vec<i64> = {
            let mut stmt =
                tx.prepare("SELECT DISTINCT playlist_id FROM playlist_items WHERE track_id = ?1")?;
            stmt.query_map(params![old], |r| r.get(0))?
                .collect::<SqlResult<_>>()?
        };
        for playlist in playlists {
            let items: Vec<TrackId> = {
                let mut stmt = tx.prepare(
                    "SELECT track_id FROM playlist_items WHERE playlist_id = ?1 \
                     ORDER BY position, id",
                )?;
                stmt.query_map(params![playlist], |r| r.get(0))?
                    .collect::<SqlResult<_>>()?
            };
            let handed: Vec<TrackId> = items
                .into_iter()
                .flat_map(|track| {
                    if track == old {
                        heirs.to_vec()
                    } else {
                        vec![track]
                    }
                })
                .collect();
            tx.execute(
                "DELETE FROM playlist_items WHERE playlist_id = ?1",
                params![playlist],
            )?;
            append_items(&tx, playlist, &handed)?;
        }
        tx.execute("DELETE FROM tracks WHERE id = ?1", params![old])?;
        tx.commit()?;
        Ok((favorite, rating))
    }

    /// Replaces everything in a playlist with `tracks` in one transaction: if anything
    /// fails, the playlist keeps what it had. Ids of tracks that do not exist are
    /// skipped. Returns false (and changes nothing) if the playlist does not exist.
    pub fn replace_playlist_items(&mut self, id: i64, tracks: &[TrackId]) -> SqlResult<bool> {
        let tx = self.conn.transaction()?;
        let exists = tx
            .query_row("SELECT 1 FROM playlists WHERE id = ?1", params![id], |_| {
                Ok(())
            })
            .optional()?
            .is_some();
        if !exists {
            return Ok(false);
        }
        tx.execute(
            "DELETE FROM playlist_items WHERE playlist_id = ?1",
            params![id],
        )?;
        append_items(&tx, id, tracks)?;
        tx.commit()?;
        Ok(true)
    }

    pub fn remove_playlist_items(&mut self, id: i64, item_ids: &[i64]) -> SqlResult<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt =
                tx.prepare_cached("DELETE FROM playlist_items WHERE id = ?1 AND playlist_id = ?2")?;
            for item in item_ids {
                stmt.execute(params![item, id])?;
            }
        }
        tx.commit()
    }

    /// Rewrites the positions of a playlist so its items follow `ordered_item_ids`.
    pub fn set_playlist_order(&mut self, id: i64, ordered_item_ids: &[i64]) -> SqlResult<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "UPDATE playlist_items SET position = ?1 WHERE id = ?2 AND playlist_id = ?3",
            )?;
            for (position, item) in ordered_item_ids.iter().enumerate() {
                stmt.execute(params![to_i64(position as u64), item, id])?;
            }
        }
        tx.commit()
    }

    // ---- session state -------------------------------------------------------------

    pub fn kv_get(&self, key: &str) -> SqlResult<Option<String>> {
        self.conn
            .query_row("SELECT value FROM kv WHERE key = ?1", params![key], |r| {
                r.get(0)
            })
            .optional()
    }

    pub fn kv_set(&self, key: &str, value: &str) -> SqlResult<()> {
        self.conn.execute(KV_UPSERT_SQL, params![key, value])?;
        Ok(())
    }

    /// Follows the folder the library lives in when it has moved.
    ///
    /// An iPhone app's container is given another path when the app is updated or
    /// restored from a backup: what was `…/Application/A/Documents/Album` is then
    /// `…/Application/B/Documents/Album`. The paths in the database are absolute, so
    /// without this every song would be a file that is gone. `home` is that folder as
    /// it is now. If it is not the one this was last called with, every path below the
    /// old one is rewritten to lie below the new one: music folders, tracks, and the
    /// record of read CUE sheets (the JSON under `sheets_key`, a list of pairs keyed by
    /// path). Paths elsewhere are left alone. All of it happens or none of it.
    ///
    /// Returns how many paths were rewritten (0 the first time and whenever the folder
    /// is where it was).
    pub fn follow_home(&mut self, home: &Path, sheets_key: &str) -> SqlResult<usize> {
        // A folder whose name is not text cannot be remembered in `kv`: left alone.
        let Some(now) = home.to_str().map(|h| h.trim_end_matches('/').to_string()) else {
            return Ok(0);
        };
        if now.is_empty() {
            return Ok(0);
        }
        let before = self.kv_get(HOME_KEY)?;
        let Some(before) = before.filter(|before| *before != now) else {
            self.kv_set(HOME_KEY, &now)?;
            return Ok(0);
        };
        let tx = self.conn.transaction()?;
        let moved = rewrite_below(&tx, &before, &now, sheets_key)?;
        tx.execute(KV_UPSERT_SQL, params![HOME_KEY, now])?;
        tx.commit()?;
        Ok(moved)
    }

    /// The tracks that name no album artist: their album is told apart by the folder
    /// they lie in, so their key is made of their path. With what the key is made of
    /// (path, album title, compilation flag) and the key they have.
    pub fn tracks_without_album_artist(&self) -> SqlResult<Vec<FolderKeyed>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, album, compilation, album_key FROM tracks \
             WHERE TRIM(album_artist) = ''",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get(0)?,
                blob_to_path(row.get(1)?),
                row.get(2)?,
                row.get(3)?,
                row.get::<_, i64>(4)?.cast_unsigned(),
            ))
        })?;
        rows.collect()
    }

    /// Gives tracks other album keys, all of them or none.
    pub fn set_album_keys(&mut self, keys: &[(TrackId, u64)]) -> SqlResult<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare("UPDATE tracks SET album_key = ?2 WHERE id = ?1")?;
            for (id, key) in keys {
                stmt.execute(params![id, key.cast_signed()])?;
            }
        }
        tx.commit()
    }

    /// Follows a music folder that was moved or renamed: every path at or below
    /// `before` is rewritten to lie at or below `now`, as [`Db::follow_home`] does for
    /// the folder the library lives in. For a folder the system can still tell after it
    /// was moved (one linked on an iPhone, found again by its bookmark).
    ///
    /// Nothing happens unless `before` is a music folder of the library. If something
    /// is already known at the new place (the folder was added there as well) this
    /// fails and nothing is changed.
    ///
    /// Returns how many paths were rewritten.
    pub fn follow_folder(
        &mut self,
        before: &Path,
        now: &Path,
        sheets_key: &str,
    ) -> SqlResult<usize> {
        let text = |path: &Path| path.to_str().map(|p| p.trim_end_matches('/').to_string());
        let (Some(before), Some(now)) = (text(before), text(now)) else {
            return Ok(0);
        };
        if before.is_empty() || now.is_empty() || before == now {
            return Ok(0);
        }
        let tx = self.conn.transaction()?;
        let known: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM roots WHERE path = ?1)",
            params![before.as_bytes()],
            |row| row.get(0),
        )?;
        if !known {
            return Ok(0);
        }
        let moved = rewrite_below(&tx, &before, &now, sheets_key)?;
        tx.commit()?;
        Ok(moved)
    }

    /// Saves the playback session in one transaction: the play queue when `queue` is
    /// given (it replaces the saved one; entries whose track does not exist are
    /// skipped), and the rest of the session as `value` under `key`. Both are written or
    /// neither is, so the saved current item always belongs to the saved queue.
    /// A negative track id names a track of a media server (`remote_tracks`); its
    /// entry goes to `remote_queue` under the same position.
    pub fn save_session(
        &mut self,
        queue: Option<&[(u64, TrackId)]>,
        key: &str,
        value: &str,
    ) -> SqlResult<()> {
        let tx = self.conn.transaction()?;
        if let Some(items) = queue {
            tx.execute("DELETE FROM queue", [])?;
            tx.execute("DELETE FROM remote_queue", [])?;
            let mut stmt = tx.prepare_cached(
                "INSERT INTO queue (position, item_id, track_id) \
                 SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM tracks WHERE id = ?3)",
            )?;
            let mut remote = tx.prepare_cached(
                "INSERT INTO remote_queue (position, item_id, remote_id) \
                 SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM remote_tracks WHERE id = ?3)",
            )?;
            for (position, (item_id, track_id)) in items.iter().enumerate() {
                let position = to_i64(position as u64);
                if *track_id < 0 {
                    remote.execute(params![position, item_id.cast_signed(), -track_id])?;
                } else {
                    stmt.execute(params![position, item_id.cast_signed(), track_id])?;
                }
            }
        }
        tx.execute(KV_UPSERT_SQL, params![key, value])?;
        tx.commit()
    }

    /// Where the user left off in a track, if that was remembered.
    pub fn resume_point(&self, track: TrackId) -> SqlResult<Option<u64>> {
        self.conn
            .query_row(
                "SELECT position_ms FROM resume_points WHERE track_id = ?1",
                params![track],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .map(|ms| ms.map(|ms| ms.max(0).cast_unsigned()))
    }

    /// Remembers where the user left off in a track, or forgets it (`None`).
    pub fn set_resume_point(
        &self,
        track: TrackId,
        position_ms: Option<u64>,
        unix_secs: i64,
    ) -> SqlResult<()> {
        match position_ms {
            Some(ms) => self.conn.execute(
                "INSERT INTO resume_points (track_id, position_ms, saved_at) \
                 SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM tracks WHERE id = ?1) \
                 ON CONFLICT(track_id) DO UPDATE SET position_ms = excluded.position_ms, \
                 saved_at = excluded.saved_at",
                params![track, to_i64(ms), unix_secs],
            )?,
            None => self.conn.execute(
                "DELETE FROM resume_points WHERE track_id = ?1",
                params![track],
            )?,
        };
        Ok(())
    }

    pub fn load_queue(&self) -> SqlResult<Vec<(u64, TrackId)>> {
        let mut stmt = self.conn.prepare(
            "SELECT item_id, track_id FROM ( \
                 SELECT position, item_id, track_id FROM queue \
                 UNION ALL \
                 SELECT position, item_id, -remote_id FROM remote_queue \
             ) ORDER BY position",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?.cast_unsigned(), r.get(1)?)))?;
        rows.collect()
    }

    /// The tracks of media servers that are kept.
    pub fn remote_tracks(&self) -> SqlResult<Vec<RemoteRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, url, server, title, artist, album_artist, album, track_no, genre, \
                    duration_ms, container, sample_rate, channels, bit_depth, file_size, \
                    art_url, art_id, added_at \
             FROM remote_tracks",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(RemoteRow {
                id: r.get(0)?,
                url: r.get(1)?,
                server: r.get(2)?,
                title: r.get(3)?,
                artist: r.get(4)?,
                album_artist: r.get(5)?,
                album: r.get(6)?,
                track_no: r.get(7)?,
                genre: r.get(8)?,
                duration_ms: r.get::<_, i64>(9)?.max(0).cast_unsigned(),
                container: r.get(10)?,
                sample_rate: r.get(11)?,
                channels: r.get(12)?,
                bit_depth: r.get(13)?,
                file_size: r.get::<_, i64>(14)?.max(0).cast_unsigned(),
                art_url: r.get(15)?,
                art_id: r.get(16)?,
                added_at: r.get(17)?,
            })
        })?;
        rows.collect()
    }

    /// Keeps a track of a media server, or brings what is kept of it (by its address)
    /// up to date, and returns its row. `row.id` and `row.art_id` are not used: a
    /// cover that was fetched stays for as long as the cover's address is the same.
    pub fn upsert_remote_track(&self, row: &RemoteRow) -> SqlResult<i64> {
        self.conn.query_row(
            "INSERT INTO remote_tracks (url, server, title, artist, album_artist, album, \
                 track_no, genre, duration_ms, container, sample_rate, channels, bit_depth, \
                 file_size, art_url, added_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16) \
             ON CONFLICT(url) DO UPDATE SET \
                 server = excluded.server, title = excluded.title, artist = excluded.artist, \
                 album_artist = excluded.album_artist, album = excluded.album, \
                 track_no = excluded.track_no, genre = excluded.genre, \
                 duration_ms = excluded.duration_ms, container = excluded.container, \
                 sample_rate = excluded.sample_rate, channels = excluded.channels, \
                 bit_depth = excluded.bit_depth, file_size = excluded.file_size, \
                 art_id = CASE WHEN remote_tracks.art_url IS excluded.art_url \
                               THEN remote_tracks.art_id END, \
                 art_url = excluded.art_url \
             RETURNING id",
            params![
                row.url,
                row.server,
                row.title,
                row.artist,
                row.album_artist,
                row.album,
                row.track_no,
                row.genre,
                to_i64(row.duration_ms),
                row.container,
                row.sample_rate,
                row.channels,
                row.bit_depth,
                to_i64(row.file_size),
                row.art_url,
                row.added_at,
            ],
            |r| r.get(0),
        )
    }

    /// A transaction for writes that are made one by one through this handle
    /// (`upsert_remote_track`): they reach the disk together when it is committed,
    /// and none of them does when it is dropped without.
    pub fn batch(&self) -> SqlResult<rusqlite::Transaction<'_>> {
        self.conn.unchecked_transaction()
    }

    /// The cover that was fetched for a server's track (its id in the cover cache).
    pub fn set_remote_art(&self, id: i64, art_id: Option<&str>) -> SqlResult<()> {
        self.conn.execute(
            "UPDATE remote_tracks SET art_id = ?2 WHERE id = ?1",
            params![id, art_id],
        )?;
        Ok(())
    }

    /// Forgets the servers' tracks that the saved queue does not name: they are kept
    /// for the queue only. Returns how many went.
    pub fn prune_remote_tracks(&self) -> SqlResult<usize> {
        self.conn.execute(
            "DELETE FROM remote_tracks WHERE id NOT IN (SELECT remote_id FROM remote_queue)",
            [],
        )
    }
}

fn append_items(tx: &rusqlite::Transaction<'_>, id: i64, tracks: &[TrackId]) -> SqlResult<usize> {
    let mut position: i64 = tx.query_row(
        "SELECT COALESCE(MAX(position), -1) + 1 FROM playlist_items WHERE playlist_id = ?1",
        params![id],
        |r| r.get(0),
    )?;
    let mut added = 0;
    let mut stmt = tx.prepare_cached(
        "INSERT INTO playlist_items (playlist_id, track_id, position) \
         SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM tracks WHERE id = ?2)",
    )?;
    for track in tracks {
        if stmt.execute(params![id, track, position])? > 0 {
            position += 1;
            added += 1;
        }
    }
    Ok(added)
}

/// Opens a connection, verifies the file and applies the connection settings.
fn open_checked(path: &Path) -> SqlResult<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    let check: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    if !check.eq_ignore_ascii_case("ok") {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
            Some(check),
        ));
    }
    conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    // On Apple's systems a file that is "synced" may still sit in the drive's own
    // cache; only the full sync puts it on the disk. A power cut while the log is
    // written back into the database (a checkpoint) could otherwise leave the file
    // damaged, and a damaged database is replaced by an empty one. Costs at
    // checkpoints only; elsewhere it changes nothing.
    conn.pragma_update(None, "checkpoint_fullfsync", true)?;
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(conn)
}

/// True for SQLITE_CORRUPT and SQLITE_NOTADB (and a failed `quick_check`, which
/// `open_checked` reports as SQLITE_CORRUPT).
fn is_corruption(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(e, _)
            if e.code == ErrorCode::DatabaseCorrupt || e.code == ErrorCode::NotADatabase
    )
}

fn has_user_tables(conn: &Connection) -> SqlResult<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        [],
        |r| r.get(0),
    )?;
    Ok(count > 0)
}

/// Runs one migration step inside the upgrade's transaction (see [`Db::open`]).
fn apply_migration(conn: &Connection, sql: &str, target_version: u32) -> SqlResult<()> {
    conn.execute_batch(sql)?;
    conn.pragma_update(None, "user_version", target_version)
}

/// How often [`backup_and_lock`] tries again when another connection commits between
/// the copy and the lock.
const BACKUP_ATTEMPTS: usize = 5;

/// Copies the database to `<path>.bak-v<version>` and leaves `conn` inside a
/// `BEGIN IMMEDIATE` transaction (holding the write lock), with nothing committed by
/// anyone between the copy and the lock: the copy is exactly the state the upgrade
/// starts from.
///
/// The copy is made with `VACUUM INTO`, which reads one consistent snapshot of the
/// database, write-ahead log included, while other connections go on using it. (Copying
/// the file itself is only complete after a checkpoint, which a reader can keep from
/// finishing.) It is written under a temporary name and renamed when complete, so a
/// backup file is never a partial one.
fn backup_and_lock(conn: &Connection, path: &Path, version: u32) -> Result<PathBuf, DbError> {
    let backup = sibling(path, &format!(".bak-v{version}"));
    let partial = sibling(path, &format!(".bak-v{version}.partial"));
    let partial_name = partial.to_str().ok_or_else(|| {
        DbError::Io(io::Error::new(
            io::ErrorKind::InvalidFilename,
            "the database folder's path cannot be passed to SQLite",
        ))
    })?;
    for attempt in 1..=BACKUP_ATTEMPTS {
        // `VACUUM INTO` refuses a target that exists (a leftover of an interrupted run).
        match fs::remove_file(&partial) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let before: i64 = conn.pragma_query_value(None, "data_version", |r| r.get(0))?;
        conn.execute("VACUUM INTO ?1", params![partial_name])?;
        conn.execute_batch("BEGIN IMMEDIATE")?;
        // Changes only when another connection committed since `before`.
        let after: i64 = conn.pragma_query_value(None, "data_version", |r| r.get(0))?;
        if after == before {
            // Opened for writing: Windows only flushes a file through a handle that
            // may write to it (a read-only handle fails with "access denied").
            let finished = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&partial)
                .and_then(|f| f.sync_all())
                .and_then(|()| fs::rename(&partial, &backup));
            if let Err(e) = finished {
                let _ = conn.execute_batch("ROLLBACK");
                let _ = fs::remove_file(&partial);
                return Err(e.into());
            }
            return Ok(backup);
        }
        conn.execute_batch("ROLLBACK")?;
        tracing::debug!(attempt, "the database changed while it was being backed up");
    }
    let _ = fs::remove_file(&partial);
    Err(DbError::Sqlite(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
        Some("the database kept changing while it was being backed up".into()),
    )))
}

/// Renames an unusable database (and its WAL/SHM companions) out of the way.
fn move_aside(path: &Path) -> io::Result<PathBuf> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut target = sibling(path, &format!(".corrupt-{stamp}"));
    let mut attempt = 1;
    while target.exists() {
        target = sibling(path, &format!(".corrupt-{stamp}-{attempt}"));
        attempt += 1;
    }
    fs::rename(path, &target)?;
    for suffix in ["-wal", "-shm"] {
        let companion = sibling(path, suffix);
        if companion.exists() {
            let _ = fs::rename(&companion, sibling(&target, suffix));
        }
    }
    Ok(target)
}

/// `path` with `suffix` appended to its file name.
/// The copies made before upgrades (`<name>.bak-v<N>`) are kept one deep: that of the
/// last upgrade stays, those before it go. Each is a whole copy of the library, and
/// nothing else ever removes them.
fn prune_backups(path: &Path) {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let prefix = format!("{name}.bak-v");
    let mut backups: Vec<(u32, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let file = entry.file_name().into_string().ok()?;
            let version = file.strip_prefix(&prefix)?.parse().ok()?;
            Some((version, entry.path()))
        })
        .collect();
    backups.sort_by_key(|(version, _)| *version);
    backups.pop();
    for (version, old) in backups {
        match fs::remove_file(&old) {
            Ok(()) => tracing::info!(version, "an older copy of the library database was removed"),
            Err(error) => tracing::debug!(version, %error, "an older copy of the database stays"),
        }
    }
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Rewrites every path at or below the folder `before` to lie at or below `now`: music
/// folders, tracks, the files remembered as removed from the library, and the record
/// of read CUE sheets (the JSON under `sheets_key`, a list of pairs keyed by path).
/// Returns how many paths were rewritten.
fn rewrite_below(
    tx: &rusqlite::Transaction<'_>,
    before: &str,
    now: &str,
    sheets_key: &str,
) -> SqlResult<usize> {
    // What follows `before` in a path below it (or nothing, for the folder itself).
    let below = |path: &[u8]| -> Option<Vec<u8>> {
        let rest = path.strip_prefix(before.as_bytes())?;
        (rest.is_empty() || rest[0] == b'/').then(|| [now.as_bytes(), rest].concat())
    };
    let mut moved = 0;
    for table in ["roots", "tracks"] {
        let rows: Vec<(i64, Vec<u8>)> = tx
            .prepare(&format!("SELECT id, path FROM {table}"))?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<SqlResult<_>>()?;
        let mut update = tx.prepare(&format!("UPDATE {table} SET path = ?2 WHERE id = ?1"))?;
        for (id, path) in rows {
            if let Some(path) = below(&path) {
                update.execute(params![id, path])?;
                moved += 1;
            }
        }
    }
    // What was removed from the library is known by its path as well: left behind, a
    // removed song would be back after the move (on an iPhone: after every update of
    // the app), and the list would name files that are nowhere.
    let removed: Vec<Vec<u8>> = tx
        .prepare("SELECT path FROM removed_files")?
        .query_map([], |row| row.get(0))?
        .collect::<SqlResult<_>>()?;
    {
        let mut update =
            tx.prepare("UPDATE OR REPLACE removed_files SET path = ?2 WHERE path = ?1")?;
        for path in removed {
            if let Some(moved_to) = below(&path) {
                update.execute(params![path, moved_to])?;
                moved += 1;
            }
        }
    }
    let sheets: Option<String> = tx
        .query_row(
            "SELECT value FROM kv WHERE key = ?1",
            params![sheets_key],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(mut records) =
        sheets.and_then(|json| serde_json::from_str::<Vec<(String, serde_json::Value)>>(&json).ok())
    {
        for (key, _) in &mut records {
            if let Some(path) = below(key.as_bytes()).and_then(|p| String::from_utf8(p).ok()) {
                *key = path;
                moved += 1;
            }
        }
        if let Ok(json) = serde_json::to_string(&records) {
            tx.execute(KV_UPSERT_SQL, params![sheets_key, json])?;
        }
    }
    Ok(moved)
}

/// The `kv` key of the folder the library moves with (see [`Db::follow_home`]).
const HOME_KEY: &str = "home";

fn path_to_blob(path: &Path) -> std::borrow::Cow<'_, [u8]> {
    crate::platform::path_to_bytes(path)
}

fn blob_to_path(blob: Vec<u8>) -> PathBuf {
    crate::platform::path_from_bytes(blob)
}

fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn clamp_u32(value: i64) -> u32 {
    u32::try_from(value.max(0)).unwrap_or(u32::MAX)
}

fn clamp_u8(value: i64) -> u8 {
    u8::try_from(value.max(0)).unwrap_or(u8::MAX)
}

fn row_to_track(row: &Row<'_>) -> SqlResult<StoredTrack> {
    let gain = |index: usize| -> SqlResult<Option<f32>> {
        Ok(row.get::<_, Option<f64>>(index)?.map(|v| v as f32))
    };
    Ok(StoredTrack {
        track: Track {
            id: row.get(0)?,
            path: blob_to_path(row.get(1)?),
            root_id: row.get(2)?,
            available: row.get(3)?,
            title: row.get(4)?,
            artist: row.get(5)?,
            album_artist: row.get(6)?,
            album: row.get(7)?,
            album_key: row.get::<_, i64>(8)?.cast_unsigned(),
            disc_no: row.get::<_, Option<i64>>(10)?.map(clamp_u32),
            track_no: row.get::<_, Option<i64>>(11)?.map(clamp_u32),
            year: row
                .get::<_, Option<i64>>(12)?
                .map(|y| y.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32),
            genre: row.get(13)?,
            duration_ms: row.get::<_, i64>(14)?.max(0).cast_unsigned(),
            codec: row.get(15)?,
            container: row.get(16)?,
            sample_rate: clamp_u32(row.get(17)?),
            channels: clamp_u8(row.get(18)?),
            bit_depth: row.get::<_, Option<i64>>(19)?.map(clamp_u8),
            bitrate_kbps: row.get::<_, Option<i64>>(20)?.map(clamp_u32),
            file_size: row.get::<_, i64>(21)?.max(0).cast_unsigned(),
            mtime_ns: row.get(22)?,
            replaygain: ReplayGainTags {
                track_gain_db: gain(23)?,
                track_peak: gain(24)?,
                album_gain_db: gain(25)?,
                album_peak: gain(26)?,
            },
            measured: gain(33)?
                .map(|track_gain| -> SqlResult<ReplayGainTags> {
                    Ok(ReplayGainTags {
                        track_gain_db: Some(track_gain),
                        track_peak: gain(34)?,
                        album_gain_db: gain(35)?,
                        album_peak: gain(36)?,
                    })
                })
                .transpose()?,
            unmeasurable: false,
            check: row
                .get::<_, Option<i64>>(37)?
                .and_then(CheckVerdict::from_code)
                .map(|verdict| -> SqlResult<FileCheck> {
                    Ok(FileCheck {
                        verdict,
                        dr: gain(38)?,
                        true_peak: gain(39)?,
                    })
                })
                .transpose()?,
            art_id: row.get(27)?,
            favorite: row.get(29)?,
            rating: row.get::<_, i64>(40)?.clamp(0, 5) as u8,
            play_count: clamp_u32(row.get(30)?),
            last_played_at: row.get(31)?,
            added_at: row.get(32)?,
        },
        compilation: row.get(9)?,
        art_from_folder: row.get(28)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_track(path: &str, root_id: Option<i64>) -> StoredTrack {
        StoredTrack {
            track: Track {
                id: 0,
                path: PathBuf::from(path),
                root_id,
                available: true,
                title: "밤편지".into(),
                artist: "아이유".into(),
                album_artist: "아이유".into(),
                album: "Palette 🎨".into(),
                album_key: 0xfedc_ba98_7654_3210,
                disc_no: Some(1),
                track_no: Some(4),
                year: Some(2017),
                genre: "K-Pop".into(),
                duration_ms: 253_000,
                codec: "FLAC".into(),
                container: "FLAC".into(),
                sample_rate: 44_100,
                channels: 2,
                bit_depth: Some(16),
                bitrate_kbps: Some(912),
                file_size: 28_000_000,
                mtime_ns: 1_700_000_000_123_456_789,
                replaygain: ReplayGainTags {
                    track_gain_db: Some(-6.54),
                    track_peak: Some(0.98),
                    album_gain_db: None,
                    album_peak: None,
                },
                measured: None,
                unmeasurable: false,
                check: None,
                art_id: Some("0123456789abcdef".into()),
                favorite: false,
                rating: 0,
                play_count: 0,
                last_played_at: None,
                added_at: 1_700_000_000,
            },
            compilation: true,
            art_from_folder: false,
        }
    }

    /// A song that is removed is written down as removed in the same step, and the
    /// list survives the database being opened again.
    #[test]
    fn removed_files_are_remembered_with_the_removal() {
        let dir = tempfile::tempdir().unwrap();
        let (kept, gone, also_gone) = {
            let mut db = Db::open(&db_path(&dir)).unwrap().db;
            let ids = db
                .upsert_tracks(&[
                    sample_track("/music/kept.flac", None),
                    sample_track("/music/gone.flac", None),
                    sample_track("/music/also gone.flac", None),
                ])
                .unwrap();
            assert!(db.removed_files().unwrap().is_empty());
            db.delete_tracks_remembering(&[
                (ids[1], PathBuf::from("/music/gone.flac")),
                (ids[2], PathBuf::from("/music/also gone.flac")),
            ])
            .unwrap();
            // The same file removed twice is on the list once.
            db.delete_tracks_remembering(&[(ids[1], PathBuf::from("/music/gone.flac"))])
                .unwrap();
            (ids[0], ids[1], ids[2])
        };
        let mut db = Db::open(&db_path(&dir)).unwrap().db;
        assert!(db.get_track(kept).unwrap().is_some());
        assert!(db.get_track(gone).unwrap().is_none());
        assert!(db.get_track(also_gone).unwrap().is_none());
        let mut removed = db.removed_files().unwrap();
        removed.sort();
        assert_eq!(
            removed,
            [
                PathBuf::from("/music/also gone.flac"),
                PathBuf::from("/music/gone.flac")
            ]
        );
        db.forget_removed_files(&[
            PathBuf::from("/music/gone.flac"),
            PathBuf::from("/music/never there.flac"),
        ])
        .unwrap();
        assert_eq!(
            db.removed_files().unwrap(),
            [PathBuf::from("/music/also gone.flac")]
        );
    }

    /// The folder the library lives in has moved (an iPhone app after an update): what
    /// was removed from the library moves with the tracks, or it would be back.
    #[test]
    fn removed_files_follow_the_library_when_its_folder_moves() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&db_path(&dir)).unwrap().db;
        let old = "/var/mobile/Containers/Data/Application/AAAA/Documents";
        let new = "/var/mobile/Containers/Data/Application/BBBB/Documents";
        assert_eq!(db.follow_home(Path::new(old), "sheets").unwrap(), 0);
        let ids = db
            .upsert_tracks(&[
                sample_track(&format!("{old}/Album/kept.flac"), None),
                sample_track(&format!("{old}/Album/gone.flac"), None),
                sample_track("/elsewhere/also gone.flac", None),
            ])
            .unwrap();
        db.delete_tracks_remembering(&[
            (ids[1], PathBuf::from(format!("{old}/Album/gone.flac"))),
            (ids[2], PathBuf::from("/elsewhere/also gone.flac")),
        ])
        .unwrap();

        // One track and one removed file lie below the folder that moved.
        assert_eq!(db.follow_home(Path::new(new), "sheets").unwrap(), 2);
        let mut removed = db.removed_files().unwrap();
        removed.sort();
        assert_eq!(
            removed,
            [
                PathBuf::from("/elsewhere/also gone.flac"),
                PathBuf::from(format!("{new}/Album/gone.flac")),
            ]
        );
        assert_eq!(
            db.get_track(ids[0]).unwrap().unwrap().track.path,
            PathBuf::from(format!("{new}/Album/kept.flac"))
        );
    }

    /// The current migration list plus one more step, to simulate a future upgrade.
    fn with_next(next: &'static str) -> Vec<&'static str> {
        let mut list = MIGRATIONS.to_vec();
        list.push(next);
        list
    }

    fn db_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join(DB_FILE_NAME)
    }

    #[test]
    fn measured_loudness_belongs_to_one_state_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&db_path(&dir)).unwrap().db;
        let mut row = sample_track("/Music/a.flac", None);
        let id = db.upsert_tracks(std::slice::from_ref(&row)).unwrap()[0];
        assert_eq!(db.get_track(id).unwrap().unwrap().track.measured, None);
        let values = ReplayGainTags {
            track_gain_db: Some(-3.25),
            track_peak: Some(0.5),
            album_gain_db: None,
            album_peak: None,
        };
        // A track that does not exist is skipped, not an error.
        db.set_loudness(&[
            (id, row.track.mtime_ns, row.track.file_size, values),
            (id + 100, 1, 1, values),
        ])
        .unwrap();
        assert_eq!(
            db.get_track(id).unwrap().unwrap().track.measured,
            Some(values)
        );
        assert_eq!(db.load_tracks().unwrap()[0].track.measured, Some(values));
        // Measured again, with album values: the row is replaced.
        let with_album = ReplayGainTags {
            album_gain_db: Some(-2.0),
            album_peak: Some(0.75),
            ..values
        };
        db.set_loudness(&[(id, row.track.mtime_ns, row.track.file_size, with_album)])
            .unwrap();
        assert_eq!(
            db.get_track(id).unwrap().unwrap().track.measured,
            Some(with_album)
        );
        // The file changed: what was measured before no longer applies.
        row.track.mtime_ns += 1;
        assert_eq!(db.upsert_tracks(std::slice::from_ref(&row)).unwrap(), [id]);
        assert_eq!(db.get_track(id).unwrap().unwrap().track.measured, None);
        // Measured anew, forgotten on request, and gone with the track.
        db.set_loudness(&[(id, row.track.mtime_ns, row.track.file_size, values)])
            .unwrap();
        assert_eq!(
            db.get_track(id).unwrap().unwrap().track.measured,
            Some(values)
        );
        assert_eq!(db.clear_loudness().unwrap(), 1);
        assert_eq!(db.get_track(id).unwrap().unwrap().track.measured, None);
        db.set_loudness(&[(id, row.track.mtime_ns, row.track.file_size, values)])
            .unwrap();
        db.delete_tracks(&[id]).unwrap();
        let left: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM loudness", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0);
    }

    #[test]
    fn fresh_create_then_reopen_keeps_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_path(&dir);
        {
            let opened = Db::open(&path).unwrap();
            assert!(opened.recovered_corrupt_db.is_none());
            assert!(opened.migrated_from.is_none());
            let mut db = opened.db;
            assert_eq!(db.schema_version().unwrap(), MIGRATIONS.len() as u32);
            let root = db.insert_root(Path::new("/Music")).unwrap();
            let ids = db
                .upsert_tracks(&[sample_track("/Music/a.flac", Some(root))])
                .unwrap();
            assert_eq!(ids.len(), 1);
            db.kv_set("volume", "0.8").unwrap();
        }
        let opened = Db::open(&path).unwrap();
        assert!(opened.recovered_corrupt_db.is_none());
        assert!(opened.migrated_from.is_none());
        let db = opened.db;
        let tracks = db.load_tracks().unwrap();
        assert_eq!(tracks.len(), 1);
        let mut expected = sample_track("/Music/a.flac", Some(1));
        expected.track.id = tracks[0].track.id;
        assert_eq!(tracks[0], expected);
        assert_eq!(db.kv_get("volume").unwrap().as_deref(), Some("0.8"));
        assert_eq!(db.roots().unwrap()[0].path, PathBuf::from("/Music"));
        // No backup is made when nothing was migrated.
        assert!(!sibling(&path, ".bak-v1").exists());
    }

    #[test]
    fn upsert_keeps_user_columns_and_id() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&db_path(&dir)).unwrap().db;
        let id = db
            .upsert_tracks(&[sample_track("/m/a.flac", None)])
            .unwrap()[0];
        db.set_favorite(&[id], true).unwrap();
        assert!(db.record_play(id, 42).unwrap());

        let mut changed = sample_track("/m/a.flac", None);
        changed.track.title = "새 제목".into();
        changed.track.file_size = 1;
        let id2 = db.upsert_tracks(&[changed]).unwrap()[0];
        assert_eq!(id, id2);
        let row = db.get_track(id).unwrap().unwrap().track;
        assert_eq!(row.title, "새 제목");
        assert_eq!(row.file_size, 1);
        assert!(row.favorite);
        assert_eq!(row.play_count, 1);
        assert_eq!(row.last_played_at, Some(42));
        assert_eq!(row.added_at, 1_700_000_000);
    }

    #[test]
    fn corrupt_file_is_moved_aside_and_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_path(&dir);
        let garbage: Vec<u8> = (0..8192u32).map(|i| (i * 31 % 251) as u8).collect();
        fs::write(&path, &garbage).unwrap();

        let opened = Db::open(&path).unwrap();
        let aside = opened
            .recovered_corrupt_db
            .expect("corruption must be reported");
        assert!(
            aside
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("library.db.corrupt-")
        );
        assert_eq!(fs::read(&aside).unwrap(), garbage);
        // The replacement is a working, empty database.
        assert!(opened.db.load_tracks().unwrap().is_empty());
        opened.db.kv_set("k", "v").unwrap();
    }

    #[test]
    fn truncated_database_is_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_path(&dir);
        {
            let mut db = Db::open(&path).unwrap().db;
            let rows: Vec<_> = (0..500)
                .map(|i| sample_track(&format!("/m/{i}.flac"), None))
                .collect();
            db.upsert_tracks(&rows).unwrap();
            db.checkpoint().unwrap();
        }
        let bytes = fs::read(&path).unwrap();
        assert!(bytes.len() > 8192);
        // Keep the header page but cut the file in the middle of the b-tree.
        fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
        let opened = Db::open(&path).unwrap();
        assert!(opened.recovered_corrupt_db.is_some());
        assert!(opened.db.load_tracks().unwrap().is_empty());
    }

    #[test]
    fn migration_makes_backup_and_reports_old_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_path(&dir);
        let current = MIGRATIONS.len() as u32;
        {
            let mut db = Db::open(&path).unwrap().db;
            db.upsert_tracks(&[sample_track("/m/a.flac", None)])
                .unwrap();
        }
        let next = "ALTER TABLE tracks ADD COLUMN mood INTEGER NOT NULL DEFAULT 0;";
        let opened = Db::open_with_migrations(&path, &with_next(next)).unwrap();
        assert_eq!(opened.migrated_from, Some(current));
        assert_eq!(opened.db.schema_version().unwrap(), current + 1);
        assert_eq!(opened.db.load_tracks().unwrap().len(), 1);
        let backup = sibling(&path, &format!(".bak-v{current}"));
        assert!(backup.exists());
        // The backup is a complete database of the old version.
        let old = Connection::open(&backup).unwrap();
        let v: u32 = old
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, current);
        let n: i64 = old
            .query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn v1_database_is_upgraded_in_place() {
        // A database as written by the first release: schema v1, rows inserted with
        // plain SQL (the current code cannot write that shape any more).
        let dir = tempfile::tempdir().unwrap();
        let path = db_path(&dir);
        {
            let old = Db::open_with_migrations(&path, &[SCHEMA_V1]).unwrap().db;
            old.conn
                .execute_batch(
                    "INSERT INTO roots (path) VALUES (x'2f4d75736963');
                     INSERT INTO tracks (path, root_id, title, artist, album_artist, album,
                         album_key, genre, duration_ms, codec, container, sample_rate,
                         channels, file_size, mtime_ns, art_id, favorite, play_count, added_at)
                     VALUES (x'2f4d757369632f612e666c6163', 1, '밤편지', '아이유', '', 'Palette',
                         -42, 'K-Pop', 253000, 'FLAC', 'FLAC', 44100, 2, 1234, 5678,
                         '0123456789abcdef', 1, 7, 1700000000);
                     INSERT INTO playlists (name, created_at) VALUES ('mix', 1);
                     INSERT INTO playlist_items (playlist_id, track_id, position) VALUES (1, 1, 0);
                     INSERT INTO kv (key, value) VALUES ('volume', '0.5');",
                )
                .unwrap();
            assert_eq!(old.schema_version().unwrap(), 1);
        }

        let opened = Db::open(&path).unwrap();
        assert_eq!(opened.migrated_from, Some(1));
        assert!(opened.recovered_corrupt_db.is_none());
        assert!(sibling(&path, ".bak-v1").exists());
        let mut db = opened.db;
        assert_eq!(db.schema_version().unwrap(), MIGRATIONS.len() as u32);
        let tracks = db.load_tracks().unwrap();
        assert_eq!(tracks.len(), 1);
        let t = &tracks[0];
        assert_eq!(t.track.path, PathBuf::from("/Music/a.flac"));
        assert_eq!(t.track.title, "밤편지");
        assert_eq!(t.track.album_key, (-42i64).cast_unsigned());
        assert_eq!(t.track.art_id.as_deref(), Some("0123456789abcdef"));
        assert!(t.track.favorite);
        assert_eq!(t.track.play_count, 7);
        assert!(
            !t.art_from_folder,
            "old rows start as 'not known to be folder art'"
        );
        assert_eq!(db.all_playlist_items().unwrap(), vec![(1, 1, 1)]);
        assert_eq!(db.kv_get("volume").unwrap().as_deref(), Some("0.5"));
        let roots = db.roots().unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].path, PathBuf::from("/Music"));
        assert_eq!(roots[0].unconfirmed_after, None);
        db.set_root_unconfirmed(roots[0].id, Some(41)).unwrap();
        assert_eq!(db.roots().unwrap()[0].unconfirmed_after, Some(41));
        db.set_root_unconfirmed(roots[0].id, None).unwrap();
        assert_eq!(db.roots().unwrap()[0].unconfirmed_after, None);
        // The upgraded database takes current writes.
        let mut row = sample_track("/Music/b.flac", Some(1));
        row.art_from_folder = true;
        let id = db.upsert_tracks(&[row]).unwrap()[0];
        assert!(db.get_track(id).unwrap().unwrap().art_from_folder);
        // Opening again does not migrate again.
        drop(db);
        assert_eq!(Db::open(&path).unwrap().migrated_from, None);
    }

    #[test]
    fn failing_migration_restores_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_path(&dir);
        let current = MIGRATIONS.len() as u32;
        {
            let mut db = Db::open(&path).unwrap().db;
            db.upsert_tracks(&[sample_track("/m/a.flac", None)])
                .unwrap();
            db.kv_set("marker", "before").unwrap();
        }
        // The first statement succeeds, the second fails: the whole step must roll back.
        let broken = "ALTER TABLE tracks ADD COLUMN mood INTEGER; \
                      INSERT INTO table_that_does_not_exist VALUES (1);";
        let err = match Db::open_with_migrations(&path, &with_next(broken)) {
            Ok(_) => panic!("migration must fail"),
            Err(err) => err,
        };
        match err {
            DbError::MigrationFailed {
                from, to, restored, ..
            } => {
                assert_eq!((from, to), (current, current + 1));
                assert!(restored);
            }
            other => panic!("unexpected error: {other}"),
        }
        assert!(sibling(&path, &format!(".bak-v{current}")).exists());

        // The original database is intact and opens with the current migration list.
        let opened = Db::open(&path).unwrap();
        assert!(opened.recovered_corrupt_db.is_none());
        assert!(opened.migrated_from.is_none());
        assert_eq!(opened.db.schema_version().unwrap(), current);
        assert_eq!(opened.db.load_tracks().unwrap().len(), 1);
        assert_eq!(
            opened.db.kv_get("marker").unwrap().as_deref(),
            Some("before")
        );
    }

    /// Writes that are still only in the write-ahead log while another connection holds
    /// an older snapshot open (so no checkpoint can fold them into the main file).
    fn with_unfolded_writes(path: &Path) -> (Db, Connection) {
        let mut writer = Db::open(path).unwrap().db;
        writer
            .upsert_tracks(&[sample_track("/m/a.flac", None)])
            .unwrap();
        writer.kv_set("marker", "before").unwrap();
        let reader = Connection::open(path).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let n: i64 = reader
            .query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        writer
            .upsert_tracks(&[sample_track("/m/b.flac", None)])
            .unwrap();
        writer.kv_set("marker", "latest").unwrap();
        (writer, reader)
    }

    #[test]
    fn a_failed_upgrade_beside_a_reader_loses_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_path(&dir);
        let current = MIGRATIONS.len() as u32;
        let (writer, reader) = with_unfolded_writes(&path);

        let broken = "ALTER TABLE tracks ADD COLUMN mood INTEGER; \
                      INSERT INTO table_that_does_not_exist VALUES (1);";
        let started = std::time::Instant::now();
        match Db::open_with_migrations(&path, &with_next(broken)) {
            Err(DbError::MigrationFailed { restored, .. }) => assert!(restored),
            Err(other) => panic!("unexpected error: {other}"),
            Ok(_) => panic!("migration must fail"),
        }
        // No waiting for the reader to let go.
        assert!(started.elapsed() < std::time::Duration::from_secs(2));

        // The reader's connection still works, and nothing written is lost.
        let n: i64 = reader
            .query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "the reader keeps its snapshot");
        reader.execute_batch("COMMIT").unwrap();
        assert_eq!(writer.kv_get("marker").unwrap().as_deref(), Some("latest"));
        drop((writer, reader));
        let db = Db::open(&path).unwrap().db;
        assert_eq!(db.schema_version().unwrap(), current);
        assert_eq!(db.load_tracks().unwrap().len(), 2);
        assert_eq!(db.kv_get("marker").unwrap().as_deref(), Some("latest"));
        // And the backup holds the latest writes too.
        let backup = Connection::open(sibling(&path, &format!(".bak-v{current}"))).unwrap();
        let marker: String = backup
            .query_row("SELECT value FROM kv WHERE key = 'marker'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(marker, "latest");
    }

    #[test]
    fn an_upgrade_beside_a_reader_keeps_the_latest_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_path(&dir);
        let current = MIGRATIONS.len() as u32;
        let (writer, reader) = with_unfolded_writes(&path);
        // A leftover of an interrupted backup is replaced, not an obstacle.
        fs::write(sibling(&path, &format!(".bak-v{current}.partial")), b"junk").unwrap();

        let next = "ALTER TABLE tracks ADD COLUMN mood INTEGER NOT NULL DEFAULT 0;";
        let opened = Db::open_with_migrations(&path, &with_next(next)).unwrap();
        assert_eq!(opened.migrated_from, Some(current));
        assert_eq!(opened.db.load_tracks().unwrap().len(), 2);
        assert_eq!(
            opened.db.kv_get("marker").unwrap().as_deref(),
            Some("latest")
        );
        assert!(!sibling(&path, &format!(".bak-v{current}.partial")).exists());
        let backup = Connection::open(sibling(&path, &format!(".bak-v{current}"))).unwrap();
        let (v, n): (u32, i64) = (
            backup
                .pragma_query_value(None, "user_version", |r| r.get(0))
                .unwrap(),
            backup
                .query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get(0))
                .unwrap(),
        );
        assert_eq!((v, n), (current, 2));
        // The other connections see the upgrade once they start a new read.
        reader.execute_batch("COMMIT").unwrap();
        let rating: i64 = reader
            .query_row("SELECT SUM(rating) FROM tracks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rating, 0);
        drop((writer, reader));
    }

    #[test]
    fn an_upgrade_waits_for_no_writer_and_changes_nothing_when_it_cannot_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_path(&dir);
        let current = MIGRATIONS.len() as u32;
        {
            let db = Db::open(&path).unwrap().db;
            db.kv_set("marker", "before").unwrap();
        }
        // Another connection is in the middle of writing and does not finish in time.
        let writer = Connection::open(&path).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        writer
            .execute("UPDATE kv SET value = 'theirs' WHERE key = 'marker'", [])
            .unwrap();
        let next = "ALTER TABLE tracks ADD COLUMN mood INTEGER NOT NULL DEFAULT 0;";
        assert!(matches!(
            Db::open_with_migrations(&path, &with_next(next)),
            Err(DbError::Sqlite(_))
        ));
        // Their write goes through; the database is still the old version.
        writer.execute_batch("COMMIT").unwrap();
        drop(writer);
        let db = Db::open(&path).unwrap().db;
        assert_eq!(db.schema_version().unwrap(), current);
        assert_eq!(db.kv_get("marker").unwrap().as_deref(), Some("theirs"));
    }

    #[test]
    fn unopenable_but_healthy_database_is_left_alone() {
        // A healthy database that cannot be opened for a reason other than corruption:
        // a directory sits where SQLite needs to create its write-ahead log.
        let dir = tempfile::tempdir().unwrap();
        let path = db_path(&dir);
        {
            let mut db = Db::open(&path).unwrap().db;
            db.upsert_tracks(&[sample_track("/m/a.flac", None)])
                .unwrap();
            db.kv_set("marker", "precious").unwrap();
        }
        let healthy = fs::read(&path).unwrap();
        let wal = sibling(&path, "-wal");
        let _ = fs::remove_file(&wal);
        fs::create_dir(&wal).unwrap();

        match Db::open(&path) {
            Err(DbError::Sqlite(err)) => assert!(!is_corruption(&err), "{err}"),
            Err(other) => panic!("unexpected error kind: {other}"),
            Ok(_) => panic!("the database cannot be opened in this state"),
        }
        // Nothing was renamed or rewritten.
        assert_eq!(fs::read(&path).unwrap(), healthy);
        let names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.iter().all(|name| !name.contains("corrupt")),
            "nothing may be moved aside: {names:?}"
        );

        // Once the obstacle is gone the data is all there.
        fs::remove_dir(&wal).unwrap();
        let opened = Db::open(&path).unwrap();
        assert!(opened.recovered_corrupt_db.is_none());
        assert_eq!(opened.db.load_tracks().unwrap().len(), 1);
        assert_eq!(
            opened.db.kv_get("marker").unwrap().as_deref(),
            Some("precious")
        );
    }

    #[test]
    fn only_corruption_codes_count_as_corruption() {
        let failure = |code: std::ffi::c_int| {
            rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None)
        };
        assert!(is_corruption(&failure(rusqlite::ffi::SQLITE_CORRUPT)));
        assert!(is_corruption(&failure(rusqlite::ffi::SQLITE_NOTADB)));
        for code in [
            rusqlite::ffi::SQLITE_CANTOPEN,
            rusqlite::ffi::SQLITE_IOERR,
            rusqlite::ffi::SQLITE_FULL,
            rusqlite::ffi::SQLITE_READONLY,
            rusqlite::ffi::SQLITE_BUSY,
            rusqlite::ffi::SQLITE_LOCKED,
            rusqlite::ffi::SQLITE_PERM,
            rusqlite::ffi::SQLITE_NOMEM,
        ] {
            assert!(!is_corruption(&failure(code)), "code {code}");
        }
        assert!(!is_corruption(&rusqlite::Error::QueryReturnedNoRows));
    }

    #[test]
    fn newer_schema_is_refused_and_left_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_path(&dir);
        {
            let next = "CREATE TABLE future (x INTEGER);";
            Db::open_with_migrations(&path, &with_next(next)).unwrap();
        }
        let current = MIGRATIONS.len() as u32;
        match Db::open(&path) {
            Err(DbError::NewerSchema { found, supported }) => {
                assert_eq!((found, supported), (current + 1, current));
            }
            Err(other) => panic!("unexpected error: {other}"),
            Ok(_) => panic!("must refuse a newer schema"),
        }
        assert!(path.exists());
        assert!(Db::open_with_migrations(&path, &with_next("SELECT 1;")).is_ok());
    }

    #[test]
    fn deleting_tracks_cascades() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&db_path(&dir)).unwrap().db;
        let root = db.insert_root(Path::new("/Music")).unwrap();
        let ids = db
            .upsert_tracks(&[
                sample_track("/Music/a.flac", Some(root)),
                sample_track("/Music/b.flac", Some(root)),
                sample_track("/elsewhere/c.flac", None),
            ])
            .unwrap();
        let playlist = db.create_playlist("mix", 1).unwrap();
        assert_eq!(
            db.append_playlist_items(playlist, &[ids[0], ids[1], ids[2], 9999])
                .unwrap(),
            3
        );
        db.record_play(ids[0], 10).unwrap();
        db.record_play(ids[2], 11).unwrap();
        db.save_session(
            Some(&[(7, ids[0]), (8, 9999), (u64::MAX, ids[2])][..]),
            "test_session",
            "{}",
        )
        .unwrap();
        assert_eq!(
            db.load_queue().unwrap(),
            vec![(7, ids[0]), (u64::MAX, ids[2])]
        );

        db.delete_tracks(&[ids[0]]).unwrap();
        assert_eq!(db.all_playlist_items().unwrap().len(), 2);
        assert_eq!(db.recent_plays(10).unwrap(), vec![ids[2]]);
        assert_eq!(db.load_queue().unwrap(), vec![(u64::MAX, ids[2])]);

        // Removing the root removes its remaining track and that track's references.
        assert!(db.delete_root_with_tracks(root).unwrap());
        let left = db.load_tracks().unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].track.id, ids[2]);
        let items = db.all_playlist_items().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].2, ids[2]);
    }

    #[test]
    fn unreferenced_delete_spares_favourites_and_playlist_tracks() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&db_path(&dir)).unwrap().db;
        let ids = db
            .upsert_tracks(&[
                sample_track("/m/plain.flac", None),
                sample_track("/m/favourite.flac", None),
                sample_track("/m/listed.flac", None),
            ])
            .unwrap();
        db.set_favorite(&[ids[1]], true).unwrap();
        let playlist = db.create_playlist("p", 1).unwrap();
        db.append_playlist_items(playlist, &[ids[2]]).unwrap();

        let deleted = db
            .delete_unreferenced_tracks(&[ids[0], ids[1], ids[2], 4242])
            .unwrap();
        assert_eq!(deleted, vec![ids[0]]);
        let left: Vec<TrackId> = db
            .load_tracks()
            .unwrap()
            .iter()
            .map(|t| t.track.id)
            .collect();
        assert_eq!(left, vec![ids[1], ids[2]]);
    }

    #[test]
    fn relink_takes_over_file_columns() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&db_path(&dir)).unwrap().db;
        let old = db
            .upsert_tracks(&[sample_track("/m/old.flac", None)])
            .unwrap()[0];
        db.set_favorite(&[old], true).unwrap();
        db.record_play(old, 5).unwrap();
        let mut moved = sample_track("/m/sub/new.flac", None);
        moved.track.album_key = 77;
        moved.track.mtime_ns = 9;
        let new = db.upsert_tracks(&[moved]).unwrap()[0];
        assert_ne!(old, new);

        let relinked = db.relink_track(old, new).unwrap().unwrap().track;
        assert_eq!(relinked.id, old);
        assert_eq!(relinked.path, PathBuf::from("/m/sub/new.flac"));
        assert_eq!(relinked.album_key, 77);
        assert_eq!(relinked.mtime_ns, 9);
        assert!(relinked.favorite);
        assert_eq!(relinked.play_count, 1);
        assert!(db.get_track(new).unwrap().is_none());
        assert_eq!(db.load_tracks().unwrap().len(), 1);
    }

    #[test]
    fn relink_keeps_what_was_attached_to_the_new_row() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&db_path(&dir)).unwrap().db;
        let old = db
            .upsert_tracks(&[sample_track("/m/old.flac", None)])
            .unwrap()[0];
        let other = db
            .upsert_tracks(&[sample_track("/m/other.flac", None)])
            .unwrap()[0];
        let mut moved = sample_track("/m/new.flac", None);
        moved.art_from_folder = true;
        let new = db.upsert_tracks(&[moved]).unwrap()[0];

        let playlist = db.create_playlist("p", 1).unwrap();
        db.append_playlist_items(playlist, &[old, new, other, new])
            .unwrap();
        db.save_session(
            Some(&[(1, new), (2, other), (3, old)][..]),
            "test_session",
            "{}",
        )
        .unwrap();
        db.record_play(old, 100).unwrap();
        db.record_play(new, 200).unwrap();
        db.record_play(new, 300).unwrap();
        db.set_favorite(&[new], true).unwrap();

        let row = db.relink_track(old, new).unwrap().unwrap();
        assert_eq!(row.track.id, old);
        assert_eq!(row.track.path, PathBuf::from("/m/new.flac"));
        assert!(row.art_from_folder);
        assert!(row.track.favorite, "a favourite set on the new row is kept");
        assert_eq!(row.track.play_count, 3);
        assert_eq!(row.track.last_played_at, Some(300));
        assert!(db.get_track(new).unwrap().is_none());

        // Playlist entries, queue entries and history of the new row now belong to the
        // kept row, in their original places.
        let items: Vec<TrackId> = db
            .all_playlist_items()
            .unwrap()
            .into_iter()
            .map(|(_, _, track)| track)
            .collect();
        assert_eq!(items, vec![old, old, other, old]);
        assert_eq!(
            db.load_queue().unwrap(),
            vec![(1, old), (2, other), (3, old)]
        );
        assert_eq!(db.recent_plays(10).unwrap(), vec![old]);
        let plays: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM history WHERE track_id = ?1",
                params![old],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(plays, 3);
    }

    #[test]
    fn playlist_with_items_is_created_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&db_path(&dir)).unwrap().db;
        let ids = db
            .upsert_tracks(&[
                sample_track("/m/a.flac", None),
                sample_track("/m/b.flac", None),
            ])
            .unwrap();
        let (playlist, added) = db
            .create_playlist_with_items("가져온 목록", 5, &[ids[1], 999, ids[0], ids[1]])
            .unwrap();
        assert_eq!(added, 3);
        assert_eq!(
            db.playlists().unwrap(),
            vec![(playlist, "가져온 목록".to_string())]
        );
        let items: Vec<TrackId> = db
            .all_playlist_items()
            .unwrap()
            .into_iter()
            .map(|(_, _, track)| track)
            .collect();
        assert_eq!(items, vec![ids[1], ids[0], ids[1]]);

        // If filling the playlist fails, the playlist itself is not left behind.
        db.conn
            .execute_batch(
                "CREATE TRIGGER refuse_items BEFORE INSERT ON playlist_items
                 BEGIN SELECT RAISE(ABORT, 'simulated failure'); END;",
            )
            .unwrap();
        assert!(db.create_playlist_with_items("실패", 6, &ids).is_err());
        assert_eq!(db.playlists().unwrap().len(), 1);
    }

    #[test]
    fn replacing_a_playlist_is_all_or_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&db_path(&dir)).unwrap().db;
        let ids = db
            .upsert_tracks(&[
                sample_track("/m/a.flac", None),
                sample_track("/m/b.flac", None),
            ])
            .unwrap();
        let (playlist, _) = db
            .create_playlist_with_items("목록", 5, &[ids[0], ids[1]])
            .unwrap();
        let tracks = |db: &Db| -> Vec<TrackId> {
            db.all_playlist_items()
                .unwrap()
                .into_iter()
                .map(|(_, _, track)| track)
                .collect()
        };
        assert!(
            db.replace_playlist_items(playlist, &[ids[1], 999, ids[0]])
                .unwrap()
        );
        assert_eq!(tracks(&db), vec![ids[1], ids[0]]);
        assert!(!db.replace_playlist_items(playlist + 1, &ids).unwrap());

        // The second half fails: the playlist keeps what it had (not emptied).
        db.conn
            .execute_batch(
                "CREATE TRIGGER refuse_items BEFORE INSERT ON playlist_items
                 BEGIN SELECT RAISE(ABORT, 'simulated failure'); END;",
            )
            .unwrap();
        assert!(db.replace_playlist_items(playlist, &[ids[0]]).is_err());
        assert_eq!(tracks(&db), vec![ids[1], ids[0]]);
    }

    #[test]
    fn moving_a_track_changes_path_and_album_key_only() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&db_path(&dir)).unwrap().db;
        let ids = db
            .upsert_tracks(&[
                sample_track("/private/var/m/a.flac", None),
                sample_track("/var/m/taken.flac", None),
            ])
            .unwrap();
        db.set_favorite(&[ids[0]], true).unwrap();
        assert!(
            db.move_track(ids[0], Path::new("/var/m/a.flac"), 99)
                .unwrap()
        );
        let row = db.get_track(ids[0]).unwrap().unwrap().track;
        assert_eq!(row.path, PathBuf::from("/var/m/a.flac"));
        assert_eq!(row.album_key, 99);
        assert!(row.favorite);
        // A path that another row already has is refused, and nothing changes.
        assert!(
            !db.move_track(ids[0], Path::new("/var/m/taken.flac"), 1)
                .unwrap()
        );
        assert_eq!(
            db.get_track(ids[0]).unwrap().unwrap().track.path,
            PathBuf::from("/var/m/a.flac")
        );
        assert!(!db.move_track(4242, Path::new("/var/m/x.flac"), 1).unwrap());

        db.set_tracks_art(&[
            (ids[0], Some("00000000000000aa".into()), true),
            (ids[1], None, false),
        ])
        .unwrap();
        let a = db.get_track(ids[0]).unwrap().unwrap();
        assert_eq!(a.track.art_id.as_deref(), Some("00000000000000aa"));
        assert!(a.art_from_folder);
        assert_eq!(db.get_track(ids[1]).unwrap().unwrap().track.art_id, None);
    }

    #[test]
    fn values_are_bound_not_interpolated() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Db::open(&db_path(&dir)).unwrap().db;
        let nasty = "x'); DROP TABLE tracks; --";
        let mut t = sample_track("/m/'; DROP TABLE roots; --.flac", None);
        t.track.title = nasty.into();
        let id = db.upsert_tracks(&[t]).unwrap()[0];
        let playlist = db.create_playlist(nasty, 1).unwrap();
        db.kv_set(nasty, nasty).unwrap();
        assert_eq!(db.get_track(id).unwrap().unwrap().track.title, nasty);
        assert_eq!(db.playlists().unwrap(), vec![(playlist, nasty.to_string())]);
        assert_eq!(db.kv_get(nasty).unwrap().as_deref(), Some(nasty));
    }
    /// Of the copies made before upgrades the newest is kept; the older ones go when
    /// the database is opened. What is no such copy stays.
    #[test]
    fn older_copies_from_before_upgrades_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.db");
        drop(Db::open(&path).unwrap());
        for name in [
            ".bak-v1",
            ".bak-v3",
            ".bak-v5",
            ".bak-v5.partial",
            ".bak-vx",
            ".corrupt-1",
        ] {
            fs::write(sibling(&path, name), b"copy").unwrap();
        }
        drop(Db::open(&path).unwrap());
        let there = |name: &str| sibling(&path, name).exists();
        assert!(!there(".bak-v1") && !there(".bak-v3"));
        assert!(there(".bak-v5"), "the newest stays");
        assert!(there(".bak-v5.partial") && there(".bak-vx") && there(".corrupt-1"));
    }
}
