//! Playback control: the queue model (pure logic) and the player thread that drives the
//! audio engine. Behaviour is specified in `docs/UX.md` ("재생 동작 정의").
pub mod player;
pub mod queue;

pub use player::{
    Command, Notice, NowPlaying, PlayState, PlayableTrack, PlayerConfig, PlayerHandle, PlayerHost,
    PlayerSnapshot, SectionRepeat, Session,
};
pub use queue::{ItemId, Queue, QueueItem, QueueState, RepeatMode};
