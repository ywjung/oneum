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
    /// How fast the song goes, as a multiple of its own speed: the system moves the
    /// position it shows by this between what it is told.
    pub rate: f64,
    /// Cached cover image file, if any.
    pub art_path: Option<PathBuf>,
}

type Send = Box<dyn Fn(RemoteCommand) + std::marker::Send + Sync>;
static SEND: OnceLock<Send> = OnceLock::new();

/// The app's side of the media session. The session itself lives in Java.
pub struct MediaSession {
    /// What the session was told last, and when.
    told: Option<(NowPlayingInfo, std::time::Instant)>,
    /// The session has been told that nothing plays.
    told_nothing: bool,
}

impl MediaSession {
    pub fn install(send: impl Fn(RemoteCommand) + std::marker::Send + Sync + 'static) -> Self {
        let _ = SEND.set(Box::new(send));
        Self {
            told: None,
            told_nothing: false,
        }
    }

    /// Tells the session what plays, when that is something else than it was told.
    /// Every change of the player comes by here (the volume's slider, a setting of
    /// the equalizer, an edit of the queue: dozens a second while a slider moves),
    /// and each telling is the cover sent across to the system, the session's
    /// state and a notification made and posted anew.
    pub fn update(&mut self, info: Option<&NowPlayingInfo>) {
        match info {
            Some(info) => {
                let same = self.told.as_ref().is_some_and(|(told, at)| {
                    told.title == info.title
                        && told.artist == info.artist
                        && told.album == info.album
                        && told.art_path == info.art_path
                        && told.playing == info.playing
                        && told.rate == info.rate
                        && (told.duration_s - info.duration_s).abs() < 0.5
                        && crate::platform::position_as_told(
                            told.position_s,
                            told.playing,
                            told.rate,
                            at.elapsed().as_secs_f64(),
                            info.position_s,
                        )
                });
                if same {
                    return;
                }
                self.told = Some((info.clone(), std::time::Instant::now()));
                self.told_nothing = false;
                super::java::update_playback(info);
            }
            None => {
                if self.told.take().is_some() || !self.told_nothing {
                    self.told_nothing = true;
                    super::java::stop_playback();
                }
            }
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
