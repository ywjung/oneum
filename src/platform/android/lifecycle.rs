//! Leaving the screen and coming back. An Android app that is not on screen may be
//! ended without a word (unless it is playing, see `nowplaying`), so what must be kept
//! is saved when the activity is left.
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// The app is on screen (or on its way there).
static FOREGROUND: AtomicBool = AtomicBool::new(true);

/// False while the activity is not on screen: nothing is drawn then.
pub fn in_foreground() -> bool {
    FOREGROUND.load(Ordering::Acquire)
}

type Hook = Box<dyn Fn() + Send + Sync>;
static HOOKS: OnceLock<(Hook, Hook)> = OnceLock::new();

/// `on_background` is called when the app has left the screen, `on_foreground` when it
/// is about to come back (both on Java's main thread).
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
