//! Who has the sound. Android shares it through "audio focus": the app asks for it when
//! it starts to play (the Java side does, see `nowplaying`), and is told when a call or
//! another app takes it and when it comes back, and when the output it played to is
//! gone (headphones pulled out).
use crate::platform::SoundEvent;
use std::sync::OnceLock;

type Listener = Box<dyn Fn(SoundEvent) + Send + Sync>;
static LISTENER: OnceLock<Listener> = OnceLock::new();

/// Nothing to prepare before a stream is opened: focus is asked for when playing
/// starts, not when the app does (opening the app must not silence another one).
pub fn activate() -> bool {
    true
}

/// Calls `on_event` (on Java's main thread) when the sound is taken away, when it comes
/// back, and when the output is gone.
pub fn observe(on_event: impl Fn(SoundEvent) + Send + Sync + 'static) {
    let _ = LISTENER.set(Box::new(on_event));
}

pub(super) fn notify(event: SoundEvent) {
    tracing::info!(?event, "sound event");
    if let Some(listener) = LISTENER.get() {
        listener(event);
    }
}
