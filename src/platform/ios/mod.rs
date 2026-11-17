//! iPhone integration through objc2 bindings. Mirrors the names of the `macos` module so
//! the rest of the app is the same on every system.
//!
//! What an iPhone does differently, and what is not there:
//! - There is no menu bar and no window to hide, move or resize: one view fills the
//!   screen (`menu`, `window`).
//! - The app only reads its own container. Music is put into its Documents folder (the
//!   Files app and Finder show it), and files from elsewhere are copied in through the
//!   document picker (`files`).
//! - The system decides where sound goes and at which rate: there is no exclusive
//!   access and no per-device sample rate (`DEVICE_CONTROL`). The audio session is told
//!   that this is a music player, so that it plays on with the screen off (`session`).
//! - The app is suspended and ended without being asked: what must be kept is saved
//!   when it leaves the foreground (`lifecycle`).
pub mod app_hooks;
pub mod files;
pub mod lifecycle;
mod scene;
pub mod sound;
pub mod widget;

pub use super::apple::{decode_cp949, locale, nowplaying, pump_main_loop, run_on_main, system};
pub mod device;

use objc2::MainThreadMarker;
use objc2_foundation::{NSDictionary, NSString, NSURL};
use objc2_ui_kit::UIApplication;

/// Hands a web address (already checked by the caller) to Safari.
pub(super) fn open_in_browser(address: &str) -> bool {
    let address = address.to_string();
    let open = move || {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let Some(url) = NSURL::URLWithString(&NSString::from_str(&address)) else {
            return;
        };
        // SAFETY: a valid URL, no options and no completion handler.
        unsafe {
            UIApplication::sharedApplication(mtm).openURL_options_completionHandler(
                &url,
                &NSDictionary::new(),
                None,
            );
        }
    };
    if MainThreadMarker::new().is_some() {
        open();
    } else {
        run_on_main(open);
    }
    true
}

/// Puts `text` on the general pasteboard. Main thread (the UI's).
pub fn copy_text(text: &str) -> bool {
    use objc2_foundation::NSString;
    use objc2_ui_kit::UIPasteboard;
    let pasteboard = UIPasteboard::generalPasteboard();
    // SAFETY: called on the main thread, with a string that lives for the call.
    unsafe { pasteboard.setString(Some(&NSString::from_str(text))) };
    true
}

/// A start-up failure. Before the app's view exists nothing can be shown on an iPhone,
/// so it goes to the log (Console.app shows it).
pub fn fatal_alert(title: &str, body: &str) {
    eprintln!("OnEum: {title}: {body}");
}

pub mod finder {
    pub use crate::platform::apple::is_on_local_volume;
    use std::path::Path;

    /// There is no Finder on an iPhone to show a file in.
    pub fn reveal(_path: &Path) -> bool {
        false
    }
}

pub mod menu {
    use crate::i18n::Strings;
    use crate::platform::MenuAction;

    /// An iPhone has no menu bar. This type only keeps the same shape as the macOS one.
    pub struct AppMenu;

    impl AppMenu {
        pub fn install(
            _s: &Strings,
            _version: &str,
            _on_action: impl Fn(MenuAction) + Send + Sync + 'static,
        ) -> Option<Self> {
            Some(Self)
        }
    }
}

pub mod power {
    /// An iPhone does not announce sleep to an app. What matters there (a phone call,
    /// headphones pulled out) arrives through the audio session and reaches the engine
    /// as a change of the output stream.
    pub fn observe_sleep(_on_sleep: impl Fn() + Send + Sync + 'static) {}
}

pub mod responder {
    /// Edit commands that a system menu would send. There is no such menu on an iPhone.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum EditAction {
        Cut,
        Copy,
        Paste,
        SelectAll,
        Undo,
        Redo,
    }

    pub fn install(_waker: impl Fn() + Send + Sync + 'static) -> bool {
        true
    }

    pub fn drain() -> Vec<EditAction> {
        Vec::new()
    }

    pub fn set_text_input_active(_active: bool) {}

    pub fn pasteboard_text() -> Option<String> {
        None
    }
}

pub mod testing {
    //! The scripted-verification helpers that drive macOS itself have no counterpart.

    pub fn post_key(_key_code: u16, _command: bool, _shift: bool, _option: bool) -> bool {
        false
    }

    pub fn app_is_active() -> bool {
        false
    }

    pub fn menu_key(_key_code: u16, _command: bool, _shift: bool, _option: bool) -> bool {
        false
    }

    pub fn select_input_source(_id: &str) -> bool {
        false
    }

    pub fn current_input_source() -> Option<String> {
        None
    }
}

pub mod window {
    //! The one view of the app fills the screen and is never hidden by the app itself.
    use objc2::rc::Retained;
    use objc2_ui_kit::UIView;

    /// Remembers the view winit made, so that its window can be put into the app's scene
    /// (see `scene`). Main thread only.
    pub fn remember_main_window(handle: winit::raw_window_handle::RawWindowHandle) {
        let winit::raw_window_handle::RawWindowHandle::UiKit(handle) = handle else {
            return;
        };
        // SAFETY: winit hands out a pointer to its `UIView`, which is alive (the window
        // that owns it exists); retaining it keeps it so.
        let view = unsafe { Retained::retain(handle.ui_view.as_ptr().cast::<UIView>()) };
        if let Some(view) = view {
            super::scene::remember_view(view);
        }
    }

    /// The margins the system asks to keep free (status bar, Dynamic Island, home
    /// indicator): top, left, bottom, right in points.
    pub fn safe_area_insets() -> Option<[f32; 4]> {
        super::scene::safe_area_insets()
    }

    /// Whether the phone is set to its dark appearance (winit does not tell on iOS).
    pub fn system_is_dark() -> Option<bool> {
        super::scene::system_is_dark()
    }

    /// Has what the system draws for the app (the status bar's text, the file picker)
    /// be dark, light, or as the phone is set (`None`).
    pub fn set_appearance(dark: Option<bool>) {
        super::scene::set_appearance(dark);
    }

    /// How much of the screen's lower end the on-screen keyboard covers, in points.
    pub fn keyboard_height() -> f32 {
        super::scene::keyboard_height()
    }

    pub fn hide_main_window() {}

    pub fn show_main_window() {}

    pub fn order_front_without_activating() {}

    pub fn send_back() {}

    pub fn is_main_window_visible() -> bool {
        true
    }
}
