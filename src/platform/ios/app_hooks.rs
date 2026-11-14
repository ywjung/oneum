//! Files handed to the app by the system ("Open in OnEum" in the Files app, the share
//! sheet). They arrive through the scene delegate (see `scene`).
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

static OPENED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static WAKER: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

/// Called (on the main thread) whenever new paths arrive.
pub fn set_waker(waker: impl Fn() + Send + Sync + 'static) {
    let _ = WAKER.set(Box::new(waker));
}

/// Paths the system asked us to open since the last call.
pub fn take_opened_paths() -> Vec<PathBuf> {
    OPENED
        .lock()
        .map(|mut q| std::mem::take(&mut *q))
        .unwrap_or_default()
}

pub(super) fn receive(paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    if let Ok(mut queue) = OPENED.lock() {
        queue.extend(paths);
    }
    if let Some(wake) = WAKER.get() {
        wake();
    }
}

/// Has the app look again (something it shows has changed outside it).
pub(super) fn wake() {
    if let Some(wake) = WAKER.get() {
        wake();
    }
}

pub fn install_now() -> bool {
    true
}

/// Registers the scene delegate. Call once from `main` before the event loop is created.
pub fn install_before_launch() {
    super::scene::register();
}
