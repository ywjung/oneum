//! The play queue. Pure data and rules, no audio or I/O.
//!
//! `items` is always the actual play order: with shuffle on it is the shuffled order, and
//! the pre-shuffle order is remembered so that turning shuffle off restores it. Queue items
//! have their own ids, so the same track can be queued several times.
use crate::model::TrackId;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub type ItemId = u64;

/// "Previous" restarts the current track when it has played longer than this.
pub const PREVIOUS_RESTART_THRESHOLD_MS: u64 = 3000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueItem {
    pub id: ItemId,
    pub track: TrackId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RepeatMode {
    #[default]
    Off,
    All,
    One,
}

/// Result of pressing "previous".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Previous {
    /// Restart the current track from the beginning.
    Restart,
    /// Move to this item.
    Item(QueueItem),
}

/// Serializable form used for session restore.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QueueState {
    pub items: Vec<QueueItem>,
    pub current: Option<usize>,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub unshuffled: Vec<ItemId>,
}

pub struct Queue {
    items: Vec<QueueItem>,
    current: Option<usize>,
    shuffle: bool,
    repeat: RepeatMode,
    /// Item ids in pre-shuffle order (only meaningful while `shuffle` is on).
    unshuffled: Vec<ItemId>,
    next_id: ItemId,
    rng: fastrand::Rng,
    /// Counts every change to `items` (content or order), so that whoever mirrors the
    /// queue can tell when its copy is stale — also for changes that happen as a side
    /// effect of moving on, like the reshuffle at a "repeat all" wrap.
    revision: u64,
    /// Shuffle whole albums instead of single tracks (with `album_of` set).
    by_album: bool,
    /// The album a track belongs to, for shuffling by album.
    album_of: Option<AlbumOf>,
    /// Shuffle with the listener's stars in mind (with `rating_of` set): songs that
    /// have more of them are more likely to come early.
    by_rating: bool,
    /// The stars a track has, 0 to 5.
    rating_of: Option<RatingOf>,
    /// The pass over a shuffled queue that ended at the last wrap, in the order it was
    /// heard. The wrap mixes the order anew, so the order no longer tells what came
    /// before the first item: "previous" from there walks back through this. Not
    /// kept with the session.
    heard_before: Vec<ItemId>,
}

/// Tells which album a track belongs to.
pub type AlbumOf = Box<dyn Fn(TrackId) -> u64 + Send>;

/// Tells how many stars a track has (0: none).
pub type RatingOf = Box<dyn Fn(TrackId) -> u8 + Send>;

/// How much a song weighs in a shuffle that goes by stars, by its stars: each star
/// more doubles it. A song without stars weighs what one with three does.
const RATING_WEIGHTS: [f64; 6] = [1.0, 0.25, 0.5, 1.0, 2.0, 4.0];

impl Default for Queue {
    fn default() -> Self {
        Self::new()
    }
}

impl Queue {
    pub fn new() -> Self {
        Self::with_rng(fastrand::Rng::new())
    }

    /// Deterministic queue for tests.
    pub fn with_seed(seed: u64) -> Self {
        Self::with_rng(fastrand::Rng::with_seed(seed))
    }

    fn with_rng(rng: fastrand::Rng) -> Self {
        Self {
            items: Vec::new(),
            current: None,
            shuffle: false,
            repeat: RepeatMode::Off,
            unshuffled: Vec::new(),
            next_id: 1,
            rng,
            revision: 0,
            by_album: false,
            by_rating: false,
            rating_of: None,
            album_of: None,
            heard_before: Vec::new(),
        }
    }

    pub fn from_state(state: QueueState) -> Self {
        let mut q = Self::new();
        q.next_id = state.items.iter().map(|i| i.id).max().unwrap_or(0) + 1;
        q.current = state.current.filter(|c| *c < state.items.len());
        q.shuffle = state.shuffle;
        q.repeat = state.repeat;
        q.unshuffled = if state.shuffle {
            state.unshuffled
        } else {
            Vec::new()
        };
        q.items = state.items;
        q
    }

    pub fn to_state(&self) -> QueueState {
        QueueState {
            items: self.items.clone(),
            current: self.current,
            shuffle: self.shuffle,
            repeat: self.repeat,
            unshuffled: self.unshuffled.clone(),
        }
    }

    pub fn items(&self) -> &[QueueItem] {
        &self.items
    }
    /// Changes whenever the items or their order change.
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    pub fn current_index(&self) -> Option<usize> {
        self.current
    }
    pub fn current(&self) -> Option<QueueItem> {
        self.current.and_then(|i| self.items.get(i).copied())
    }
    pub fn shuffle(&self) -> bool {
        self.shuffle
    }
    pub fn repeat(&self) -> RepeatMode {
        self.repeat
    }
    pub fn set_repeat(&mut self, mode: RepeatMode) {
        self.repeat = mode;
    }

    /// Gives the queue a way to tell albums apart (needed for shuffling by album).
    pub fn set_album_lookup(&mut self, album_of: AlbumOf) {
        self.album_of = Some(album_of);
    }

    /// Shuffle by album: whole albums come in random order, each album's tracks in
    /// their order (and without gaps where the album has none). Changing it while
    /// shuffled mixes the queue again; the current track stays current.
    pub fn set_shuffle_by_album(&mut self, on: bool) {
        if self.by_album == on {
            return;
        }
        self.by_album = on;
        if self.shuffle {
            self.set_shuffle(false);
            self.set_shuffle(true);
        }
    }

    /// Sets the album mode without mixing anything (for a queue restored as it was).
    pub fn restore_shuffle_by_album(&mut self, on: bool) {
        self.by_album = on;
    }

    pub fn set_rating_lookup(&mut self, rating_of: RatingOf) {
        self.rating_of = Some(rating_of);
    }

    /// Shuffle by stars: every song still comes once, but one with more stars is more
    /// likely to come early (not in album mode, which mixes albums, not songs).
    /// Changing it while shuffled mixes the queue again; the current track stays
    /// current.
    pub fn set_shuffle_by_rating(&mut self, on: bool) {
        if self.by_rating == on {
            return;
        }
        self.by_rating = on;
        // (Albums are mixed without regard to stars: such a queue stays as it is.)
        if self.shuffle && !self.by_album {
            self.set_shuffle(false);
            self.set_shuffle(true);
        }
    }

    /// Sets the star mode without mixing anything (for a queue restored as it was).
    pub fn restore_shuffle_by_rating(&mut self, on: bool) {
        self.by_rating = on;
    }

    fn albums_shuffled(&self) -> bool {
        self.by_album && self.album_of.is_some()
    }

    /// Album mode: moves the item at `at` and the tracks of its album that come after
    /// it to the front, in their order. Returns how many were moved.
    fn bring_album_to_front(&mut self, at: usize) -> usize {
        let Some(album_of) = &self.album_of else {
            return 0;
        };
        let key = album_of(self.items[at].track);
        let mut head = Vec::new();
        let mut rest = Vec::new();
        for (index, item) in self.items.drain(..).enumerate() {
            if index >= at && album_of(item.track) == key {
                head.push(item);
            } else {
                rest.push(item);
            }
        }
        let moved = head.len();
        head.extend(rest);
        self.items = head;
        moved
    }

    fn index_of(&self, id: ItemId) -> Option<usize> {
        self.items.iter().position(|i| i.id == id)
    }

    fn make_items(&mut self, tracks: &[TrackId]) -> Vec<QueueItem> {
        tracks
            .iter()
            .map(|&track| {
                let id = self.next_id;
                self.next_id += 1;
                QueueItem { id, track }
            })
            .collect()
    }

    /// Shuffles `items[from..]` in place: track by track, or in album mode album by
    /// album (each album's tracks together, in the order they had).
    fn shuffle_tail(&mut self, from: usize) {
        if from >= self.items.len() {
            return;
        }
        match &self.album_of {
            Some(album_of) if self.by_album => {
                let tail = self.items.split_off(from);
                let mut order: Vec<u64> = Vec::new();
                let mut albums: HashMap<u64, Vec<QueueItem>> = HashMap::new();
                for item in tail {
                    let key = album_of(item.track);
                    albums
                        .entry(key)
                        .or_insert_with(|| {
                            order.push(key);
                            Vec::new()
                        })
                        .push(item);
                }
                let mut groups: Vec<Vec<QueueItem>> = order
                    .into_iter()
                    .filter_map(|key| albums.remove(&key))
                    .collect();
                self.rng.shuffle(&mut groups);
                self.items.extend(groups.into_iter().flatten());
            }
            _ => match &self.rating_of {
                Some(rating_of) if self.by_rating => {
                    // Each song draws a number between 0 and 1 and raises it to one
                    // over its weight; the highest come first. (A heavier song's
                    // number is the likelier to be high: a draw without putting back
                    // in which each song's chance is in proportion to its weight.)
                    let weights: Vec<f64> = self.items[from..]
                        .iter()
                        .map(|item| RATING_WEIGHTS[usize::from(rating_of(item.track).min(5))])
                        .collect();
                    let tail = self.items.split_off(from);
                    let mut drawn: Vec<(f64, QueueItem)> = tail
                        .into_iter()
                        .zip(weights)
                        .map(|(item, weight)| {
                            let number = (1.0 - self.rng.f64()).max(f64::MIN_POSITIVE);
                            (number.powf(1.0 / weight), item)
                        })
                        .collect();
                    drawn.sort_by(|a, b| b.0.total_cmp(&a.0));
                    self.items.extend(drawn.into_iter().map(|(_, item)| item));
                }
                _ => self.rng.shuffle(&mut self.items[from..]),
            },
        }
    }

    /// Replaces the queue with `tracks` and makes `start_index` current. With shuffle on,
    /// the chosen track is played first and the rest follow in random order.
    pub fn replace(&mut self, tracks: &[TrackId], start_index: usize) -> Option<QueueItem> {
        self.revision += 1;
        self.items = self.make_items(tracks);
        self.unshuffled.clear();
        self.heard_before.clear();
        if self.items.is_empty() {
            self.current = None;
            return None;
        }
        let start = start_index.min(self.items.len() - 1);
        if self.shuffle {
            self.unshuffled = self.items.iter().map(|i| i.id).collect();
            if self.albums_shuffled() {
                let moved = self.bring_album_to_front(start);
                self.shuffle_tail(moved);
            } else {
                self.items.swap(0, start);
                self.shuffle_tail(1);
            }
            self.current = Some(0);
        } else {
            self.current = Some(start);
        }
        self.current()
    }

    /// Inserts `tracks` right after the current item, in the given order ("다음에 재생").
    /// On an empty or stopped queue they go to the front and the first becomes current.
    pub fn play_next(&mut self, tracks: &[TrackId]) -> Vec<QueueItem> {
        let new = self.make_items(tracks);
        if new.is_empty() {
            return new;
        }
        self.revision += 1;
        let at = self.current.map_or(0, |c| c + 1);
        if self.shuffle {
            // In the original order they follow the current item as well.
            let anchor = self
                .current()
                .and_then(|c| self.unshuffled.iter().position(|id| *id == c.id));
            let pos = anchor.map_or(0, |p| p + 1);
            self.unshuffled.splice(pos..pos, new.iter().map(|i| i.id));
        }
        self.items.splice(at..at, new.iter().copied());
        if self.current.is_none() {
            self.current = Some(0);
        }
        new
    }

    /// Appends `tracks` at the very end of the play order ("대기열 끝에 추가"), also when shuffled.
    pub fn append(&mut self, tracks: &[TrackId]) -> Vec<QueueItem> {
        let new = self.make_items(tracks);
        self.revision += 1;
        if self.shuffle {
            self.unshuffled.extend(new.iter().map(|i| i.id));
        }
        self.items.extend(new.iter().copied());
        if self.current.is_none() && !self.items.is_empty() {
            self.current = Some(0);
        }
        new
    }

    /// Removes items. Returns true when the current item was among them; the item that
    /// followed it (if any) is then current, otherwise the queue has no current item.
    pub fn remove(&mut self, ids: &[ItemId]) -> bool {
        // Sets, so that removing thousands of items from a large queue stays linear.
        let ids: HashSet<ItemId> = ids.iter().copied().collect();
        let current_id = self.current().map(|c| c.id);
        let current_removed = current_id.is_some_and(|id| ids.contains(&id));
        // The successor of a removed current item is the first survivor after it.
        let successor = if current_removed {
            self.current.and_then(|c| {
                self.items[c + 1..]
                    .iter()
                    .find(|i| !ids.contains(&i.id))
                    .map(|i| i.id)
            })
        } else {
            current_id
        };
        self.revision += 1;
        self.items.retain(|i| !ids.contains(&i.id));
        self.unshuffled.retain(|id| !ids.contains(id));
        self.current = successor.and_then(|id| self.index_of(id));
        current_removed
    }

    /// Removes every item whose track fails `keep`. Same return value as [`Queue::remove`].
    pub fn retain_tracks(&mut self, mut keep: impl FnMut(TrackId) -> bool) -> bool {
        let gone: Vec<ItemId> = self
            .items
            .iter()
            .filter(|i| !keep(i.track))
            .map(|i| i.id)
            .collect();
        if gone.is_empty() {
            false
        } else {
            self.remove(&gone)
        }
    }

    pub fn clear(&mut self) {
        self.revision += 1;
        self.items.clear();
        self.unshuffled.clear();
        self.heard_before.clear();
        self.current = None;
    }

    /// Moves the given items (keeping their relative order) so that they start at
    /// `to_index` of the list with those items taken out. The current item stays current.
    pub fn move_items(&mut self, ids: &[ItemId], to_index: usize) {
        let ids: HashSet<ItemId> = ids.iter().copied().collect();
        let current_id = self.current().map(|c| c.id);
        let moved: Vec<QueueItem> = self
            .items
            .iter()
            .filter(|i| ids.contains(&i.id))
            .copied()
            .collect();
        if moved.is_empty() {
            return;
        }
        self.revision += 1;
        self.items.retain(|i| !ids.contains(&i.id));
        let at = to_index.min(self.items.len());
        self.items.splice(at..at, moved);
        self.current = current_id.and_then(|id| self.index_of(id));
    }

    /// Makes `id` current (double-click in the queue panel).
    pub fn jump_to(&mut self, id: ItemId) -> Option<QueueItem> {
        let index = self.index_of(id)?;
        self.current = Some(index);
        self.current()
    }

    /// Turns shuffle on or off. On: the current item moves to the front and the rest is
    /// shuffled. Off: the pre-shuffle order is restored. The current item never changes.
    pub fn set_shuffle(&mut self, on: bool) {
        if on == self.shuffle {
            return;
        }
        self.shuffle = on;
        self.revision += 1;
        self.heard_before.clear();
        let current_id = self.current().map(|c| c.id);
        if on {
            self.unshuffled = self.items.iter().map(|i| i.id).collect();
            match self.current {
                Some(c) if self.albums_shuffled() => {
                    let moved = self.bring_album_to_front(c);
                    self.shuffle_tail(moved);
                }
                Some(c) => {
                    let item = self.items.remove(c);
                    self.items.insert(0, item);
                    self.shuffle_tail(1);
                }
                None => self.shuffle_tail(0),
            }
        } else {
            let order: HashMap<ItemId, usize> = std::mem::take(&mut self.unshuffled)
                .into_iter()
                .enumerate()
                .map(|(position, id)| (id, position))
                .collect();
            // Items missing from the remembered order keep their relative order at the end
            // (the sort is stable).
            self.items
                .sort_by_key(|i| order.get(&i.id).copied().unwrap_or(usize::MAX));
        }
        self.current = current_id.and_then(|id| self.index_of(id));
    }

    /// Starts another pass over a shuffled queue in a fresh order. The track that just
    /// played may come anywhere in it but first (in album mode: its album), so that it
    /// is not heard twice in a row; the pass that ended is remembered, so that
    /// "previous" from the first item still leads to what was heard before it.
    fn reshuffle_for_wrap(&mut self) {
        self.revision += 1;
        let heard: Vec<ItemId> = match self.current {
            Some(c) => self.items[..=c].iter().map(|item| item.id).collect(),
            None => Vec::new(),
        };
        let last = self.current();
        self.shuffle_tail(0);
        if let Some(last) = last {
            self.keep_from_the_front(last);
        }
        self.heard_before = heard;
    }

    /// After a fresh shuffle: `last` (the track that just played) or, in album mode,
    /// its album, if it came first, goes to a place drawn among the others. Every
    /// other order is as likely as before.
    fn keep_from_the_front(&mut self, last: QueueItem) {
        if self.items.len() < 2 {
            return;
        }
        match &self.album_of {
            Some(album_of) if self.by_album => {
                let key = album_of(last.track);
                let lead = self
                    .items
                    .iter()
                    .take_while(|item| album_of(item.track) == key)
                    .count();
                if lead == 0 || lead == self.items.len() {
                    return;
                }
                let album: Vec<QueueItem> = self.items.drain(..lead).collect();
                // Where another album ends: after the first one at the earliest.
                let ends: Vec<usize> = (1..=self.items.len())
                    .filter(|&at| {
                        at == self.items.len()
                            || album_of(self.items[at].track) != album_of(self.items[at - 1].track)
                    })
                    .collect();
                let at = ends[self.rng.usize(..ends.len())];
                self.items.splice(at..at, album);
            }
            _ => {
                if self.items[0].id == last.id {
                    let to = self.rng.usize(1..self.items.len());
                    self.items.swap(0, to);
                }
            }
        }
    }

    /// "Previous" from the first item of a pass that a wrap mixed: the item heard
    /// before it in the pass that ended comes to the front and is current. `None` when
    /// there is no such pass, or nothing of it is left to go back to.
    fn back_into_the_pass_before(&mut self) -> Option<QueueItem> {
        if !self.shuffle {
            self.heard_before.clear();
            return None;
        }
        while let Some(id) = self.heard_before.pop() {
            // (One that has been taken out of the queue since, or that is the current
            // item itself, is passed over.)
            match self.index_of(id) {
                Some(at) if at > 0 => {
                    self.revision += 1;
                    let item = self.items.remove(at);
                    self.items.insert(0, item);
                    self.current = Some(0);
                    return Some(item);
                }
                _ => {}
            }
        }
        None
    }

    /// The item that follows `id` when playback continues on its own, without changing
    /// anything. `None` at the end of the queue — including the "repeat all + shuffle"
    /// wrap, where the new order is only decided when the wrap actually happens.
    pub fn peek_after(&self, id: ItemId) -> Option<QueueItem> {
        let index = self.index_of(id)?;
        match self.repeat {
            RepeatMode::One => self.items.get(index).copied(),
            _ if index + 1 < self.items.len() => self.items.get(index + 1).copied(),
            RepeatMode::All if !self.shuffle => self.items.first().copied(),
            _ => None,
        }
    }

    /// The item that follows the current one when it ends naturally (see [`Queue::peek_after`]).
    pub fn peek_next_auto(&self) -> Option<QueueItem> {
        self.current().and_then(|c| self.peek_after(c.id))
    }

    /// The current track ended naturally: advance according to the repeat mode.
    /// `None` means playback stops; the last item stays current.
    pub fn advance_auto(&mut self) -> Option<QueueItem> {
        let c = self.current?;
        match self.repeat {
            RepeatMode::One => {}
            _ if c + 1 < self.items.len() => self.current = Some(c + 1),
            RepeatMode::All => {
                if self.shuffle {
                    self.reshuffle_for_wrap();
                }
                self.current = Some(0);
            }
            RepeatMode::Off => return None,
        }
        self.current()
    }

    /// The current track cannot be played: move on. Like a natural end, except that
    /// "repeat one" does not retry the broken track.
    pub fn advance_after_failure(&mut self) -> Option<QueueItem> {
        if self.repeat == RepeatMode::One {
            self.next_manual()
        } else {
            self.advance_auto()
        }
    }

    /// The user pressed "next". At the end: repeat off stops (`None`, last item stays
    /// current); repeat all/one wraps to the start. "Repeat one" does not hold the user
    /// on the same track.
    pub fn next_manual(&mut self) -> Option<QueueItem> {
        let c = self.current?;
        if c + 1 < self.items.len() {
            self.current = Some(c + 1);
        } else if self.repeat == RepeatMode::Off {
            return None;
        } else {
            if self.shuffle {
                self.reshuffle_for_wrap();
            }
            self.current = Some(0);
        }
        self.current()
    }

    /// The user pressed "previous" at `position_ms` of the current track. In a shuffled
    /// queue this walks back through the order that was actually played.
    pub fn previous_manual(&mut self, position_ms: u64) -> Option<Previous> {
        let c = self.current?;
        if position_ms > PREVIOUS_RESTART_THRESHOLD_MS {
            return Some(Previous::Restart);
        }
        if c > 0 {
            self.current = Some(c - 1);
        } else if let Some(item) = self.back_into_the_pass_before() {
            return Some(Previous::Item(item));
        } else if self.repeat == RepeatMode::Off || self.items.len() == 1 {
            return Some(Previous::Restart);
        } else {
            self.current = Some(self.items.len() - 1);
        }
        self.current().map(Previous::Item)
    }

    /// Makes the first item current (used when "play" is pressed after the queue ended).
    pub fn rewind(&mut self) -> Option<QueueItem> {
        if self.items.is_empty() {
            self.current = None;
        } else {
            self.current = Some(0);
        }
        self.current()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracks(q: &Queue) -> Vec<TrackId> {
        q.items().iter().map(|i| i.track).collect()
    }
    fn cur(q: &Queue) -> TrackId {
        q.current().expect("has current").track
    }
    fn cur_item(q: &Queue) -> QueueItem {
        q.current().expect("has current")
    }
    fn queue(n: i64, start: usize) -> Queue {
        let mut q = Queue::with_seed(42);
        q.replace(&(1..=n).collect::<Vec<_>>(), start);
        q
    }

    /// Albums of four tracks: 11–14, 21–24, … (the tens digit is the album).
    fn album_queue(seed: u64) -> Queue {
        let mut q = Queue::with_seed(seed);
        q.set_album_lookup(Box::new(|track| (track / 10) as u64));
        q.set_shuffle_by_album(true);
        let tracks: Vec<TrackId> = (1..=6)
            .flat_map(|a| (1..=4).map(move |t| a * 10 + t))
            .collect();
        q.replace(&tracks, 0);
        q
    }

    /// Each album's tracks stay together and in order.
    fn albums_are_whole(order: &[TrackId]) -> bool {
        order
            .chunks(4)
            .all(|album| album.windows(2).all(|w| w[1] == w[0] + 1) && album[0] % 10 == 1)
    }

    /// Shuffled by stars, every song still comes once, and over many shuffles the
    /// songs with five stars stand earlier than those without, and those earlier than
    /// the ones with a single star.
    #[test]
    fn shuffling_by_stars_brings_the_songs_with_more_of_them_forward() {
        let stars = |track: TrackId| match track {
            1..=10 => 5,
            11..=20 => 1,
            _ => 0,
        };
        let all: Vec<TrackId> = (1..=40).collect();
        let (mut five, mut one, mut none) = (0usize, 0usize, 0usize);
        for seed in 0..200 {
            let mut q = Queue::with_seed(seed);
            q.set_rating_lookup(Box::new(stars));
            q.set_shuffle_by_rating(true);
            q.replace(&all, 0);
            q.set_shuffle(true);
            let order = tracks(&q);
            let mut sorted = order.clone();
            sorted.sort_unstable();
            assert_eq!(sorted, all, "every song once");
            assert_eq!(order[0], 1, "the song that was on stays first");
            for (place, track) in order.iter().enumerate().skip(1) {
                match stars(*track) {
                    5 => five += place,
                    1 => one += place,
                    _ => none += place,
                }
            }
        }
        // Mean places (of 39 that are shuffled): nine songs with five stars, ten with
        // one, twenty without.
        let (five, one, none) = (
            five as f64 / (200.0 * 9.0),
            one as f64 / (200.0 * 10.0),
            none as f64 / (200.0 * 20.0),
        );
        assert!(five < 12.0, "five stars: mean place {five}");
        assert!(
            none > five + 6.0,
            "no stars: {none} after five stars: {five}"
        );
        assert!(one > none + 6.0, "one star: {one} after no stars: {none}");

        // Switched off, the stars make no difference worth the name.
        let (mut five, mut one) = (0usize, 0usize);
        for seed in 0..200 {
            let mut q = Queue::with_seed(seed);
            q.set_rating_lookup(Box::new(stars));
            q.replace(&all, 0);
            q.set_shuffle(true);
            for (place, track) in tracks(&q).iter().enumerate().skip(1) {
                match stars(*track) {
                    5 => five += place,
                    1 => one += place,
                    _ => {}
                }
            }
        }
        let (five, one) = (five as f64 / 1800.0, one as f64 / 2000.0);
        assert!((five - one).abs() < 3.0, "{five} and {one}");
    }

    #[test]
    fn shuffling_by_album_keeps_albums_whole() {
        for seed in 0..20 {
            let mut q = album_queue(seed);
            q.jump_to(q.items()[5].id); // track 22
            q.set_shuffle(true);
            let order = tracks(&q);
            // The current track first, then the rest of its album, then whole albums.
            assert_eq!(&order[..3], &[22, 23, 24], "seed {seed}");
            // The track before the current one in its album comes later, on its own.
            let rest: Vec<TrackId> = order[3..].iter().copied().filter(|t| *t != 21).collect();
            assert!(albums_are_whole(&rest), "seed {seed}: {order:?}");
            assert_eq!(cur(&q), 22);
            // Off: the original order, the same current track.
            q.set_shuffle(false);
            assert_eq!(
                tracks(&q),
                (1..=6)
                    .flat_map(|a| (1..=4).map(move |t| a * 10 + t))
                    .collect::<Vec<_>>()
            );
            assert_eq!(cur(&q), 22);
        }
        // Not every seed gives the albums in their original order.
        let orders: HashSet<Vec<TrackId>> = (0..20)
            .map(|seed| {
                let mut q = album_queue(seed);
                q.set_shuffle(true);
                tracks(&q)
            })
            .collect();
        assert!(orders.len() > 5);
    }

    #[test]
    fn a_new_pass_by_album_starts_with_another_album() {
        let mut q = album_queue(7);
        q.set_shuffle(true);
        q.set_repeat(RepeatMode::All);
        assert!(albums_are_whole(&tracks(&q)));
        for _ in 0..23 {
            q.advance_auto().expect("more items");
        }
        let last_album = cur(&q) / 10;
        let first = q.advance_auto().expect("wraps").track;
        assert_ne!(first / 10, last_album);
        let order = tracks(&q);
        assert!(albums_are_whole(&order), "{order:?}");
        // Over many passes the album heard last is not always the one that comes last
        // (it may come anywhere but first), and never comes first.
        let mut last_albums = HashSet::new();
        for _ in 0..40 {
            while q.current_index() != Some(q.len() - 1) {
                q.advance_auto().expect("more items");
            }
            let heard = cur(&q) / 10;
            last_albums.insert(heard);
            let first = q.advance_auto().expect("wraps").track;
            assert_ne!(first / 10, heard);
            assert!(albums_are_whole(&tracks(&q)));
        }
        assert!(last_albums.len() > 2, "{last_albums:?}");
    }

    #[test]
    fn switching_to_album_shuffle_while_shuffled_regroups() {
        let mut q = album_queue(3);
        q.set_shuffle_by_album(false);
        q.set_shuffle(true);
        let current = cur(&q);
        q.set_shuffle_by_album(true);
        assert_eq!(cur(&q), current);
        assert_eq!(q.current_index(), Some(0));
        let order = tracks(&q);
        let album = current / 10;
        let head: Vec<TrackId> = order
            .iter()
            .copied()
            .take_while(|t| t / 10 == album)
            .collect();
        assert_eq!(head[0], current);
        assert!(head.windows(2).all(|w| w[1] == w[0] + 1));
    }

    /// The queue driven through random edits, moves and mode changes (shuffle, by
    /// album or not, repeat): whatever happens, it holds exactly the items put in and
    /// not taken out, each once; the current item is one of them; the unshuffled order
    /// lists the same items; turning shuffle off always gives the same order back; and
    /// a queue restored from its saved state is the same queue.
    #[test]
    fn random_edits_keep_the_queue_whole() {
        for seed in 1..=300u64 {
            let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
            let mut roll = move |n: u64| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 11) % n
            };
            let mut q = Queue::with_seed(seed);
            q.set_album_lookup(Box::new(|track| (track / 10) as u64));
            // The items the queue should hold: id -> track.
            let mut held: HashMap<ItemId, TrackId> = HashMap::new();
            let mut log: Vec<String> = Vec::new();
            for step in 0..120 {
                let some_tracks = |roll: &mut dyn FnMut(u64) -> u64| -> Vec<TrackId> {
                    (0..1 + roll(6))
                        .map(|_| (1 + roll(4) as i64) * 10 + 1 + roll(4) as i64)
                        .collect()
                };
                let some_ids = |q: &Queue, roll: &mut dyn FnMut(u64) -> u64| -> Vec<ItemId> {
                    let n = q.len() as u64;
                    if n == 0 {
                        return Vec::new();
                    }
                    (0..1 + roll(3))
                        .map(|_| q.items()[roll(n) as usize].id)
                        .collect()
                };
                let op = roll(17);
                let current_before = q.current().map(|c| c.id);
                match op {
                    0 => {
                        let tracks = some_tracks(&mut roll);
                        let start = roll(tracks.len() as u64) as usize;
                        q.replace(&tracks, start);
                        held = q.items().iter().map(|i| (i.id, i.track)).collect();
                    }
                    1 => {
                        for item in q.play_next(&some_tracks(&mut roll)) {
                            held.insert(item.id, item.track);
                        }
                    }
                    2 => {
                        for item in q.append(&some_tracks(&mut roll)) {
                            held.insert(item.id, item.track);
                        }
                    }
                    3 => {
                        let ids = some_ids(&q, &mut roll);
                        q.remove(&ids);
                        for id in ids {
                            held.remove(&id);
                        }
                    }
                    4 => {
                        let album = 1 + roll(4) as i64;
                        q.retain_tracks(|track| track / 10 != album);
                        held.retain(|_, track| *track / 10 != album);
                    }
                    5 => {
                        let ids = some_ids(&q, &mut roll);
                        let to = roll(q.len() as u64 + 1) as usize;
                        q.move_items(&ids, to);
                    }
                    6 => {
                        if let Some(id) = some_ids(&q, &mut roll).first() {
                            q.jump_to(*id);
                        }
                    }
                    7 => {
                        let on = !q.shuffle();
                        let by_album = q.albums_shuffled();
                        q.set_shuffle(on);
                        // The current item leads the shuffled order: the rest of the
                        // queue is still to come after it.
                        if on && current_before.is_some() {
                            assert_eq!(q.current_index(), Some(0), "seed {seed} step {step}");
                        }
                        if on && by_album {
                            // After the current track and the rest of its album come
                            // whole albums: no album in more than one run (the current
                            // one: the run that leads, and its earlier tracks).
                            let albums: Vec<i64> = q.items().iter().map(|i| i.track / 10).collect();
                            let mut runs: HashMap<i64, usize> = HashMap::new();
                            for (i, album) in albums.iter().enumerate() {
                                if i == 0 || albums[i - 1] != *album {
                                    *runs.entry(*album).or_default() += 1;
                                }
                            }
                            let current = q.current().map(|c| c.track / 10);
                            for (album, n) in runs {
                                let most = if Some(album) == current { 2 } else { 1 };
                                assert!(n <= most, "seed {seed} step {step}: {albums:?}");
                            }
                        }
                    }
                    8 => q.set_shuffle_by_album(roll(2) == 0),
                    9 => q.set_repeat(
                        [RepeatMode::Off, RepeatMode::All, RepeatMode::One][roll(3) as usize],
                    ),
                    10 => {
                        q.advance_auto();
                    }
                    11 => {
                        q.next_manual();
                    }
                    12 => {
                        q.previous_manual(roll(2) * 10_000);
                    }
                    13 => {
                        q.advance_after_failure();
                    }
                    14 => {
                        q.rewind();
                    }
                    15 => {
                        if roll(8) == 0 {
                            q.clear();
                            held.clear();
                        }
                    }
                    _ => {
                        // Shuffle off always gives the same order, with the same
                        // current item.
                        if !q.shuffle() {
                            let (order, current) = (q.items().to_vec(), q.current());
                            q.set_shuffle(true);
                            q.set_shuffle(false);
                            assert_eq!(q.items(), order, "seed {seed} step {step}");
                            assert_eq!(q.current(), current, "seed {seed} step {step}");
                        }
                    }
                }
                log.push(format!("{step}: op {op}"));
                let context = || format!("seed {seed}: {}", log.join(", "));
                // Moving items and changing modes leave the current item the current one.
                if matches!(op, 5 | 7 | 8 | 9 | 16) {
                    assert_eq!(q.current().map(|c| c.id), current_before, "{}", context());
                }
                let ids: HashSet<ItemId> = q.items().iter().map(|i| i.id).collect();
                assert_eq!(ids.len(), q.len(), "an item twice: {}", context());
                let now: HashMap<ItemId, TrackId> =
                    q.items().iter().map(|i| (i.id, i.track)).collect();
                assert_eq!(now, held, "{}", context());
                match q.current_index() {
                    Some(c) => assert!(c < q.len(), "{}", context()),
                    None => assert!(q.current().is_none()),
                }
                let saved = q.to_state();
                if q.shuffle() {
                    let unshuffled: HashSet<ItemId> = saved.unshuffled.iter().copied().collect();
                    assert_eq!(saved.unshuffled.len(), q.len(), "{}", context());
                    assert_eq!(unshuffled, ids, "{}", context());
                } else {
                    assert!(saved.unshuffled.is_empty(), "{}", context());
                }
                assert_eq!(
                    Queue::from_state(saved.clone()).to_state(),
                    saved,
                    "{}",
                    context()
                );
            }
        }
    }

    #[test]
    fn replace_starts_at_the_chosen_track() {
        let q = queue(5, 2);
        assert_eq!(tracks(&q), vec![1, 2, 3, 4, 5]);
        assert_eq!(cur(&q), 3);
        let mut empty = Queue::new();
        assert_eq!(empty.replace(&[], 0), None);
        assert_eq!(empty.current(), None);
    }

    #[test]
    fn replace_with_shuffle_plays_the_chosen_track_first() {
        let mut q = Queue::with_seed(7);
        q.set_shuffle(true);
        q.replace(&(1..=50).collect::<Vec<_>>(), 20);
        assert_eq!(q.current_index(), Some(0));
        assert_eq!(cur(&q), 21);
        let mut sorted = tracks(&q);
        assert_ne!(sorted, (1..=50).collect::<Vec<_>>(), "rest is shuffled");
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            (1..=50).collect::<Vec<_>>(),
            "nothing lost or duplicated"
        );
    }

    #[test]
    fn play_next_inserts_after_current_in_order() {
        let mut q = queue(3, 0);
        q.play_next(&[10, 11]);
        assert_eq!(tracks(&q), vec![1, 10, 11, 2, 3]);
        assert_eq!(cur(&q), 1);
        // A second "play next" goes in front of the earlier one, right after the current track.
        q.play_next(&[20]);
        assert_eq!(tracks(&q), vec![1, 20, 10, 11, 2, 3]);
    }

    #[test]
    fn play_next_and_append_on_empty_queue_make_first_current() {
        let mut q = Queue::new();
        q.play_next(&[5, 6]);
        assert_eq!(tracks(&q), vec![5, 6]);
        assert_eq!(cur(&q), 5);
        let mut q = Queue::new();
        q.append(&[7]);
        assert_eq!(cur(&q), 7);
    }

    #[test]
    fn append_goes_to_the_end_even_when_shuffled() {
        let mut q = queue(10, 0);
        q.set_shuffle(true);
        q.append(&[99]);
        assert_eq!(*tracks(&q).last().expect("item"), 99);
        q.set_shuffle(false);
        assert_eq!(tracks(&q), vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 99]);
    }

    #[test]
    fn duplicates_are_distinct_items() {
        let mut q = Queue::new();
        q.append(&[1, 1, 1]);
        let ids: Vec<ItemId> = q.items().iter().map(|i| i.id).collect();
        assert_eq!(tracks(&q), vec![1, 1, 1]);
        assert!(ids[0] != ids[1] && ids[1] != ids[2]);
        q.remove(&[ids[1]]);
        assert_eq!(q.len(), 2);
        assert_eq!(q.items()[1].id, ids[2]);
    }

    #[test]
    fn removing_the_current_item_moves_to_the_next() {
        let mut q = queue(4, 1);
        let id = q.current().expect("current").id;
        assert!(q.remove(&[id]));
        assert_eq!(tracks(&q), vec![1, 3, 4]);
        assert_eq!(cur(&q), 3);
    }

    #[test]
    fn removing_the_current_last_item_leaves_no_current() {
        let mut q = queue(3, 2);
        let id = q.current().expect("current").id;
        assert!(q.remove(&[id]));
        assert_eq!(q.current(), None);
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn removing_other_items_keeps_the_current_item() {
        let mut q = queue(5, 3);
        let ids: Vec<ItemId> = q.items().iter().map(|i| i.id).collect();
        assert!(!q.remove(&[ids[0], ids[1], ids[4]]));
        assert_eq!(tracks(&q), vec![3, 4]);
        assert_eq!(cur(&q), 4);
        // Removing the current together with its successors falls back to the next survivor.
        let mut q = queue(5, 1);
        let ids: Vec<ItemId> = q.items().iter().map(|i| i.id).collect();
        assert!(q.remove(&[ids[1], ids[2]]));
        assert_eq!(cur(&q), 4);
    }

    #[test]
    fn move_items_keeps_current_and_relative_order() {
        let mut q = queue(5, 2);
        let ids: Vec<ItemId> = q.items().iter().map(|i| i.id).collect();
        q.move_items(&[ids[3], ids[0]], 1);
        // Remaining: 2 3 5 → insert [1, 4] (original relative order) at index 1.
        assert_eq!(tracks(&q), vec![2, 1, 4, 3, 5]);
        assert_eq!(cur(&q), 3);
        q.move_items(&[ids[2]], 99);
        assert_eq!(tracks(&q), vec![2, 1, 4, 5, 3]);
        assert_eq!(cur(&q), 3);
    }

    #[test]
    fn natural_end_advances_and_stops_at_the_end_without_repeat() {
        let mut q = queue(3, 1);
        assert_eq!(q.peek_next_auto().map(|i| i.track), Some(3));
        assert_eq!(q.advance_auto().map(|i| i.track), Some(3));
        assert_eq!(q.peek_next_auto(), None);
        assert_eq!(q.advance_auto(), None);
        assert_eq!(cur(&q), 3, "last item stays current after the queue ends");
    }

    #[test]
    fn repeat_all_wraps_to_the_first_item() {
        let mut q = queue(3, 2);
        q.set_repeat(RepeatMode::All);
        assert_eq!(q.peek_next_auto().map(|i| i.track), Some(1));
        assert_eq!(q.advance_auto().map(|i| i.track), Some(1));
    }

    #[test]
    fn repeat_one_repeats_on_natural_end_but_not_on_manual_next() {
        let mut q = queue(3, 1);
        q.set_repeat(RepeatMode::One);
        assert_eq!(q.peek_next_auto().map(|i| i.track), Some(2));
        assert_eq!(q.advance_auto().map(|i| i.track), Some(2));
        // The user's explicit "next" moves on, and wraps at the end like repeat-all.
        assert_eq!(q.next_manual().map(|i| i.track), Some(3));
        assert_eq!(q.next_manual().map(|i| i.track), Some(1));
        assert_eq!(q.repeat(), RepeatMode::One, "mode is kept");
    }

    #[test]
    fn manual_next_at_the_end_stops_without_repeat() {
        let mut q = queue(2, 1);
        assert_eq!(q.next_manual(), None);
        assert_eq!(cur(&q), 2);
    }

    #[test]
    fn previous_restarts_after_three_seconds() {
        let mut q = queue(3, 1);
        assert_eq!(q.previous_manual(3001), Some(Previous::Restart));
        assert_eq!(cur(&q), 2);
        assert_eq!(
            q.previous_manual(3000)
                .map(|p| matches!(p, Previous::Item(i) if i.track == 1)),
            Some(true)
        );
    }

    #[test]
    fn previous_at_the_first_item() {
        let mut q = queue(3, 0);
        assert_eq!(q.previous_manual(0), Some(Previous::Restart));
        q.set_repeat(RepeatMode::All);
        assert!(matches!(q.previous_manual(0), Some(Previous::Item(i)) if i.track == 3));
        let mut single = queue(1, 0);
        single.set_repeat(RepeatMode::All);
        assert_eq!(single.previous_manual(0), Some(Previous::Restart));
        assert_eq!(Queue::new().previous_manual(0), None);
    }

    #[test]
    fn shuffle_keeps_current_and_restores_original_order() {
        let mut q = queue(30, 12);
        q.set_shuffle(true);
        assert_eq!(q.current_index(), Some(0));
        assert_eq!(cur(&q), 13);
        assert_ne!(tracks(&q), (1..=30).collect::<Vec<_>>());
        q.next_manual();
        q.next_manual();
        let playing = cur(&q);
        q.set_shuffle(false);
        assert_eq!(tracks(&q), (1..=30).collect::<Vec<_>>());
        assert_eq!(cur(&q), playing, "current track survives un-shuffling");
    }

    #[test]
    fn previous_in_shuffle_returns_to_the_track_just_played() {
        let mut q = queue(20, 0);
        q.set_shuffle(true);
        let first = cur(&q);
        let second = q.next_manual().expect("next").track;
        let third = q.next_manual().expect("next").track;
        assert!(first != second && second != third);
        assert!(matches!(q.previous_manual(0), Some(Previous::Item(i)) if i.track == second));
        assert!(matches!(q.previous_manual(0), Some(Previous::Item(i)) if i.track == first));
        // Going forward again replays the same order: history is the order itself.
        assert_eq!(q.next_manual().map(|i| i.track), Some(second));
    }

    #[test]
    fn play_next_while_shuffled_follows_current_in_both_orders() {
        let mut q = queue(6, 2);
        q.set_shuffle(true);
        q.play_next(&[77]);
        assert_eq!(q.items()[1].track, 77);
        q.set_shuffle(false);
        assert_eq!(tracks(&q), vec![1, 2, 3, 77, 4, 5, 6]);
    }

    #[test]
    fn shuffle_with_repeat_all_reshuffles_without_immediate_repeat() {
        let mut q = queue(8, 0);
        q.set_shuffle(true);
        q.set_repeat(RepeatMode::All);
        for _ in 0..7 {
            q.advance_auto().expect("more items");
        }
        // At the last item the next order is not decided yet…
        assert_eq!(q.peek_next_auto(), None);
        let last = cur(&q);
        // …and wrapping starts a new pass that does not begin with the track just played.
        let first_of_new_pass = q.advance_auto().expect("wraps").track;
        assert_ne!(first_of_new_pass, last);
        assert_eq!(q.current_index(), Some(0));
        let mut all = tracks(&q);
        all.sort_unstable();
        assert_eq!(all, (1..=8).collect::<Vec<_>>());
    }

    /// Eight songs on "repeat all", shuffled, for many passes: every pass has every
    /// song once, none starts with the song that ended the pass before, and the song
    /// that ends a pass is not always the same one.
    #[test]
    fn the_last_song_of_a_shuffled_pass_is_not_always_the_same() {
        for seed in 0..20 {
            let mut q = queue(8, 0);
            q.rng = fastrand::Rng::with_seed(seed);
            q.set_shuffle(true);
            q.set_repeat(RepeatMode::All);
            let mut last_songs = HashSet::new();
            for pass in 0..30 {
                while q.current_index() != Some(q.len() - 1) {
                    q.advance_auto().expect("more items");
                }
                let last = cur(&q);
                last_songs.insert(last);
                let first = q.advance_auto().expect("wraps").track;
                assert_ne!(first, last, "seed {seed} pass {pass}: twice in a row");
                let mut all = tracks(&q);
                all.sort_unstable();
                assert_eq!(all, (1..=8).collect::<Vec<_>>());
            }
            assert!(last_songs.len() >= 4, "seed {seed}: {last_songs:?}");
        }
    }

    /// From the start of a pass that a wrap mixed, "previous" walks back through the
    /// pass that was heard before it, in its order; "next" then comes forward through
    /// the same songs and on into the new pass.
    #[test]
    fn previous_walks_back_through_the_pass_heard_before_the_wrap() {
        for seed in 0..30 {
            for steps in [1, 2, 6] {
                let mut q = Queue::with_seed(seed);
                q.set_repeat(RepeatMode::All);
                q.set_shuffle(true);
                q.replace(&[1, 2, 3, 4, 5, 6], 0);
                let heard: Vec<ItemId> = q.items().iter().map(|i| i.id).collect();
                while q.current_index() != Some(q.len() - 1) {
                    q.advance_auto().expect("not at the end yet");
                }
                q.advance_auto().expect("wraps");
                let new_pass: Vec<ItemId> = q.items().iter().map(|i| i.id).collect();
                let mut back = Vec::new();
                for _ in 0..steps {
                    match q.previous_manual(0) {
                        Some(Previous::Item(item)) => back.push(item.id),
                        other => panic!("seed {seed}: {other:?}"),
                    }
                    assert_eq!(q.current_index(), Some(0));
                }
                back.reverse();
                assert_eq!(back, heard[6 - steps..], "seed {seed}: the pass before");
                // Forward again through the same songs…
                let mut forward = vec![cur_item(&q).id];
                for _ in 1..steps {
                    forward.push(q.next_manual().expect("next").id);
                }
                assert_eq!(forward, back, "seed {seed}");
                // …and on into the new pass, at its first song that was not gone back to.
                if steps < 6 {
                    let next = new_pass.iter().find(|id| !back.contains(id)).copied();
                    assert_eq!(q.next_manual().map(|i| i.id), next, "seed {seed}");
                }
                let mut all = tracks(&q);
                all.sort_unstable();
                assert_eq!(
                    all,
                    [1, 2, 3, 4, 5, 6],
                    "seed {seed}: nothing lost or doubled"
                );
            }
        }
    }

    #[test]
    fn failure_does_not_loop_on_repeat_one() {
        let mut q = queue(3, 0);
        q.set_repeat(RepeatMode::One);
        assert_eq!(q.advance_after_failure().map(|i| i.track), Some(2));
        q.set_repeat(RepeatMode::Off);
        q.jump_to(q.items()[2].id);
        assert_eq!(q.advance_after_failure(), None);
    }

    #[test]
    fn state_round_trip_preserves_everything() {
        let mut q = queue(10, 4);
        q.set_shuffle(true);
        q.set_repeat(RepeatMode::All);
        q.next_manual();
        let state = q.to_state();
        let json = serde_json::to_string(&state).expect("serializes");
        let mut restored = Queue::from_state(serde_json::from_str(&json).expect("parses"));
        assert_eq!(restored.to_state(), state);
        // New items never collide with restored ids.
        let new = restored.append(&[500]);
        assert!(state.items.iter().all(|i| i.id != new[0].id));
        restored.set_shuffle(false);
        assert_eq!(tracks(&restored), vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 500]);
    }

    #[test]
    fn restoring_a_bad_state_is_safe() {
        let state = QueueState {
            items: vec![],
            current: Some(3),
            shuffle: false,
            repeat: RepeatMode::Off,
            unshuffled: vec![9],
        };
        let q = Queue::from_state(state);
        assert_eq!(q.current(), None);
    }

    #[test]
    fn retain_tracks_drops_removed_library_tracks() {
        let mut q = queue(5, 2);
        assert!(q.retain_tracks(|t| t != 3));
        assert_eq!(tracks(&q), vec![1, 2, 4, 5]);
        assert_eq!(cur(&q), 4);
        assert!(!q.retain_tracks(|_| true));
    }

    #[test]
    fn previous_after_a_shuffled_wrap_returns_to_the_track_just_played() {
        for seed in 0..50 {
            let mut q = Queue::with_seed(seed);
            q.set_repeat(RepeatMode::All);
            q.set_shuffle(true);
            q.replace(&[1, 2, 3, 4, 5, 6], 0);
            while q.current_index() != Some(q.len() - 1) {
                q.advance_auto().expect("not at the end yet");
            }
            let last = q.current().expect("current");
            let first_of_new_pass = q.advance_auto().expect("wraps");
            assert_ne!(first_of_new_pass.id, last.id, "seed {seed}");
            assert_eq!(q.current_index(), Some(0));
            // Going back from the start of the new pass leads to what was heard before.
            assert_eq!(
                q.previous_manual(0),
                Some(Previous::Item(last)),
                "seed {seed}"
            );
        }
    }

    #[test]
    fn a_single_item_survives_a_shuffled_wrap() {
        let mut q = Queue::with_seed(3);
        q.set_repeat(RepeatMode::All);
        q.set_shuffle(true);
        q.replace(&[9], 0);
        assert_eq!(q.advance_auto().map(|i| i.track), Some(9));
        assert_eq!(q.next_manual().map(|i| i.track), Some(9));
    }

    #[test]
    fn large_queue_edits_stay_fast() {
        const N: i64 = 50_000;
        let tracks: Vec<TrackId> = (0..N).collect();
        let mut q = Queue::with_seed(11);
        q.replace(&tracks, 20_000);
        let original: Vec<ItemId> = q.items().iter().map(|i| i.id).collect();
        let started = std::time::Instant::now();

        q.set_shuffle(true);
        assert_eq!(q.current().map(|c| c.track), Some(20_000));
        q.set_shuffle(false);
        assert_eq!(
            q.items().iter().map(|i| i.id).collect::<Vec<_>>(),
            original,
            "turning shuffle off restores the order"
        );
        assert_eq!(q.current_index(), Some(20_000));

        // Move every fourth item to the front, then remove every second one.
        let moved: Vec<ItemId> = original.iter().copied().step_by(4).collect();
        q.move_items(&moved, 0);
        assert_eq!(
            q.items()[..moved.len()]
                .iter()
                .map(|i| i.id)
                .collect::<Vec<_>>(),
            moved
        );
        assert_eq!(q.current().map(|c| c.track), Some(20_000));
        let removed: Vec<ItemId> = original.iter().copied().step_by(2).collect();
        assert!(q.remove(&removed), "the current item is among them");
        assert_eq!(q.len(), N as usize / 2);
        // The current item sat among the moved ones (all removed); the first survivor
        // after it is the first item that was not moved.
        assert_eq!(q.current().map(|c| c.track), Some(1));
        let gone = q.retain_tracks(|t| t % 3 != 0);
        assert!(!gone || q.current().is_some());

        // Quadratic versions of these took minutes at this size.
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn revision_follows_every_change_of_the_items() {
        let mut q = Queue::with_seed(5);
        let mut last = q.revision();
        let mut changed = |q: &Queue, what: &str| {
            assert_ne!(q.revision(), last, "{what}");
            last = q.revision();
        };
        q.replace(&[1, 2, 3], 0);
        changed(&q, "replace");
        q.append(&[4]);
        changed(&q, "append");
        q.play_next(&[5]);
        changed(&q, "play next");
        let first = q.items()[0].id;
        q.move_items(&[first], 2);
        changed(&q, "move");
        q.remove(&[first]);
        changed(&q, "remove");
        q.set_shuffle(true);
        changed(&q, "shuffle on");
        q.set_repeat(RepeatMode::All);
        while q.current_index() != Some(q.len() - 1) {
            q.advance_auto();
        }
        let before = q.revision();
        q.advance_auto();
        assert_ne!(q.revision(), before, "the wrap reshuffles the items");
        // Moving within the order changes nothing about the items.
        let before = q.revision();
        q.advance_auto();
        q.previous_manual(0);
        q.jump_to(q.items()[0].id);
        assert_eq!(q.revision(), before);
    }
}
