//! Listening statistics from the app's own play history, and what the library is made
//! of. Nothing leaves the computer.
use super::snapshot::LibrarySnapshot;
use crate::model::{CheckVerdict, TrackId};
use std::collections::HashMap;

/// How many entries each "most played" list holds.
pub const TOP: usize = 10;

/// What was listened to in a period.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ListeningStats {
    /// Plays counted (see the player's play credit).
    pub plays: usize,
    /// The lengths of the songs played, added up.
    pub listened_ms: u64,
    /// Different songs played.
    pub songs: usize,
    pub top_artists: Vec<(String, usize)>,
    /// (album key, title, artist, plays)
    pub top_albums: Vec<(u64, String, String, usize)>,
    pub top_genres: Vec<(String, usize)>,
    pub top_songs: Vec<(TrackId, usize)>,
}

/// What the library is made of.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LibraryMakeup {
    pub songs: usize,
    pub total_ms: u64,
    pub total_bytes: u64,
    pub lossless: usize,
    /// Codec names with their song counts, most first.
    pub formats: Vec<(String, usize)>,
    /// Sample rates (Hz, as played) with their song counts, highest first.
    pub rates: Vec<(u32, usize)>,
    pub checked: usize,
    pub damaged: usize,
}

/// The genres named in a genre tag ("Rock; Indie / Alternative"), as the genre view
/// splits them.
fn genre_names_in(tag: &str) -> impl Iterator<Item = &str> {
    tag.split([';', ',', '/', '|'])
        .map(str::trim)
        .filter(|genre| !genre.is_empty())
}

/// Most first; ties in name order, so that the lists do not jump around.
fn ranked<K: Clone + Ord>(counts: HashMap<K, usize>, limit: usize) -> Vec<(K, usize)> {
    let mut all: Vec<(K, usize)> = counts.into_iter().collect();
    all.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    all.truncate(limit);
    all
}

/// Statistics of the given plays (`(track, unix seconds)`), for the tracks that are
/// still in the library.
pub fn listening(snapshot: &LibrarySnapshot, plays: &[(TrackId, i64)]) -> ListeningStats {
    let mut stats = ListeningStats::default();
    let mut songs: HashMap<TrackId, usize> = HashMap::new();
    let mut artists: HashMap<String, usize> = HashMap::new();
    let mut albums: HashMap<u64, usize> = HashMap::new();
    let mut genres: HashMap<String, usize> = HashMap::new();
    // Different spellings of a genre count as one: the first one met names it.
    let mut genre_names: HashMap<String, String> = HashMap::new();
    for &(id, _) in plays {
        let Some(track) = snapshot.get(id) else {
            continue;
        };
        stats.plays += 1;
        stats.listened_ms += track.duration_ms;
        *songs.entry(id).or_default() += 1;
        let artist = if track.album_artist.is_empty() {
            &track.artist
        } else {
            &track.album_artist
        };
        if !artist.is_empty() {
            *artists.entry(artist.clone()).or_default() += 1;
        }
        // The album as the library shows it (tracks tagged inconsistently are merged
        // into one album there, and counted as one here).
        *albums
            .entry(snapshot.shown_album_key(track.album_key))
            .or_default() += 1;
        for genre in genre_names_in(&track.genre) {
            let key = genre.to_lowercase();
            genre_names
                .entry(key.clone())
                .or_insert_with(|| genre.to_string());
            *genres.entry(key).or_default() += 1;
        }
    }
    stats.songs = songs.len();
    stats.top_artists = ranked(artists, TOP);
    stats.top_genres = ranked(genres, TOP)
        .into_iter()
        .map(|(key, n)| (genre_names.get(&key).cloned().unwrap_or(key), n))
        .collect();
    stats.top_songs = ranked(songs, TOP);
    stats.top_albums = ranked(albums, TOP)
        .into_iter()
        .filter_map(|(key, n)| {
            let album = snapshot
                .album_index(key)
                .and_then(|i| snapshot.albums().get(i as usize))?;
            Some((key, album.title.clone(), album.artist.clone(), n))
        })
        .collect();
    stats
}

/// What the library is made of.
pub fn makeup(snapshot: &LibrarySnapshot) -> LibraryMakeup {
    let mut makeup = LibraryMakeup::default();
    let mut formats: HashMap<String, usize> = HashMap::new();
    let mut rates: HashMap<u32, usize> = HashMap::new();
    // The tracks of a CUE sheet share one file (each records its size): counted once.
    let mut sheets = std::collections::HashSet::new();
    for track in &snapshot.tracks {
        makeup.songs += 1;
        makeup.total_ms += track.duration_ms;
        if super::cue::sheet_of(&track.path).is_none_or(|sheet| sheets.insert(sheet)) {
            makeup.total_bytes += track.file_size;
        }
        makeup.lossless += usize::from(track.is_lossless());
        *formats.entry(track.codec.clone()).or_default() += 1;
        let rate = track.playback_rate();
        if rate > 0 {
            *rates.entry(rate).or_default() += 1;
        }
        if let Some(check) = track.check {
            makeup.checked += 1;
            makeup.damaged += usize::from(check.verdict == CheckVerdict::Damaged);
        }
    }
    makeup.formats = ranked(formats, usize::MAX);
    let mut rates: Vec<(u32, usize)> = rates.into_iter().collect();
    rates.sort_by_key(|r| std::cmp::Reverse(r.0));
    makeup.rates = rates;
    makeup
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::test_track;

    fn snapshot() -> LibrarySnapshot {
        let mut tracks = Vec::new();
        for (id, artist, album, genre, codec, rate) in [
            (1, "아이유", "Palette", "K-Pop; Ballad", "FLAC", 44_100),
            (2, "아이유", "Palette", "k-pop", "FLAC", 44_100),
            (3, "The Beatles", "Abbey Road", "Rock", "MP3", 44_100),
            (4, "Miles Davis", "Kind of Blue", "Jazz", "FLAC", 96_000),
        ] {
            let mut t = test_track(id);
            t.artist = artist.into();
            t.album = album.into();
            t.album_key = album.len() as u64;
            t.genre = genre.into();
            t.codec = codec.into();
            t.sample_rate = rate;
            t.duration_ms = 200_000;
            t.file_size = 1_000;
            tracks.push(t);
        }
        LibrarySnapshot::from_tracks(tracks)
    }

    #[test]
    fn plays_add_up_per_song_artist_album_and_genre() {
        let snap = snapshot();
        let plays = [(1, 10), (1, 20), (2, 30), (3, 40), (999, 50)];
        let stats = listening(&snap, &plays);
        assert_eq!(
            stats.plays, 4,
            "a song no longer in the library is left out"
        );
        assert_eq!(stats.listened_ms, 800_000);
        assert_eq!(stats.songs, 3);
        assert_eq!(stats.top_songs, [(1, 2), (2, 1), (3, 1)]);
        assert_eq!(
            stats.top_artists,
            [("아이유".to_string(), 3), ("The Beatles".to_string(), 1)]
        );
        assert_eq!(stats.top_albums[0].1, "Palette");
        assert_eq!(stats.top_albums[0].3, 3);
        // "K-Pop" and "k-pop" are one genre, named as first met.
        assert_eq!(stats.top_genres[0], ("K-Pop".to_string(), 3));
        assert!(stats.top_genres.contains(&("Ballad".to_string(), 2)));
        assert_eq!(listening(&snap, &[]), ListeningStats::default());
    }

    #[test]
    fn what_the_library_is_made_of() {
        let makeup = makeup(&snapshot());
        assert_eq!(makeup.songs, 4);
        assert_eq!(makeup.total_ms, 800_000);
        assert_eq!(makeup.total_bytes, 4_000);
        assert_eq!(
            makeup.formats,
            [("FLAC".to_string(), 3), ("MP3".to_string(), 1)]
        );
        assert_eq!(makeup.rates, [(96_000, 1), (44_100, 3)]);
        assert_eq!(makeup.lossless, 3);
    }

    /// The tracks of a CUE sheet are one file: its size counts once. Plays of an album
    /// the library shows as one (merged keys) count as one album.
    #[test]
    fn a_sheet_counts_once_and_merged_albums_are_one() {
        let mut tracks = Vec::new();
        for (id, path, album_artist) in [
            (1, "/m/Live/concert.cue/01", "Band"),
            (2, "/m/Live/concert.cue/02", "Band"),
            (3, "/m/Live/concert.cue/03", ""),
        ] {
            let mut t = test_track(id);
            t.path = path.into();
            t.album = "Live".into();
            t.album_artist = album_artist.into();
            t.artist = "Band".into();
            // The tagged tracks share a key; the untagged one has its folder's.
            t.album_key = if album_artist.is_empty() {
                3_000
            } else {
                1_000
            };
            t.file_size = 500_000_000;
            tracks.push(t);
        }
        let snap = LibrarySnapshot::from_tracks(tracks);
        assert_eq!(makeup(&snap).total_bytes, 500_000_000);
        assert_eq!(snap.albums().len(), 1, "the library shows one album");
        let stats = listening(&snap, &[(1, 0), (2, 0), (3, 0)]);
        assert_eq!(stats.top_albums.len(), 1, "{:?}", stats.top_albums);
        assert_eq!(stats.top_albums[0].3, 3);
    }
}
