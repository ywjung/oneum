//! Hiding and re-showing the main window (the app keeps running and playing while hidden).
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSApplication, NSFloatingWindowLevel, NSNormalWindowLevel, NSWindow, NSWindowStyleMask,
};

fn main_window(mtm: MainThreadMarker) -> Option<Retained<NSWindow>> {
    let app = NSApplication::sharedApplication(mtm);
    // winit's window class (possibly a KVO-generated subclass of it, e.g.
    // `NSKVONotifying_WinitWindow`); panels and dialogs are other classes.
    app.windows()
        .iter()
        .find(|w| w.class().name().to_string_lossy().ends_with("WinitWindow"))
}

/// Hides the main window without closing it. Main thread only (no-op elsewhere).
/// The main window as the windowing library knows it. AppKit finds it on its own, so
/// nothing needs to be kept here.
pub fn remember_main_window(_handle: winit::raw_window_handle::RawWindowHandle) {}

/// A window on a desk has no margins the system asks to keep free.
pub fn safe_area_insets() -> Option<[f32; 4]> {
    None
}

/// There is no on-screen keyboard over a window on a desk.
pub fn keyboard_height() -> f32 {
    0.0
}

/// The window system tells egui whether the system is dark; nothing to add.
pub fn system_is_dark() -> Option<bool> {
    None
}

/// The window's title bar follows the system by itself.
pub fn set_appearance(_dark: Option<bool>) {}

pub fn hide_main_window() {
    if let Some(mtm) = MainThreadMarker::new()
        && let Some(window) = main_window(mtm)
    {
        if window.styleMask().contains(NSWindowStyleMask::FullScreen) {
            // Ordering a full-screen window out leaves the user on an empty black Space.
            // Hiding the app takes them back to where they were; the Dock icon and
            // "Show Main Window" bring the full-screen window back.
            NSApplication::sharedApplication(mtm).hide(None);
        } else {
            window.orderOut(None);
        }
        tracing::info!("main window hidden");
    }
}

/// Shows the main window and activates the app. Main thread only (no-op elsewhere).
pub fn show_main_window() {
    if let Some(mtm) = MainThreadMarker::new() {
        let app = NSApplication::sharedApplication(mtm);
        if app.isHidden() {
            app.unhide(None);
        }
        if let Some(window) = main_window(mtm) {
            if window.isMiniaturized() {
                window.deminiaturize(None);
            }
            if !window.isVisible() {
                tracing::info!("main window shown");
            }
            window.makeKeyAndOrderFront(None);
        }
        app.activate();
    }
}

/// Keeps the main window above other apps' windows without making this app active: the
/// user keeps typing where they were. Used by verification runs, which need the window
/// on screen (a fully covered window is not painted) but must not take focus. Undo with
/// [`send_back`].
pub fn order_front_without_activating() {
    if let Some(mtm) = MainThreadMarker::new()
        && let Some(window) = main_window(mtm)
    {
        window.setLevel(NSFloatingWindowLevel);
        window.orderFrontRegardless();
    }
}

/// Ends [`order_front_without_activating`]: normal level again, behind other windows.
pub fn send_back() {
    if let Some(mtm) = MainThreadMarker::new()
        && let Some(window) = main_window(mtm)
    {
        window.setLevel(NSNormalWindowLevel);
        window.orderBack(None);
    }
}

pub fn is_main_window_visible() -> bool {
    MainThreadMarker::new()
        .and_then(main_window)
        .is_some_and(|w| w.isVisible())
}
