//! Who has the sound. Android shares it through "audio focus": the app asks for it
//! right before it makes sound ([`claim`]), and is told when a call or another app
//! takes it and when it comes back, and when the output it played to is gone
//! (headphones pulled out).
use crate::platform::SoundEvent;
use std::sync::OnceLock;

type Listener = Box<dyn Fn(SoundEvent) + Send + Sync>;
static LISTENER: OnceLock<Listener> = OnceLock::new();

/// Nothing to prepare before a stream is opened: focus is asked for when playing
/// starts ([`claim`]), not when the app does or when a stream is opened to stand ready
/// (opening the app must not silence another one).
pub fn activate() -> bool {
    true
}

/// Asked right before sound is made: whether the sound is this app's, or is now. The
/// Java side sends the playback service to the foreground first where it runs (an app
/// behind the screen may only ask from there) and then asks the system. False while
/// something else keeps the sound (a call): no sound must be made then; if the sound
/// comes by itself afterwards, the app is told as at the end of an interruption.
pub fn claim() -> bool {
    super::java::claim_sound()
}

/// The player has answered the end of an interruption: it plays on (`true`) or stays
/// silent. The playback service, which kept the foreground through the interruption,
/// leaves it in the second case.
pub fn interruption_settled(resumed: bool) {
    super::java::sound_settled(resumed);
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
