//! Platform integration. The rest of the app uses the names exported here and does not
//! know which system it runs on; everything system-specific (and all `unsafe`) lives in
//! the module of that system.
//!
//! - macOS (`macos`): system menu bar, Now Playing and media keys, Finder, CoreAudio
//!   device control, sleep notification.
//! - Windows (`windows`): File Explorer, the system language, and neutral stand-ins for
//!   what Windows does differently or what is not implemented there (see
//!   docs/ARCHITECTURE.md, "플랫폼").
//! - iPhone (`ios`): the audio session, the lock screen, the document picker and the
//!   app's life in the background; `apple` holds what it shares with macOS.
//! - Android (`android`): what Java offers, reached through JNI: the on-screen
//!   keyboard, audio focus, the playback notification, pickers and storage access.
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub mod dialogs;
mod menu_action;
#[cfg(not(target_os = "macos"))]
pub mod no_device;
#[cfg(any(target_os = "android", test))]
pub(crate) mod typed;

pub use menu_action::MenuAction;

/// What the system does to the app's sound without being asked (on an iPhone: a call
/// comes in, the headphones are pulled out). See `sound::observe`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundEvent {
    /// Something else has the sound now (a call, an alarm): the app is silent.
    Interrupted,
    /// That is over; `resume` says whether playing on is what the system expects.
    InterruptionOver { resume: bool },
    /// The output the sound went to is gone (headphones unplugged, a speaker out of
    /// reach). Playing on would be heard from somewhere it was not meant for.
    OutputGone,
}

/// Where the system leaves the app's sound alone and never ends the app unasked (macOS
/// and Windows), there is nothing to prepare or to be told.
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub mod sound {
    use super::SoundEvent;

    pub fn activate() -> bool {
        true
    }

    pub fn observe(_on_event: impl Fn(SoundEvent) + Send + Sync + 'static) {}
}

/// See `ios::lifecycle`: a desktop app is not sent away and brought back.
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub mod lifecycle {
    /// A desktop app is always where it can be seen, or may draw as if it were.
    pub fn in_foreground() -> bool {
        true
    }

    pub fn observe(
        _on_background: impl Fn() + Send + Sync + 'static,
        _on_foreground: impl Fn() + Send + Sync + 'static,
    ) {
    }
}

/// What the user is asked to choose (see `files::pick`).
pub struct PickRequest {
    /// The dialog's title; empty for the system's own.
    pub title: String,
    /// Kinds of files offered: a name and the file extensions of that kind.
    pub filters: Vec<(String, Vec<String>)>,
    pub kind: PickKind,
}

pub enum PickKind {
    /// A folder.
    Folder,
    /// A folder that is to be used where it is. On a desk that is what choosing a
    /// folder means anyway; on a phone, where a chosen folder is otherwise copied into
    /// the app's own, the app keeps the right to read this one instead.
    FolderInPlace,
    /// One or more files.
    Files,
    /// One file.
    File,
    /// Where to write a file, which is suggested this name.
    SaveAs(String),
}

impl PickRequest {
    pub fn new(kind: PickKind) -> Self {
        Self {
            title: String::new(),
            filters: Vec::new(),
            kind,
        }
    }

    pub fn title(mut self, title: &str) -> Self {
        self.title = title.to_string();
        self
    }

    pub fn filter(mut self, name: &str, extensions: &[&str]) -> Self {
        let extensions = extensions.iter().map(|e| (*e).to_string()).collect();
        self.filters.push((name.to_string(), extensions));
        self
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod apple;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub use macos::{
    app_hooks, coreaudio as device, decode_cp949, fatal_alert, files, finder, locale, menu,
    nowplaying, power, pump_main_loop, responder, run_on_main, system, testing, window,
};

#[cfg(target_os = "ios")]
pub mod ios;
#[cfg(target_os = "ios")]
pub use ios::{
    app_hooks, decode_cp949, device, fatal_alert, files, finder, lifecycle, locale, menu,
    nowplaying, power, pump_main_loop, responder, run_on_main, sound, system, testing, window,
};

#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub use windows::{
    app_hooks, decode_cp949, device, fatal_alert, files, finder, locale, menu, nowplaying, power,
    pump_main_loop, responder, run_on_main, system, testing, window,
};

#[cfg(target_os = "android")]
pub mod android;
#[cfg(target_os = "android")]
pub use android::{
    app_hooks, decode_cp949, device, fatal_alert, files, finder, lifecycle, locale, menu,
    nowplaying, power, pump_main_loop, responder, run_on_main, sound, system, testing, text_input,
    window,
};

/// True for an address a browser can be sent to: `http://` or `https://`, nothing else,
/// and nothing in it that is not part of an address.
pub fn is_web_address(text: &str) -> bool {
    let rest = text
        .strip_prefix("https://")
        .or_else(|| text.strip_prefix("http://"));
    rest.is_some_and(|rest| {
        !rest.is_empty()
            && text.len() <= 2048
            && text.chars().all(|c| {
                c.is_ascii_graphic()
                    && !matches!(
                        c,
                        '"' | '<' | '>' | '\\' | '^' | '`' | '{' | '|' | '}' | ','
                    )
            })
    })
}

/// Opens a web page in the user's browser. Anything that is not a web address (see
/// [`is_web_address`]) is not passed on to the system. Returns false if nothing was opened.
pub fn open_web_page(address: &str) -> bool {
    is_web_address(address) && open_in_browser(address)
}

#[cfg(target_os = "android")]
use android::open_in_browser;
#[cfg(target_os = "ios")]
use ios::open_in_browser;
#[cfg(target_os = "macos")]
use macos::open_in_browser;
#[cfg(windows)]
use windows::open_in_browser;

/// True on a phone: one narrow screen that is touched, not a window with a pointer and
/// a keyboard. The app then shows its phone screens (`ui::phone`).
pub const PHONE: bool = cfg!(any(target_os = "ios", target_os = "android"));

/// True where the app lives in a folder of its own that the system gives another path
/// now and then (an iPhone app's container, on an update), and where music lies in
/// that folder: the library follows it.
pub const CONTAINER_MOVES: bool = cfg!(target_os = "ios");

/// True where a music folder is copied into the app's own folder to be read (an
/// iPhone, unless the folder is linked). Elsewhere folders are read where they are.
pub const COPIES_MUSIC_IN: bool = cfg!(target_os = "ios");

/// True where reading music needs a right the user gives on a page of the system's
/// settings (Android's "all files access"), see `files::has_storage_access`.
pub const STORAGE_NEEDS_ACCESS: bool = cfg!(target_os = "android");

/// True where the menu bar is drawn inside the window and its shortcuts are handled by
/// the app (there is no system menu bar to hold them).
pub const MENU_IN_WINDOW: bool = cfg!(windows);

/// True where closing the window only hides it and the app keeps playing (the Mac
/// convention for music apps: the Dock icon brings the window back). Elsewhere closing
/// the window quits.
pub const CLOSE_HIDES_WINDOW: bool = cfg!(target_os = "macos");

/// True on a phone whose keyboards (plugged in or paired) send their keys to the app:
/// the shortcuts of the menus work there too. An iPhone only passes on typed text.
pub const PHONE_KEYBOARD: bool = cfg!(target_os = "android");

/// True on a phone that has a file manager which can be asked to show a folder.
pub const PHONE_FILE_MANAGER: bool = cfg!(target_os = "android");

/// True where the output device's sample rate and exclusive access can be controlled
/// (the "match the song's sample rate" and "exclusive access" options).
pub const DEVICE_CONTROL: bool = cfg!(target_os = "macos");

/// True where the output can at least be asked to run at a song's sample rate: on a
/// Mac the device is set to it, on an iPhone the system is asked and decides.
pub const RATE_CONTROL: bool = cfg!(any(target_os = "macos", target_os = "ios"));

/// A path as the bytes that are stored in the database. On Unix these are the path's own
/// bytes, whatever they are. Windows paths are UTF-16: they are stored as UTF-8, and a
/// name that is not valid Unicode (very rare there) is stored with replacement
/// characters, i.e. such a file can be listed but not found again.
pub fn path_to_bytes(path: &std::path::Path) -> std::borrow::Cow<'_, [u8]> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        std::borrow::Cow::Borrowed(path.as_os_str().as_bytes())
    }
    #[cfg(not(unix))]
    {
        match path.to_string_lossy() {
            std::borrow::Cow::Borrowed(text) => std::borrow::Cow::Borrowed(text.as_bytes()),
            std::borrow::Cow::Owned(text) => std::borrow::Cow::Owned(text.into_bytes()),
        }
    }
}

/// The reverse of [`path_to_bytes`].
pub fn path_from_bytes(bytes: Vec<u8>) -> std::path::PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        std::path::PathBuf::from(std::ffi::OsString::from_vec(bytes))
    }
    #[cfg(not(unix))]
    {
        match String::from_utf8(bytes) {
            Ok(text) => std::path::PathBuf::from(text),
            Err(e) => std::path::PathBuf::from(String::from_utf8_lossy(e.as_bytes()).into_owned()),
        }
    }
}

/// Rewrites Mac key symbols for the keyboard of this system: on Windows "⌘⇧U" becomes
/// "Ctrl+Shift+U". On a Mac the text is returned as it is.
pub fn key_names(text: &str) -> std::borrow::Cow<'_, str> {
    if cfg!(target_os = "macos") {
        std::borrow::Cow::Borrowed(text)
    } else {
        std::borrow::Cow::Owned(pc_key_names(text))
    }
}

/// [`key_names`] for a PC keyboard, on every system (so that it can be tested anywhere).
pub fn pc_key_names(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let name = match c {
            '⌘' => "Ctrl",
            '⇧' => "Shift",
            '⌥' => "Alt",
            _ => {
                out.push(c);
                continue;
            }
        };
        out.push_str(name);
        // "⌘ U" and "⌘U" both read "Ctrl+U".
        while chars.peek() == Some(&' ') {
            chars.next();
        }
        if chars.peek().is_some() {
            out.push('+');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{is_web_address, open_web_page, pc_key_names};

    #[test]
    fn only_web_addresses_are_handed_to_the_browser() {
        assert!(is_web_address("https://github.com/pdeljanov/Symphonia"));
        assert!(is_web_address("http://example.org/a?b=c#d"));
        for not_one in [
            "",
            "https://",
            "github.com/pdeljanov/Symphonia",
            "file:///etc/passwd",
            "x-apple.systempreferences:com.apple.preference",
            "-a Calculator https://example.org",
            "https://example.org/ and more",
            "https://example.org/\"quoted\"",
            // Explorer on Windows splits its command line at commas.
            "https://example.org/a,b",
            " https://example.org",
        ] {
            assert!(!is_web_address(not_one), "{not_one:?}");
            // Nothing is started for it either.
            assert!(!open_web_page(not_one), "{not_one:?}");
        }
        assert!(!is_web_address(&format!(
            "https://example.org/{}",
            "a".repeat(3000)
        )));
    }

    #[test]
    fn mac_key_symbols_become_pc_key_names() {
        assert_eq!(pc_key_names("⌘U"), "Ctrl+U");
        assert_eq!(pc_key_names("⌘ U"), "Ctrl+U");
        assert_eq!(pc_key_names("⇧ ⌘ O"), "Shift+Ctrl+O");
        assert_eq!(pc_key_names("⌥⌘I"), "Alt+Ctrl+I");
        assert_eq!(pc_key_names("⌘→ / ⌘←"), "Ctrl+→ / Ctrl+←");
        assert_eq!(pc_key_names("⌘,"), "Ctrl+,");
        assert_eq!(pc_key_names("⌘클릭이나 ⇧클릭"), "Ctrl+클릭이나 Shift+클릭");
        assert_eq!(pc_key_names("Space"), "Space");
        assert_eq!(pc_key_names("끝의 ⌘"), "끝의 Ctrl");
    }
}
