//! The home and lock screen widgets (ios/Widget): what the app leaves for them.
//!
//! A widget is drawn by a process of its own, which the system starts when it wants
//! the widget drawn; it cannot ask the app anything. So the app leaves the song that
//! plays (title, artist, and a small copy of the cover) where both may read it, and
//! tells the system that the widgets are out of date.
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

/// The song as the widgets are told it: title, artist, the file of its cover.
type Song = (String, String, Option<PathBuf>);

/// What the widgets were last told: nothing is written twice.
static TOLD: Mutex<Option<Song>> = Mutex::new(None);

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

/// The app says what plays (`None`: nothing). Any thread.
pub fn update(info: Option<&NowPlayingInfo>) {
    let Some(info) = info else {
        // Nothing plays any more: the widgets keep the song that was on last.
        return;
    };
    let song: Song = (
        info.title.clone(),
        info.artist.clone(),
        info.art_path.clone(),
    );
    {
        let mut told = told();
        if told.as_ref() == Some(&song) {
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
    let (title, artist, art) = song;
    // The cover first, the song last: a widget that is drawn in between shows the
    // song before with a wrong cover at worst, and is drawn again right after.
    // A cover that cannot be made (its file is being written again, say) is a song
    // without one: the title and the artist are told all the same.
    let cover = art.as_deref().and_then(small_cover);
    let has_cover = match &cover {
        Some(jpeg) => put(group, COVER, jpeg).is_ok(),
        None => {
            remove(group, COVER);
            false
        }
    };
    let told = serde_json::json!({
        "title": title,
        "artist": artist,
        "cover": has_cover,
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
