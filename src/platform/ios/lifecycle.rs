//! Leaving the foreground and coming back. An iPhone app is suspended soon after it
//! leaves the foreground (unless it is playing) and may be ended without a word, so
//! what must be kept is saved then.
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// The app is on screen (or on its way there).
static FOREGROUND: AtomicBool = AtomicBool::new(true);

/// False while the app is in the background. It must not draw then: the system does
/// not let a background app use the graphics processor, and nobody would see it.
pub fn in_foreground() -> bool {
    FOREGROUND.load(Ordering::Acquire)
}

type Hook = Box<dyn Fn() + Send + Sync>;
static HOOKS: OnceLock<(Hook, Hook)> = OnceLock::new();

/// `on_background` is called (on the main thread) when the app has left the foreground,
/// `on_foreground` when it is about to come back.
pub fn observe(
    on_background: impl Fn() + Send + Sync + 'static,
    on_foreground: impl Fn() + Send + Sync + 'static,
) {
    let _ = HOOKS.set((Box::new(on_background), Box::new(on_foreground)));
}

pub(super) fn entered_background() {
    FOREGROUND.store(false, Ordering::Release);
    if let Some((on_background, _)) = HOOKS.get() {
        on_background();
    }
}

pub(super) fn entering_foreground() {
    FOREGROUND.store(true, Ordering::Release);
    if let Some((_, on_foreground)) = HOOKS.get() {
        on_foreground();
    }
    // What changed while nothing was drawn is shown now.
    super::app_hooks::wake();
}
