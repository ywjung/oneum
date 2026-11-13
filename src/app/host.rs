//! Connects the player to the library: track lookup, play counts, session persistence.
use crate::library::Library;
use crate::model::TrackId;
use crate::playback::{PlayableTrack, PlayerHost, QueueItem, QueueState, RepeatMode, Session};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

const SESSION_KEY: &str = "playback_session";

/// Everything of the session except the queue items, which live in their own table.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct SessionMeta {
    current: Option<usize>,
    shuffle: bool,
    repeat: RepeatMode,
    unshuffled: Vec<u64>,
    position_ms: u64,
    volume: Option<f32>,
}

pub struct LibraryHost {
    library: Arc<Library>,
    on_change: Box<dyn Fn() + Send + Sync>,
}

impl LibraryHost {
    pub fn new(library: Arc<Library>, on_change: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            library,
            on_change: Box::new(on_change),
        }
    }
}

impl PlayerHost for LibraryHost {
    fn track(&self, id: TrackId) -> Option<PlayableTrack> {
        self.library.track(id).map(|t| PlayableTrack {
            path: t.path.clone(),
            replaygain: t.leveling(),
            duration_ms: t.duration_ms,
            available: t.available,
            sample_rate: t.playback_rate(),
            bit_depth: t.bit_depth,
        })
    }

    fn record_play(&self, id: TrackId) {
        self.library.record_play(id);
    }

    fn radio_picks(&self, seed: TrackId, exclude: &[TrackId], count: usize) -> Vec<TrackId> {
        self.library.radio_picks(seed, exclude, count)
    }

    fn load_session(&self) -> Option<Session> {
        let meta: SessionMeta = serde_json::from_str(&self.library.kv_get(SESSION_KEY)?).ok()?;
        let items = self
            .library
            .load_queue()
            .into_iter()
            .map(|(id, track)| QueueItem { id, track })
            .collect();
        Some(Session {
            queue: QueueState {
                items,
                current: meta.current,
                shuffle: meta.shuffle,
                repeat: meta.repeat,
                unshuffled: meta.unshuffled,
            },
            position_ms: meta.position_ms,
            volume: meta.volume.unwrap_or(0.8),
        })
    }

    fn save_session(&self, session: &Session, queue_changed: bool) {
        if queue_changed {
            let items: Vec<(u64, TrackId)> = session
                .queue
                .items
                .iter()
                .map(|i| (i.id, i.track))
                .collect();
            self.library.save_queue(&items);
        }
        let meta = SessionMeta {
            current: session.queue.current,
            shuffle: session.queue.shuffle,
            repeat: session.queue.repeat,
            unshuffled: session.queue.unshuffled.clone(),
            position_ms: session.position_ms,
            volume: Some(session.volume),
        };
        if let Ok(json) = serde_json::to_string(&meta) {
            self.library.kv_set(SESSION_KEY, &json);
        }
    }

    fn changed(&self) {
        (self.on_change)();
    }
}
