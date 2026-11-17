//! System media integration: Now Playing info and remote commands (media keys, Control
//! Center, the lock screen of an iPhone, headphones). Handlers only forward a
//! [`RemoteCommand`]; the caller routes it to the same player channel the GUI uses.
//!
//! The same on macOS and iOS but for two things: the image type of the cover, and the
//! playback state, which only macOS is told (iOS takes it from the audio session).
use block2::RcBlock;
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
#[cfg(target_os = "macos")]
use objc2_app_kit::NSImage as Image;
use objc2_core_foundation::CGSize;
use objc2_foundation::{NSMutableDictionary, NSNumber, NSString};
use objc2_media_player::{
    MPChangePlaybackPositionCommandEvent, MPMediaItemArtwork, MPMediaItemPropertyAlbumTitle,
    MPMediaItemPropertyArtist, MPMediaItemPropertyArtwork, MPMediaItemPropertyPlaybackDuration,
    MPMediaItemPropertyTitle, MPNowPlayingInfoCenter, MPNowPlayingInfoPropertyElapsedPlaybackTime,
    MPNowPlayingInfoPropertyPlaybackRate, MPRemoteCommand, MPRemoteCommandCenter,
    MPRemoteCommandEvent, MPRemoteCommandHandlerStatus,
};
#[cfg(target_os = "ios")]
use objc2_ui_kit::UIImage as Image;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::Arc;

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
    /// How fast the song goes, as a multiple of its own speed: the system moves the
    /// position it shows by this between what it is told.
    pub rate: f64,
    /// Cached cover image file, if any.
    pub art_path: Option<PathBuf>,
}

/// Keeps the registered handlers alive; dropping it unregisters them and clears Now Playing.
pub struct MediaSession {
    targets: Vec<(Retained<MPRemoteCommand>, Retained<AnyObject>)>,
    artwork: Option<(PathBuf, Retained<MPMediaItemArtwork>)>,
    last: Option<NowPlayingInfo>,
}

impl MediaSession {
    /// Registers the remote command handlers. Call on the main thread.
    pub fn install(send: impl Fn(RemoteCommand) + Send + Sync + 'static) -> Self {
        let send = Arc::new(send);
        let mut targets = Vec::new();
        // SAFETY: MediaPlayer objects are used as documented; handler blocks only call a
        // `Send + Sync` closure and return a status value.
        unsafe {
            let center = MPRemoteCommandCenter::sharedCommandCenter();
            let simple: [(Retained<MPRemoteCommand>, RemoteCommand); 6] = [
                (center.playCommand(), RemoteCommand::Play),
                (center.pauseCommand(), RemoteCommand::Pause),
                (center.togglePlayPauseCommand(), RemoteCommand::Toggle),
                (center.stopCommand(), RemoteCommand::Stop),
                (center.nextTrackCommand(), RemoteCommand::Next),
                (center.previousTrackCommand(), RemoteCommand::Previous),
            ];
            for (command, action) in simple {
                let send = send.clone();
                let handler = RcBlock::new(move |_e: NonNull<MPRemoteCommandEvent>| {
                    send(action);
                    MPRemoteCommandHandlerStatus::Success
                });
                command.setEnabled(true);
                let token = command.addTargetWithHandler(&handler);
                targets.push((command, token));
            }
            let seek = center.changePlaybackPositionCommand();
            let send_seek = send.clone();
            let handler = RcBlock::new(move |event: NonNull<MPRemoteCommandEvent>| {
                // The system passes an `MPChangePlaybackPositionCommandEvent` to handlers of
                // this command; the pointer is valid during the call.
                let event = event
                    .cast::<MPChangePlaybackPositionCommandEvent>()
                    .as_ref();
                let seconds = event.positionTime();
                if seconds.is_finite() && seconds >= 0.0 {
                    send_seek(RemoteCommand::Seek(seconds));
                    MPRemoteCommandHandlerStatus::Success
                } else {
                    MPRemoteCommandHandlerStatus::CommandFailed
                }
            });
            seek.setEnabled(true);
            let token = seek.addTargetWithHandler(&handler);
            targets.push((Retained::into_super(seek), token));
        }
        Self {
            targets,
            artwork: None,
            last: None,
        }
    }

    fn artwork_for(&mut self, path: &Path) -> Option<Retained<MPMediaItemArtwork>> {
        if let Some((cached_path, artwork)) = &self.artwork
            && cached_path == path
        {
            return Some(artwork.clone());
        }
        let image = load_image(path)?;
        #[cfg(target_os = "macos")]
        let size: CGSize = image.size();
        // SAFETY: reading the size of an image that was just loaded.
        #[cfg(target_os = "ios")]
        let size: CGSize = unsafe { image.size() };
        let handler_image = image.clone();
        let handler = RcBlock::new(move |_size: CGSize| NonNull::from(&*handler_image));
        // SAFETY: the handler returns a pointer to an image that the block itself keeps alive.
        #[cfg(target_os = "macos")]
        let artwork = unsafe {
            MPMediaItemArtwork::initWithBoundsSize_requestHandler(
                MPMediaItemArtwork::alloc(),
                size,
                &handler,
            )
        };
        // The bindings declare this initialiser for macOS only (with an `NSImage`). It
        // is the same method on iOS, taking a block that returns a `UIImage`.
        // SAFETY: as above; the argument types are those of
        // `-[MPMediaItemArtwork initWithBoundsSize:requestHandler:]` on iOS.
        #[cfg(target_os = "ios")]
        let artwork: Retained<MPMediaItemArtwork> = unsafe {
            objc2::msg_send![
                MPMediaItemArtwork::alloc(),
                initWithBoundsSize: size,
                requestHandler: &*handler,
            ]
        };
        self.artwork = Some((path.to_path_buf(), artwork.clone()));
        Some(artwork)
    }

    /// Publishes what is playing (or clears it with `None`). Call on the main thread.
    pub fn update(&mut self, info: Option<&NowPlayingInfo>) {
        // The position only needs re-publishing when it jumps; the system extrapolates.
        let unchanged = match (&self.last, info) {
            (Some(a), Some(b)) => {
                a.title == b.title
                    && a.artist == b.artist
                    && a.album == b.album
                    && a.playing == b.playing
                    && a.art_path == b.art_path
                    && (a.duration_s - b.duration_s).abs() < 0.5
                    && (a.position_s - b.position_s).abs() < 0.001
            }
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            return;
        }
        self.last = info.cloned();
        let center = unsafe { MPNowPlayingInfoCenter::defaultCenter() };
        let Some(info) = info else {
            // SAFETY: clearing the dictionary is a plain property set.
            unsafe { center.setNowPlayingInfo(None) };
            set_playback_state(&center, None);
            return;
        };
        let artwork = info.art_path.clone().and_then(|p| self.artwork_for(&p));
        // SAFETY: keys are MediaPlayer's documented constants; values have the documented types.
        unsafe {
            let dict = NSMutableDictionary::<NSString, AnyObject>::new();
            dict.insert(MPMediaItemPropertyTitle, &*NSString::from_str(&info.title));
            dict.insert(
                MPMediaItemPropertyArtist,
                &*NSString::from_str(&info.artist),
            );
            dict.insert(
                MPMediaItemPropertyAlbumTitle,
                &*NSString::from_str(&info.album),
            );
            dict.insert(
                MPMediaItemPropertyPlaybackDuration,
                &*NSNumber::new_f64(info.duration_s),
            );
            dict.insert(
                MPNowPlayingInfoPropertyElapsedPlaybackTime,
                &*NSNumber::new_f64(info.position_s),
            );
            dict.insert(
                MPNowPlayingInfoPropertyPlaybackRate,
                &*NSNumber::new_f64(if info.playing { info.rate } else { 0.0 }),
            );
            if let Some(artwork) = &artwork {
                dict.insert(MPMediaItemPropertyArtwork, &**artwork);
            }
            center.setNowPlayingInfo(Some(&dict));
        }
        set_playback_state(&center, Some(info.playing));
    }
}

/// The cover image in the file at `path`.
fn load_image(path: &Path) -> Option<Retained<Image>> {
    let path = NSString::from_str(path.to_str()?);
    #[cfg(target_os = "macos")]
    {
        Image::initWithContentsOfFile(Image::alloc(), &path)
    }
    #[cfg(target_os = "ios")]
    {
        Image::imageWithContentsOfFile(&path)
    }
}

/// Tells macOS whether we are playing (`None`: stopped). iOS has no such property: it
/// reads the state from the audio session and the playback rate in the info.
fn set_playback_state(center: &MPNowPlayingInfoCenter, playing: Option<bool>) {
    #[cfg(target_os = "macos")]
    {
        use objc2_media_player::MPNowPlayingPlaybackState;
        let state = match playing {
            Some(true) => MPNowPlayingPlaybackState::Playing,
            Some(false) => MPNowPlayingPlaybackState::Paused,
            None => MPNowPlayingPlaybackState::Stopped,
        };
        // SAFETY: a plain property set.
        unsafe { center.setPlaybackState(state) };
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (center, playing);
    }
}

impl Drop for MediaSession {
    fn drop(&mut self) {
        // SAFETY: removing the targets we added and clearing our own Now Playing entry.
        unsafe {
            for (command, token) in self.targets.drain(..) {
                command.removeTarget(Some(&token));
            }
            let center = MPNowPlayingInfoCenter::defaultCenter();
            center.setNowPlayingInfo(None);
            set_playback_state(&center, None);
        }
    }
}
