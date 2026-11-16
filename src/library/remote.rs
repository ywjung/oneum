//! Tracks of media servers on the network.
//!
//! They are not part of the library: a server is browsed as it presents itself, and
//! only what is put into the play queue is kept, so that the queue can name it and be
//! restored at the next start. Such a track has a negative id (the library's own are
//! positive), and its address travels where a file's path does ([`Track::path`]),
//! written as the `http://` address; `model::http_address` tells the two apart. Nothing
//! of the library's lists, search, statistics, favourites or playlists sees them.
use super::{Library, lock, unix_now};
use crate::model::{ReplayGainTags, Track, TrackId};
use crate::storage::db::RemoteRow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// The largest cover taken from a server.
const MAX_COVER_BYTES: u64 = 16 << 20;

/// What browsing a server tells of one of its tracks.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteTrack {
    /// Where the audio is (`http://…`).
    pub url: String,
    /// The server's name.
    pub server: String,
    pub title: String,
    pub artist: String,
    pub album_artist: String,
    pub album: String,
    pub track_no: Option<u32>,
    pub genre: String,
    /// 0 when the server does not say.
    pub duration_ms: u64,
    /// The file's kind as the app names kinds ("flac", "mp3", …).
    pub container: String,
    pub sample_rate: u32,
    pub channels: u8,
    pub bit_depth: Option<u8>,
    pub file_size: u64,
    pub art_url: Option<String>,
}

/// The servers' tracks that are kept (see the module).
#[derive(Default)]
pub(crate) struct Catalog {
    /// Replaced, not changed, on every change: the window takes a copy of the handle.
    tracks: Arc<HashMap<TrackId, Arc<Track>>>,
    by_url: HashMap<String, TrackId>,
    /// The server's name and the cover's address, by track.
    extra: HashMap<TrackId, (String, Option<String>)>,
}

impl Catalog {
    /// What was kept by the last run. The saved queue names all of it: the rest was
    /// forgotten before this is read (see [`Library::open`]).
    pub(crate) fn from_rows(rows: Vec<RemoteRow>) -> Self {
        let mut catalog = Catalog::default();
        let mut tracks = HashMap::with_capacity(rows.len());
        for row in rows {
            let id = -row.id;
            catalog.by_url.insert(row.url.clone(), id);
            catalog
                .extra
                .insert(id, (row.server.clone(), row.art_url.clone()));
            tracks.insert(id, Arc::new(track_of(&row)));
        }
        catalog.tracks = Arc::new(tracks);
        catalog
    }

    pub(crate) fn art_ids(&self) -> impl Iterator<Item = &String> {
        self.tracks.values().filter_map(|t| t.art_id.as_ref())
    }
}

/// The track as the rest of the app knows tracks. What only a file in the library has
/// (a folder, tags read from the file, counts, measurements) is empty.
fn track_of(row: &RemoteRow) -> Track {
    let path = PathBuf::from(&row.url);
    Track {
        id: -row.id,
        root_id: None,
        available: true,
        title: row.title.clone(),
        artist: row.artist.clone(),
        album_artist: row.album_artist.clone(),
        album: row.album.clone(),
        // By its names alone (no folder): the same album of the same server is one.
        album_key: super::ident::album_key(&row.album, &row.album_artist, false, &path),
        disc_no: None,
        track_no: row.track_no,
        year: None,
        genre: row.genre.clone(),
        duration_ms: row.duration_ms,
        codec: row.container.clone(),
        container: row.container.clone(),
        sample_rate: row.sample_rate,
        channels: row.channels,
        bit_depth: row.bit_depth,
        bitrate_kbps: None,
        file_size: row.file_size,
        mtime_ns: 0,
        replaygain: ReplayGainTags::default(),
        measured: None,
        unmeasurable: false,
        check: None,
        art_id: row.art_id.clone(),
        favorite: false,
        rating: 0,
        play_count: 0,
        last_played_at: None,
        added_at: row.added_at,
        path,
    }
}

impl Library {
    /// True for the id of a media server's track.
    pub fn is_remote(id: TrackId) -> bool {
        id < 0
    }

    /// Keeps tracks of a media server so that they can be queued, and returns their
    /// ids in the same order (a track that is kept already keeps its id; one that
    /// could not be kept is left out). Their covers are fetched in the background.
    pub fn add_remote(self: &Arc<Self>, tracks: &[RemoteTrack]) -> Vec<TrackId> {
        let now = unix_now();
        let mut ids = Vec::with_capacity(tracks.len());
        let mut wanted_art: Vec<(TrackId, String)> = Vec::new();
        {
            let db = lock(&self.inner.db);
            let mut catalog = lock(&self.inner.remote);
            let mut all = (*catalog.tracks).clone();
            for track in tracks {
                if crate::model::http_address(std::path::Path::new(&track.url)).is_none() {
                    continue;
                }
                let mut row = RemoteRow {
                    id: 0,
                    url: track.url.clone(),
                    server: track.server.clone(),
                    title: track.title.clone(),
                    artist: track.artist.clone(),
                    album_artist: track.album_artist.clone(),
                    album: track.album.clone(),
                    track_no: track.track_no,
                    genre: track.genre.clone(),
                    duration_ms: track.duration_ms,
                    container: track.container.clone(),
                    sample_rate: track.sample_rate,
                    channels: track.channels,
                    bit_depth: track.bit_depth,
                    file_size: track.file_size,
                    art_url: track.art_url.clone(),
                    art_id: None,
                    added_at: now,
                };
                match db.upsert_remote_track(&row) {
                    Ok(id) => row.id = id,
                    Err(err) => {
                        tracing::warn!(%err, "could not keep a server's track");
                        continue;
                    }
                }
                let id = -row.id;
                // A cover fetched before stays while the cover's address is the same.
                let known = catalog.extra.get(&id).map(|(_, art)| art.clone());
                if known.as_ref() == Some(&row.art_url) {
                    row.art_id = all.get(&id).and_then(|t| t.art_id.clone());
                }
                if row.art_id.is_none()
                    && let Some(art) = &row.art_url
                {
                    wanted_art.push((id, art.clone()));
                }
                catalog.by_url.insert(row.url.clone(), id);
                catalog
                    .extra
                    .insert(id, (row.server.clone(), row.art_url.clone()));
                all.insert(id, Arc::new(track_of(&row)));
                ids.push(id);
            }
            catalog.tracks = Arc::new(all);
        }
        self.inner.remote_generation.fetch_add(1, Ordering::Release);
        self.inner.notify();
        if !wanted_art.is_empty() {
            self.fetch_remote_art(wanted_art);
        }
        ids
    }

    /// The servers' tracks that are kept. A new handle after every change (see
    /// [`Library::remote_generation`]).
    pub fn remote_tracks(&self) -> Arc<HashMap<TrackId, Arc<Track>>> {
        Arc::clone(&lock(&self.inner.remote).tracks)
    }

    /// Bumped whenever [`Library::remote_tracks`] gives something else.
    pub fn remote_generation(&self) -> u64 {
        self.inner.remote_generation.load(Ordering::Acquire)
    }

    /// The name of the server a kept track is on.
    pub fn remote_server(&self, id: TrackId) -> Option<String> {
        lock(&self.inner.remote)
            .extra
            .get(&id)
            .map(|(server, _)| server.clone())
    }

    pub(crate) fn remote_track(&self, id: TrackId) -> Option<Arc<Track>> {
        lock(&self.inner.remote).tracks.get(&id).cloned()
    }

    /// Fetches covers from their server, one per address, and gives them to the
    /// tracks that asked. On a thread of its own: a server may be slow or gone.
    fn fetch_remote_art(self: &Arc<Self>, wanted: Vec<(TrackId, String)>) {
        let library = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("oneum-remote-art".into())
            .spawn(move || {
                let mut by_url: HashMap<String, Vec<TrackId>> = HashMap::new();
                for (id, url) in wanted {
                    by_url.entry(url).or_default().push(id);
                }
                for (url, ids) in by_url {
                    if library.inner.shutdown.load(Ordering::Relaxed) {
                        return;
                    }
                    let limits = crate::net::http::Limits::new(MAX_COVER_BYTES);
                    let art_id = match crate::net::http::get(&url, &limits) {
                        Ok(bytes) => library.inner.art.store_embedded(&bytes),
                        Err(err) => {
                            tracing::debug!(%err, "no cover from the server");
                            None
                        }
                    };
                    if let Some(art_id) = art_id {
                        library.set_remote_art(&ids, &url, &art_id);
                    }
                }
            });
        if let Err(err) = spawned {
            tracing::warn!(%err, "could not start fetching covers");
        }
    }

    fn set_remote_art(&self, ids: &[TrackId], art_url: &str, art_id: &str) {
        {
            let db = lock(&self.inner.db);
            let mut catalog = lock(&self.inner.remote);
            let mut all = (*catalog.tracks).clone();
            for id in ids {
                // Only where the cover is still the one that was asked for.
                let same = catalog
                    .extra
                    .get(id)
                    .is_some_and(|(_, art)| art.as_deref() == Some(art_url));
                let Some(track) = all.get(id).filter(|_| same) else {
                    continue;
                };
                if let Err(err) = db.set_remote_art(-id, Some(art_id)) {
                    tracing::warn!(%err, "could not keep a server track's cover");
                }
                let mut track = (**track).clone();
                track.art_id = Some(art_id.to_owned());
                all.insert(*id, Arc::new(track));
            }
            catalog.tracks = Arc::new(all);
        }
        self.inner.remote_generation.fetch_add(1, Ordering::Release);
        self.inner.art_generation.fetch_add(1, Ordering::Release);
        self.inner.notify();
    }
}
