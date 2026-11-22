//! The home and lock screen widgets (ios/Widget): what the app leaves for them.
//!
//! A widget is drawn by a process of its own, which the system starts when it wants
//! the widget drawn; it cannot ask the app anything. So the app leaves the song that
//! plays (title, artist, a small copy of the cover, and whether it plays) where both
//! may read it, and tells the system that the widgets are out of date.
//!
//! "Plays" is told with a time: when the song would end as it goes now. The app can
//! be ended without a word (the system ends it, or it fails), and a widget that said
//! "playing" would then say so for good; past that time it no longer does. The next
//! song, a pause or a seek tells the widgets anew.
//!
//! That place is the keychain: a group of its items that the app and its widgets
//! share. The place made for this, a folder of an App Group, has to be registered on
//! the developer account, which the build cannot do by itself (and an app signed
//! without the group has no such folder: its widgets said that nothing plays). A
//! group of keychain items takes nothing but the team's own prefix, which every
//! signing profile allows. Nothing that is left there is secret; it is the one store
//! both processes reach. The passwords of network folders are in the app's own
//! group, which the widgets cannot read (ios/OnEum.entitlements).
use crate::platform::nowplaying::NowPlayingInfo;
use objc2_foundation::{NSBundle, NSString};
use security_framework::access_control::{ProtectionMode, SecAccessControl};
use security_framework::passwords::{
    PasswordOptions, delete_generic_password_options, set_generic_password_options,
};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The song as the widgets are told it.
#[derive(Debug, Clone, PartialEq)]
struct Song {
    title: String,
    artist: String,
    /// The file of its cover.
    art: Option<PathBuf>,
    /// It plays, and until when it would (seconds since 1970): see the module.
    /// `None`: paused.
    playing_until: Option<u64>,
}

impl Song {
    /// Whether the widgets, told `self`, need not be told `other`: the same song in
    /// the same state, with an end that moved by no more than a few seconds (the
    /// position as it is read wanders that much from one telling to the next).
    fn tells_as_much_as(&self, other: &Song) -> bool {
        let ends_alike = match (self.playing_until, other.playing_until) {
            (None, None) => true,
            (Some(a), Some(b)) => a.abs_diff(b) <= END_SLACK_S,
            _ => false,
        };
        ends_alike
            && self.title == other.title
            && self.artist == other.artist
            && self.art == other.art
    }
}

/// How far the end of the song may move before the widgets are told again.
const END_SLACK_S: u64 = 5;
/// What the end that is told lies after the song's: the next song takes a moment to
/// begin, and tells the widgets itself.
const END_GRACE_S: u64 = 15;
/// How long a song whose length is not known is said to play without a new word.
const UNKNOWN_LENGTH_S: u64 = 30 * 60;

/// What the widgets were last told: nothing is written twice.
static TOLD: Mutex<Option<Song>> = Mutex::new(None);
/// The cover that lies in the shared group now (the file it was made from; `None`
/// inside: none lies there). A pause or a seek leaves the song again, not its cover.
static COVER_LEFT: Mutex<Option<Option<PathBuf>>> = Mutex::new(None);

/// The items of the shared group: under this service, the song (JSON) and its cover
/// (a JPEG). ios/Widget/OnEumWidget.swift reads them by the same names.
const SERVICE: &str = "OnEum widgets";
const SONG: &str = "song";
const COVER: &str = "cover";
/// The longer side of the cover that is left, in pixels: no widget shows it larger.
const COVER_EDGE_PX: u32 = 320;
/// A cover that is larger than this as a file is left out (the keychain is no place
/// for pictures of any size).
const COVER_MAX_BYTES: usize = 96 * 1024;

#[cfg(oneum_widget_bridge)]
unsafe extern "C" {
    /// ios/Bridge/WidgetBridge.swift.
    fn oneum_reload_widgets();
}

fn told() -> std::sync::MutexGuard<'static, Option<Song>> {
    TOLD.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The full name of the group of keychain items the app shares with its widgets (with
/// the team's prefix, which the build writes into the app's Info.plist). `None` in an
/// app that was put together without it.
fn shared_group() -> Option<String> {
    let key = NSString::from_str("OneumSharedKeychain");
    let value = NSBundle::mainBundle().objectForInfoDictionaryKey(&key)?;
    let name = value.downcast::<NSString>().ok()?.to_string();
    // (A name the build did not fill in is no group.)
    (!name.is_empty() && !name.contains("$(")).then_some(name)
}

/// Until when (seconds since 1970) the song of `info` would play as it goes now
/// (`None`: it is paused). `now`: seconds since 1970.
fn playing_until(info: &NowPlayingInfo, now: u64) -> Option<u64> {
    if !info.playing {
        return None;
    }
    let left = if info.duration_s > 0.0 {
        let left = (info.duration_s - info.position_s).max(0.0) / info.rate.max(0.1);
        // (A length that is no number is no length.)
        if left.is_finite() {
            left.ceil().min(u64::MAX as f64 / 2.0) as u64
        } else {
            UNKNOWN_LENGTH_S
        }
    } else {
        UNKNOWN_LENGTH_S
    };
    Some(now.saturating_add(left).saturating_add(END_GRACE_S))
}

fn now_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

/// The app says what plays (`None`: nothing). Any thread.
pub fn update(info: Option<&NowPlayingInfo>) {
    let Some(info) = info else {
        // Nothing plays any more: the widgets keep the song that was on last.
        return;
    };
    tell(Song {
        title: info.title.clone(),
        artist: info.artist.clone(),
        art: info.art_path.clone(),
        playing_until: playing_until(info, now_s()),
    });
}

/// The app is ending: a song that the widgets were told plays is told paused (the
/// next start brings it back paused). Waits until that is written.
pub fn app_ending() {
    let song = {
        let told = told();
        match told.as_ref() {
            Some(song) if song.playing_until.is_some() => Song {
                playing_until: None,
                ..song.clone()
            },
            _ => return,
        }
    };
    *told() = Some(song.clone());
    if let Some(group) = shared_group() {
        leave(&group, &song);
    }
}

/// Tells the widgets `song`, unless they were told as much.
fn tell(song: Song) {
    {
        let mut told = told();
        if told.as_ref().is_some_and(|before| before.tells_as_much_as(&song)) {
            return;
        }
        *told = Some(song.clone());
    }
    let Some(group) = shared_group() else {
        return;
    };
    // Not on the caller's thread: a picture is made smaller and the keychain is
    // asked four things, which the player's thread need not wait for.
    let spawned = std::thread::Builder::new()
        .name("widgets".into())
        .spawn(move || leave(&group, &song));
    if let Err(error) = spawned {
        tracing::warn!("the widgets cannot be told what plays: {error}");
    }
}

/// Leaves `song` for the widgets and tells the system that they are out of date.
fn leave(group: &str, song: &Song) {
    // One at a time, and only the song that is the last one told: a song that was
    // told while this waited is left by its own thread.
    static LEAVING: Mutex<()> = Mutex::new(());
    let _one = LEAVING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if told().as_ref() != Some(song) {
        return;
    }
    // The cover first, the song last: a widget that is drawn in between shows the
    // song before with a wrong cover at worst, and is drawn again right after.
    // A cover that cannot be made (its file is being written again, say) is a song
    // without one: the title and the artist are told all the same.
    let has_cover = {
        let mut left = COVER_LEFT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match left.as_ref() {
            Some(lying) if *lying == song.art => lying.is_some(),
            _ => {
                let cover = song.art.as_deref().and_then(small_cover);
                let kept = match &cover {
                    Some(jpeg) => put(group, COVER, jpeg).is_ok(),
                    None => {
                        remove(group, COVER);
                        false
                    }
                };
                *left = Some(song.art.clone().filter(|_| kept));
                kept
            }
        }
    };
    let told = serde_json::json!({
        "title": song.title,
        "artist": song.artist,
        "cover": has_cover,
        "playing_until": song.playing_until,
    });
    if let Err(error) = put(group, SONG, told.to_string().as_bytes()) {
        tracing::warn!("the widgets cannot be told what plays: {error}");
        return;
    }
    // SAFETY: a function without arguments that calls WidgetKit, from any thread.
    #[cfg(oneum_widget_bridge)]
    unsafe {
        oneum_reload_widgets()
    };
}

/// The cover in `file` (one of the library's cache, 600 pixels at most) as a JPEG no
/// larger than a widget shows it.
fn small_cover(file: &Path) -> Option<Vec<u8>> {
    use image::codecs::jpeg::JpegEncoder;
    use image::imageops::FilterType;
    let image = image::ImageReader::open(file)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    let image = if image.width().max(image.height()) > COVER_EDGE_PX {
        image.resize(COVER_EDGE_PX, COVER_EDGE_PX, FilterType::Triangle)
    } else {
        image
    };
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, 80)
        .encode_image(&image.into_rgb8())
        .ok()?;
    (jpeg.len() <= COVER_MAX_BYTES).then_some(jpeg)
}

fn item(group: &str, account: &str) -> PasswordOptions {
    let mut options = PasswordOptions::new_generic_password(SERVICE, account);
    options.set_access_group(group);
    options
}

/// Keeps `bytes` under `account` in the shared group. The widgets of the lock screen
/// are drawn while the phone is locked: the item can be read from the first unlock
/// on. It stays on this device.
fn put(group: &str, account: &str, bytes: &[u8]) -> Result<(), String> {
    let refused = |error| format!("the keychain refused: {error}");
    let access = SecAccessControl::create_with_protection(
        Some(ProtectionMode::AccessibleAfterFirstUnlockThisDeviceOnly),
        0,
    )
    .map_err(refused)?;
    // (An item's protection is set when it is made: one that is there is replaced
    // whole.)
    remove(group, account);
    let mut options = item(group, account);
    options.set_access_control(access);
    set_generic_password_options(bytes, options).map_err(refused)
}

fn remove(group: &str, account: &str) {
    let _ = delete_generic_password_options(item(group, account));
}

/// What is left for the widgets now, as they would read it: the song (its JSON) and
/// whether a cover lies beside it. For verification.
#[cfg(any(debug_assertions, feature = "automation"))]
pub fn left() -> Option<(String, bool)> {
    use security_framework::passwords::generic_password;
    let group = shared_group()?;
    let song = String::from_utf8(generic_password(item(&group, SONG)).ok()?).ok()?;
    let cover = generic_password(item(&group, COVER)).is_ok_and(|jpeg| !jpeg.is_empty());
    Some((song, cover))
}
