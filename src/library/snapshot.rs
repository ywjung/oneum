//! Immutable in-memory view of the library: tracks in default order plus the search,
//! sort and album/artist indexes derived from them.
//!
//! A snapshot is built off the UI thread and published as `Arc<LibrarySnapshot>`; readers
//! never lock. Everything that depends on a track's text (normalised strings, choseong
//! form) is cached per track in [`TrackIndex`], so rebuilding after a scan batch only
//! pays for sorting and concatenation, not for re-normalising unchanged tracks.
use super::search::{FIELD_SEP, Field, Filter, PreparedQuery, Term, choseong_form, normalize};
use crate::model::{Track, TrackId};
use std::cmp::{Ordering, Reverse};
use std::collections::HashMap;
use std::sync::Arc;

/// Column a track list can be sorted by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    /// Album artist (or artist), album, disc, track number, title — the snapshot order.
    Default,
    Title,
    Artist,
    Album,
    Duration,
    TrackNo,
    Year,
    DateAdded,
    PlayCount,
    LastPlayed,
    Format,
    /// The listener's stars.
    Rating,
}

/// A group of tracks with the same album identity.
#[derive(Clone, Debug, PartialEq)]
pub struct Album {
    pub key: u64,
    pub title: String,
    /// Album artist; for albums without one, the common track artist or "Various Artists".
    pub artist: String,
    pub year: Option<i32>,
    pub art_id: Option<String>,
    /// Indices into `LibrarySnapshot::tracks`, sorted by disc, track number, title.
    pub tracks: Vec<u32>,
    pub duration_ms: u64,
}

/// Tracks grouped by album artist (falling back to the track artist).
#[derive(Clone, Debug, PartialEq)]
pub struct Artist {
    pub name: String,
    /// Indices into `LibrarySnapshot::albums()`.
    pub albums: Vec<u32>,
    pub track_count: usize,
}

/// Normalised text of one track, computed once when the track is loaded or changed.
#[derive(Debug)]
pub(crate) struct TrackIndex {
    title: String,
    artist: String,
    album: String,
    album_artist: String,
    /// Not part of the plain search; `genre:` terms look here.
    genre: String,
    /// Choseong form of the four fields joined by [`FIELD_SEP`]; empty without Hangul.
    cho: String,
}

impl TrackIndex {
    pub(crate) fn new(track: &Track) -> Self {
        let title = normalize(&track.title);
        let artist = normalize(&track.artist);
        let album = normalize(&track.album);
        let album_artist = normalize(&track.album_artist);
        let forms = [&title, &artist, &album, &album_artist].map(|s| choseong_form(s));
        let mut cho = String::new();
        if forms.iter().any(Option::is_some) {
            let fields = [&title, &artist, &album, &album_artist];
            for (i, (form, field)) in forms.iter().zip(fields).enumerate() {
                if i > 0 {
                    cho.push(FIELD_SEP);
                }
                cho.push_str(form.as_deref().unwrap_or(field));
            }
        }
        TrackIndex {
            title,
            artist,
            album,
            album_artist,
            genre: normalize(&track.genre),
            cho,
        }
    }

    /// True if the field holds the term (`artist:` looks at both artists).
    fn field_contains(&self, field: Field, term: &Term, track: &Track) -> bool {
        let text = term.text.as_str();
        if field == Field::Format {
            let lossless = track.is_lossless();
            return match text {
                "lossless" | "무손실" => lossless,
                "lossy" | "손실" => !lossless,
                _ => {
                    track.codec.to_lowercase().contains(text)
                        || track.container.to_lowercase().contains(text)
                }
            };
        }
        if term.choseong {
            if field == Field::Genre {
                return choseong_form(&self.genre).is_some_and(|form| form.contains(text));
            }
            // `cho` holds title, artist, album, album artist, in that order.
            let wanted: &[usize] = match field {
                Field::Title => &[0],
                Field::Artist => &[1, 3],
                _ => &[2],
            };
            return self
                .cho
                .split(FIELD_SEP)
                .enumerate()
                .any(|(i, form)| wanted.contains(&i) && form.contains(text));
        }
        match field {
            Field::Title => self.title.contains(text),
            Field::Artist => self.artist.contains(text) || self.album_artist.contains(text),
            Field::Album => self.album.contains(text),
            Field::Genre => self.genre.contains(text),
            Field::Format => false,
        }
    }

    /// Normalised name the track is grouped under in artist views and the default order.
    fn sort_artist(&self) -> &str {
        if self.album_artist.is_empty() {
            &self.artist
        } else {
            &self.album_artist
        }
    }

    /// Heap bytes held by the strings (for the memory estimate).
    pub(crate) fn heap_bytes(&self) -> usize {
        self.title.capacity()
            + self.artist.capacity()
            + self.album.capacity()
            + self.album_artist.capacity()
            + self.genre.capacity()
            + self.cho.capacity()
    }
}

/// Concatenated records with their end offsets; record `i` is `data[ends[i-1]..ends[i]]`.
#[derive(Default)]
struct Blob {
    data: String,
    ends: Vec<u32>,
}

impl Blob {
    fn end_record(&mut self) {
        self.ends
            .push(u32::try_from(self.data.len()).unwrap_or(u32::MAX));
    }

    fn record(&self, index: u32) -> &str {
        let index = index as usize;
        let Some(&end) = self.ends.get(index) else {
            return "";
        };
        let start = if index == 0 { 0 } else { self.ends[index - 1] };
        self.data.get(start as usize..end as usize).unwrap_or("")
    }

    /// Indices of all records containing `needle`, ascending. `needle` must not contain
    /// [`FIELD_SEP`], which guarantees a match never spans two records.
    fn records_containing(&self, needle: &str) -> Vec<u32> {
        let mut out = Vec::new();
        let mut record = 0usize;
        for (pos, _) in self.data.match_indices(needle) {
            while record < self.ends.len() && self.ends[record] as usize <= pos {
                record += 1;
            }
            if record >= self.ends.len() {
                break;
            }
            if out.last() != Some(&(record as u32)) {
                out.push(record as u32);
            }
        }
        out
    }

    fn heap_bytes(&self) -> usize {
        self.data.capacity() + self.ends.capacity() * size_of::<u32>()
    }
}

/// Everything derived from the track list. Shared between snapshots that differ only in
/// non-indexed fields (favourite, play count).
struct Index {
    by_id: HashMap<TrackId, u32>,
    text: Blob,
    cho: Blob,
    title_rank: Vec<u32>,
    artist_rank: Vec<u32>,
    album_rank: Vec<u32>,
    albums: Vec<Album>,
    artists: Vec<Artist>,
    album_by_key: HashMap<u64, u32>,
    root_counts: HashMap<i64, usize>,
    /// Per-track normalised text, parallel to `tracks` (kept for cheap rebuilds).
    per_track: Vec<Arc<TrackIndex>>,
}

/// Immutable view of all tracks, in default order, with search and sort indexes.
pub struct LibrarySnapshot {
    /// All tracks sorted by album artist/artist, album, disc, track number, title.
    pub tracks: Vec<Arc<Track>>,
    index: Arc<Index>,
}

impl LibrarySnapshot {
    /// A snapshot without tracks.
    pub fn empty() -> Arc<LibrarySnapshot> {
        Arc::new(Self::build(Vec::new()))
    }

    /// Builds a snapshot from plain tracks (used by tests and benchmarks; the library
    /// itself reuses cached per-track index data).
    pub fn from_tracks(tracks: Vec<Track>) -> LibrarySnapshot {
        let entries = tracks
            .into_iter()
            .map(|track| {
                let index = Arc::new(TrackIndex::new(&track));
                (Arc::new(track), index)
            })
            .collect();
        Self::build(entries)
    }

    pub(crate) fn build(entries: Vec<(Arc<Track>, Arc<TrackIndex>)>) -> LibrarySnapshot {
        let n = entries.len();

        // Dense ranks of the normalised strings; all later ordering is integer compares.
        let title_rank = rank_strings(entries.iter().map(|e| e.1.title.as_str()));
        let artist_rank = rank_strings(entries.iter().map(|e| e.1.artist.as_str()));
        let album_rank = rank_strings(entries.iter().map(|e| e.1.album.as_str()));
        let sort_artist_rank = rank_strings(entries.iter().map(|e| e.1.sort_artist()));

        let mut order: Vec<u32> = (0..n as u32).collect();
        order.sort_by_cached_key(|&i| {
            let i = i as usize;
            let track = &entries[i].0;
            (
                sort_artist_rank[i],
                album_rank[i],
                track.album_key,
                super::ident::disc_of(track),
                track.track_no.unwrap_or(u32::MAX),
                title_rank[i],
                track.id,
            )
        });
        let permute =
            |ranks: &[u32]| -> Vec<u32> { order.iter().map(|&i| ranks[i as usize]).collect() };
        let title_rank = permute(&title_rank);
        let artist_rank = permute(&artist_rank);
        let album_rank = permute(&album_rank);

        let mut tracks = Vec::with_capacity(n);
        let mut indexes = Vec::with_capacity(n);
        for &i in &order {
            let (track, index) = &entries[i as usize];
            tracks.push(Arc::clone(track));
            indexes.push(Arc::clone(index));
        }
        drop(entries);

        let mut by_id = HashMap::with_capacity(n);
        let mut root_counts: HashMap<i64, usize> = HashMap::new();
        let mut text = Blob::default();
        let mut cho = Blob::default();
        text.ends.reserve_exact(n);
        cho.ends.reserve_exact(n);
        text.data.reserve_exact(
            indexes
                .iter()
                .map(|ix| {
                    ix.title.len() + ix.artist.len() + ix.album.len() + ix.album_artist.len() + 4
                })
                .sum(),
        );
        cho.data
            .reserve_exact(indexes.iter().map(|ix| ix.cho.len() + 1).sum());
        for (i, (track, ix)) in tracks.iter().zip(&indexes).enumerate() {
            by_id.insert(track.id, i as u32);
            if let Some(root) = track.root_id {
                *root_counts.entry(root).or_default() += 1;
            }
            for field in [&ix.title, &ix.artist, &ix.album, &ix.album_artist] {
                text.data.push_str(field);
                text.data.push(FIELD_SEP);
            }
            text.end_record();
            if !ix.cho.is_empty() {
                cho.data.push_str(&ix.cho);
                cho.data.push(FIELD_SEP);
            }
            cho.end_record();
        }

        let (albums, album_by_key, album_of_track) = build_albums(&tracks, &indexes, &title_rank);
        let artists = build_artists(&tracks, &indexes, &album_of_track);

        LibrarySnapshot {
            tracks,
            index: Arc::new(Index {
                by_id,
                text,
                cho,
                title_rank,
                artist_rank,
                album_rank,
                albums,
                artists,
                album_by_key,
                root_counts,
                per_track: indexes,
            }),
        }
    }

    /// A snapshot in which the given tracks replace the ones with the same id. Only valid
    /// for changes to fields that are not indexed (favourite, play statistics); tracks
    /// that are not part of this snapshot are ignored.
    pub(crate) fn with_replaced(&self, changed: &[Arc<Track>]) -> LibrarySnapshot {
        let mut tracks = self.tracks.clone();
        for track in changed {
            if let Some(i) = self.index_of(track.id)
                && let Some(slot) = tracks.get_mut(i as usize)
            {
                *slot = Arc::clone(track);
            }
        }
        LibrarySnapshot {
            tracks,
            index: Arc::clone(&self.index),
        }
    }

    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    pub fn get(&self, id: TrackId) -> Option<&Arc<Track>> {
        self.index_of(id).and_then(|i| self.tracks.get(i as usize))
    }

    /// Position of the track in `tracks`.
    pub fn index_of(&self, id: TrackId) -> Option<u32> {
        self.index.by_id.get(&id).copied()
    }

    /// Normalises and splits a query once so it can be matched against many tracks.
    pub fn prepare_query(query: &str) -> PreparedQuery {
        PreparedQuery::new(query)
    }

    /// True if the track at `index` matches every term of the query.
    pub fn matches(&self, index: u32, q: &PreparedQuery) -> bool {
        (index as usize) < self.tracks.len()
            && q.terms.iter().all(|t| self.term_matches(index, t))
            && q.filters.iter().all(|f| self.filter_matches(index, f))
    }

    fn filter_matches(&self, index: u32, filter: &Filter) -> bool {
        let index = index as usize;
        let (Some(track), Some(ix)) = (self.tracks.get(index), self.index.per_track.get(index))
        else {
            return false;
        };
        match filter {
            Filter::Text { field, term } => ix.field_contains(*field, term, track),
            Filter::Year(from, to) => track.year.is_some_and(|y| (*from..=*to).contains(&y)),
            Filter::Plays(from, to) => {
                let plays = i32::try_from(track.play_count).unwrap_or(i32::MAX);
                (*from..=*to).contains(&plays)
            }
            Filter::Favorite(wanted) => track.favorite == *wanted,
            Filter::Rating(from, to) => (*from..=*to).contains(&i32::from(track.rating)),
            Filter::AddedSince(since) => track.added_at >= *since,
            Filter::PlayedSince(since) => track.last_played_at.is_some_and(|at| at >= *since),
            Filter::Check(wanted) => {
                use super::search::CheckWanted;
                use crate::model::CheckVerdict;
                let verdict = track.check.map(|c| c.verdict);
                match wanted {
                    CheckWanted::Damaged => verdict == Some(CheckVerdict::Damaged),
                    CheckWanted::Intact => matches!(
                        verdict,
                        Some(CheckVerdict::Verified | CheckVerdict::Decoded)
                    ),
                    CheckWanted::Verified => verdict == Some(CheckVerdict::Verified),
                    CheckWanted::Unchecked => verdict.is_none(),
                }
            }
            Filter::Dr(from, to) => track
                .check
                .and_then(|c| c.dr)
                .is_some_and(|dr| (*from..=*to).contains(&(dr.round() as i32))),
            Filter::Never => false,
        }
    }

    fn term_matches(&self, index: u32, term: &Term) -> bool {
        let blob = if term.choseong {
            &self.index.cho
        } else {
            &self.index.text
        };
        blob.record(index).contains(term.text.as_str())
    }

    /// Indices of the tracks matching `query`, in snapshot order. An empty query matches
    /// every track.
    pub fn search(&self, query: &str) -> Vec<u32> {
        self.search_prepared(&PreparedQuery::new(query))
    }

    /// Like [`LibrarySnapshot::search`] for an already prepared query.
    pub fn search_prepared(&self, q: &PreparedQuery) -> Vec<u32> {
        // Scan the whole blob once for the longest (most selective) term, then check the
        // remaining terms only on those candidates.
        let longest = q
            .terms
            .iter()
            .enumerate()
            .max_by_key(|(_, term)| term.text.len());
        let mut hits = match longest {
            Some((first_pos, first)) => {
                let blob = if first.choseong {
                    &self.index.cho
                } else {
                    &self.index.text
                };
                let mut hits = blob.records_containing(&first.text);
                if q.terms.len() > 1 {
                    hits.retain(|&i| {
                        q.terms
                            .iter()
                            .enumerate()
                            .all(|(pos, term)| pos == first_pos || self.term_matches(i, term))
                    });
                }
                hits
            }
            None => (0..self.tracks.len() as u32).collect(),
        };
        if !q.filters.is_empty() {
            hits.retain(|&i| q.filters.iter().all(|f| self.filter_matches(i, f)));
        }
        // The order the query asks for, and no more songs than it wants.
        if let Some((key, ascending)) = q.order {
            self.sort(&mut hits, key, ascending);
        }
        if let Some(most) = q.limit {
            hits.truncate(most);
        }
        hits
    }

    /// Stable sort of track indices by the given column. Text columns compare the
    /// normalised strings (empty values last); equal keys keep their current order, so
    /// sorting by several columns in turn gives a multi-level sort.
    pub fn sort(&self, indices: &mut [u32], key: SortKey, ascending: bool) {
        let ix = &*self.index;
        let rank = |ranks: &[u32], i: u32| ranks.get(i as usize).copied().unwrap_or(u32::MAX);
        match key {
            SortKey::Default => {
                if ascending {
                    indices.sort();
                } else {
                    indices.sort_by_key(|&i| Reverse(i));
                }
            }
            SortKey::Title => sort_keyed(indices, ascending, |i| rank(&ix.title_rank, i)),
            SortKey::Artist => sort_keyed(indices, ascending, |i| rank(&ix.artist_rank, i)),
            SortKey::Album => sort_keyed(indices, ascending, |i| rank(&ix.album_rank, i)),
            SortKey::Duration => {
                sort_keyed(indices, ascending, |i| self.field(i, |t| t.duration_ms))
            }
            SortKey::TrackNo => sort_keyed(indices, ascending, |i| {
                self.field(i, |t| {
                    (super::ident::disc_of(t), t.track_no.unwrap_or(u32::MAX))
                })
            }),
            SortKey::Year => sort_keyed(indices, ascending, |i| {
                self.field(i, |t| t.year.unwrap_or(i32::MAX))
            }),
            SortKey::DateAdded => sort_keyed(indices, ascending, |i| self.field(i, |t| t.added_at)),
            SortKey::PlayCount => {
                sort_keyed(indices, ascending, |i| self.field(i, |t| t.play_count))
            }
            SortKey::Rating => sort_keyed(indices, ascending, |i| self.field(i, |t| t.rating)),
            SortKey::LastPlayed => sort_keyed(indices, ascending, |i| {
                self.field(i, |t| t.last_played_at.unwrap_or(i64::MIN))
            }),
            SortKey::Format => sort_keyed(indices, ascending, |i| {
                self.field(i, |t| {
                    (
                        codec_order(&t.codec),
                        t.bit_depth.unwrap_or(0),
                        t.sample_rate,
                        t.bitrate_kbps.unwrap_or(0),
                    )
                })
            }),
        }
    }

    fn field<K>(&self, index: u32, f: impl Fn(&Track) -> K) -> Option<K> {
        self.tracks.get(index as usize).map(|t| f(t))
    }

    /// Albums sorted by artist, then year, then title.
    pub fn albums(&self) -> &[Album] {
        &self.index.albums
    }

    /// Artists sorted by name.
    pub fn artists(&self) -> &[Artist] {
        &self.index.artists
    }

    /// Position of the album with the given key in [`LibrarySnapshot::albums`].
    pub fn album_index(&self, key: u64) -> Option<u32> {
        self.index.album_by_key.get(&key).copied()
    }

    /// The key of the album a track's own album key is shown as: tracks of one album
    /// tagged inconsistently have different keys but are shown as one album, whose key
    /// this is. The key itself if it is not merged (or not known).
    pub fn shown_album_key(&self, key: u64) -> u64 {
        self.album_index(key)
            .and_then(|i| self.albums().get(i as usize))
            .map_or(key, |album| album.key)
    }

    /// Builds the snapshot again from its own tracks, reusing the cached normalised
    /// text — the work the library does whenever tracks change (for the benchmark tool).
    #[doc(hidden)]
    pub fn rebuilt(&self) -> LibrarySnapshot {
        let entries = self
            .tracks
            .iter()
            .zip(&self.index.per_track)
            .map(|(track, index)| (Arc::clone(track), Arc::clone(index)))
            .collect();
        Self::build(entries)
    }

    /// Number of tracks that belong to the given music folder.
    pub(crate) fn root_track_count(&self, root_id: i64) -> usize {
        self.index.root_counts.get(&root_id).copied().unwrap_or(0)
    }

    /// Rough heap size of the snapshot including the tracks it references (for the
    /// benchmark tool; not exact).
    #[doc(hidden)]
    pub fn approx_memory_bytes(&self) -> usize {
        let ix = &*self.index;
        let tracks: usize = self
            .tracks
            .iter()
            .map(|t| {
                size_of::<Track>()
                    + 16 // Arc counters
                    + t.path.as_os_str().len()
                    + t.title.capacity()
                    + t.artist.capacity()
                    + t.album_artist.capacity()
                    + t.album.capacity()
                    + t.genre.capacity()
                    + t.codec.capacity()
                    + t.container.capacity()
                    + t.art_id.as_ref().map_or(0, |s| s.capacity())
            })
            .sum();
        let albums: usize = ix
            .albums
            .iter()
            .map(|a| {
                size_of::<Album>()
                    + a.title.capacity()
                    + a.artist.capacity()
                    + a.tracks.capacity() * 4
            })
            .sum();
        let artists: usize = ix
            .artists
            .iter()
            .map(|a| size_of::<Artist>() + a.name.capacity() + a.albums.capacity() * 4)
            .sum();
        tracks
            + self.tracks.capacity() * size_of::<Arc<Track>>()
            + ix.per_track
                .iter()
                .map(|t| size_of::<TrackIndex>() + 16 + t.heap_bytes())
                .sum::<usize>()
            + ix.per_track.capacity() * size_of::<Arc<TrackIndex>>()
            + ix.text.heap_bytes()
            + ix.cho.heap_bytes()
            + (ix.title_rank.capacity() + ix.artist_rank.capacity() + ix.album_rank.capacity()) * 4
            + ix.by_id.capacity() * (size_of::<TrackId>() + size_of::<u32>() + 8)
            + ix.album_by_key.capacity() * 24
            + albums
            + artists
    }
}

fn sort_keyed<K: Ord>(indices: &mut [u32], ascending: bool, key: impl Fn(u32) -> K) {
    if ascending {
        indices.sort_by_cached_key(|&i| key(i));
    } else {
        indices.sort_by_cached_key(|&i| Reverse(key(i)));
    }
}

fn codec_order(codec: &str) -> u8 {
    match codec {
        "AAC" => 0,
        "ADPCM" => 1,
        "ALAC" => 2,
        "APE" => 3,
        c if c.starts_with("DSD") => 4,
        "FLAC" => 5,
        "MP3" => 6,
        "Opus" => 7,
        "PCM" => 8,
        "Vorbis" => 9,
        "WavPack" => 10,
        _ => 11,
    }
}

/// Text order used everywhere: empty values last, otherwise by code point of the
/// normalised string.
fn cmp_text(a: &str, b: &str) -> Ordering {
    a.is_empty().cmp(&b.is_empty()).then_with(|| a.cmp(b))
}

/// Maps every string to its dense rank in [`cmp_text`] order (equal strings share a rank).
fn rank_strings<'a>(values: impl Iterator<Item = &'a str>) -> Vec<u32> {
    let mut ids: HashMap<&str, u32> = HashMap::new();
    let mut unique: Vec<&str> = Vec::new();
    let per_item: Vec<u32> = values
        .map(|s| {
            *ids.entry(s).or_insert_with(|| {
                unique.push(s);
                (unique.len() - 1) as u32
            })
        })
        .collect();
    let mut sorted: Vec<u32> = (0..unique.len() as u32).collect();
    sorted.sort_unstable_by(|&a, &b| cmp_text(unique[a as usize], unique[b as usize]));
    let mut rank_of = vec![0u32; unique.len()];
    for (rank, &id) in sorted.iter().enumerate() {
        rank_of[id as usize] = rank as u32;
    }
    per_item
        .into_iter()
        .map(|id| rank_of[id as usize])
        .collect()
}

type AlbumTables = (Vec<Album>, HashMap<u64, u32>, Vec<u32>);

/// Groups tracks into albums. Returns the albums in display order, the key → position
/// map and the album position of every track.
fn build_albums(
    tracks: &[Arc<Track>],
    indexes: &[Arc<TrackIndex>],
    title_rank: &[u32],
) -> AlbumTables {
    let aliases = album_aliases(tracks, indexes);
    let mut slot_by_key: HashMap<u64, usize> = HashMap::new();
    let mut albums: Vec<Album> = Vec::new();
    for (i, track) in tracks.iter().enumerate() {
        let key = aliases
            .get(&track.album_key)
            .copied()
            .unwrap_or(track.album_key);
        let slot = *slot_by_key.entry(key).or_insert_with(|| {
            albums.push(Album {
                key,
                title: track.album.clone(),
                artist: String::new(),
                year: None,
                art_id: None,
                tracks: Vec::new(),
                duration_ms: 0,
            });
            albums.len() - 1
        });
        let album = &mut albums[slot];
        album.tracks.push(i as u32);
        album.duration_ms += track.duration_ms;
        if album.year.is_none() {
            album.year = track.year;
        }
        if album.art_id.is_none() {
            album.art_id.clone_from(&track.art_id);
        }
    }

    // Per album: order its tracks, pick the display artist, and remember the normalised
    // artist/title for sorting the album list.
    let mut keyed: Vec<(Album, String, String)> = albums
        .into_iter()
        .map(|mut album| {
            album.tracks.sort_by_key(|&i| {
                let t = &tracks[i as usize];
                (
                    super::ident::disc_of(t),
                    t.track_no.unwrap_or(u32::MAX),
                    title_rank[i as usize],
                    i,
                )
            });
            let first = album.tracks[0] as usize;
            let tagged = album
                .tracks
                .iter()
                .map(|&i| i as usize)
                .find(|&i| !tracks[i].album_artist.is_empty());
            let (artist, artist_norm) = if let Some(i) = tagged {
                (
                    tracks[i].album_artist.clone(),
                    indexes[i].album_artist.clone(),
                )
            } else if album
                .tracks
                .iter()
                .all(|&i| indexes[i as usize].artist == indexes[first].artist)
            {
                (tracks[first].artist.clone(), indexes[first].artist.clone())
            } else {
                ("Various Artists".to_string(), "various artists".to_string())
            };
            album.artist = artist;
            let title_norm = indexes[first].album.clone();
            (album, artist_norm, title_norm)
        })
        .collect();
    keyed.sort_by(|a, b| {
        cmp_text(&a.1, &b.1)
            .then_with(|| {
                a.0.year
                    .unwrap_or(i32::MAX)
                    .cmp(&b.0.year.unwrap_or(i32::MAX))
            })
            .then_with(|| cmp_text(&a.2, &b.2))
            .then_with(|| a.0.key.cmp(&b.0.key))
    });

    let albums: Vec<Album> = keyed.into_iter().map(|(album, _, _)| album).collect();
    let mut by_key = HashMap::with_capacity(albums.len());
    let mut album_of_track = vec![0u32; tracks.len()];
    for (position, album) in albums.iter().enumerate() {
        by_key.insert(album.key, position as u32);
        for &i in &album.tracks {
            album_of_track[i as usize] = position as u32;
        }
    }
    // Tracks keep their own key; make it resolve to the album they were merged into.
    for (from, to) in &aliases {
        if let Some(&position) = by_key.get(to) {
            by_key.insert(*from, position);
        }
    }
    (albums, by_key, album_of_track)
}

/// Album keys that should be shown as part of another album.
///
/// A track without an album-artist tag gets a folder-based key, a tagged track an
/// artist-based one, so an album whose files are tagged inconsistently would show up
/// twice. When a folder holds, for one album title, untagged tracks and tracks of exactly
/// one tagged album, the untagged ones join that album. Two differently tagged albums of
/// the same title are never merged, and a key used in more than one folder is left alone.
fn album_aliases(tracks: &[Arc<Track>], indexes: &[Arc<TrackIndex>]) -> HashMap<u64, u64> {
    #[derive(Default)]
    struct Bucket {
        tagged: Vec<u64>,
        untagged: Vec<u64>,
    }
    let mut buckets: HashMap<(&std::path::Path, &str), Bucket> = HashMap::new();
    let mut folders_of_key: HashMap<u64, (&std::path::Path, bool)> = HashMap::new();
    for (track, index) in tracks.iter().zip(indexes) {
        if track.album.is_empty() {
            continue;
        }
        // A sheet's tracks are in the sheet's folder.
        let Some(folder) = track.file().parent() else {
            continue;
        };
        folders_of_key
            .entry(track.album_key)
            .and_modify(|(first, single)| *single &= *first == folder)
            .or_insert((folder, true));
        let bucket = buckets.entry((folder, index.album.as_str())).or_default();
        let list = if track.album_artist.is_empty() {
            &mut bucket.untagged
        } else {
            &mut bucket.tagged
        };
        if !list.contains(&track.album_key) {
            list.push(track.album_key);
        }
    }
    let mut aliases = HashMap::new();
    for bucket in buckets.values() {
        if let [target] = bucket.tagged[..] {
            for &key in &bucket.untagged {
                if key != target && folders_of_key.get(&key).is_some_and(|(_, single)| *single) {
                    aliases.insert(key, target);
                }
            }
        }
    }
    aliases
}

fn build_artists(
    tracks: &[Arc<Track>],
    indexes: &[Arc<TrackIndex>],
    album_of_track: &[u32],
) -> Vec<Artist> {
    let mut slot_by_name: HashMap<&str, usize> = HashMap::new();
    let mut artists: Vec<(Artist, &str)> = Vec::new();
    for (i, track) in tracks.iter().enumerate() {
        let norm = indexes[i].sort_artist();
        let slot = *slot_by_name.entry(norm).or_insert_with(|| {
            let name = if track.album_artist.is_empty() {
                track.artist.clone()
            } else {
                track.album_artist.clone()
            };
            artists.push((
                Artist {
                    name,
                    albums: Vec::new(),
                    track_count: 0,
                },
                norm,
            ));
            artists.len() - 1
        });
        let artist = &mut artists[slot].0;
        artist.track_count += 1;
        let album = album_of_track[i];
        if artist.albums.last() != Some(&album) {
            artist.albums.push(album);
        }
    }
    for (artist, _) in &mut artists {
        artist.albums.sort_unstable();
        artist.albums.dedup();
    }
    artists.sort_by(|a, b| cmp_text(a.1, b.1));
    artists.into_iter().map(|(artist, _)| artist).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::ident::album_key;
    use crate::model::ReplayGainTags;
    use std::path::PathBuf;

    fn track(id: TrackId, title: &str, artist: &str, album: &str) -> Track {
        let path = PathBuf::from(format!("/music/{artist}/{album}/{id:03}.flac"));
        Track {
            id,
            album_key: album_key(album, "", false, &path),
            path,
            root_id: Some(1),
            available: true,
            title: title.into(),
            artist: artist.into(),
            album_artist: String::new(),
            album: album.into(),
            disc_no: None,
            track_no: Some(id as u32),
            year: None,
            genre: String::new(),
            duration_ms: 180_000 + id as u64,
            codec: "FLAC".into(),
            container: "FLAC".into(),
            sample_rate: 44_100,
            channels: 2,
            bit_depth: Some(16),
            bitrate_kbps: Some(900),
            file_size: 1,
            mtime_ns: 1,
            replaygain: ReplayGainTags::default(),
            measured: None,
            unmeasurable: false,
            check: None,
            art_id: None,
            favorite: false,
            rating: 0,
            play_count: 0,
            last_played_at: None,
            added_at: id,
        }
    }

    fn sample() -> LibrarySnapshot {
        LibrarySnapshot::from_tracks(vec![
            track(1, "Hey Jude", "The Beatles", "1"),
            track(2, "Let It Be", "The Beatles", "Let It Be"),
            track(3, "밤편지", "아이유", "Palette"),
            track(4, "좋은 날", "아이유", "Real"),
            // NFD title and artist, as written by some taggers on macOS.
            track(
                5,
                "\u{1107}\u{1173}\u{11AF}\u{1105}\u{1165}\u{1106}\u{1175}\u{11BC}", // 블러밍
                "\u{110B}\u{1161}\u{110B}\u{1175}\u{110B}\u{1172}",                 // 아이유
                "Love Poem",
            ),
            track(6, "Ｄｙｎａｍｉｔｅ", "방탄소년단", "BE"),
            track(7, "Beautiful", "Crush", "도깨비 OST"),
            track(8, "ㅋㅋㅋ", "Unknown", ""),
        ])
    }

    fn titles(snapshot: &LibrarySnapshot, hits: &[u32]) -> Vec<String> {
        let mut titles: Vec<String> = hits
            .iter()
            .map(|&i| normalize(&snapshot.tracks[i as usize].title))
            .collect();
        titles.sort();
        titles
    }

    #[test]
    fn inconsistently_tagged_album_in_one_folder_is_one_album() {
        let folder = "/music/mixed/Sessions";
        let mut tagged = track(1, "One", "A", "Sessions");
        tagged.path = PathBuf::from(format!("{folder}/01.flac"));
        tagged.album_artist = "Various Artists".into();
        tagged.album_key = album_key("Sessions", "Various Artists", false, &tagged.path);
        let mut untagged = track(2, "Two", "B", "Sessions");
        untagged.path = PathBuf::from(format!("{folder}/02.wav"));
        untagged.album_key = album_key("Sessions", "", false, &untagged.path);
        assert_ne!(tagged.album_key, untagged.album_key);
        let (tagged_key, untagged_key) = (tagged.album_key, untagged.album_key);
        let snapshot = LibrarySnapshot::from_tracks(vec![tagged, untagged]);
        assert_eq!(snapshot.albums().len(), 1);
        assert_eq!(snapshot.albums()[0].tracks.len(), 2);
        assert_eq!(snapshot.albums()[0].artist, "Various Artists");
        // Both keys lead to the merged album.
        assert_eq!(snapshot.album_index(tagged_key), Some(0));
        assert_eq!(snapshot.album_index(untagged_key), Some(0));

        // The tracks of a CUE sheet in that folder are in it too.
        let mut tagged = track(3, "Three", "A", "Sessions");
        tagged.path = PathBuf::from(format!("{folder}/bonus.flac"));
        tagged.album_artist = "Various Artists".into();
        tagged.album_key = album_key("Sessions", "Various Artists", false, &tagged.path);
        let mut in_sheet = track(4, "Four", "B", "Sessions");
        in_sheet.path = PathBuf::from(format!("{folder}/Sessions.cue/01"));
        in_sheet.album_key = album_key("Sessions", "", false, &in_sheet.path);
        let snapshot = LibrarySnapshot::from_tracks(vec![tagged, in_sheet]);
        assert_eq!(snapshot.albums().len(), 1, "one album");
    }

    #[test]
    fn same_title_albums_by_different_album_artists_in_one_folder_stay_apart() {
        let folder = "/music/flat";
        let mut tracks = Vec::new();
        for (id, artist) in [(1, "Queen"), (2, "ABBA")] {
            let mut t = track(id, "Song", artist, "Greatest Hits");
            t.path = PathBuf::from(format!("{folder}/{id}.flac"));
            t.album_artist = artist.into();
            t.album_key = album_key("Greatest Hits", artist, false, &t.path);
            tracks.push(t);
        }
        // An untagged file next to two differently tagged albums cannot be assigned.
        let mut loose = track(3, "Song", "Someone", "Greatest Hits");
        loose.path = PathBuf::from(format!("{folder}/3.flac"));
        loose.album_key = album_key("Greatest Hits", "", false, &loose.path);
        tracks.push(loose);
        let snapshot = LibrarySnapshot::from_tracks(tracks);
        assert_eq!(snapshot.albums().len(), 3);
    }

    #[test]
    fn english_partial_case_insensitive() {
        let s = sample();
        assert_eq!(
            titles(&s, &s.search("bea")),
            ["beautiful", "hey jude", "let it be"]
        );
        assert_eq!(titles(&s, &s.search("BEATLES")), ["hey jude", "let it be"]);
        assert_eq!(titles(&s, &s.search("JUDE")), ["hey jude"]);
    }

    #[test]
    fn korean_partial() {
        let s = sample();
        assert_eq!(
            titles(&s, &s.search("이유")),
            ["밤편지", "블러밍", "좋은 날"]
        );
        assert_eq!(titles(&s, &s.search("편지")), ["밤편지"]);
        assert_eq!(titles(&s, &s.search("소년")), ["dynamite"]);
    }

    #[test]
    fn nfd_text_and_nfd_query() {
        let s = sample();
        // NFC query finds NFD-tagged text.
        assert_eq!(titles(&s, &s.search("블러밍")), ["블러밍"]);
        // NFD query finds NFC-tagged text.
        let nfd_query = "\u{1107}\u{1161}\u{11B7}\u{1111}\u{1167}\u{11AB}"; // 밤편
        assert_eq!(titles(&s, &s.search(nfd_query)), ["밤편지"]);
    }

    #[test]
    fn full_width_latin() {
        let s = sample();
        assert_eq!(titles(&s, &s.search("dynamite")), ["dynamite"]);
        assert_eq!(
            titles(&s, &s.search("ＢＥＡＴＬＥＳ")),
            ["hey jude", "let it be"]
        );
    }

    #[test]
    fn choseong_search() {
        let s = sample();
        assert_eq!(
            titles(&s, &s.search("ㅇㅇㅇ")),
            ["밤편지", "블러밍", "좋은 날"]
        );
        assert_eq!(titles(&s, &s.search("ㅂㅌㅅㄴㄷ")), ["dynamite"]);
        assert_eq!(titles(&s, &s.search("ㅂㅍㅈ")), ["밤편지"]);
        assert_eq!(titles(&s, &s.search("ㄷㄲㅂ")), ["beautiful"]);
        // Literal consonants in a title are found too.
        assert_eq!(titles(&s, &s.search("ㅋㅋ")), ["ㅋㅋㅋ"]);
        // A choseong term never matches across fields.
        assert!(s.search("ㅈㅇㅇ").is_empty());
        assert!(s.search("ㅎㅎㅎ").is_empty());
    }

    #[test]
    fn multi_term_and() {
        let s = sample();
        assert_eq!(titles(&s, &s.search("beatles jude")), ["hey jude"]);
        assert_eq!(titles(&s, &s.search("jude beatles")), ["hey jude"]);
        assert_eq!(titles(&s, &s.search("아이유 real")), ["좋은 날"]);
        assert_eq!(titles(&s, &s.search("ㅇㅇㅇ palette")), ["밤편지"]);
        assert_eq!(titles(&s, &s.search("ㅇㅇㅇ ㅂㅍㅈ")), ["밤편지"]);
        assert!(s.search("beatles 아이유").is_empty());
        // Terms may come from different fields but not span a field boundary.
        assert!(s.search("judethe").is_empty());
    }

    #[test]
    fn field_terms_look_at_one_field_only() {
        let mut tracks = vec![
            track(1, "Hey Jude", "The Beatles", "1"),
            track(2, "Beatles Medley", "Stars on 45", "Long Play Album"),
            track(3, "밤편지", "아이유", "Palette"),
            track(4, "Palette", "아이유", "Palette"),
            track(5, "So What", "Miles Davis", "Kind of Blue"),
        ];
        tracks[0].year = Some(1968);
        tracks[0].genre = "Rock".into();
        tracks[1].year = Some(1981);
        tracks[1].genre = "Pop; Disco".into();
        tracks[1].codec = "MP3".into();
        tracks[1].container = "MP3".into();
        tracks[2].year = Some(2017);
        tracks[2].genre = "발라드".into();
        tracks[3].year = Some(2017);
        tracks[3].genre = "K-Pop".into();
        tracks[4].year = Some(1959);
        tracks[4].genre = "Jazz".into();
        tracks[4].album_artist = "Miles Davis Sextet".into();
        let s = LibrarySnapshot::from_tracks(tracks);

        // Plain: anywhere. With a field: only there.
        assert_eq!(
            titles(&s, &s.search("beatles")),
            ["beatles medley", "hey jude"]
        );
        assert_eq!(titles(&s, &s.search("artist:beatles")), ["hey jude"]);
        assert_eq!(titles(&s, &s.search("title:beatles")), ["beatles medley"]);
        assert_eq!(titles(&s, &s.search("title:palette")), ["palette"]);
        assert_eq!(
            titles(&s, &s.search("album:palette")),
            ["palette", "밤편지"]
        );
        assert_eq!(
            titles(&s, &s.search("artist:\"the beatles\"")),
            ["hey jude"]
        );
        // The album artist counts as an artist.
        assert_eq!(titles(&s, &s.search("artist:sextet")), ["so what"]);
        // Korean names and initial consonants work in a field too.
        assert_eq!(titles(&s, &s.search("가수:ㅇㅇㅇ 제목:ㅂㅍㅈ")), ["밤편지"]);
        assert_eq!(titles(&s, &s.search("장르:발라드")), ["밤편지"]);
        assert_eq!(titles(&s, &s.search("장르:ㅂㄹㄷ")), ["밤편지"]);
        assert!(s.search("제목:ㅇㅇㅇ").is_empty());

        // Genre is only searched when asked for.
        assert!(s.search("jazz").is_empty());
        assert_eq!(titles(&s, &s.search("genre:jazz")), ["so what"]);
        assert_eq!(titles(&s, &s.search("genre:disco")), ["beatles medley"]);

        // Years.
        assert_eq!(titles(&s, &s.search("year:2017")), ["palette", "밤편지"]);
        assert_eq!(
            titles(&s, &s.search("year:1960-1989")),
            ["beatles medley", "hey jude"]
        );
        assert_eq!(titles(&s, &s.search("year:<1960")), ["so what"]);
        assert_eq!(titles(&s, &s.search("year:>=2000 밤")), ["밤편지"]);
        assert!(s.search("year:abc").is_empty());

        // Format.
        assert_eq!(titles(&s, &s.search("format:mp3")), ["beatles medley"]);
        assert_eq!(titles(&s, &s.search("format:lossless")).len(), 4);
        assert_eq!(titles(&s, &s.search("형식:무손실 year:1959")), ["so what"]);

        // What the library itself keeps: favourites, play counts, dates.
        let mut kept = vec![
            track(1, "Often", "A", "X"),
            track(2, "Never", "A", "X"),
            track(3, "New", "B", "Y"),
        ];
        const NOW: i64 = 1_800_000_000;
        kept[0].play_count = 12;
        kept[0].favorite = true;
        kept[0].added_at = NOW - 400 * 86_400;
        kept[0].last_played_at = Some(NOW - 3 * 86_400);
        kept[1].added_at = NOW - 400 * 86_400;
        kept[2].play_count = 1;
        kept[2].added_at = NOW - 5 * 86_400;
        kept[2].last_played_at = Some(NOW - 90 * 86_400);
        let k = LibrarySnapshot::from_tracks(kept);
        let found =
            |query: &str| titles(&k, &k.search_prepared(&PreparedQuery::new_at(query, NOW)));
        assert_eq!(found("plays:>=5"), ["often"]);
        assert_eq!(found("plays:0"), ["never"]);
        assert_eq!(found("plays:1-20"), ["new", "often"]);
        assert_eq!(found("fav:yes"), ["often"]);
        assert_eq!(found("fav:no artist:a"), ["never"]);
        assert_eq!(found("added:30d"), ["new"]);
        assert_eq!(found("played:1w"), ["often"]);
        assert_eq!(found("played:1y"), ["new", "often"]);
        assert!(found("fav:perhaps").is_empty());

        // A field still being typed changes nothing; `matches` agrees with `search`.
        assert_eq!(s.search("artist:").len(), 5);
        let q = LibrarySnapshot::prepare_query("artist:beatles");
        let by_matches: Vec<u32> = (0..5).filter(|&i| s.matches(i, &q)).collect();
        assert_eq!(by_matches, s.search("artist:beatles"));
    }

    #[test]
    fn empty_query_matches_everything_in_snapshot_order() {
        let s = sample();
        let all: Vec<u32> = (0..s.len() as u32).collect();
        assert_eq!(s.search(""), all);
        assert_eq!(s.search("   "), all);
        let q = LibrarySnapshot::prepare_query("");
        assert!(q.is_empty());
        assert!(s.matches(0, &q));
    }

    #[test]
    fn matches_agrees_with_search() {
        let s = sample();
        for query in [
            "bea",
            "이유",
            "ㅇㅇㅇ",
            "the jude",
            "zzz",
            "ㅇㅇㅇ real",
            "a",
        ] {
            let q = LibrarySnapshot::prepare_query(query);
            let by_matches: Vec<u32> = (0..s.len() as u32).filter(|&i| s.matches(i, &q)).collect();
            assert_eq!(s.search(query), by_matches, "query {query:?}");
        }
        let q = LibrarySnapshot::prepare_query("bea");
        assert!(!s.matches(10_000, &q));
    }

    #[test]
    fn default_order_and_lookup() {
        let s = sample();
        let order: Vec<&str> = s.tracks.iter().map(|t| t.artist.as_str()).collect();
        // Code point order of the normalised artist: latin before hangul.
        assert_eq!(
            order,
            [
                "Crush",
                "The Beatles",
                "The Beatles",
                "Unknown",
                "방탄소년단",
                // Albums in order: Love Poem (NFD-tagged artist), Palette, Real.
                "\u{110B}\u{1161}\u{110B}\u{1175}\u{110B}\u{1172}",
                "아이유",
                "아이유"
            ]
        );
        for (i, t) in s.tracks.iter().enumerate() {
            assert_eq!(s.index_of(t.id), Some(i as u32));
            assert_eq!(s.get(t.id).unwrap().id, t.id);
        }
        assert!(s.get(999).is_none());
        assert_eq!(s.root_track_count(1), 8);
        assert_eq!(s.root_track_count(2), 0);
    }

    #[test]
    fn sorting_is_stable_and_reversible() {
        let s = sample();
        let mut idx: Vec<u32> = (0..s.len() as u32).collect();
        s.sort(&mut idx, SortKey::Title, true);
        let sorted: Vec<String> = idx
            .iter()
            .map(|&i| normalize(&s.tracks[i as usize].title))
            .collect();
        let mut expected = sorted.clone();
        expected.sort();
        assert_eq!(sorted, expected);

        s.sort(&mut idx, SortKey::Title, false);
        let reversed: Vec<String> = idx
            .iter()
            .map(|&i| normalize(&s.tracks[i as usize].title))
            .collect();
        expected.reverse();
        assert_eq!(reversed, expected);

        // Stable: sorting by artist keeps the previous (title descending) order per artist.
        s.sort(&mut idx, SortKey::Artist, true);
        let beatles: Vec<&str> = idx
            .iter()
            .map(|&i| &s.tracks[i as usize])
            .filter(|t| t.artist == "The Beatles")
            .map(|t| t.title.as_str())
            .collect();
        assert_eq!(beatles, ["Let It Be", "Hey Jude"]);

        s.sort(&mut idx, SortKey::Default, true);
        assert_eq!(idx, (0..s.len() as u32).collect::<Vec<_>>());
        s.sort(&mut idx, SortKey::Default, false);
        assert_eq!(idx, (0..s.len() as u32).rev().collect::<Vec<_>>());

        // Every key works on arbitrary (even out-of-range) indices without panicking.
        for key in [
            SortKey::Default,
            SortKey::Title,
            SortKey::Artist,
            SortKey::Album,
            SortKey::Duration,
            SortKey::TrackNo,
            SortKey::Year,
            SortKey::DateAdded,
            SortKey::PlayCount,
            SortKey::LastPlayed,
            SortKey::Format,
            SortKey::Rating,
        ] {
            let mut odd = vec![3, 900, 0, 3, 7];
            s.sort(&mut odd, key, true);
            s.sort(&mut odd, key, false);
            assert_eq!(odd.len(), 5);
        }
        let mut by_duration: Vec<u32> = (0..s.len() as u32).collect();
        s.sort(&mut by_duration, SortKey::Duration, false);
        let ids: Vec<TrackId> = by_duration
            .iter()
            .map(|&i| s.tracks[i as usize].id)
            .collect();
        assert_eq!(ids, [8, 7, 6, 5, 4, 3, 2, 1]);
    }

    #[test]
    fn empty_text_sorts_last() {
        let s = LibrarySnapshot::from_tracks(vec![
            track(1, "b", "", "x"),
            track(2, "a", "zed", "x"),
            track(3, "c", "alpha", "x"),
        ]);
        let artists: Vec<&str> = s.tracks.iter().map(|t| t.artist.as_str()).collect();
        assert_eq!(artists, ["alpha", "zed", ""]);
    }

    #[test]
    fn albums_and_artists() {
        let mut a1 = track(1, "One", "Queen", "Greatest Hits");
        a1.year = Some(1981);
        a1.art_id = Some("00112233aabbccdd".into());
        a1.track_no = Some(5);
        let mut a2 = track(2, "Two", "Queen", "Greatest Hits");
        a2.disc_no = Some(1);
        a2.track_no = Some(1);
        let b1 = track(3, "Uno", "ABBA", "Greatest Hits");
        // A compilation: same album artist, different track artists, different folders.
        let mut c1 = track(4, "Song A", "Artist A", "Mix");
        let mut c2 = track(5, "Song B", "Artist B", "Mix");
        for c in [&mut c1, &mut c2] {
            c.album_artist = "Various Artists".into();
            c.album_key = album_key("Mix", "Various Artists", true, &c.path);
        }
        let s = LibrarySnapshot::from_tracks(vec![a1, a2, b1, c1, c2]);

        let albums = s.albums();
        let summary: Vec<(&str, &str, usize)> = albums
            .iter()
            .map(|a| (a.artist.as_str(), a.title.as_str(), a.tracks.len()))
            .collect();
        assert_eq!(
            summary,
            [
                ("ABBA", "Greatest Hits", 1),
                ("Queen", "Greatest Hits", 2),
                ("Various Artists", "Mix", 2),
            ]
        );
        let queen = &albums[1];
        assert_eq!(queen.year, Some(1981));
        assert_eq!(queen.art_id.as_deref(), Some("00112233aabbccdd"));
        assert_eq!(queen.duration_ms, 360_003);
        // Track 2 has track number 1 and comes first within the album.
        let ids: Vec<TrackId> = queen
            .tracks
            .iter()
            .map(|&i| s.tracks[i as usize].id)
            .collect();
        assert_eq!(ids, [2, 1]);
        for (i, album) in albums.iter().enumerate() {
            assert_eq!(s.album_index(album.key), Some(i as u32));
        }
        assert_eq!(s.album_index(12345), None);

        let artists: Vec<(&str, usize, usize)> = s
            .artists()
            .iter()
            .map(|a| (a.name.as_str(), a.albums.len(), a.track_count))
            .collect();
        assert_eq!(
            artists,
            [("ABBA", 1, 1), ("Queen", 1, 2), ("Various Artists", 1, 2)]
        );
        assert_eq!(s.artists()[2].albums, [2]);
    }

    #[test]
    fn replacing_unindexed_fields_keeps_index() {
        let s = sample();
        let id = s.tracks[2].id;
        let mut changed = (*s.tracks[2]).clone();
        changed.favorite = true;
        changed.play_count = 9;
        let ghost = Arc::new(track(777, "ghost", "nobody", "none"));
        let patched = s.with_replaced(&[Arc::new(changed), ghost]);
        assert_eq!(patched.len(), s.len());
        assert!(patched.get(id).unwrap().favorite);
        assert!(!s.get(id).unwrap().favorite);
        assert!(patched.get(777).is_none());
        assert_eq!(patched.search("beatles"), s.search("beatles"));
        let mut idx: Vec<u32> = (0..patched.len() as u32).collect();
        patched.sort(&mut idx, SortKey::PlayCount, false);
        assert_eq!(patched.tracks[idx[0] as usize].id, id);
    }

    #[test]
    fn empty_snapshot() {
        let s = LibrarySnapshot::empty();
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
        assert!(s.search("x").is_empty());
        assert!(s.search("").is_empty());
        assert!(s.albums().is_empty());
        assert!(s.artists().is_empty());
        let mut none: Vec<u32> = Vec::new();
        s.sort(&mut none, SortKey::Title, true);
    }
}
