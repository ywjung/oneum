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

/// Shows a blocking error alert. Works before the event loop exists (start-up failures):
/// the process is made a regular, active app first, otherwise the alert would open
/// behind everything or not at all. Main thread only (no-op elsewhere).
pub fn fatal_alert(title: &str, body: &str) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSAlert, NSAlertStyle, NSApplication, NSApplicationActivationPolicy};
    use objc2_foundation::NSString;
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    app.activate();
    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(NSAlertStyle::Critical);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(body));
    alert.runModal();
}
