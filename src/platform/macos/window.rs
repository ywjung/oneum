//! Hiding and re-showing the main window (the app keeps running and playing while hidden).
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSWindow};

fn main_window(mtm: MainThreadMarker) -> Option<Retained<NSWindow>> {
    let app = NSApplication::sharedApplication(mtm);
    // winit's window class (possibly a KVO-generated subclass of it, e.g.
    // `NSKVONotifying_WinitWindow`); panels and dialogs are other classes.
    app.windows()
        .iter()
        .find(|w| w.class().name().to_string_lossy().ends_with("WinitWindow"))
}

/// Hides the main window without closing it. Main thread only (no-op elsewhere).
pub fn hide_main_window() {
    if let Some(mtm) = MainThreadMarker::new()
        && let Some(window) = main_window(mtm)
    {
        window.orderOut(None);
    }
}

/// Shows the main window and activates the app. Main thread only (no-op elsewhere).
pub fn show_main_window() {
    if let Some(mtm) = MainThreadMarker::new() {
        if let Some(window) = main_window(mtm) {
            if window.isMiniaturized() {
                window.deminiaturize(None);
            }
            window.makeKeyAndOrderFront(None);
        }
        NSApplication::sharedApplication(mtm).activate();
    }
}

pub fn is_main_window_visible() -> bool {
    MainThreadMarker::new()
        .and_then(main_window)
        .is_some_and(|w| w.isVisible())
}
