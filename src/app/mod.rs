//! Application start-up, wiring and shutdown.
pub mod host;
pub mod logging;
pub mod paths;
pub mod settings;

use crate::i18n::Lang;
use crate::library::{Library, LibraryPaths, OpenReport};
use crate::platform::nowplaying::{MediaSession, NowPlayingInfo, RemoteCommand};
use crate::platform::{self, app_hooks};
use crate::playback::{self, Command, PlayState, PlayerConfig, PlayerHandle};
use paths::AppPaths;
use settings::Settings;
use std::cell::RefCell;
use std::sync::{Arc, Mutex, OnceLock};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
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

thread_local! {
    /// Main-thread-only media session (Objective-C objects are not `Send`).
    static MEDIA: RefCell<Option<MediaSession>> = const { RefCell::new(None) };
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
    platform::run_on_main(move || {
        MEDIA.with(|media| {
            if let Some(session) = media.borrow_mut().as_mut() {
                session.update(info.as_ref());
            }
        });
    });
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
    MEDIA.with(|media| *media.borrow_mut() = Some(session));
    refresh_now_playing(core);
}

/// Unregisters the media session. Main thread.
pub fn remove_media_session() {
    MEDIA.with(|media| *media.borrow_mut() = None);
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

    let (library, open_report) = Library::open(LibraryPaths {
        data_dir: paths.data_dir.clone(),
        cache_dir: paths.cache_dir.clone(),
    })?;

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
        correction: settings.active_correction().cloned(),
        radio: settings.radio,
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
    });
    let _ = core_slot.set(core.clone());

    // Library changes: repaint, and let the player drop tracks that no longer exist.
    let notify_core = core.clone();
    library.set_notifier(Box::new(move || {
        notify_core.request_repaint();
        notify_core.player.send(Command::LibraryChanged);
    }));
    library.rescan();
    library.start_watching();

    let wake_core = core.clone();
    app_hooks::set_waker(move || wake_core.request_repaint());
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
