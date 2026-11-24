//! macOS integration through objc2 bindings. What iOS has as well lives in `apple`.
pub mod app_hooks;
pub mod coreaudio;
pub mod files;
pub mod finder;
pub mod menu;
pub mod power;
pub mod responder;
pub mod testing;
pub mod window;

pub use super::apple::{decode_cp949, locale, nowplaying, pump_main_loop, run_on_main, system};

/// Hands a web address (already checked by the caller) to the default browser. Asked of
/// the workspace, not of `/usr/bin/open`: a process the app starts is held to the app's
/// sandbox where it has one.
pub(super) fn open_in_browser(address: &str) -> bool {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSString, NSURL};
    let Some(url) = NSURL::URLWithString(&NSString::from_str(address)) else {
        return false;
    };
    NSWorkspace::sharedWorkspace().openURL(&url)
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
