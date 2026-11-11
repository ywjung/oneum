//! SQLite layer of the library: schema, migrations, corruption recovery and every query.
//!
//! The database is the source of truth; `crate::library` keeps an in-memory copy of the
//! track table and talks to this type through a mutex. All user-provided values are bound
//! as parameters. SQL text is only ever assembled from the constant column lists below.
use crate::model::{ReplayGainTags, Track, TrackId};
use rusqlite::{Connection, ErrorCode, OptionalExtension, Row, params};
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// File name of the library database inside the data directory.
pub const DB_FILE_NAME: &str = "library.db";

/// Number of history rows kept; older plays are trimmed when new ones are recorded.
const HISTORY_KEEP_ROWS: i64 = 20_000;

/// Ordered migration list. Entry `i` upgrades the schema from version `i` to `i + 1`;
/// the current version is stored in `PRAGMA user_version`.
const MIGRATIONS: &[&str] = &[SCHEMA_V1];

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

/// Columns filled from the file on disk (re-written on every rescan of a changed file).
const SCAN_COLS: [&str; 26] = [
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
];

/// Columns owned by the user's activity; only set when a row is first inserted.
const USER_COLS: [&str; 4] = ["favorite", "play_count", "last_played_at", "added_at"];

static SELECT_TRACKS_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "SELECT id, path, {}, {} FROM tracks",
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
    /// A migration failed. `restored` tells whether the pre-migration backup was put back.
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
                    "the previous database was restored"
                } else {
                    "the previous database could not be restored automatically"
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
}

/// A track row: the shared model type plus columns that are not part of it.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredTrack {
    pub track: Track,
    /// The file's compilation flag (input of the album identity; kept so album keys can be
    /// recomputed without re-reading files).
    pub compilation: bool,
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
        let mut conn = if path.exists() {
            match open_checked(path) {
                Ok(conn) => conn,
                Err(err) if is_busy(&err) => return Err(DbError::Sqlite(err)),
                Err(err) => {
                    tracing::warn!(error = %err, "library database is unusable; moving it aside");
                    recovered_corrupt_db = Some(move_aside(path)?);
                    open_checked(path)?
                }
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
            let backup = if fresh {
                None
            } else {
                // Fold the WAL into the main file and close it so that a plain file copy
                // is a complete backup.
                let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
                drop(conn);
                let backup = sibling(path, &format!(".bak-v{version}"));
                fs::copy(path, &backup)?;
                conn = open_checked(path)?;
                Some(backup)
            };

            for (index, sql) in migrations.iter().enumerate().skip(version as usize) {
                let target = u32::try_from(index + 1).unwrap_or(u32::MAX);
                if let Err(err) = apply_migration(&mut conn, sql, target) {
                    drop(conn);
                    let restored = match &backup {
                        Some(backup) => match restore_backup(path, backup) {
                            Ok(()) => true,
                            Err(restore_err) => {
                                tracing::error!(error = %restore_err, "could not restore database backup");
                                false
                            }
                        },
                        None => {
                            // Nothing to lose: remove the half-initialised file.
                            remove_db_files(path);
                            false
                        }
                    };
                    return Err(DbError::MigrationFailed {
                        from: version,
                        to: target,
                        message: err.to_string(),
                        restored,
                    });
                }
            }
            if !fresh {
                migrated_from = Some(version);
                tracing::info!(from = version, to = latest, "library database upgraded");
            }
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
            .prepare("SELECT id, path, last_scan_at FROM roots ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            Ok(RootRow {
                id: row.get(0)?,
                path: blob_to_path(row.get(1)?),
                last_scan_at: row.get(2)?,
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
    pub fn delete_root(&self, id: i64) -> SqlResult<bool> {
        let n = self
            .conn
            .execute("DELETE FROM roots WHERE id = ?1", params![id])?;
        Ok(n > 0)
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
                "DELETE FROM tracks WHERE id = ?1 AND favorite = 0 \
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

    /// Makes row `keep` point at the file described by row `take` (path and every
    /// file-derived column) and deletes row `take`. Id, favourite, play counts, history
    /// and playlist membership of `keep` are preserved. Returns the updated row.
    pub fn relink_track(&mut self, keep: TrackId, take: TrackId) -> SqlResult<Option<StoredTrack>> {
        let Some(source) = self.get_track(take)? else {
            return Ok(None);
        };
        let Some(target) = self.get_track(keep)? else {
            return Ok(None);
        };
        let tx = self.conn.transaction()?;
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
            ],
        )?;
        if source.track.favorite && !target.track.favorite {
            tx.execute(
                "UPDATE tracks SET favorite = 1 WHERE id = ?1",
                params![keep],
            )?;
        }
        tx.commit()?;
        self.get_track(keep)
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
        let mut position: i64 = tx.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM playlist_items WHERE playlist_id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        let mut added = 0;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO playlist_items (playlist_id, track_id, position) \
                 SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM tracks WHERE id = ?2)",
            )?;
            for track in tracks {
                let n = stmt.execute(params![id, track, position])?;
                if n > 0 {
                    position += 1;
                    added += 1;
                }
            }
        }
        tx.commit()?;
        Ok(added)
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
        self.conn.execute(
            "INSERT INTO kv (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Replaces the saved play queue. Entries whose track does not exist are skipped.
    pub fn save_queue(&mut self, items: &[(u64, TrackId)]) -> SqlResult<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM queue", [])?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO queue (position, item_id, track_id) \
                 SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM tracks WHERE id = ?3)",
            )?;
            for (position, (item_id, track_id)) in items.iter().enumerate() {
                stmt.execute(params![
                    to_i64(position as u64),
                    item_id.cast_signed(),
                    track_id
                ])?;
            }
        }
        tx.commit()
    }

    pub fn load_queue(&self) -> SqlResult<Vec<(u64, TrackId)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT item_id, track_id FROM queue ORDER BY position")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?.cast_unsigned(), r.get(1)?)))?;
        rows.collect()
    }
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
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(conn)
}

fn is_busy(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(e, _)
            if e.code == ErrorCode::DatabaseBusy || e.code == ErrorCode::DatabaseLocked
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

fn apply_migration(conn: &mut Connection, sql: &str, target_version: u32) -> SqlResult<()> {
    let tx = conn.transaction()?;
    tx.execute_batch(sql)?;
    tx.pragma_update(None, "user_version", target_version)?;
    tx.commit()
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

fn remove_db_files(path: &Path) {
    let _ = fs::remove_file(path);
    for suffix in ["-wal", "-shm"] {
        let _ = fs::remove_file(sibling(path, suffix));
    }
}

fn restore_backup(path: &Path, backup: &Path) -> io::Result<()> {
    remove_db_files(path);
    fs::copy(backup, path)?;
    Ok(())
}

/// `path` with `suffix` appended to its file name.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn path_to_blob(path: &Path) -> &[u8] {
    path.as_os_str().as_bytes()
}

fn blob_to_path(blob: Vec<u8>) -> PathBuf {
    PathBuf::from(OsString::from_vec(blob))
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
            art_id: row.get(27)?,
            favorite: row.get(28)?,
            play_count: clamp_u32(row.get(29)?),
            last_played_at: row.get(30)?,
            added_at: row.get(31)?,
        },
        compilation: row.get(9)?,
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
                art_id: Some("0123456789abcdef".into()),
                favorite: false,
                play_count: 0,
                last_played_at: None,
                added_at: 1_700_000_000,
            },
            compilation: true,
        }
    }

    fn db_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join(DB_FILE_NAME)
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
        {
            let mut db = Db::open_with_migrations(&path, &[SCHEMA_V1]).unwrap().db;
            db.upsert_tracks(&[sample_track("/m/a.flac", None)])
                .unwrap();
        }
        let v2 = "ALTER TABLE tracks ADD COLUMN rating INTEGER NOT NULL DEFAULT 0;";
        let opened = Db::open_with_migrations(&path, &[SCHEMA_V1, v2]).unwrap();
        assert_eq!(opened.migrated_from, Some(1));
        assert_eq!(opened.db.schema_version().unwrap(), 2);
        assert_eq!(opened.db.load_tracks().unwrap().len(), 1);
        let backup = sibling(&path, ".bak-v1");
        assert!(backup.exists());
        // The backup is a complete v1 database.
        let old = Connection::open(&backup).unwrap();
        let v: u32 = old
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, 1);
        let n: i64 = old
            .query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn failing_migration_restores_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_path(&dir);
        {
            let mut db = Db::open_with_migrations(&path, &[SCHEMA_V1]).unwrap().db;
            db.upsert_tracks(&[sample_track("/m/a.flac", None)])
                .unwrap();
            db.kv_set("marker", "before").unwrap();
        }
        // The first statement succeeds, the second fails: the whole step must roll back.
        let broken = "ALTER TABLE tracks ADD COLUMN rating INTEGER; \
                      INSERT INTO table_that_does_not_exist VALUES (1);";
        let err = match Db::open_with_migrations(&path, &[SCHEMA_V1, broken]) {
            Ok(_) => panic!("migration must fail"),
            Err(err) => err,
        };
        match err {
            DbError::MigrationFailed {
                from, to, restored, ..
            } => {
                assert_eq!((from, to), (1, 2));
                assert!(restored);
            }
            other => panic!("unexpected error: {other}"),
        }
        assert!(sibling(&path, ".bak-v1").exists());

        // The original database is intact and opens with the old migration list.
        let opened = Db::open_with_migrations(&path, &[SCHEMA_V1]).unwrap();
        assert!(opened.recovered_corrupt_db.is_none());
        assert!(opened.migrated_from.is_none());
        assert_eq!(opened.db.schema_version().unwrap(), 1);
        assert_eq!(opened.db.load_tracks().unwrap().len(), 1);
        assert_eq!(
            opened.db.kv_get("marker").unwrap().as_deref(),
            Some("before")
        );
    }

    #[test]
    fn newer_schema_is_refused_and_left_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_path(&dir);
        {
            let v2 = "CREATE TABLE future (x INTEGER);";
            Db::open_with_migrations(&path, &[SCHEMA_V1, v2]).unwrap();
        }
        match Db::open_with_migrations(&path, &[SCHEMA_V1]) {
            Err(DbError::NewerSchema { found, supported }) => {
                assert_eq!((found, supported), (2, 1));
            }
            Err(other) => panic!("unexpected error: {other}"),
            Ok(_) => panic!("must refuse a newer schema"),
        }
        assert!(path.exists());
        assert!(Db::open_with_migrations(&path, &[SCHEMA_V1, "SELECT 1;"]).is_ok());
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
        db.save_queue(&[(7, ids[0]), (8, 9999), (u64::MAX, ids[2])])
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
        assert!(db.delete_root(root).unwrap());
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
}
