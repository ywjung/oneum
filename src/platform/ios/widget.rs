//! The home and lock screen widgets (ios/Widget): what the app leaves for them.
//!
//! A widget is drawn by a process of its own, which the system starts when it wants
//! the widget drawn; it cannot ask the app anything. So the app writes the song that
//! plays (title, artist, and a copy of the cover) into a folder both may read, the
//! app group's container, and tells the system that the widgets are out of date. An
//! app that was built without the group (no entitlement) has no such folder: nothing
//! is written then, and the widgets say that nothing plays.
use crate::platform::nowplaying::NowPlayingInfo;
use objc2_foundation::{NSBundle, NSFileManager, NSString};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// What the widgets were last told: nothing is written twice.
static TOLD: Mutex<Option<(String, String, Option<PathBuf>)>> = Mutex::new(None);

#[cfg(oneum_widget_bridge)]
unsafe extern "C" {
    /// ios/Bridge/WidgetBridge.swift.
    fn oneum_reload_widgets();
}

/// The folder the app and its widgets share: that of the group named after the app
/// ("group." and the app's identifier).
fn shared_folder() -> Option<PathBuf> {
    let bundle = NSBundle::mainBundle().bundleIdentifier()?;
    let group = NSString::from_str(&format!("group.{bundle}"));
    let url = NSFileManager::defaultManager()
        .containerURLForSecurityApplicationGroupIdentifier(&group)?;
    Some(PathBuf::from(url.path()?.to_string()))
}

/// The app says what plays (`None`: nothing). Any thread.
pub fn update(info: Option<&NowPlayingInfo>) {
    let now = info.map(|info| {
        (
            info.title.clone(),
            info.artist.clone(),
            info.art_path.clone(),
        )
    });
    let Some((title, artist, art)) = now.clone() else {
        // Nothing plays any more: the widgets keep the song that was on last.
        return;
    };
    {
        let mut told = TOLD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *told == now {
            return;
        }
        *told = now;
    }
    let Some(folder) = shared_folder() else {
        return;
    };
    if let Err(error) = write(&folder, &title, &artist, art.as_deref()) {
        tracing::warn!("the widgets cannot be told what plays: {error}");
        return;
    }
    // SAFETY: a function without arguments that calls WidgetKit, from any thread.
    #[cfg(oneum_widget_bridge)]
    unsafe {
        oneum_reload_widgets()
    };
}

/// The song as the widgets read it: `now-playing.json` and, with a cover, `cover.jpg`.
/// Each is written beside its place and moved there, so that a widget never reads half
/// of one.
fn write(folder: &Path, title: &str, artist: &str, art: Option<&Path>) -> std::io::Result<()> {
    std::fs::create_dir_all(folder)?;
    let cover = folder.join("cover.jpg");
    let beside = folder.join("cover.jpg.new");
    // A cover that cannot be copied (its file is being made again, say) is a song
    // without one: the title and the artist are told all the same.
    let has_cover = art.is_some_and(|art| {
        std::fs::copy(art, &beside).is_ok() && std::fs::rename(&beside, &cover).is_ok()
    });
    if !has_cover {
        let _ = std::fs::remove_file(&beside);
        let _ = std::fs::remove_file(&cover);
    }
    let song = serde_json::json!({
        "title": title,
        "artist": artist,
        "cover": has_cover,
    });
    let beside = folder.join("now-playing.json.new");
    std::fs::write(&beside, song.to_string())?;
    std::fs::rename(&beside, folder.join("now-playing.json"))
}
