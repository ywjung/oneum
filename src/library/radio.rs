//! "Radio": what to play when the queue runs out. Picks songs from the library that go
//! with the one that just played, from what the library knows offline: genre, artist,
//! year, favourites, and what has been heard lately.
use crate::model::{Track, TrackId};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// A song heard less than this long ago is passed over while there are others.
const HEARD_LATELY_SECONDS: i64 = 2 * 3600;
/// No more than this many songs of one artist in one batch.
const PER_ARTIST: usize = 2;

/// The genres of a tag value such as "Rock; Indie / Alternative", folded for comparing.
fn genres(value: &str) -> Vec<String> {
    value
        .split([';', ',', '/', '|'])
        .map(|g| g.trim().to_lowercase())
        .filter(|g| !g.is_empty())
        .collect()
}

fn artist_of(track: &Track) -> String {
    let name = if track.album_artist.is_empty() {
        &track.artist
    } else {
        &track.album_artist
    };
    name.trim().to_lowercase()
}

/// How well `candidate` goes with `seed`: 0 means nothing in common.
fn kinship(seed: &Track, seed_genres: &[String], candidate: &Track) -> u32 {
    let mut score = 0;
    if !seed_genres.is_empty()
        && genres(&candidate.genre)
            .iter()
            .any(|g| seed_genres.contains(g))
    {
        score += 4;
    }
    let artist = artist_of(seed);
    if !artist.is_empty() && artist_of(candidate) == artist {
        score += 3;
    }
    if let (Some(a), Some(b)) = (seed.year, candidate.year)
        && (a - b).abs() <= 5
    {
        score += 1;
    }
    score
}

/// Small deterministic generator (xorshift64*): the picks vary with `seed`, and a test
/// gets the same picks every time.
struct Dice(u64);

impl Dice {
    fn roll(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// Up to `count` songs to follow `seed`, none of them in `exclude`.
///
/// Songs that share the genre or the artist come first (in a varying order), favourites
/// a little ahead; songs heard in the last two hours and a third song by the same
/// artist are held back until nothing else is left. A library with nothing related
/// still gives songs: the music goes on.
pub fn picks(
    tracks: &[Arc<Track>],
    seed: &Track,
    exclude: &HashSet<TrackId>,
    count: usize,
    now: i64,
    dice_seed: u64,
) -> Vec<TrackId> {
    let seed_genres = genres(&seed.genre);
    let mut dice = Dice(dice_seed | 1);
    // (held back, kinship with a little chance mixed in) decides the order.
    let mut ranked: Vec<(bool, u32, u64, &Track)> = tracks
        .iter()
        .map(|t| &**t)
        .filter(|t| t.available && t.id != seed.id && !exclude.contains(&t.id))
        .map(|t| {
            let heard_lately = t
                .last_played_at
                .is_some_and(|at| now - at < HEARD_LATELY_SECONDS);
            let score = kinship(seed, &seed_genres, t) * 2 + u32::from(t.favorite);
            (heard_lately, score, dice.roll(), t)
        })
        .collect();
    ranked.sort_by(|a, b| {
        (a.0, std::cmp::Reverse(a.1), a.2).cmp(&(b.0, std::cmp::Reverse(b.1), b.2))
    });
    let mut per_artist: HashMap<String, usize> = HashMap::new();
    let mut chosen: Vec<TrackId> = Vec::new();
    let mut passed_over: Vec<TrackId> = Vec::new();
    for (_, _, _, track) in ranked {
        if chosen.len() >= count {
            break;
        }
        let seen = per_artist.entry(artist_of(track)).or_insert(0);
        if *seen >= PER_ARTIST {
            passed_over.push(track.id);
        } else {
            *seen += 1;
            chosen.push(track.id);
        }
    }
    // A library of one artist: better more of the same than silence.
    chosen.extend(
        passed_over
            .into_iter()
            .take(count.saturating_sub(chosen.len())),
    );
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::test_track;

    const NOW: i64 = 2_000_000_000;

    fn song(id: TrackId, artist: &str, genre: &str, year: i32) -> Arc<Track> {
        Arc::new(Track {
            artist: artist.into(),
            genre: genre.into(),
            year: Some(year),
            ..test_track(id)
        })
    }

    fn library() -> Vec<Arc<Track>> {
        vec![
            song(1, "The Beatles", "Rock", 1969),
            song(2, "The Beatles", "Rock", 1968),
            song(3, "The Beatles", "Rock", 1967),
            song(4, "The Beatles", "Rock", 1966),
            song(5, "The Kinks", "rock; Pop", 1967),
            song(6, "The Who", "Rock / Mod", 1969),
            song(7, "Miles Davis", "Jazz", 1959),
            song(8, "Bill Evans", "Jazz", 1961),
            song(9, "아이유", "K-Pop", 2017),
            song(10, "Unknown", "", 1969),
        ]
    }

    fn pick(seed: TrackId, exclude: &[TrackId], count: usize, dice: u64) -> Vec<TrackId> {
        let tracks = library();
        let seed = tracks.iter().find(|t| t.id == seed).expect("seed");
        picks(
            &tracks,
            seed,
            &exclude.iter().copied().collect(),
            count,
            NOW,
            dice,
        )
    }

    #[test]
    fn songs_that_go_with_the_last_one_come_first() {
        for dice in 1..20 {
            let chosen = pick(1, &[], 4, dice);
            assert_eq!(chosen.len(), 4);
            // All rock, whatever the genre tag's spelling; never the seed itself; no
            // more than two more by the same artist.
            assert!(chosen.iter().all(|id| (2..=6).contains(id)), "{chosen:?}");
            let beatles = chosen.iter().filter(|id| (2..=4).contains(*id)).count();
            assert_eq!(beatles, 2, "{chosen:?}");
            assert!(chosen.contains(&5) && chosen.contains(&6));
        }
        // Jazz follows jazz.
        assert_eq!(pick(7, &[], 1, 3), [8]);
    }

    #[test]
    fn the_order_varies_but_not_for_the_same_roll() {
        let a = pick(1, &[], 3, 11);
        assert_eq!(a, pick(1, &[], 3, 11));
        let orders: HashSet<Vec<TrackId>> = (1..40).map(|d| pick(1, &[], 3, d)).collect();
        assert!(orders.len() > 3, "{orders:?}");
    }

    #[test]
    fn what_is_in_the_queue_or_was_just_heard_is_left_out() {
        let chosen = pick(1, &[2, 3, 4, 5, 6], 3, 5);
        assert_eq!(chosen.len(), 3);
        assert!(chosen.iter().all(|id| (7..=10).contains(id)), "{chosen:?}");

        let mut tracks = library();
        // The Kinks and The Who were on an hour ago.
        for heard in [4, 5] {
            let mut t = (*tracks[heard]).clone();
            t.last_played_at = Some(NOW - 3600);
            tracks[heard] = Arc::new(t);
        }
        let seed = tracks[0].clone();
        let chosen = picks(&tracks, &seed, &HashSet::new(), 2, NOW, 9);
        assert!(chosen.iter().all(|id| (2..=4).contains(id)), "{chosen:?}");
        // They come back when nothing else is left.
        let all = picks(&tracks, &seed, &HashSet::new(), 20, NOW, 9);
        assert_eq!(all.len(), 9);
        assert_eq!(all.iter().copied().collect::<HashSet<_>>().len(), 9);
        let position = |id| all.iter().position(|x| *x == id).expect("in the list");
        assert!(position(5) > position(9) && position(6) > position(9));
    }

    #[test]
    fn a_library_with_nothing_related_still_gives_songs() {
        // Nothing shares 아이유's genre or artist: the music goes on all the same.
        let chosen = pick(9, &[], 3, 2);
        assert_eq!(chosen.len(), 3);
        assert!(!chosen.contains(&9));
        // A favourite is a little ahead among equals.
        let mut tracks = library();
        let mut liked = (*tracks[7]).clone();
        liked.favorite = true;
        tracks[7] = Arc::new(liked);
        let seed = tracks[8].clone();
        for dice in 1..10 {
            assert_eq!(picks(&tracks, &seed, &HashSet::new(), 1, NOW, dice), [8]);
        }
        // Unavailable files are never picked; an empty library gives nothing.
        let mut gone = (*tracks[7]).clone();
        gone.available = false;
        tracks[7] = Arc::new(gone);
        assert!(!picks(&tracks, &seed, &HashSet::new(), 20, NOW, 1).contains(&8));
        assert!(picks(&[], &seed, &HashSet::new(), 5, NOW, 1).is_empty());
    }

    #[test]
    fn one_artist_only_is_better_than_silence() {
        let tracks: Vec<Arc<Track>> = (1..=6).map(|id| song(id, "Solo", "Folk", 2000)).collect();
        let chosen = picks(&tracks, &tracks[0], &HashSet::new(), 4, NOW, 7);
        assert_eq!(chosen.len(), 4);
    }
}
