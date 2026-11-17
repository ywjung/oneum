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

/// Days from 1970-01-01 to the given day of the calendar (Gregorian, also before 1970).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let year_of_era = y.rem_euclid(400);
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The year a moment (Unix seconds) falls in, where local time is `offset_secs` ahead
/// of UTC.
pub fn year_of(unix_secs: i64, offset_secs: i32) -> i32 {
    let days = (unix_secs + i64::from(offset_secs)).div_euclid(86_400);
    // Civil from days (the same reckoning as `days_from_civil`, the other way round).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let year = year_of_era + era * 400 + i64::from(month_index >= 10);
    year as i32
}

/// A calendar year as a span of Unix seconds, from its first moment to the first of
/// the next year (not included), where local time is `offset_secs` ahead of UTC.
pub fn year_span(year: i32, offset_secs: i32) -> (i64, i64) {
    year_span_by(year, |_| offset_secs)
}

/// [`year_span`] where local time is not the same distance from UTC all year (clocks
/// moved for the summer): `offset_at` tells it for a moment. A year begins at
/// midnight by the clock of that night, not by today's.
pub fn year_span_by(year: i32, offset_at: impl Fn(i64) -> i32) -> (i64, i64) {
    let start = |year: i32| {
        let by_utc = days_from_civil(i64::from(year), 1, 1) * 86_400;
        // (The offset of that night, asked for at about that night: no clock is
        // moved within hours of the new year.)
        let about = by_utc - i64::from(offset_at(by_utc));
        by_utc - i64::from(offset_at(about))
    };
    (start(year), start(year + 1))
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

    #[test]
    fn a_year_begins_and_ends_where_the_clock_on_the_wall_says() {
        // 2026-01-01 00:00:00 UTC is 1,767,225,600.
        assert_eq!(year_span(2026, 0), (1_767_225_600, 1_798_761_600));
        // In Seoul the year begins nine hours earlier.
        let (start, end) = year_span(2026, 9 * 3600);
        assert_eq!(start, 1_767_225_600 - 32_400);
        assert_eq!(end - start, 365 * 86_400);
        assert_eq!(year_of(start, 9 * 3600), 2026);
        assert_eq!(year_of(start - 1, 9 * 3600), 2025);
        assert_eq!(year_of(end - 1, 9 * 3600), 2026);
        assert_eq!(year_of(end, 9 * 3600), 2027);
        // The same moment is another year further west.
        assert_eq!(year_of(1_767_225_600, 0), 2026);
        assert_eq!(year_of(1_767_225_600, -5 * 3600), 2025);
        // Leap years, and days before 1970.
        assert_eq!(year_span(2024, 0).1 - year_span(2024, 0).0, 366 * 86_400);
        // Where the clocks are moved for the summer (New York: five hours behind in
        // winter, four in summer), a year begins by the clock of its first night,
        // whenever it is looked at.
        let new_york = |at: i64| {
            let (march, november) = (1_772_953_200, 1_793_512_800);
            if (march..november).contains(&at) {
                -4 * 3600
            } else {
                -5 * 3600
            }
        };
        assert_eq!(
            year_span_by(2026, new_york),
            (1_767_225_600 + 5 * 3600, 1_798_761_600 + 5 * 3600)
        );
        assert_eq!(year_span_by(2026, |_| 9 * 3600), year_span(2026, 9 * 3600));
        assert_eq!(year_of(0, 0), 1970);
        assert_eq!(year_of(-1, 0), 1969);
        assert_eq!(year_of(951_782_400, 0), 2000, "2000-02-29");
    }
}
