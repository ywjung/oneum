//! System media integration: Now Playing info and remote commands (media keys, Control
//! Center, headphones). Handlers only forward a [`RemoteCommand`]; the caller routes it to
//! the same player channel the GUI uses.
use block2::RcBlock;
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::NSImage;
use objc2_foundation::{NSMutableDictionary, NSNumber, NSSize, NSString};
use objc2_media_player::{
    MPChangePlaybackPositionCommandEvent, MPMediaItemArtwork, MPMediaItemPropertyAlbumTitle,
    MPMediaItemPropertyArtist, MPMediaItemPropertyArtwork, MPMediaItemPropertyPlaybackDuration,
    MPMediaItemPropertyTitle, MPNowPlayingInfoCenter, MPNowPlayingInfoPropertyElapsedPlaybackTime,
    MPNowPlayingInfoPropertyPlaybackRate, MPNowPlayingPlaybackState, MPRemoteCommand,
    MPRemoteCommandCenter, MPRemoteCommandEvent, MPRemoteCommandHandlerStatus,
};
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
        let image =
            NSImage::initWithContentsOfFile(NSImage::alloc(), &NSString::from_str(path.to_str()?))?;
        let size: NSSize = image.size();
        let handler_image = image.clone();
        let handler = RcBlock::new(move |_size: NSSize| NonNull::from(&*handler_image));
        // SAFETY: the handler returns a pointer to an NSImage that the block itself keeps alive.
        let artwork = unsafe {
            MPMediaItemArtwork::initWithBoundsSize_requestHandler(
                MPMediaItemArtwork::alloc(),
                size,
                &handler,
            )
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
            // SAFETY: clearing the dictionary and setting a state are plain property sets.
            unsafe {
                center.setNowPlayingInfo(None);
                center.setPlaybackState(MPNowPlayingPlaybackState::Stopped);
            }
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
                &*NSNumber::new_f64(if info.playing { 1.0 } else { 0.0 }),
            );
            if let Some(artwork) = &artwork {
                dict.insert(MPMediaItemPropertyArtwork, &**artwork);
            }
            center.setNowPlayingInfo(Some(&dict));
            center.setPlaybackState(if info.playing {
                MPNowPlayingPlaybackState::Playing
            } else {
                MPNowPlayingPlaybackState::Paused
            });
        }
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
            center.setPlaybackState(MPNowPlayingPlaybackState::Stopped);
        }
    }
}
