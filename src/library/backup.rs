//! The app's own library data as one file, to keep or to take to another device: what
//! was listened to (play counts, last plays, the plays the statistics go by), dates
//! added, favourites, ratings, playlists and saved searches.
//!
//! The file names a song by the end of its path, from its music folder's own name on
//! ("Music/Artist/Album/01 Song.flac"), not by where that folder lies: it fits a copy
//! of the music on another computer or on a phone, and nothing in it tells of a place
//! on the device it was written on. Taking a file in merges, as the Apple Music import
//! does: the higher count, the later play, the earlier date added, a rating where the
//! song has none here; twice changes nothing more. Music files are neither read nor changed by any of this.
use super::{Library, LibraryError, apple, lock};
use crate::model::TrackId;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path};
use unicode_normalization::UnicodeNormalization;

/// A file larger than this is not library data.
pub const MAX_FILE_BYTES: u64 = 256 << 20;
/// What the file says it is.
const FORMAT: &str = "oneum-library-data";
/// The version this build writes, and the newest it reads.
pub const VERSION: u32 = 1;
/// What a playlist is marked with whose name is taken by another list.
const MARK: &str = "2";

/// The file as a whole.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LibraryData {
    pub format: String,
    pub version: u32,
    /// Unix seconds.
    pub written_at: i64,
    pub songs: Vec<Song>,
    /// The plays that are remembered one by one: `(index into songs, unix seconds)`,
    /// oldest first.
    #[serde(default)]
    pub plays: Vec<(u32, i64)>,
    #[serde(default)]
    pub playlists: Vec<Playlist>,
    #[serde(default)]
    pub searches: Vec<Search>,
}

/// What the library knows of one song beyond what its file says.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Song {
    /// The song's file, as [`song_key`] names it.
    pub file: String,
    /// For someone who reads the file; the song is not found by it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub plays: u32,
    /// Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_played: Option<i64>,
    /// Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added: Option<i64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub favorite: bool,
    /// 1 to 5 stars; 0 (left out): none.
    #[serde(default, skip_serializing_if = "is_no_rating")]
    pub rating: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Playlist {
    pub name: String,
    /// Indexes into the file's songs, in the playlist's order. Empty: the list was
    /// empty where the file was written (a list whose songs the file does not hold is
    /// not written at all).
    pub songs: Vec<u32>,
}

/// A saved search ("smart playlist").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Search {
    pub name: String,
    pub query: String,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

fn is_false(flag: &bool) -> bool {
    !*flag
}

fn is_no_rating(rating: &u8) -> bool {
    *rating == 0
}

/// Why a file was not taken in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unreadable {
    /// Not the app's library data.
    NotData,
    /// Written by a later version of the app, in a form this one does not know.
    Newer,
}

/// What an export wrote.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DataExport {
    pub songs: usize,
    pub playlists: usize,
}

/// What an import did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DataImport {
    /// Songs of the file that are in the library.
    pub matched: usize,
    /// Songs of the file that are not.
    pub unmatched: usize,
    /// Songs of the file that were left alone because their name fits more than one
    /// song, of the file or of the library (two music folders of the same name, say):
    /// which is which cannot be told, and what one of them was given must not go to
    /// the other.
    pub ambiguous: usize,
    /// Playlists made.
    pub playlists: usize,
    /// Playlists of the file none of whose songs is in the library: not made.
    pub playlists_without_songs: usize,
    /// The file's saved searches: the settings keep those, so they are for the caller
    /// to take in.
    pub searches: Vec<Search>,
}

/// How the file names a song: its path below its music folder, the folder's own name
/// first, with `/` between the parts ("Music/Artist/Album/01 Song.flac"). A song of no
/// music folder (a file that was opened by itself) is named by its file's name
/// alone: the folders above it may tell of the person or the device.
pub fn song_key(root: Option<&Path>, path: &Path) -> String {
    let parts = |path: &Path| -> Vec<String> {
        path.components()
            .filter_map(|part| match part {
                Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect()
    };
    let all = parts(path);
    let kept = match root.and_then(|root| Some((root, path.strip_prefix(root).ok()?))) {
        Some((root, below)) => parts(below).len() + usize::from(root.file_name().is_some()),
        None => 1,
    };
    all[all.len().saturating_sub(kept)..].join("/")
}

/// Reads the file. Songs, plays and playlists that point at nothing are left out (a
/// playlist that was empty where the file was written is kept).
pub fn parse(bytes: &[u8]) -> Result<LibraryData, Unreadable> {
    /// What every version of the file begins with.
    #[derive(Deserialize)]
    struct Head {
        format: String,
        version: u32,
    }
    let head: Head = serde_json::from_slice(bytes).map_err(|_| Unreadable::NotData)?;
    if head.format != FORMAT || head.version == 0 {
        return Err(Unreadable::NotData);
    }
    if head.version > VERSION {
        return Err(Unreadable::Newer);
    }
    let mut data: LibraryData = serde_json::from_slice(bytes).map_err(|_| Unreadable::NotData)?;
    let songs = data.songs.len();
    let known = |index: &u32| (*index as usize) < songs;
    data.plays.retain(|(song, _)| known(song));
    data.playlists.retain_mut(|playlist| {
        let empty = playlist.songs.is_empty();
        playlist.songs.retain(known);
        !playlist.name.trim().is_empty() && (empty || !playlist.songs.is_empty())
    });
    data.searches
        .retain(|search| !search.name.trim().is_empty() && !search.query.trim().is_empty());
    Ok(data)
}

fn unreadable(why: Unreadable) -> LibraryError {
    let (kind, text) = match why {
        Unreadable::NotData => (
            std::io::ErrorKind::InvalidData,
            "not the app's library data",
        ),
        Unreadable::Newer => (
            std::io::ErrorKind::Unsupported,
            "library data of a later version of the app",
        ),
    };
    LibraryError::Io(std::io::Error::new(kind, text))
}

/// Whether an import failed because the file is of a later version of the app (as
/// against a file that is not library data at all, or cannot be read).
pub fn is_of_a_later_version(error: &LibraryError) -> bool {
    matches!(error, LibraryError::Io(error) if error.kind() == std::io::ErrorKind::Unsupported)
}

fn composed(text: &str) -> String {
    text.nfc().collect()
}

impl Library {
    /// Every song of the library with the name the file gives it ([`song_key`]), in
    /// the order of those names.
    fn songs_by_key(&self) -> Vec<(TrackId, String, std::sync::Arc<crate::model::Track>)> {
        let state = lock(&self.inner.state);
        let roots: HashMap<i64, &Path> = state
            .roots
            .iter()
            .map(|root| (root.id, root.path.as_path()))
            .collect();
        let mut songs: Vec<_> = state
            .tracks
            .values()
            .filter(|entry| crate::model::http_address(&entry.track.path).is_none())
            .map(|entry| {
                let track = &entry.track;
                let root = track.root_id.and_then(|id| roots.get(&id).copied());
                (
                    track.id,
                    song_key(root, &track.path),
                    std::sync::Arc::clone(track),
                )
            })
            .collect();
        songs.sort_by(|a, b| a.1.cmp(&b.1));
        songs
    }

    /// Writes the library's own data to `dest` (see the module). `searches` are the
    /// saved searches, which the settings keep. Blocking.
    pub fn export_data(
        &self,
        dest: &Path,
        searches: &[Search],
    ) -> Result<DataExport, LibraryError> {
        let ours = self.songs_by_key();
        let index: HashMap<TrackId, u32> = ours
            .iter()
            .enumerate()
            .map(|(at, (id, _, _))| (*id, at as u32))
            .collect();
        let songs: Vec<Song> = ours
            .iter()
            .map(|(_, key, track)| Song {
                file: key.clone(),
                title: track.title.clone(),
                plays: track.play_count,
                last_played: track.last_played_at,
                added: Some(track.added_at),
                favorite: track.favorite,
                rating: track.rating,
            })
            .collect();
        let plays: Vec<(u32, i64)> = lock(&self.inner.db)
            .plays_since(i64::MIN)?
            .into_iter()
            .filter_map(|(id, at)| Some((*index.get(&id)?, at)))
            .collect();
        // A list of songs the file does not hold (those of a media server) is left out:
        // in the file it would be a list that was empty, and come back as one.
        let playlists: Vec<Playlist> = lock(&self.inner.playlists)
            .iter()
            .filter_map(|playlist| {
                let songs: Vec<u32> = playlist
                    .items
                    .iter()
                    .filter_map(|item| index.get(&item.track_id).copied())
                    .collect();
                (!songs.is_empty() || playlist.items.is_empty()).then(|| Playlist {
                    name: playlist.name.clone(),
                    songs,
                })
            })
            .collect();
        let data = LibraryData {
            format: FORMAT.into(),
            version: VERSION,
            written_at: super::unix_now(),
            songs,
            plays,
            playlists,
            searches: searches.to_vec(),
        };
        let bytes = serde_json::to_vec(&data)
            .map_err(|error| LibraryError::Io(std::io::Error::other(error)))?;
        super::write_atomic(dest, &bytes)?;
        let written = DataExport {
            songs: data.songs.len(),
            playlists: data.playlists.len(),
        };
        tracing::info!(
            songs = written.songs,
            playlists = written.playlists,
            "library data written"
        );
        Ok(written)
    }

    /// Takes in library data that [`Self::export_data`] wrote, here or on another
    /// device. A song is found by the name the file gives it; failing that by the end
    /// of that name (the folders it lies in and its file name, see
    /// [`apple::match_by_tail`]). Either way only where the name, or its end, belongs
    /// to one song of the file and one here: two music folders of the same name give
    /// two songs one name, and neither is taken for the other. What the file knows is
    /// merged into what the library knows, and the playlists are made (one that was
    /// empty too); songs that are not in the library, and those that cannot be told
    /// apart, are counted, not added. Blocking.
    pub fn import_data(&self, file: &Path) -> Result<DataImport, LibraryError> {
        let bytes = crate::storage::files::read_bounded(file, MAX_FILE_BYTES)?
            .ok_or_else(|| unreadable(Unreadable::NotData))?;
        let data = parse(&bytes).map_err(unreadable)?;
        drop(bytes);

        let ours = self.songs_by_key();
        // A name that two songs answer to, here or in the file, finds none of them.
        let mut by_key: HashMap<String, Option<TrackId>> = HashMap::new();
        for (id, key, _) in &ours {
            by_key
                .entry(composed(key))
                .and_modify(|one| *one = None)
                .or_insert(Some(*id));
        }
        let keys: Vec<String> = data.songs.iter().map(|song| composed(&song.file)).collect();
        let mut in_file: HashMap<&str, usize> = HashMap::new();
        for key in &keys {
            *in_file.entry(key).or_default() += 1;
        }
        let mut found: HashMap<u32, TrackId> = HashMap::new();
        let mut doubtful: HashSet<u32> = HashSet::new();
        for (at, key) in keys.iter().enumerate() {
            match by_key.get(key) {
                Some(Some(id)) if in_file[key.as_str()] == 1 => {
                    found.insert(at as u32, *id);
                }
                Some(_) => {
                    doubtful.insert(at as u32);
                }
                None => {}
            }
        }
        let taken: HashSet<TrackId> = found.values().copied().collect();
        let theirs: Vec<(u32, &Path)> = data
            .songs
            .iter()
            .enumerate()
            .filter(|(at, _)| !found.contains_key(&(*at as u32)))
            .map(|(at, song)| (at as u32, Path::new(&song.file)))
            .collect();
        let rest: Vec<(TrackId, &Path)> = ours
            .iter()
            .filter(|(id, _, _)| !taken.contains(id))
            .map(|(id, key, _)| (*id, Path::new(key)))
            .collect();
        // (The songs whose name was in doubt go in as well: their ends are as much in
        // doubt, and no other song of the file is to be taken for theirs.)
        let tails = apple::tails(&theirs, &rest);
        found.extend(tails.found);
        doubtful.extend(tails.doubtful);
        doubtful.retain(|at| !found.contains_key(at));

        let rows: Vec<crate::storage::db::ListeningRow> = data
            .songs
            .iter()
            .enumerate()
            .filter_map(|(at, song)| {
                let id = *found.get(&(at as u32))?;
                Some((
                    id,
                    song.plays,
                    song.last_played,
                    song.added,
                    song.favorite,
                    song.rating,
                ))
            })
            .collect();
        let playlists: Vec<super::ListIn> = data
            .playlists
            .iter()
            .map(|playlist| super::ListIn {
                name: playlist.name.clone(),
                tracks: playlist
                    .songs
                    .iter()
                    .filter_map(|song| found.get(song).copied())
                    .collect(),
                empty_there: playlist.songs.is_empty(),
            })
            .collect();
        let playlists_without_songs = playlists
            .iter()
            .filter(|list| !list.empty_there && list.tracks.is_empty())
            .count();
        let made = self.take_over(&rows, &playlists, MARK)?;

        // The plays themselves, for the statistics and for what was heard last.
        let plays: Vec<(TrackId, i64)> = data
            .plays
            .iter()
            .filter_map(|(song, at)| Some((*found.get(song)?, *at)))
            .collect();
        if !plays.is_empty() {
            let mut db = lock(&self.inner.db);
            if db.merge_plays(&plays)? > 0 {
                *lock(&self.inner.recent_plays) = db.recent_plays(super::RECENT_PLAYS_KEPT)?;
            }
        }

        let (matched, ambiguous) = (found.len(), doubtful.len());
        tracing::info!(
            matched,
            ambiguous,
            playlists = made,
            "library data taken in"
        );
        Ok(DataImport {
            matched,
            unmatched: data.songs.len() - matched - ambiguous,
            ambiguous,
            playlists: made,
            playlists_without_songs,
            searches: data.searches,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_song_is_named_from_its_music_folder_on() {
        let root = Path::new("/Users/someone/Music");
        assert_eq!(
            song_key(
                Some(root),
                Path::new("/Users/someone/Music/아이유/앨범/01 곡.flac")
            ),
            "Music/아이유/앨범/01 곡.flac"
        );
        // Right below the folder.
        assert_eq!(
            song_key(Some(root), Path::new("/Users/someone/Music/a.mp3")),
            "Music/a.mp3"
        );
        // A folder without a name of its own (the top of a drive).
        assert_eq!(
            song_key(Some(Path::new("/")), Path::new("/Artist/Album/a.mp3")),
            "Artist/Album/a.mp3"
        );
        // Of no music folder, or of one it does not lie in: its file's name alone
        // (the folders above it are the person's, not the library's).
        assert_eq!(
            song_key(None, Path::new("/Volumes/Stick/x/y/z/a.mp3")),
            "a.mp3"
        );
        assert_eq!(
            song_key(Some(root), Path::new("/elsewhere/b/a.mp3")),
            "a.mp3"
        );
    }

    fn sample() -> LibraryData {
        LibraryData {
            format: FORMAT.into(),
            version: VERSION,
            written_at: 1_800_000_000,
            songs: vec![
                Song {
                    file: "Music/A/B/1.flac".into(),
                    title: "One".into(),
                    plays: 3,
                    last_played: Some(1_700_000_000),
                    added: Some(1_600_000_000),
                    favorite: true,
                    rating: 4,
                },
                Song {
                    file: "Music/A/B/2.flac".into(),
                    ..Song::default()
                },
            ],
            plays: vec![(0, 1_700_000_000)],
            playlists: vec![Playlist {
                name: "드라이브".into(),
                songs: vec![1, 0],
            }],
            searches: vec![Search {
                name: "Loved".into(),
                query: "fav:yes".into(),
            }],
        }
    }

    #[test]
    fn what_is_written_is_read_back_the_same() {
        let data = sample();
        let bytes = serde_json::to_vec(&data).unwrap();
        assert_eq!(parse(&bytes), Ok(data));
        // Nothing of a song that has nothing to tell but its name.
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains(r#"{"file":"Music/A/B/2.flac"}"#), "{text}");
    }

    #[test]
    fn what_is_not_library_data_is_refused_and_a_later_version_is_told_apart() {
        for text in [
            "",
            "{}",
            "[1,2,3]",
            r#"{"format":"something-else","version":1,"written_at":0,"songs":[]}"#,
            r#"{"format":"oneum-library-data","version":0,"written_at":0,"songs":[]}"#,
            r#"{"format":"oneum-library-data","version":1}"#,
            r#"{"format":"oneum-library-data","version":1,"written_at":0,"songs":"none"}"#,
        ] {
            assert_eq!(parse(text.as_bytes()), Err(Unreadable::NotData), "{text}");
        }
        let later = r#"{"format":"oneum-library-data","version":2,"something":"new"}"#;
        assert_eq!(parse(later.as_bytes()), Err(Unreadable::Newer));
        assert!(is_of_a_later_version(&unreadable(Unreadable::Newer)));
        assert!(!is_of_a_later_version(&unreadable(Unreadable::NotData)));
    }

    #[test]
    fn what_points_at_nothing_is_left_out() {
        let text = r#"{"format":"oneum-library-data","version":1,"written_at":5,
            "songs":[{"file":"a/b/c.mp3","plays":2}],
            "plays":[[0,10],[7,11]],
            "playlists":[{"name":"Kept","songs":[0,9,0]},{"name":"Gone","songs":[4]},{"name":" ","songs":[0]},{"name":"Nothing yet","songs":[]}],
            "searches":[{"name":"","query":"x"},{"name":"n","query":" "},{"name":"n","query":"q"}],
            "later":"fields of a later build are passed over"}"#;
        let data = parse(text.as_bytes()).expect("reads");
        assert_eq!(data.plays, [(0, 10)]);
        // A list whose songs are all gone is left out; one that was empty is kept.
        let names: Vec<&str> = data.playlists.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Kept", "Nothing yet"]);
        assert_eq!(data.playlists[0].songs, [0, 0]);
        assert!(data.playlists[1].songs.is_empty());
        assert_eq!(
            data.searches,
            [Search {
                name: "n".into(),
                query: "q".into()
            }]
        );
    }
}
