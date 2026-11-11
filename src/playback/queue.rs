//! The play queue. Pure data and rules, no audio or I/O.
//!
//! `items` is always the actual play order: with shuffle on it is the shuffled order, and
//! the pre-shuffle order is remembered so that turning shuffle off restores it. Queue items
//! have their own ids, so the same track can be queued several times.
use crate::model::TrackId;
use serde::{Deserialize, Serialize};

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
}

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

    /// Shuffles `items[from..]` in place.
    fn shuffle_tail(&mut self, from: usize) {
        if from < self.items.len() {
            self.rng.shuffle(&mut self.items[from..]);
        }
    }

    /// Replaces the queue with `tracks` and makes `start_index` current. With shuffle on,
    /// the chosen track is played first and the rest follow in random order.
    pub fn replace(&mut self, tracks: &[TrackId], start_index: usize) -> Option<QueueItem> {
        self.items = self.make_items(tracks);
        self.unshuffled.clear();
        if self.items.is_empty() {
            self.current = None;
            return None;
        }
        let start = start_index.min(self.items.len() - 1);
        if self.shuffle {
            self.unshuffled = self.items.iter().map(|i| i.id).collect();
            self.items.swap(0, start);
            self.shuffle_tail(1);
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
        self.items.clear();
        self.unshuffled.clear();
        self.current = None;
    }

    /// Moves the given items (keeping their relative order) so that they start at
    /// `to_index` of the list with those items taken out. The current item stays current.
    pub fn move_items(&mut self, ids: &[ItemId], to_index: usize) {
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
        let current_id = self.current().map(|c| c.id);
        if on {
            self.unshuffled = self.items.iter().map(|i| i.id).collect();
            match self.current {
                Some(c) => {
                    let item = self.items.remove(c);
                    self.items.insert(0, item);
                    self.shuffle_tail(1);
                }
                None => self.shuffle_tail(0),
            }
        } else {
            let order = std::mem::take(&mut self.unshuffled);
            // Items missing from the remembered order keep their relative order at the end.
            self.items.sort_by_key(|i| {
                order
                    .iter()
                    .position(|id| *id == i.id)
                    .unwrap_or(usize::MAX)
            });
        }
        self.current = current_id.and_then(|id| self.index_of(id));
    }

    /// Starts another pass over a shuffled queue in a fresh order, avoiding an immediate
    /// repeat of the track that just played.
    fn reshuffle_for_wrap(&mut self) {
        let last = self.current().map(|c| c.id);
        self.shuffle_tail(0);
        if self.items.len() > 1 && self.items.first().map(|i| i.id) == last {
            let other = self.rng.usize(1..self.items.len());
            self.items.swap(0, other);
        }
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
    fn queue(n: i64, start: usize) -> Queue {
        let mut q = Queue::with_seed(42);
        q.replace(&(1..=n).collect::<Vec<_>>(), start);
        q
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
}
