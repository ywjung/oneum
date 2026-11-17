//! Application start-up, wiring and shutdown.
pub mod host;
#[cfg(test)]
mod hostile;
pub mod logging;
pub mod panics;
pub mod paths;
pub mod relaunch;
pub mod settings;

use crate::i18n::Lang;
use crate::library::{Library, LibraryPaths, OpenReport};
use crate::platform::nowplaying::{MediaSession, NowPlayingInfo, RemoteCommand};
use crate::platform::{self, app_hooks};
use crate::playback::{self, Command, PlayState, PlayerConfig, PlayerHandle};
use paths::AppPaths;
use settings::Settings;
#[cfg(not(target_os = "android"))]
use std::cell::RefCell;
use std::sync::{Arc, Mutex, OnceLock};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// When this program was built, as "2026-11-16 00:52 UTC" (see build.rs): it tells two
/// builds of one version apart.
pub const BUILT: &str = env!("ONEUM_BUILT");
/// How long quitting waits for the player thread (a format switch back on a slow DAC
/// can take a few seconds).
const SHUTDOWN_PATIENCE: std::time::Duration = std::time::Duration::from_secs(8);

/// Waits for `thread` to finish, but not longer than `patience`. Returns whether it did;
/// a thread that did not is left to itself.
pub fn join_within(thread: std::thread::JoinHandle<()>, patience: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + patience;
    while !thread.is_finished() {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let _ = thread.join();
    true
}
/// Remote "play" commands arriving this soon after launch are not user intent.
const LAUNCH_GUARD: std::time::Duration = std::time::Duration::from_secs(3);

/// Handles shared by the UI and the platform callbacks.
pub struct Core {
    pub library: Arc<Library>,
    pub player: PlayerHandle,
    pub paths: AppPaths,
    player_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// Wakes the UI (set once the egui context exists).
    repaint: OnceLock<Box<dyn Fn() + Send + Sync>>,
    /// Settings the UI changed and has not handed to its writer yet (it waits a
    /// moment so that a slider on its way is not written at every step).
    unsaved_settings: Mutex<Option<Settings>>,
}

impl Core {
    pub fn set_repaint(&self, f: impl Fn() + Send + Sync + 'static) {
        let _ = self.repaint.set(Box::new(f));
    }

    pub fn request_repaint(&self) {
        if let Some(f) = self.repaint.get() {
            f();
        }
    }

    /// The UI says which settings it still has to save (`None`: none, they are on
    /// their way to the file or in it).
    pub fn note_unsaved_settings(&self, settings: Option<&Settings>) {
        let mut unsaved = self
            .unsaved_settings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Called every frame while a control is moved: copied only when it differs.
        if unsaved.as_ref() != settings {
            *unsaved = settings.cloned();
        }
    }

    /// Writes the settings the UI has not saved yet, if there are any. For the moment
    /// a phone's app leaves the screen: it may be ended there without another frame.
    pub fn save_unsaved_settings(&self) {
        let unsaved = self
            .unsaved_settings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(settings) = unsaved
            && let Err(error) =
                crate::storage::settings::save_json_atomic(&self.paths.settings_file(), &settings)
        {
            tracing::warn!("could not save the settings on leaving the screen: {error}");
        }
    }

    /// What the system's Now Playing display should show.
    pub fn now_playing_info(&self) -> Option<NowPlayingInfo> {
        let snap = self.player.snapshot();
        let current = snap.current?;
        let track = self.library.track(current.track_id)?;
        Some(NowPlayingInfo {
            title: track.title.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            duration_s: current.duration_ms as f64 / 1000.0,
            position_s: snap.position_now_ms() as f64 / 1000.0,
            playing: matches!(
                snap.state,
                PlayState::Playing | PlayState::Loading | PlayState::Seeking
            ),
            art_path: track
                .art_id
                .as_deref()
                .map(|id| self.library.art_path(id))
                .filter(|p| p.exists()),
        })
    }

    /// Saves state and stops background work. Safe to call more than once.
    pub fn shutdown(&self) {
        self.player.send(Command::Shutdown);
        let thread = self.player_thread.lock().ok().and_then(|mut t| t.take());
        // The player thread saves the session and gives the output device back, which
        // takes a moment. It is not waited for without end: a thread stuck in a device
        // driver that has hung would keep the app from ever quitting.
        if let Some(thread) = thread
            && !join_within(thread, SHUTDOWN_PATIENCE)
        {
            tracing::error!("the player thread did not stop; quitting without it");
        }
        self.library.shutdown();
    }
}

#[cfg(not(target_os = "android"))]
thread_local! {
    /// Main-thread-only media session (Objective-C objects are not `Send`).
    static MEDIA: RefCell<Option<MediaSession>> = const { RefCell::new(None) };
}

/// Keeps the media session (or drops it, with `None`). Main thread.
#[cfg(not(target_os = "android"))]
fn set_media(session: Option<MediaSession>) {
    MEDIA.with(|media| *media.borrow_mut() = session);
}

/// Tells the media session what plays. Any thread: it is done on the main one.
#[cfg(not(target_os = "android"))]
fn update_media(info: Option<platform::nowplaying::NowPlayingInfo>) {
    platform::run_on_main(move || {
        MEDIA.with(|media| {
            if let Some(session) = media.borrow_mut().as_mut() {
                session.update(info.as_ref());
            }
        });
    });
}

/// On Android the session lives in Java and is bound to no thread of the app's. (The
/// thread that draws does not run while the app is off screen, which is when the
/// session matters most.)
#[cfg(target_os = "android")]
static MEDIA: std::sync::Mutex<Option<MediaSession>> = std::sync::Mutex::new(None);

#[cfg(target_os = "android")]
fn set_media(session: Option<MediaSession>) {
    *MEDIA
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = session;
}

#[cfg(target_os = "android")]
fn update_media(info: Option<platform::nowplaying::NowPlayingInfo>) {
    if let Some(session) = MEDIA
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_mut()
    {
        session.update(info.as_ref());
    }
}

/// Pushes the current playback state to the system. Any thread.
fn refresh_now_playing(core: &Arc<Core>) {
    // Until something has actually been played in this run, stay out of the system's
    // Now Playing slot: merely launching OnEum (with a restored, paused session) must not
    // take the media keys away from another app that is playing.
    static ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let info = core.now_playing_info();
    if info.as_ref().is_some_and(|i| i.playing) {
        ACTIVE.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    if !ACTIVE.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    // The home and lock screen widgets of an iPhone show the song too (Android's are
    // told by the Java side, where its media session is).
    #[cfg(target_os = "ios")]
    platform::ios::widget::update(info.as_ref());
    update_media(info);
}

/// Registers media keys / Control Center commands. Main thread, after launch.
pub fn install_media_session(core: &Arc<Core>) {
    // Verification runs can opt out so they never take over the user's media keys.
    if std::env::var_os("ONEUM_NO_MEDIA_SESSION").is_some() {
        return;
    }
    let player = core.player.clone();
    let started = std::time::Instant::now();
    let session = MediaSession::install(move |command| {
        tracing::info!(?command, "remote command");
        // The system may replay a "play" to an app that was playing when it last quit.
        // Launching must never start sound on its own, so start commands are ignored
        // for a moment after start-up; everything else is always honoured.
        let starts_playback = matches!(command, RemoteCommand::Play | RemoteCommand::Toggle);
        if starts_playback && started.elapsed() < LAUNCH_GUARD {
            tracing::info!("ignored: within the launch guard period");
            return;
        }
        player.send(match command {
            RemoteCommand::Play => Command::Play,
            RemoteCommand::Pause => Command::Pause,
            RemoteCommand::Toggle => Command::TogglePlayPause,
            RemoteCommand::Stop => Command::Stop,
            RemoteCommand::Next => Command::Next,
            RemoteCommand::Previous => Command::Previous,
            RemoteCommand::Seek(seconds) => Command::SeekTo((seconds * 1000.0) as u64),
        });
    });
    set_media(Some(session));
    refresh_now_playing(core);
}

/// Unregisters the media session. Main thread.
pub fn remove_media_session() {
    set_media(None);
}

pub struct Startup {
    pub core: Arc<Core>,
    pub settings: Settings,
    pub open_report: OpenReport,
    pub lang: Lang,
    pub system_prefers_korean: bool,
}

static PROCESS_START: OnceLock<std::time::Instant> = OnceLock::new();

/// Time since `start()` was entered (close to process start).
pub fn process_uptime() -> std::time::Duration {
    PROCESS_START
        .get()
        .map_or(std::time::Duration::ZERO, |t| t.elapsed())
}

/// Where the log lives, in a form fit for an error dialog (no user name in the common case).
pub fn log_dir_hint() -> String {
    match std::env::var_os("ONEUM_DATA_DIR").filter(|v| !v.is_empty()) {
        Some(root) => std::path::PathBuf::from(root)
            .join("logs")
            .display()
            .to_string(),
        None if cfg!(windows) => "%LOCALAPPDATA%\\OnEum\\Logs".to_string(),
        None => "~/Library/Logs/OnEum".to_string(),
    }
}

/// Of `files`, those that are the app's own copies (in `copies`, the folder it keeps
/// them in): the songs they were made for have left the library, and nothing else
/// would ever remove them. No file anywhere else is touched, whatever is passed in.
pub fn delete_own_copies(copies: &std::path::Path, files: &[std::path::PathBuf]) {
    // Compared as the system names the folders: a phone's app folder is reached
    // through a link, and the library keeps the files under the name behind it.
    let Ok(copies) = std::fs::canonicalize(copies) else {
        return;
    };
    for file in files {
        let own = file
            .parent()
            .and_then(|folder| std::fs::canonicalize(folder).ok())
            .is_some_and(|folder| folder == copies);
        if !own {
            continue;
        }
        if let Err(error) = std::fs::remove_file(file)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("a copy the app made could not be removed: {error}");
        }
    }
}

/// What the last panic was (its message and where it happened), as the panic hooks
/// tell it: for code that catches a panic and wants to say what it was.
static LAST_PANIC: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Called by the panic hooks.
pub fn note_panic(what: String) {
    *LAST_PANIC.lock().unwrap_or_else(|p| p.into_inner()) = Some(what);
}

/// The last panic of this run, if there was one.
pub fn last_panic() -> Option<String> {
    LAST_PANIC.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

/// The app's own copies (files directly in `copies`) that no song stands for and that
/// are older than `spare`: a file that turned out not to be music, a copy left half
/// written when the app was ended, one whose song was removed while the file could not
/// be. Nothing else would ever remove them, and the listener cannot reach the folder.
/// A newer one is left alone: it may be on its way into the library.
pub fn sweep_own_copies(
    copies: &std::path::Path,
    in_library: impl Fn(&std::path::Path) -> bool,
    spare: std::time::Duration,
) {
    let Ok(copies) = std::fs::canonicalize(copies) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&copies) else {
        return;
    };
    for entry in entries.flatten() {
        let file = copies.join(entry.file_name());
        let old_file = entry
            .metadata()
            .ok()
            .filter(|meta| meta.is_file())
            .is_some_and(|meta| {
                meta.modified()
                    .ok()
                    .and_then(|at| at.elapsed().ok())
                    .is_some_and(|age| age >= spare)
            });
        if old_file && !in_library(&file) {
            tracing::info!("a copy that no song stands for is removed");
            delete_own_copies(&copies, std::slice::from_ref(&file));
        }
    }
}

/// What the system does to the sound (a phone: a call, headphones pulled out) is
/// answered the way a listener expects of a music player: paused while something else
/// has the sound, playing on afterwards only if it was this that stopped it and the
/// listener did not stop or pause it in the meantime, and silent when the output it
/// played to is gone. The player itself keeps track of that (it sees the listener's
/// commands in the order they came): see [`Command::InterruptionOver`].
fn observe_sound(player: &PlayerHandle) {
    use platform::SoundEvent;
    let player = player.clone();
    platform::sound::observe(move |event| match event {
        SoundEvent::Interrupted => player.send(Command::Interrupted),
        SoundEvent::InterruptionOver { resume } => {
            player.send(Command::InterruptionOver { resume });
        }
        SoundEvent::OutputGone => player.send(Command::Pause),
    });
}

/// On an iPhone the music folder is not chosen: it is the app's Documents folder, the
/// one place the user can put files into. It is made a music folder of the library
/// the first time the app starts.
///
/// On Android the phone's shared Music folder takes that place, but only as a first
/// choice: it is added once (`marker` says that it was), and the user may take it out
/// and add others.
fn add_phone_music_folder(library: &Library, marker: &std::path::Path) {
    let Some(folder) = AppPaths::phone_music_dir() else {
        tracing::warn!("the phone's music folder cannot be told: none is added");
        return;
    };
    // Noted once the folder is in the library: a start before the app may read the
    // phone's storage leaves it for the next one.
    let note = || {
        if !platform::COPIES_MUSIC_IN
            && let Err(error) = std::fs::write(marker, b"")
        {
            tracing::warn!("cannot note that the music folder was added: {error}");
        }
    };
    if !platform::COPIES_MUSIC_IN {
        if marker.exists() {
            return;
        }
    } else if let Err(error) = std::fs::create_dir_all(&folder) {
        tracing::warn!("cannot make the music folder: {error}");
        return;
    }
    if library.roots().iter().any(|root| root.path == folder) {
        note();
        return;
    }
    match library.add_folder(&folder) {
        Ok(_) => note(),
        Err(error) => tracing::warn!("cannot add the music folder: {error}"),
    }
}

/// Opens the library, starts the player and the file watcher.
pub fn start() -> Result<Startup, Box<dyn std::error::Error>> {
    let _ = PROCESS_START.set(std::time::Instant::now());
    let paths = AppPaths::resolve()?;
    logging::init(&paths.log_dir);
    tracing::info!(version = VERSION, "starting OnEum");
    let settings: Settings =
        crate::storage::settings::load_json::<Settings>(&paths.settings_file()).sanitized();
    let system_prefers_korean = platform::locale::prefers_korean();
    let lang = settings.language.resolve(system_prefers_korean);

    let library_paths = LibraryPaths {
        data_dir: paths.data_dir.clone(),
        cache_dir: paths.cache_dir.clone(),
    };
    // An iPhone app's container (and the music in it) is given another path when the
    // app is updated or restored: the library follows it.
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .filter(|_| platform::CONTAINER_MOVES);
    // Folders that were linked on a phone (used where they are): the right to read
    // them has to be there before the library looks into them, and one that was moved
    // since is followed to its new place.
    let linked = platform::files::restore_linked_folders(&paths.data_dir);
    let moved: Vec<(std::path::PathBuf, std::path::PathBuf)> = linked
        .iter()
        .filter_map(|(was, now)| Some((was.clone(), now.clone()?)))
        .filter(|(was, now)| was != now)
        .collect();
    let (library, open_report) = match &home {
        Some(home) => Library::open_following(library_paths, home, &moved)?,
        None => Library::open(library_paths)?,
    };

    if platform::PHONE {
        add_phone_music_folder(&library, &paths.data_dir.join("music-folder-added"));
    }
    // A linked folder that was moved and could not be followed is at least read at
    // its new place.
    for (_, now) in &moved {
        if !library.roots().iter().any(|root| root.path == *now)
            && let Err(error) = library.add_folder(now)
        {
            tracing::warn!("cannot add a linked folder at its new place: {error}");
        }
    }

    // The player host needs a way back to the core for change notifications; the core
    // needs the player handle. Tie the knot through a late-initialised slot.
    let core_slot: Arc<OnceLock<Arc<Core>>> = Arc::new(OnceLock::new());
    let slot = core_slot.clone();
    let host = Arc::new(host::LibraryHost::new(library.clone(), move || {
        if let Some(core) = slot.get() {
            core.request_repaint();
            refresh_now_playing(core);
        }
    }));
    let config = PlayerConfig {
        default_volume: 0.8,
        replaygain: settings.replaygain,
        eq: settings.eq,
        crossfeed: settings.crossfeed,
        crossfade_secs: settings
            .crossfade_secs
            .min(crate::app::settings::MAX_CROSSFADE_SECS),
        speed_percent: settings.speed_percent,
        correction: settings.active_correction().cloned(),
        radio: settings.radio,
        shuffle_by_album: settings.shuffle_by_album,
        shuffle_by_rating: settings.shuffle_by_rating,
        resume_long_tracks: settings.resume_long_tracks,
        device_pref: settings.output_device.clone(),
        output: settings.output,
        ..PlayerConfig::default()
    };
    let (player, player_thread) = playback::player::spawn(host, config);
    let core = Arc::new(Core {
        library: library.clone(),
        player: player.clone(),
        paths,
        player_thread: Mutex::new(Some(player_thread)),
        repaint: OnceLock::new(),
        unsaved_settings: Mutex::new(None),
    });
    let _ = core_slot.set(core.clone());

    // Library changes: repaint, and let the player drop tracks that no longer exist.
    // Only when tracks did leave the library: most notifications are progress (a scan,
    // loudness measuring) or edits, many a second during a long run, and each
    // `LibraryChanged` has the player look up every track in its queue.
    let notify_core = core.clone();
    let seen_removals = std::sync::atomic::AtomicU64::new(library.removals());
    library.set_notifier(Box::new(move || {
        notify_core.request_repaint();
        let removals = notify_core.library.removals();
        // `fetch_max`: of notifications racing on two threads, only one sends.
        if seen_removals.fetch_max(removals, std::sync::atomic::Ordering::AcqRel) < removals {
            notify_core.player.send(Command::LibraryChanged);
        }
    }));
    library.rescan();
    library.start_watching();
    // Where the app keeps copies of its own (Android): those of earlier runs that no
    // song stands for go. Not on this thread: it reads a folder.
    if let Some(copies) = AppPaths::imported_copies_dir() {
        let library = library.clone();
        let swept = std::thread::Builder::new()
            .name("oneum-copies".into())
            .spawn(move || {
                let songs: std::collections::HashSet<std::path::PathBuf> = library
                    .snapshot()
                    .tracks
                    .iter()
                    .map(|track| track.path.clone())
                    .collect();
                sweep_own_copies(
                    &copies,
                    |file| songs.contains(file),
                    std::time::Duration::from_secs(3600),
                );
            });
        if let Err(error) = swept {
            tracing::warn!("no thread to look through the app's copies: {error}");
        }
    }

    let wake_core = core.clone();
    app_hooks::set_waker(move || wake_core.request_repaint());
    observe_sound(&player);
    // A phone's app that has left the screen may be ended without another word: what
    // is not saved yet (where the song stands, the volume, a setting just changed) is
    // saved as it leaves. Music put into the app's folder while it was away is read
    // when it comes back.
    let (leaving_player, leaving_core) = (player.clone(), core.clone());
    let returning = library.clone();
    platform::lifecycle::observe(
        move || {
            leaving_player.send(Command::SaveSession);
            leaving_core.save_unsaved_settings();
        },
        move || returning.rescan(),
    );
    let sleep_player = player;
    platform::power::observe_sleep(move || sleep_player.send(Command::SystemWillSleep));

    Ok(Startup {
        core,
        settings,
        open_report,
        lang,
        system_prefers_korean,
    })
}

#[cfg(test)]
mod tests {
    use super::{delete_own_copies, sweep_own_copies};

    /// The sweep removes old files of the copies folder that no song stands for (a
    /// file that was no music, a half-written copy), and nothing else: not a copy a
    /// song stands for, not a new one (it may be on its way in), not a folder.
    #[test]
    fn the_sweep_removes_old_copies_no_song_stands_for() {
        use std::time::{Duration, SystemTime};
        let temp = tempfile::tempdir().unwrap();
        let copies = temp.path().join("Imported");
        std::fs::create_dir_all(copies.join("folder")).unwrap();
        let old = SystemTime::now() - Duration::from_secs(7200);
        let make = |name: &str, aged: bool| {
            let file = copies.join(name);
            std::fs::write(&file, b"x").unwrap();
            if aged {
                std::fs::File::options()
                    .write(true)
                    .open(&file)
                    .unwrap()
                    .set_modified(old)
                    .unwrap();
            }
            file
        };
        let song = make("song.flac", true);
        let no_music = make("notes.pls", true);
        let half = make("copy123.part", true);
        let fresh = make("just now.flac", false);
        let in_folder = copies.join("folder").join("deep.flac");
        std::fs::write(&in_folder, b"x").unwrap();

        let real = std::fs::canonicalize(&copies).unwrap();
        let in_library = |file: &std::path::Path| file == real.join("song.flac");
        sweep_own_copies(&copies, in_library, Duration::from_secs(3600));
        assert!(song.exists(), "a song stands for it");
        assert!(!no_music.exists() && !half.exists(), "old and unused: gone");
        assert!(fresh.exists(), "a new one is left alone");
        assert!(in_folder.exists() && copies.join("folder").is_dir());
        // A folder that is not there: nothing happens.
        sweep_own_copies(&temp.path().join("none"), |_| false, Duration::ZERO);
    }

    /// Only the app's own copies are deleted: files directly in the folder it keeps
    /// them in. A file elsewhere (the listener's music), one deeper inside that
    /// folder, and the folder itself are left alone.
    #[test]
    fn only_the_apps_own_copies_are_deleted() {
        let temp = tempfile::tempdir().unwrap();
        let copies = temp.path().join("Imported");
        let music = temp.path().join("Music");
        std::fs::create_dir_all(copies.join("deeper")).unwrap();
        std::fs::create_dir_all(&music).unwrap();
        let copy = copies.join("song.flac");
        let other_copy = copies.join("song (2).flac");
        let deeper = copies.join("deeper").join("song.flac");
        let theirs = music.join("song.flac");
        for file in [&copy, &other_copy, &deeper, &theirs] {
            std::fs::write(file, b"music").unwrap();
        }
        delete_own_copies(
            &copies,
            &[
                copy.clone(),
                deeper.clone(),
                theirs.clone(),
                copies.clone(),
                copies.join("not-there.flac"),
                copies.join("..").join("Music").join("song.flac"),
            ],
        );
        assert!(!copy.exists(), "the copy of the removed song is gone");
        assert!(other_copy.exists(), "a copy that was not named stays");
        assert!(deeper.exists() && theirs.exists() && copies.is_dir());

        // The folder is named through a link (as an Android app's folder is), the
        // file by the folder's real name: it is the same folder.
        #[cfg(unix)]
        {
            let link = temp.path().join("link");
            std::os::unix::fs::symlink(&copies, &link).unwrap();
            delete_own_copies(&link, std::slice::from_ref(&other_copy));
            assert!(!other_copy.exists(), "found through the link");
        }
        // A folder that is not there: nothing is deleted, nothing fails.
        delete_own_copies(&temp.path().join("none"), std::slice::from_ref(&theirs));
        assert!(theirs.exists());
    }
}
