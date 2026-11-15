//! What plays, for the system: a media session and its notification, which a
//! foreground service of the app shows (`PlaybackService` in `android/`). The service
//! is also what keeps the process alive while music plays with the app off screen, and
//! its buttons (and those of the lock screen and of headsets) come back as commands.
use std::path::PathBuf;
use std::sync::OnceLock;

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

type Send = Box<dyn Fn(RemoteCommand) + std::marker::Send + Sync>;
static SEND: OnceLock<Send> = OnceLock::new();

/// The app's side of the media session. The session itself lives in Java.
pub struct MediaSession;

impl MediaSession {
    pub fn install(send: impl Fn(RemoteCommand) + std::marker::Send + Sync + 'static) -> Self {
        let _ = SEND.set(Box::new(send));
        Self
    }

    pub fn update(&mut self, info: Option<&NowPlayingInfo>) {
        match info {
            Some(info) => super::java::update_playback(
                &info.title,
                &info.artist,
                &info.album,
                (info.duration_s * 1000.0) as i64,
                (info.position_s * 1000.0) as i64,
                info.playing,
                &info
                    .art_path
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            None => super::java::stop_playback(),
        }
    }
}

impl Drop for MediaSession {
    fn drop(&mut self) {
        super::java::stop_playback();
    }
}

/// A button was pressed (see `natives`).
pub(super) fn remote(command: i32, argument: i64) {
    let command = match command {
        0 => RemoteCommand::Play,
        1 => RemoteCommand::Pause,
        2 => RemoteCommand::Toggle,
        3 => RemoteCommand::Stop,
        4 => RemoteCommand::Next,
        5 => RemoteCommand::Previous,
        6 => RemoteCommand::Seek(argument.max(0) as f64 / 1000.0),
        _ => return,
    };
    if let Some(send) = SEND.get() {
        send(command);
    }
}
