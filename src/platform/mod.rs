//! Platform integration. The rest of the app uses the names exported here and does not
//! know which system it runs on; everything system-specific (and all `unsafe`) lives in
//! the module of that system.
//!
//! - macOS (`macos`): system menu bar, Now Playing and media keys, Finder, CoreAudio
//!   device control, sleep notification.
//! - Windows (`windows`): File Explorer, the system language, and neutral stand-ins for
//!   what Windows does differently or what is not implemented there (see
//!   docs/ARCHITECTURE.md, "플랫폼").
mod menu_action;

pub use menu_action::MenuAction;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub use macos::{
    app_hooks, coreaudio as device, fatal_alert, finder, locale, menu, nowplaying, power,
    pump_main_loop, responder, run_on_main, testing, window,
};

#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub use windows::{
    app_hooks, device, fatal_alert, finder, locale, menu, nowplaying, power, pump_main_loop,
    responder, run_on_main, testing, window,
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

#[cfg(target_os = "macos")]
use macos::open_in_browser;
#[cfg(windows)]
use windows::open_in_browser;

/// True where the menu bar is drawn inside the window and its shortcuts are handled by
/// the app (there is no system menu bar to hold them).
pub const MENU_IN_WINDOW: bool = cfg!(not(target_os = "macos"));

/// True where closing the window only hides it and the app keeps playing (the Mac
/// convention for music apps: the Dock icon brings the window back). Elsewhere closing
/// the window quits.
pub const CLOSE_HIDES_WINDOW: bool = cfg!(target_os = "macos");

/// True where the output device's sample rate and exclusive access can be controlled
/// (the "match the song's sample rate" and "exclusive access" options).
pub const DEVICE_CONTROL: bool = cfg!(target_os = "macos");

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
