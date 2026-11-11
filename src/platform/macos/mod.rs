//! macOS integration through objc2 bindings. All `unsafe` in the app lives in this module.
pub mod app_hooks;
pub mod finder;
pub mod locale;
pub mod menu;
pub mod nowplaying;
pub mod power;
pub mod responder;
pub mod testing;
pub mod window;

use block2::RcBlock;
use objc2_foundation::NSOperationQueue;
use std::cell::Cell;

/// Runs `f` on the main thread soon (from any thread).
pub fn run_on_main(f: impl FnOnce() + Send + 'static) {
    let cell = Cell::new(Some(f));
    let block = RcBlock::new(move || {
        if let Some(f) = cell.take() {
            f();
        }
    });
    // SAFETY: the block is copied by the queue and run once on the main thread; the
    // closure it wraps is `Send`.
    unsafe { NSOperationQueue::mainQueue().addOperationWithBlock(&block) };
}
