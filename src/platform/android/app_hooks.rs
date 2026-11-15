//! Files handed to the app by the system ("Open with" in a file manager). They arrive
//! as an intent of the activity, which copies them where the app can read them.
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

static OPENED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static WAKER: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

/// Called whenever new paths arrive, or when something the app shows has changed
/// outside it.
pub fn set_waker(waker: impl Fn() + Send + Sync + 'static) {
    let _ = WAKER.set(Box::new(waker));
}

/// Paths the system asked us to open since the last call.
pub fn take_opened_paths() -> Vec<PathBuf> {
    OPENED
        .lock()
        .map(|mut queue| std::mem::take(&mut *queue))
        .unwrap_or_default()
}

pub(super) fn receive(paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    if let Ok(mut queue) = OPENED.lock() {
        queue.extend(paths);
    }
    wake();
}

/// Has the app look again.
pub(super) fn wake() {
    if let Some(wake) = WAKER.get() {
        wake();
    }
}

pub fn install_now() -> bool {
    true
}

pub fn install_before_launch() {}
