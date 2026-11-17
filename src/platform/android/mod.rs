//! Android integration. The app is a shared library that a native activity loads
//! (`entry`). What Android offers only in Java is reached through JNI: the activity of
//! the project in `android/` (`MainActivity`) has a method for each such thing (`java`)
//! and calls back into the functions of `natives`. The names mirror the other systems'
//! modules so that the rest of the app is the same everywhere.
//!
//! What Android does differently:
//! - The app is one activity that fills the screen; there is no menu bar and no window
//!   to move (`menu`, `window`). The system's Back gesture arrives as a key.
//! - Shared storage is closed to apps. With "all files access" the app reads folders by
//!   their paths as on a desk (`files`); without it only its own folder.
//! - A process that is not on screen is ended whenever the system likes. While music
//!   plays a foreground service with a notification keeps it (`nowplaying`), and what
//!   must be kept is saved when the activity is left (`lifecycle`).
//! - Sound is shared through "audio focus": another app or a call takes it, and the
//!   app is told (`sound`).
//! - Text comes from an input method that composes it (Korean): a view of the activity
//!   hands the composed text over (`text_input`).
pub mod app_hooks;
mod entry;
pub mod files;
mod java;
pub mod lifecycle;
mod natives;
pub mod nowplaying;
pub mod sound;
pub mod text_input;

pub use super::no_device as device;
use std::path::Path;

/// Runs `f` now. On Android nothing of the system is bound to one thread of the app's:
/// what has to happen on Java's main thread is sent there on the Java side.
pub fn run_on_main(f: impl FnOnce() + Send + 'static) {
    f();
}

/// There is no loop of the system's to turn while waiting.
pub fn pump_main_loop(duration: std::time::Duration) {
    std::thread::sleep(duration);
}

/// Text in the Korean code page 949 (old tags), as Unicode. Java knows the code page.
pub fn decode_cp949(bytes: &[u8]) -> Option<String> {
    java::decode_cp949(bytes)
}

/// Hands a web address (already checked by the caller) to the browser.
pub(super) fn open_in_browser(address: &str) -> bool {
    java::open_web_page(address)
}

/// A failure that ends the app, or keeps it from starting. It goes to the log (which
/// `adb logcat` shows) and, where the activity is up, into a dialog.
pub fn fatal_alert(title: &str, body: &str) {
    eprintln!("OnEum: {title}: {body}");
    java::alert(title, body);
}

/// Asks for the app to be opened again once this process is gone: for a process that
/// cannot go on (its graphics device is lost, and a process gets no second one). The
/// caller saves what it keeps and ends the process. False when it could not be asked
/// for.
pub fn open_again_after_exit() -> bool {
    java::relaunch()
}

/// Puts `text` on the system's clipboard. False when that could not be asked for.
pub fn copy_text(text: &str) -> bool {
    java::copy_text(text)
}

/// Keeps the password of a network folder in the phone's store (`Secrets.java`).
pub fn secret_put(id: &str, secret: &str) -> bool {
    java::secret_put(id, secret)
}

/// The password kept under `id`, if there is one.
pub fn secret_get(id: &str) -> Option<String> {
    java::secret_get(id)
}

pub fn secret_remove(id: &str) {
    java::secret_remove(id);
}

/// A line the system shows for a moment, also once the app is gone.
pub fn notice(text: &str) {
    java::notice(text);
}

pub mod finder {
    use super::Path;

    /// Shows the folder `path` (or the one a file is in) in the system's file
    /// manager. It shows folders of shared storage, not single files.
    pub fn reveal(path: &Path) -> bool {
        let folder = if path.is_dir() {
            path
        } else {
            match path.parent() {
                Some(parent) => parent,
                None => return false,
            }
        };
        super::java::show_folder(&folder.to_string_lossy())
    }

    /// Whether `path` lies on storage of the phone itself. Not told on Android: folders
    /// are looked into again at intervals whatever they lie on (`library::watch`).
    pub fn is_on_local_volume(_path: &Path) -> Option<bool> {
        None
    }
}

pub mod locale {
    /// Whether the phone is set to Korean.
    pub fn prefers_korean() -> bool {
        super::java::language().is_some_and(|language| language.starts_with("ko"))
    }
}

pub mod menu {
    use crate::i18n::Strings;
    use crate::platform::MenuAction;

    /// A phone has no menu bar. This type only keeps the same shape as the macOS one.
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
    /// Android does not announce sleep to an app. What matters there (a call, headphones
    /// pulled out) arrives as a change of audio focus (`sound`).
    pub fn observe_sleep(_on_sleep: impl Fn() + Send + Sync + 'static) {}
}

pub mod responder {
    /// Edit commands that a system menu would send. There is no such menu on a phone.
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

    /// A text field has the keyboard's attention, or none has: the on-screen keyboard
    /// comes up or goes away with it.
    pub fn set_text_input_active(active: bool) {
        super::text_input::set_active(active);
    }

    /// What is typed into the field that has the keyboard is a password.
    pub fn set_text_input_secret(secret: bool) {
        super::text_input::set_secret(secret);
    }

    pub fn pasteboard_text() -> Option<String> {
        None
    }
}

pub mod system {
    /// Installed memory in bytes.
    pub fn physical_memory_bytes() -> Option<u64> {
        // SAFETY: `sysconf` only reads two numbers of the system's.
        let (pages, size) = unsafe { (libc_sysconf(SC_PHYS_PAGES), libc_sysconf(SC_PAGESIZE)) };
        (pages > 0 && size > 0).then(|| pages as u64 * size as u64)
    }

    // Android's C library (bionic), as on Linux.
    const SC_PAGESIZE: i32 = 39;
    const SC_PHYS_PAGES: i32 = 98;

    unsafe extern "C" {
        #[link_name = "sysconf"]
        fn libc_sysconf(name: i32) -> i64;
    }
}

pub mod testing {
    //! The scripted-verification helpers that drive macOS itself have no counterpart.

    pub fn post_key(_key_code: u16, _command: bool, _shift: bool, _option: bool) -> bool {
        false
    }

    pub fn app_is_active() -> bool {
        super::lifecycle::in_foreground()
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
    //! The one activity of the app fills the screen, under the system's bars.
    use std::sync::atomic::{AtomicU32, Ordering};

    /// What the system's bars, the camera cut-out and the keyboard cover, in points
    /// (as `f32` bits): top, left, bottom, right, keyboard.
    static INSETS: [AtomicU32; 5] = [const { AtomicU32::new(0) }; 5];
    /// 0: not told yet, 1: light, 2: dark.
    static DARK: AtomicU32 = AtomicU32::new(0);

    pub(in crate::platform::android) fn set_insets(points: [f32; 5]) {
        // The keyboard went away without the app asking (the system's Back, its own
        // key for that): the field it typed into is told.
        if inset(4) > 0.0 && points[4] <= 0.0 {
            super::text_input::keyboard_went();
        }
        for (slot, value) in INSETS.iter().zip(points) {
            slot.store(value.max(0.0).to_bits(), Ordering::Relaxed);
        }
    }

    pub(in crate::platform::android) fn set_system_dark(dark: bool) {
        DARK.store(if dark { 2 } else { 1 }, Ordering::Relaxed);
    }

    fn inset(index: usize) -> f32 {
        f32::from_bits(INSETS[index].load(Ordering::Relaxed))
    }

    pub fn remember_main_window(_handle: winit::raw_window_handle::RawWindowHandle) {}

    /// The margins the system asks to keep free (status bar, camera, navigation bar):
    /// top, left, bottom, right in points.
    pub fn safe_area_insets() -> Option<[f32; 4]> {
        Some([inset(0), inset(1), inset(2), inset(3)])
    }

    /// Whether the phone is set to its dark theme (winit does not tell on Android).
    pub fn system_is_dark() -> Option<bool> {
        match DARK.load(Ordering::Relaxed) {
            0 => None,
            told => Some(told == 2),
        }
    }

    /// Has the icons of the system's bars be drawn for a dark or a light app, or as
    /// the phone is set (`None`: the activity follows the phone's theme by itself).
    pub fn set_appearance(dark: Option<bool>) {
        super::java::set_appearance(match dark {
            None => 0,
            Some(false) => 1,
            Some(true) => 2,
        });
    }

    /// How much of the screen's lower end the on-screen keyboard covers, in points.
    pub fn keyboard_height() -> f32 {
        inset(4)
    }

    pub fn hide_main_window() {}

    pub fn show_main_window() {}

    pub fn order_front_without_activating() {}

    /// Leaves the app without ending it (the Back gesture on its first page): it goes
    /// behind what was there before and plays on.
    pub fn send_back() {
        super::java::move_to_back();
    }

    pub fn is_main_window_visible() -> bool {
        super::lifecycle::in_foreground()
    }
}
