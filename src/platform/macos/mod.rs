//! macOS integration through objc2 bindings. All `unsafe` in the app lives in this module.
pub mod app_hooks;
pub mod coreaudio;
pub mod finder;
pub mod locale;
pub mod menu;
pub mod nowplaying;
pub mod power;
pub mod responder;
pub mod system;
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

/// Text in the Korean code page of older Windows (CP949, a superset of EUC-KR) as
/// Unicode. `None` if the bytes are not that.
pub fn decode_cp949(bytes: &[u8]) -> Option<String> {
    use objc2_core_foundation::CFString;
    /// `kCFStringEncodingDOSKorean`.
    const CP949: u32 = 0x0422;
    let length = isize::try_from(bytes.len()).ok()?;
    // SAFETY: pointer and length describe `bytes`, which is alive for the call;
    // CoreFoundation copies what it converts and keeps nothing of ours.
    let string = unsafe { CFString::with_bytes(None, bytes.as_ptr(), length, CP949, false) }?;
    Some(string.to_string())
}

/// Hands a web address (already checked by the caller) to the default browser.
pub(super) fn open_in_browser(address: &str) -> bool {
    std::process::Command::new("/usr/bin/open")
        .arg(address)
        .spawn()
        .is_ok()
}

/// Runs the main thread's run loop for `duration`. The app has a run loop of its own;
/// command-line tools that open audio streams need this, because CoreAudio serves some
/// requests and all its notifications through the main run loop and calls made while
/// that loop is not running can wait for ever. Main thread only.
pub fn pump_main_loop(duration: std::time::Duration) {
    use objc2_core_foundation::{CFRunLoop, kCFRunLoopDefaultMode};
    // SAFETY: the constant is initialised by CoreFoundation before any code of ours runs.
    let mode = unsafe { kCFRunLoopDefaultMode };
    CFRunLoop::run_in_mode(mode, duration.as_secs_f64(), false);
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
