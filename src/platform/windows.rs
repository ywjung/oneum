//! Windows integration. Mirrors the names of the `macos` module so the rest of the app is
//! the same on both systems.
//!
//! What Windows does differently, and what is not implemented there:
//! - The menu bar is drawn inside the window (`ui::menubar`); there is no system menu.
//! - Closing the window quits (there is no Dock icon to bring a hidden window back).
//! - Files are handed to the app on its command line (Explorer's "Open with").
//! - Media keys and the system's media overlay are not connected.
//! - The output device is always used in shared mode at the rate Windows runs it at:
//!   exclusive access and sample-rate matching are macOS features (`DEVICE_CONTROL`).

use std::path::{Path, PathBuf};

/// Runs `f` "on the main thread". Nothing on Windows needs that thread, so it runs here.
pub fn run_on_main(f: impl FnOnce() + Send + 'static) {
    f();
}

/// Lets `duration` pass on the main thread (the counterpart of the macOS run loop pump:
/// nothing on Windows is served through it).
pub fn pump_main_loop(duration: std::time::Duration) {
    std::thread::sleep(duration);
}

/// Text in the Korean code page (CP949) as Unicode. `None` if the bytes are not that.
pub fn decode_cp949(bytes: &[u8]) -> Option<String> {
    const CP949: u32 = 949;
    /// Fail on bytes that are not valid in the code page instead of replacing them.
    const MB_ERR_INVALID_CHARS: u32 = 8;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MultiByteToWideChar(
            code_page: u32,
            flags: u32,
            bytes: *const u8,
            byte_count: i32,
            wide: *mut u16,
            wide_count: i32,
        ) -> i32;
    }
    let count = i32::try_from(bytes.len()).ok()?;
    if count == 0 {
        return Some(String::new());
    }
    // SAFETY: pointer and length describe `bytes`; with no output buffer the function
    // only reports how many UTF-16 units the text needs.
    let needed = unsafe {
        MultiByteToWideChar(
            CP949,
            MB_ERR_INVALID_CHARS,
            bytes.as_ptr(),
            count,
            std::ptr::null_mut(),
            0,
        )
    };
    let mut wide = vec![0u16; usize::try_from(needed).ok().filter(|n| *n > 0)?];
    // SAFETY: as above, and the output buffer has exactly the `needed` units.
    let written = unsafe {
        MultiByteToWideChar(
            CP949,
            MB_ERR_INVALID_CHARS,
            bytes.as_ptr(),
            count,
            wide.as_mut_ptr(),
            needed,
        )
    };
    (written == needed)
        .then(|| String::from_utf16(&wide).ok())
        .flatten()
}

/// Windows' own Explorer, by its full path: a program of that name lying next to
/// OnEum.exe (the app can be unzipped into any folder) must never be started instead.
fn explorer() -> std::process::Command {
    let system = std::env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .filter(|root| root.is_absolute())
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"));
    std::process::Command::new(system.join("explorer.exe"))
}

/// Hands a web address (already checked by the caller) to the default browser.
pub(super) fn open_in_browser(address: &str) -> bool {
    // Explorer opens a web address with whatever handles web addresses.
    explorer().arg(address).spawn().is_ok()
}

/// Shows a blocking error message (start-up failures).
pub fn fatal_alert(title: &str, body: &str) {
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title(title)
        .set_description(body)
        .show();
}

pub mod app_hooks {
    use super::PathBuf;
    use std::sync::Mutex;

    static OPENED: Mutex<Option<Vec<PathBuf>>> = Mutex::new(None);

    /// Remembers the files the app was started with (double-click, "Open with", a file
    /// dropped on the program).
    pub fn install_before_launch() {
        let paths: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
        if let Ok(mut opened) = OPENED.lock() {
            *opened = Some(paths);
        }
    }

    pub fn install_now() -> bool {
        true
    }

    /// Nothing arrives later on Windows, so there is nothing to wake the UI for.
    pub fn set_waker(_waker: impl Fn() + Send + Sync + 'static) {}

    /// The files to open, once.
    pub fn take_opened_paths() -> Vec<PathBuf> {
        OPENED
            .lock()
            .ok()
            .and_then(|mut opened| opened.take())
            .unwrap_or_default()
    }
}

pub mod finder {
    use super::Path;

    /// Shows `path` in File Explorer: a file is selected in its folder, a folder is opened.
    pub fn reveal(path: &Path) -> bool {
        let mut explorer = super::explorer();
        if path.is_dir() {
            explorer.arg(path);
        } else {
            // Explorer parses its own command line and splits it at commas: the path
            // goes in quotes of its own (a file name cannot contain a quote), written
            // as it is rather than quoted as a whole by the standard library.
            use std::os::windows::process::CommandExt;
            let mut select = std::ffi::OsString::from("/select,\"");
            select.push(path);
            select.push("\"");
            explorer.raw_arg(select);
        }
        // Explorer returns a non-zero status even when it worked; starting it is enough.
        explorer.spawn().is_ok()
    }
}

pub mod locale {
    /// Primary language id of Korean (`LANG_KOREAN`).
    const LANG_KOREAN: u16 = 0x12;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetUserDefaultUILanguage() -> u16;
    }

    /// True when Windows is set to Korean.
    pub fn prefers_korean() -> bool {
        // SAFETY: the function takes no arguments, cannot fail and only reads the
        // user's language setting.
        let language = unsafe { GetUserDefaultUILanguage() };
        // The low ten bits are the primary language.
        language & 0x3FF == LANG_KOREAN
    }
}

pub mod menu {
    use crate::i18n::Strings;
    use crate::platform::MenuAction;

    /// There is no system menu on Windows: the menu bar is part of the window and is
    /// drawn (and its shortcuts handled) by `ui::menubar`. This type only keeps the
    /// same shape as the macOS one.
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

pub mod nowplaying {
    use super::PathBuf;

    #[derive(Debug, Clone, Copy, PartialEq)]
    pub enum RemoteCommand {
        Play,
        Pause,
        Toggle,
        Stop,
        Next,
        Previous,
        /// Seek to this many seconds.
        Seek(f64),
    }

    #[derive(Debug, Clone, PartialEq)]
    pub struct NowPlayingInfo {
        pub title: String,
        pub artist: String,
        pub album: String,
        pub duration_s: f64,
        pub position_s: f64,
        pub playing: bool,
        /// Cached cover image file, if any.
        pub art_path: Option<PathBuf>,
    }

    /// Media keys and the system media overlay are not connected on Windows.
    pub struct MediaSession;

    impl MediaSession {
        pub fn install(_send: impl Fn(RemoteCommand) + Send + Sync + 'static) -> Self {
            Self
        }

        pub fn update(&mut self, _info: Option<&NowPlayingInfo>) {}
    }
}

pub mod power {
    /// Sleep is not observed on Windows: the output stream is invalidated by the system
    /// when the device goes away, which the engine handles like any device change.
    pub fn observe_sleep(_on_sleep: impl Fn() + Send + Sync + 'static) {}
}

pub mod responder {
    /// Edit commands that a system menu would send. Windows has no such menu: the
    /// keyboard shortcuts reach the text fields directly.
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
    //! The window is never hidden on Windows (closing it quits), so there is nothing to
    //! hide or bring back.

    pub fn hide_main_window() {}

    pub fn show_main_window() {}

    pub fn order_front_without_activating() {}

    pub fn send_back() {}

    pub fn is_main_window_visible() -> bool {
        true
    }
}

pub mod device {
    //! Output device control is not available on Windows (see `DEVICE_CONTROL`): no
    //! device is ever found here, so the engine leaves the device as the system has it.
    use std::time::Duration;

    /// One hardware stream format of a device.
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub struct PhysicalFormat {
        pub sample_rate: u32,
        pub min_rate: u32,
        pub max_rate: u32,
        pub bits: u32,
        pub channels: u32,
        pub float: bool,
        pub pcm: bool,
        pub mixable: bool,
    }

    impl PhysicalFormat {
        pub fn supports_rate(&self, rate: u32) -> bool {
            (self.min_rate..=self.max_rate).contains(&rate)
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct AudioDevice;

    impl AudioDevice {
        pub fn from_cpal_id(_id: &str) -> Option<Self> {
            None
        }

        pub fn nominal_rate(&self) -> Option<u32> {
            None
        }

        pub fn rate_ranges(&self) -> Vec<(u32, u32)> {
            Vec::new()
        }

        pub fn set_nominal_rate(&self, _rate: u32, _timeout: Duration) -> bool {
            false
        }

        pub fn hog_owner(&self) -> Option<i32> {
            None
        }

        pub fn is_hogged_by_us(&self) -> bool {
            false
        }

        pub fn take_hog(&self) -> bool {
            false
        }

        pub fn release_hog(&self) {}

        pub fn physical_format(&self) -> Option<PhysicalFormat> {
            None
        }

        pub fn physical_formats(&self) -> Vec<PhysicalFormat> {
            Vec::new()
        }

        pub fn set_physical_format(
            &self,
            _format: &PhysicalFormat,
            _rate: u32,
            _timeout: Duration,
        ) -> bool {
            false
        }
    }
}
