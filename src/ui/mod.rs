//! The egui user interface. Layout and behaviour follow `docs/UX.md`.
//!
//! The UI thread only draws and handles input: it reads immutable snapshots of the
//! library and the player and sends commands. Decoding, scanning and image processing
//! happen on other threads.
mod art;
mod automation;
mod background;
mod dialogs;
mod equalizer;
mod guide;
#[cfg(any(target_os = "android", test))]
mod hardkeys;
mod icons;
mod lists;
mod markdown;
mod memory;
mod menubar;
mod meter;
mod network;
mod network_folders;
mod panels;
mod phone;
mod player_bar;
pub mod renderer;
mod settings_window;
mod spectrum;
mod stats_view;
pub mod theme;
mod widgets;

use crate::app::settings::{MAX_PINS, Pin, RightPanel, Settings};
use crate::app::{self, Core};
use crate::audio::{OutputDeviceInfo, PlaybackErrorKind};
use crate::i18n::{Lang, Strings};
use crate::library::{Library, LibraryError, LibrarySnapshot, OpenReport, PlaylistInfo, SortKey};
use crate::model::{Track, TrackId};
use crate::platform::MenuAction;
use crate::platform::menu::AppMenu;
use crate::platform::responder::{self, EditAction};
use crate::platform::{PickKind, PickRequest, app_hooks, files, finder, window};
use crate::playback::{Command, Notice, PlayState, PlayerSnapshot, RepeatMode};
use art::ArtCache;
use dialogs::Dialog;
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "mp2", "mp1", "flac", "wav", "aif", "aiff", "m4a", "mp4", "m4b", "ogg", "oga", "opus",
    "wv", "ape", "dsf", "dff", "mka", "webm",
];
const PLAYLIST_EXTENSIONS: &[&str] = &["m3u8", "m3u"];
const VOLUME_STEP: f32 = 0.05;
/// How wide the mini player is (its height is the player bar's).
const MINI_WIDTH: f32 = 640.0;
/// How far the "skip ahead/back" commands move within a song.
const SEEK_STEP_MS: i64 = 5_000;
/// After a failed attempt to save the settings, how long until the next one.
const SETTINGS_RETRY: Duration = Duration::from_secs(10);
/// How long quitting waits for pending library edits and for the settings writer.
const QUIT_PATIENCE: Duration = Duration::from_secs(3);
/// The player thread answers within a fraction of a second, or a few seconds while an
/// output device switches its format. Silence for this long means it is stuck.
const PLAYER_STUCK_AFTER: Duration = Duration::from_secs(12);

/// What the central area shows.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Nav {
    Songs,
    Albums,
    Album(u64),
    Artists,
    Artist(String),
    Favorites,
    Recent,
    Playlist(i64),
    /// The genres named in the library's tags.
    Genres,
    /// Every song of one genre.
    Genre(String),
    /// A smart playlist (a saved search), by its name.
    Smart(String),
    /// Every song in a folder and the folders inside it; the empty path stands for
    /// all music folders.
    Folder(std::path::PathBuf),
    /// Listening statistics and what the library is made of.
    Stats,
    /// The media servers of the network, and what the one that is open holds.
    Network,
}

/// An optional column of the song lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColumnKind {
    Format,
    Year,
    Genre,
    Plays,
    Added,
    LastPlayed,
    Rating,
}

/// A genre and how many songs carry it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GenreEntry {
    name: String,
    songs: usize,
}

/// One row of a list: `key` identifies the row for selection (track id, playlist item
/// id, album key …), `idx` is an index into the snapshot's tracks/albums/artists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Row {
    key: u64,
    idx: u32,
}

/// `(library generation, playlists generation, query, sort)` a row list was built from.
type RowsStamp = (u64, u64, String, Option<(SortKey, bool)>);

/// Per-view state that survives switching views.
#[derive(Default)]
struct ListState {
    query: String,
    sort: Option<(SortKey, bool)>,
    selection: HashSet<u64>,
    /// Row the keyboard cursor / shift-range starts from.
    anchor: Option<u64>,
    /// Row the keyboard cursor is on; a shift-range spans from `anchor` to here.
    cursor: Option<u64>,
    rows: Vec<Row>,
    /// Inputs `rows` was built from: library generation, playlists generation, query, sort.
    built: Option<RowsStamp>,
    scroll_to: Option<usize>,
    total_ms: u64,
    /// Letters typed to jump to a song, and when the last one arrived.
    typeahead: String,
    typeahead_at: Option<Instant>,
}

/// Things decided while drawing that are carried out after the frame's borrows end.
enum Action {
    PlayTracks(Vec<TrackId>, usize),
    PlayNext(Vec<TrackId>),
    Enqueue(Vec<TrackId>),
    AddToPlaylist(i64, Vec<TrackId>),
    NewPlaylistWith(Vec<TrackId>),
    SetFavorite(Vec<TrackId>, bool),
    /// Stars for these songs: 1 to 5, 0 for none.
    SetRating(Vec<TrackId>, u8),
    Reveal(TrackId),
    ShowInfo(TrackId),
    RemoveFromPlaylist(i64, Vec<i64>),
    MovePlaylistItems(i64, Vec<i64>, usize),
    RemoveFromLibrary(Vec<TrackId>),
    Go(Nav),
    Sort(SortKey),
    /// Show or hide an optional column of the song lists.
    ToggleColumn(ColumnKind),
    /// Rename a smart playlist (asks for the new name).
    RenameSmart(String),
    DeleteSmart(String),
    /// Put a smart playlist's search into the search field of the song list.
    EditSmart(String),
    /// Pin to the sidebar, or unpin if pinned.
    TogglePin(Pin),
}

/// What is being dragged out of a list: songs, and — when they come from a playlist view
/// — which playlist items they are, so that dropping inside the same list reorders it.
#[derive(Clone)]
struct DragTracks {
    tracks: Vec<TrackId>,
    playlist_items: Option<(i64, Vec<i64>)>,
}

/// Typing letters jumps to a song; the letters typed so far are forgotten after this.
const TYPEAHEAD_TIMEOUT: Duration = Duration::from_millis(1000);

/// Which list the list keys (arrows, Return, Delete, ⌘A) belong to: the one the user
/// last clicked in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum KeyPane {
    #[default]
    List,
    Queue,
}

/// A message shown in the strip above the player bar.
struct Toast {
    text: String,
    warning: bool,
    action: Option<(String, ToastAction)>,
    /// `None` stays until dismissed.
    expires: Option<Instant>,
}

#[derive(Clone)]
enum ToastAction {
    RetryOutput,
    OpenPlaybackSettings,
    /// Puts a playlist back the way it was before items were removed.
    RestorePlaylist(i64, Vec<TrackId>),
}

/// Results of background work started by the UI.
enum AppEvent {
    FilesImported(Vec<TrackId>),
    PlaylistImported(Result<crate::library::M3uImport, LibraryError>),
    /// A library edit (see `background.rs`) failed.
    EditFailed(LibraryError),
    AddedToPlaylist {
        id: i64,
        count: usize,
    },
    /// Items were removed from a playlist that held `before`: offer to undo.
    RemovedFromPlaylist {
        id: i64,
        before: Vec<TrackId>,
    },
    PlaylistCreated(i64),
    /// The settings file was written (or not): the generation handed to the writer.
    SettingsWritten(u64, std::io::Result<()>),
    /// The statistics page's figures were worked out.
    StatsReady(Box<stats_view::StatsData>),
    AppleImported(Result<crate::library::AppleImport, LibraryError>),
    /// The library's own data was written to a file, or could not be.
    DataExported(Option<crate::library::backup::DataExport>),
    /// Library data from a file was taken in, or could not be.
    DataImported(Result<crate::library::backup::DataImport, LibraryError>),
    /// The file to show in the file manager was found (a CUE sheet's audio file).
    Reveal(PathBuf),
    /// A music folder was added.
    FolderAdded(PathBuf),
    /// A playlist was written to a file: how many entries, or that it could not be.
    PlaylistExported(Option<usize>),
    /// The log could not be written to the file the user is to keep.
    LogNotSaved,
    /// The user chose these in a file dialog that was shown for this.
    Picked(PickFor, Vec<PathBuf>),
}

/// A correction profile (parametric equalizer) that is being made or changed on screen
/// (see `settings_window::correction_editor`). What it holds is heard while it is
/// edited; nothing is kept until it is saved.
pub(super) struct PeqEdit {
    /// The name the profile is kept under, when one of the user's own is being
    /// changed: saving replaces that one, also under a new name.
    pub(super) original: Option<String>,
    pub(super) profile: crate::audio::PeqProfile,
    /// What was selected when the editing began: cancelling goes back to it.
    pub(super) selected_before: Option<String>,
    /// What the player was last sent, so that it is sent again only when it changed.
    pub(super) heard: Option<crate::audio::PeqProfile>,
    /// The editor has been on screen with it. (One that a verification script makes
    /// without the screen never is.)
    pub(super) shown: bool,
}

/// What a file dialog was shown for.
#[derive(Clone, Copy)]
enum PickFor {
    MusicFolder,
    /// On a phone: music to bring into the app's own folder.
    ImportMusic,
    Open,
    Correction,
    /// The impulse response of a room correction.
    Room,
    Playlist,
    AppleLibrary,
    /// Where to write the library's own data.
    ExportData,
    /// The library data to take in.
    ImportData,
    /// Where to write this playlist.
    ExportPlaylist(i64),
    /// Where to write a copy of the log.
    SaveLog,
}

/// Requests from the menu bar that need the window.
#[derive(Clone, Copy)]
enum UiRequest {
    Settings,
    OpenFiles,
    AddFolder,
    NewPlaylist,
    ImportPlaylist,
    Find,
    GoToCurrent,
    ToggleQueue,
    ToggleInfo,
    ToggleMeter,
    MiniPlayer,
    Equalizer,
    UserGuide,
    ToggleMute,
    PlaylistRename,
    PlaylistExport,
    PlaylistDelete,
    SongPlay,
    SongPlayNext,
    SongAddToQueue,
    SongToggleFavorite,
    SongShowInFinder,
    SongInfo,
}

/// Timings reported by verification dumps (cost: a few `Instant` reads per frame).
#[derive(Default)]
struct Perf {
    /// Process start to the first UI frame.
    first_frame_ms: Option<f64>,
    /// CPU time of the UI pass (layout and input; excludes GPU presentation).
    last_frame_ms: f64,
    max_frame_ms: f64,
    frames: u64,
    /// Time of the most recent row-list rebuild (search, sort or library change).
    last_rows_build_ms: f64,
    max_rows_build_ms: f64,
}

pub struct OneumApp {
    perf: Perf,
    core: Arc<Core>,
    settings: Settings,
    saved_settings: Settings,
    lang: Lang,
    system_prefers_korean: bool,

    snapshot: Arc<LibrarySnapshot>,
    lib_generation: u64,
    /// The tracks of media servers that the play queue names (not in `snapshot`: they
    /// are not part of the library), and the library's count of their changes.
    remote: Arc<HashMap<TrackId, Arc<Track>>>,
    remote_generation: u64,
    /// The media servers of the network and the folder that is open on one.
    network: network::Browser,
    /// The library's artist names by their folded form, and the library generation
    /// they were collected for (the info panel looks credits up in it every frame).
    artist_names: Option<(u64, Arc<HashMap<String, String>>)>,
    /// Bumped by the library when a cover it could not deliver has been regenerated.
    art_generation: u64,
    playlists: Vec<PlaylistInfo>,
    playlists_generation: u64,
    player: PlayerSnapshot,
    last_notice_seq: u64,
    /// The player thread has not been heard from for [`PLAYER_STUCK_AFTER`].
    player_stuck: bool,
    scan_was_running: bool,
    announce_scan: bool,
    scan_started_at: Option<Instant>,

    nav: Nav,
    history: Vec<Nav>,
    views: HashMap<Nav, ListState>,
    focus_search: bool,
    /// Whether the search field had keyboard focus at the end of the previous frame.
    search_had_focus: bool,
    /// A text field had the keyboard when this frame began. Keys that end text entry
    /// (Return, Escape) belong to that field for the whole frame, although the field
    /// gives up focus while handling them.
    typing: bool,
    /// A button or other control has keyboard focus (Tab navigation): Space, Return and
    /// the arrows are that control's, not the list's.
    widget_focused: bool,
    key_pane: KeyPane,
    info_track: Option<TrackId>,
    /// Queue panel selection (queue item ids) and drag state.
    queue_selection: HashSet<u64>,
    queue_anchor: Option<u64>,
    /// Keyboard cursor in the queue (a shift-range spans from the anchor to here).
    queue_cursor: Option<u64>,
    /// Item to select once a removal has gone through.
    queue_pending_selection: Option<u64>,
    queue_scroll_to_current: bool,
    /// Scroll to the current item even while the pointer is over the queue (panel opened).
    queue_scroll_forced: bool,
    /// Scroll offset and visible height of the queue list in the previous frame.
    queue_view: (f32, f32),
    /// Where the queue list was on screen in the previous frame.
    queue_rect: egui::Rect,
    seek_drag: Option<f64>,
    /// A seek that was sent but is not yet reflected in the player snapshot:
    /// `(target seconds, when)`. The thumb stays there instead of jumping back.
    seek_pending: Option<(f64, Instant)>,
    last_settings_save: Instant,
    /// What earlier runs left of their errors (see `past_errors`): read once, when the
    /// About page first asks.
    past_errors: Option<Vec<String>>,

    art: ArtCache,
    /// Needles of the VU meter panel.
    meter: meter::MeterState,
    /// Columns of the spectrum display in the meter deck.
    spectrum: spectrum::Spectrum,
    /// The app icon shown in Settings ▸ About (decoded on first use).
    about_icon: Option<egui::TextureHandle>,
    /// The open source notices in Settings ▸ About are shown.
    licenses_shown: bool,
    /// What the file of the song in the info panel says beyond the library (on a
    /// phone: of the song whose lyrics are shown).
    extras: Option<panels::Extras>,
    /// The same for the song of the phone's Song Info sheet, which may be another
    /// one than the song whose lyrics are up behind it.
    info_extras: Option<panels::Extras>,
    /// A lookup of lyrics on the internet is out (one at a time).
    lyrics_busy: Arc<std::sync::atomic::AtomicBool>,
    /// How far the room correction that is applied is turned down, as a factor
    /// (known once it was read here: after an import, or when it was switched on).
    room_trim: Option<f32>,
    /// Where lyrics are looked for on the internet, for a listener who switched that
    /// on, and where what was found is kept.
    lyrics_source: Arc<crate::library::online_lyrics::Source>,
    /// Which of the "listen again" lists is shown.
    rediscover: lists::Rediscover,
    /// The equalizer sheet is open.
    eq_open: bool,
    /// A correction profile that is being made or changed on screen.
    peq_edit: Option<PeqEdit>,
    /// The mini player is shown (the window is as small as the player bar and stays
    /// above other windows): the size the full window had, to go back to.
    mini: Option<egui::Vec2>,
    /// The mini player was asked for, or the full window again (done in the next
    /// frame, where the window can be told).
    mini_toggle: bool,
    /// The name being typed for a preset of the user's own.
    eq_name: String,
    /// Transient message.
    toast: Option<Toast>,
    /// Message that stays until dismissed or resolved (lost device, …). A transient
    /// message is shown in front of it for its few seconds and never replaces it.
    alert: Option<Toast>,
    dialog: Option<Dialog>,
    /// The form that adds a network folder, while it is open.
    folder_form: Option<network_folders::FolderForm>,
    renaming: Option<(i64, String, bool)>,
    settings_open: bool,
    settings_tab: settings_window::Tab,
    /// The user guide sheet.
    guide_open: bool,
    /// Section of the guide to scroll to in the next frame (0 = the top).
    guide_jump: Option<usize>,
    devices: Vec<OutputDeviceInfo>,
    devices_checked: Option<Instant>,

    menu: Option<AppMenu>,
    /// What a menu command does, shared by the system menu and the menu bar in the window.
    menu_dispatch: Option<Arc<dyn Fn(MenuAction) + Send + Sync>>,
    /// The genres of the library, for the genre view (built when that view is shown).
    genre_list: Vec<GenreEntry>,
    /// The folders inside the open folder view: (folder, library generation, entries).
    folder_children: Option<(PathBuf, u64, Vec<lists::FolderEntry>)>,
    stats_period: stats_view::Period,
    stats: Option<stats_view::StatsData>,
    /// The period whose statistics are being worked out.
    stats_pending: Option<stats_view::Period>,
    /// Library generation `genre_list` was built from.
    genre_list_generation: Option<u64>,
    /// Verification runs: whether the menu bar took each `menukey` so far.
    menu_keys: Vec<bool>,
    /// Draw the menu bar inside the window (always where there is no system menu bar;
    /// `ONEUM_WINDOW_MENU` forces it elsewhere, to look at it).
    window_menu: bool,
    automation: Option<automation::Automation>,
    ui_requests: Arc<Mutex<Vec<UiRequest>>>,
    events_tx: Sender<AppEvent>,
    events_rx: Receiver<AppEvent>,
    /// The phone screens are shown (always on a phone; `ONEUM_PHONE` asks for them in
    /// a window on a desk, to work on them).
    phone_mode: bool,
    phone: phone::Phone,
    edits: background::Edits,
    settings_writer: background::SettingsWriter,
    /// Bumped for every version of the settings handed to the writer.
    settings_generation: u64,
    /// The newest version handed to the writer and not yet reported back.
    settings_in_flight: Option<(u64, Settings)>,
    shut_down: bool,
}

impl OneumApp {
    pub fn new(cc: &eframe::CreationContext<'_>, startup: app::Startup) -> Self {
        let app::Startup {
            core,
            settings,
            open_report,
            lang,
            system_prefers_korean,
        } = startup;
        let ctx = cc.egui_ctx.clone();
        let phone_mode = crate::platform::PHONE
            || (automation::AVAILABLE && std::env::var_os("ONEUM_PHONE").is_some());
        theme::install(&ctx, phone_mode);
        theme::apply_choice(&ctx, settings.theme);

        // Nothing is drawn while the app is in the background (a phone); what changed
        // meanwhile is drawn when it comes back.
        let repaint_ctx = ctx.clone();
        core.set_repaint(move || {
            if crate::platform::lifecycle::in_foreground() {
                repaint_ctx.request_repaint();
            }
        });
        let bridge_ctx = ctx.clone();
        if !responder::install(move || bridge_ctx.request_repaint()) {
            tracing::warn!(
                "edit-menu bridge unavailable: copy/paste shortcuts may not reach text fields"
            );
        }
        if !app_hooks::install_now() {
            tracing::warn!("Finder open events are unavailable");
        }
        // The media controls of Windows belong to a window: it has to be known first.
        {
            use winit::raw_window_handle::HasWindowHandle;
            if let Ok(handle) = cc.window_handle() {
                window::remember_main_window(handle.as_raw());
            }
        }
        app::install_media_session(&core);

        let (events_tx, events_rx) = channel();
        let settings_writer = {
            let events = events_tx.clone();
            let core = core.clone();
            background::SettingsWriter::start(
                core.paths.settings_file(),
                move |generation, result| {
                    let _ = events.send(AppEvent::SettingsWritten(generation, result));
                    core.request_repaint();
                },
            )
        };
        let player = core.player.snapshot();
        // (A verification run knows no lyrics service until its script names one:
        // nothing of such a run is asked of the real one.)
        let lyrics_service = if std::env::var_os("ONEUM_SCRIPT").is_some() {
            crate::net::lyrics::NOWHERE
        } else {
            crate::net::lyrics::SERVICE
        };
        let lyrics_source = Arc::new(crate::library::online_lyrics::Source::new(
            &core.paths.cache_dir,
            lyrics_service,
        ));
        let mut this = Self {
            perf: Perf::default(),
            art: ArtCache::new(core.library.clone(), ctx.clone()),
            meter: meter::MeterState::default(),
            spectrum: spectrum::Spectrum::default(),
            snapshot: core.library.snapshot(),
            lib_generation: core.library.generation(),
            remote: core.library.remote_tracks(),
            remote_generation: core.library.remote_generation(),
            network: network::Browser::new(ctx.clone()),
            artist_names: None,
            art_generation: core.library.art_generation(),
            playlists: core.library.playlists(),
            playlists_generation: core.library.playlists_generation(),
            last_notice_seq: player.notice_seq,
            player_stuck: false,
            player,
            scan_was_running: false,
            announce_scan: false,
            scan_started_at: None,
            nav: Nav::Songs,
            history: Vec::new(),
            views: HashMap::new(),
            focus_search: false,
            search_had_focus: false,
            typing: false,
            widget_focused: false,
            key_pane: KeyPane::List,
            info_track: None,
            queue_selection: HashSet::new(),
            queue_anchor: None,
            queue_cursor: None,
            queue_pending_selection: None,
            queue_scroll_to_current: true,
            queue_scroll_forced: true,
            queue_view: (0.0, 0.0),
            queue_rect: egui::Rect::NOTHING,
            seek_drag: None,
            seek_pending: None,
            last_settings_save: Instant::now(),
            past_errors: None,
            about_icon: None,
            licenses_shown: false,
            extras: None,
            info_extras: None,
            lyrics_busy: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            room_trim: None,
            lyrics_source,
            rediscover: lists::Rediscover::default(),
            eq_open: false,
            peq_edit: None,
            mini: None,
            mini_toggle: false,
            eq_name: String::new(),
            toast: None,
            alert: None,
            dialog: None,
            folder_form: None,
            renaming: None,
            settings_open: false,
            guide_open: false,
            guide_jump: None,
            settings_tab: settings_window::Tab::General,
            devices: Vec::new(),
            devices_checked: None,
            menu: None,
            menu_dispatch: None,
            genre_list: Vec::new(),
            folder_children: None,
            stats_period: stats_view::Period::default(),
            stats: None,
            stats_pending: None,
            genre_list_generation: None,
            menu_keys: Vec::new(),
            window_menu: crate::platform::MENU_IN_WINDOW
                || std::env::var_os("ONEUM_WINDOW_MENU").is_some(),
            automation: automation::Automation::from_env(),
            ui_requests: Arc::new(Mutex::new(Vec::new())),
            events_tx,
            events_rx,
            phone_mode,
            phone: phone::Phone::new(),
            edits: background::Edits::start(),
            settings_writer,
            settings_generation: 0,
            settings_in_flight: None,
            shut_down: false,
            saved_settings: settings.clone(),
            settings,
            lang,
            system_prefers_korean,
            core,
        };
        // A room correction that is on from the start: how far it is turned down is
        // shown in the signal path.
        if this.settings.room.as_ref().is_some_and(|room| room.enabled) {
            this.room_trim = crate::audio::room::Impulse::open(&this.core.paths.room_file())
                .map(|impulse| impulse.turned_down());
        }
        this.install_menu(&ctx);
        report_open_problems(&mut this, &open_report);
        if let Some(state) = cc.wgpu_render_state.as_ref() {
            renderer::watch(state);
        }
        #[cfg(target_os = "android")]
        if let Some(gl) = cc.gl.as_ref() {
            renderer::note_glow(gl);
        }
        // A phone that ran out of memory left nothing that said what had grown.
        if cfg!(target_os = "android") {
            memory::watch();
        }
        this.after_relaunch();
        this.after_a_trial_given_up();
        // An interface to the graphics hardware that is on trial has shown that it
        // works once the app has drawn and lived a moment (a start that fails does not
        // get that far). Not counted in frames alone: an app left alone draws few.
        if app::relaunch::on_trial(&this.core.paths.data_dir) {
            let dir = this.core.paths.data_dir.clone();
            std::thread::spawn(move || {
                loop {
                    std::thread::sleep(Duration::from_secs(3));
                    if memory::frames() >= 3 {
                        app::relaunch::end_trial(&dir);
                        tracing::info!("graphics: the interface on trial works");
                        break;
                    }
                }
            });
        }
        this
    }

    /// The run before was to draw through an interface that is not the system's first
    /// choice and did not get far, so this run goes by the system's order (see
    /// `app::relaunch::begin_trial`). So that the next start does not try the same
    /// again: a choice of the settings goes back to "automatic", and what a device
    /// was lost on is forgotten.
    fn after_a_trial_given_up(&mut self) {
        if !app::relaunch::gave_up() {
            return;
        }
        self.settings.graphics = app::settings::GraphicsChoice::Auto;
        app::relaunch::forget_lost_interface(&self.core.paths.data_dir);
        self.warn_toast(self.s().graphics_gave_up);
    }

    /// The setting of what the screen is drawn through was changed: it holds from the
    /// next start, which is now (Android, the only system with the choice).
    pub(super) fn restart_for_graphics(&mut self) {
        tracing::info!(choice = ?self.settings.graphics, "graphics: starting anew for another interface");
        #[cfg(target_os = "android")]
        {
            // What a device was lost on belongs to the choice that was left.
            app::relaunch::forget_lost_interface(&self.core.paths.data_dir);
            // An interface on trial that let the settings be reached and changed has
            // shown that it works: left open, its trial would make the next start
            // give up the choice that was just made.
            app::relaunch::end_trial(&self.core.paths.data_dir);
            if crate::platform::android::open_again_after_exit() {
                self.shutdown();
                std::process::exit(0);
            }
        }
    }

    /// A run that the one before started because it could not go on (see
    /// [`Self::graphics_lost`]): it says so, and the music goes on where it was.
    fn after_relaunch(&mut self) {
        let dir = &self.core.paths.data_dir;
        let Some(note) = app::relaunch::read(dir) else {
            return;
        };
        if note.taken || !note.recent(app::relaunch::now()) {
            return;
        }
        let taken = app::relaunch::Note {
            taken: true,
            ..note
        };
        if let Err(error) = app::relaunch::write(dir, taken) {
            tracing::warn!("the note of the new start cannot be kept: {error}");
        }
        tracing::info!(
            playing = note.playing,
            "graphics: started anew after the device was lost"
        );
        if note.playing {
            self.core.player.send(Command::Play);
        }
        self.warn_toast(self.s().graphics_restarted);
    }

    /// The device the window is drawn with is gone ([`renderer::watch`]). Nothing can
    /// be drawn any more, in this process for good: the frame that would follow ends
    /// the app inside the UI library, without saving anything. So this one does not
    /// follow. What the app keeps is saved, and on Android the app is opened again in
    /// a new process, where the music goes on; elsewhere a message says what happened.
    /// Does not return.
    fn graphics_lost(&mut self) -> ! {
        let playing = sounding(self.player.state);
        tracing::error!(
            playing,
            "graphics: nothing can be drawn any more; ending this run"
        );
        // What was held of memory when it happened: the usual reason on a phone.
        tracing::error!("memory: {}", memory::read());
        #[cfg(target_os = "android")]
        {
            start_anew_after_loss(&self.core.paths.data_dir, playing, self.s());
            self.shutdown();
            std::process::exit(0);
        }
        #[cfg(not(target_os = "android"))]
        {
            self.shutdown();
            crate::platform::fatal_alert(self.s().graphics_lost_title, self.s().graphics_lost_body);
            std::process::exit(1);
        }
    }

    fn s(&self) -> &'static Strings {
        self.lang.strings()
    }

    fn install_menu(&mut self, ctx: &egui::Context) {
        self.menu = None; // remove the old menu first (language change)
        let player = self.core.player.clone();
        let library = self.core.library.clone();
        let requests = self.ui_requests.clone();
        let ctx = ctx.clone();
        let dispatch: Arc<dyn Fn(MenuAction) + Send + Sync> = Arc::new(move |action| {
            // Playback commands work even while the window is hidden; everything else
            // needs the window, so it is brought back first.
            let request = match action {
                MenuAction::PlayPause => return player.send(Command::TogglePlayPause),
                MenuAction::Next => return player.send(Command::Next),
                MenuAction::Previous => return player.send(Command::Previous),
                MenuAction::SeekForward => {
                    return player.send(Command::SeekRelative(SEEK_STEP_MS));
                }
                MenuAction::SeekBack => return player.send(Command::SeekRelative(-SEEK_STEP_MS)),
                MenuAction::StopAfterCurrent => {
                    let on = player.snapshot().stop_after_current;
                    return player.send(Command::SetStopAfterCurrent(!on));
                }
                MenuAction::RepeatSection => return player.send(Command::MarkSection),
                MenuAction::SleepTimer(minutes) => {
                    let after = (minutes > 0).then(|| Duration::from_secs(u64::from(minutes) * 60));
                    return player.send(Command::SetSleepTimer(after));
                }
                MenuAction::VolumeUp => {
                    return player.send(Command::SetVolume(player.snapshot().volume + VOLUME_STEP));
                }
                MenuAction::VolumeDown => {
                    return player.send(Command::SetVolume(player.snapshot().volume - VOLUME_STEP));
                }
                MenuAction::ToggleShuffle => {
                    return player.send(Command::SetShuffle(!player.snapshot().shuffle));
                }
                MenuAction::CycleRepeat => {
                    return player.send(Command::SetRepeat(next_repeat(player.snapshot().repeat)));
                }
                MenuAction::Rescan => return library.rescan(),
                MenuAction::ShowMainWindow => return window::show_main_window(),
                MenuAction::Settings => UiRequest::Settings,
                MenuAction::OpenFiles => UiRequest::OpenFiles,
                MenuAction::AddFolder => UiRequest::AddFolder,
                MenuAction::NewPlaylist => UiRequest::NewPlaylist,
                MenuAction::ImportPlaylist => UiRequest::ImportPlaylist,
                MenuAction::Find => UiRequest::Find,
                MenuAction::GoToCurrent => UiRequest::GoToCurrent,
                MenuAction::ToggleQueue => UiRequest::ToggleQueue,
                MenuAction::ToggleInfo => UiRequest::ToggleInfo,
                MenuAction::ToggleMeter => UiRequest::ToggleMeter,
                MenuAction::MiniPlayer => UiRequest::MiniPlayer,
                MenuAction::Equalizer => UiRequest::Equalizer,
                MenuAction::UserGuide => UiRequest::UserGuide,
                MenuAction::ToggleMute => UiRequest::ToggleMute,
                MenuAction::PlaylistRename => UiRequest::PlaylistRename,
                MenuAction::PlaylistExport => UiRequest::PlaylistExport,
                MenuAction::PlaylistDelete => UiRequest::PlaylistDelete,
                MenuAction::SongPlay => UiRequest::SongPlay,
                MenuAction::SongPlayNext => UiRequest::SongPlayNext,
                MenuAction::SongAddToQueue => UiRequest::SongAddToQueue,
                MenuAction::SongToggleFavorite => UiRequest::SongToggleFavorite,
                MenuAction::SongShowInFinder => UiRequest::SongShowInFinder,
                MenuAction::SongInfo => UiRequest::SongInfo,
            };
            // Commands that act on songs or the volume do their work without the window;
            // bringing a hidden window back for them would be an unwanted side effect.
            let needs_window = !matches!(
                request,
                UiRequest::ToggleMute
                    | UiRequest::SongPlay
                    | UiRequest::SongPlayNext
                    | UiRequest::SongAddToQueue
                    | UiRequest::SongToggleFavorite
                    | UiRequest::SongShowInFinder
            );
            if let Ok(mut queue) = requests.lock() {
                queue.push(request);
            }
            if needs_window {
                window::show_main_window();
            }
            ctx.request_repaint();
        });
        self.menu_dispatch = Some(dispatch.clone());
        self.menu = AppMenu::install(self.s(), app::VERSION, move |action| dispatch(action));
    }

    // ----- state synchronisation ------------------------------------------------------

    /// Everything that must keep working while the window is hidden or covered:
    /// library and player state, results of background work, Finder and menu requests,
    /// and the close button. Called for every pass, with or without a UI.
    fn sync(&mut self, ctx: &egui::Context) {
        let generation = self.core.library.generation();
        if generation != self.lib_generation {
            self.lib_generation = generation;
            self.snapshot = self.core.library.snapshot();
            self.art.library_changed();
        }
        let remote_generation = self.core.library.remote_generation();
        if remote_generation != self.remote_generation {
            self.remote_generation = remote_generation;
            self.remote = self.core.library.remote_tracks();
        }
        self.sync_network();
        let art_generation = self.core.library.art_generation();
        if art_generation != self.art_generation {
            // Covers that were missing a moment ago exist again: ask for them once more.
            self.art_generation = art_generation;
            self.art.library_changed();
        }
        let playlists_generation = self.core.library.playlists_generation();
        if playlists_generation != self.playlists_generation {
            self.playlists_generation = playlists_generation;
            self.playlists = self.core.library.playlists();
            if let Nav::Playlist(id) = self.nav
                && !self.playlists.iter().any(|p| p.id == id)
            {
                self.go(Nav::Songs);
                self.history.clear();
            }
        }
        let previous_track = self.player.current.map(|c| c.item_id);
        self.player = self.core.player.snapshot();
        #[cfg(target_os = "android")]
        WAS_PLAYING.store(
            sounding(self.player.state),
            std::sync::atomic::Ordering::Relaxed,
        );
        if self.player.current.map(|c| c.item_id) != previous_track {
            self.queue_scroll_to_current = true;
        }
        if self.player.notice_seq != self.last_notice_seq {
            self.last_notice_seq = self.player.notice_seq;
            if let Some(notice) = self.player.notice.clone() {
                self.show_notice(&notice);
            }
        }
        // A player thread that has gone silent is stuck in a call that does not return,
        // in practice a device driver that has hung. Nothing can be done about that
        // from here except telling the user why nothing reacts any more.
        let silent = self.core.player.unresponsive_for();
        if silent >= PLAYER_STUCK_AFTER {
            if !self.player_stuck {
                self.player_stuck = true;
                tracing::error!("the player thread has not responded for {silent:?}");
                self.alert = Some(Toast {
                    text: self.s().notice_player_stuck.into(),
                    warning: true,
                    action: None,
                    expires: None,
                });
            }
        } else if self.player_stuck {
            self.player_stuck = false;
            let ours = self.s().notice_player_stuck;
            if self.alert.as_ref().is_some_and(|t| t.text == ours) {
                self.alert = None;
            }
        }
        if silent >= Duration::from_secs(1) {
            // Keep looking: nothing else would redraw a window nobody touches.
            ctx.request_repaint_after(Duration::from_secs(1));
        }
        // A lost-device message is obsolete once playback is running again.
        if !self.player.device_lost
            && self
                .alert
                .as_ref()
                .is_some_and(|t| matches!(t.action, Some((_, ToastAction::RetryOutput))))
            && self.player.state == PlayState::Playing
        {
            self.alert = None;
        }

        let scan = self.core.library.scan_status();
        // Only scans the user asked for are announced; background rescans stay quiet.
        if self.scan_was_running && !scan.running && std::mem::take(&mut self.announce_scan) {
            // A folder macOS refuses to let us read (Downloads, Documents, Desktop and
            // external volumes need the user's consent) looks like an empty folder.
            let denied = self.core.library.roots().into_iter().any(|root| {
                root.track_count == 0
                    && std::fs::read_dir(&root.path)
                        .is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
            });
            if denied {
                self.alert = Some(Toast {
                    text: self.s().notice_folder_permission.into(),
                    warning: true,
                    action: None,
                    expires: None,
                });
            } else {
                let text =
                    self.lang
                        .scan_finished(self.snapshot.len(), scan.unsupported, scan.errors);
                self.info_toast(text);
            }
        }
        // A scan that finds nothing for a long time is most likely blocked on the
        // system's consent prompt; say so instead of spinning silently.
        if scan.running && !self.scan_was_running {
            self.scan_started_at = Some(Instant::now());
        }
        if !scan.running {
            self.scan_started_at = None;
        }
        if scan.running
            && scan.files_found == 0
            && self.toast.is_none()
            && self
                .scan_started_at
                .is_some_and(|t| t.elapsed() > Duration::from_secs(4))
        {
            self.toast = Some(Toast {
                text: self.s().scan_waiting_permission.into(),
                warning: false,
                action: None,
                expires: Some(Instant::now() + Duration::from_secs(20)),
            });
            self.scan_started_at = None;
        }
        self.scan_was_running = scan.running;

        while let Ok(event) = self.events_rx.try_recv() {
            match event {
                AppEvent::FilesImported(ids) if ids.is_empty() => {
                    self.warn_toast(self.s().notice_no_playable_files)
                }
                AppEvent::Reveal(path) => {
                    self.reveal_path(&path);
                }
                AppEvent::FolderAdded(path) => {
                    self.announce_scan = true;
                    let name = path.file_name().map_or_else(
                        || path.display().to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    );
                    self.info_toast(self.lang.folder_added(&name));
                }
                AppEvent::PlaylistExported(Some(count)) => {
                    self.info_toast(self.lang.playlist_exported(count));
                }
                AppEvent::PlaylistExported(None) => {
                    self.warn_toast(self.s().notice_export_failed);
                }
                AppEvent::LogNotSaved => self.warn_toast(self.s().notice_log_failed),
                AppEvent::Picked(purpose, paths) => self.picked(purpose, paths),
                AppEvent::FilesImported(ids) => {
                    self.core.player.send(Command::PlayTracks {
                        tracks: ids,
                        start_index: 0,
                    });
                }
                AppEvent::PlaylistImported(Ok(result)) => {
                    self.info_toast(self.lang.playlist_import_result(
                        &result.name,
                        result.matched,
                        result.missing.len(),
                    ));
                    self.playlists_generation = self.core.library.playlists_generation();
                    self.playlists = self.core.library.playlists();
                    self.go(Nav::Playlist(result.playlist_id));
                }
                AppEvent::PlaylistImported(Err(error)) => {
                    tracing::warn!("playlist import failed: {error}");
                    self.warn_toast(self.s().notice_import_failed);
                }
                AppEvent::EditFailed(error) => self.edit_error_toast(&error),
                AppEvent::AddedToPlaylist { id, count } => {
                    let name = self
                        .core
                        .library
                        .playlists()
                        .into_iter()
                        .find(|p| p.id == id)
                        .map(|p| p.name)
                        .unwrap_or_default();
                    self.info_toast(self.lang.added_to_playlist(count, &name));
                }
                AppEvent::RemovedFromPlaylist { id, before } => {
                    let s = self.s();
                    self.toast = Some(Toast {
                        text: s.notice_removed_from_playlist.into(),
                        warning: false,
                        action: Some((s.undo.into(), ToastAction::RestorePlaylist(id, before))),
                        expires: Some(Instant::now() + Duration::from_secs(8)),
                    });
                    if let Some(state) = self.views.get_mut(&Nav::Playlist(id)) {
                        state.selection.clear();
                    }
                }
                AppEvent::PlaylistCreated(id) => {
                    self.playlists_generation = self.core.library.playlists_generation();
                    self.playlists = self.core.library.playlists();
                    self.go(Nav::Playlist(id));
                }
                AppEvent::SettingsWritten(generation, result) => {
                    self.settings_written(generation, result);
                }
                AppEvent::StatsReady(data) => self.stats_ready(data),
                AppEvent::AppleImported(Ok(result)) => {
                    self.info_toast(self.lang.apple_import_result(
                        result.matched,
                        result.unmatched,
                        result.playlists,
                    ));
                    self.playlists_generation = self.core.library.playlists_generation();
                    self.playlists = self.core.library.playlists();
                }
                AppEvent::AppleImported(Err(error)) => {
                    tracing::warn!("Apple Music import failed: {error}");
                    self.warn_toast(self.s().notice_apple_import_failed);
                }
                AppEvent::DataExported(Some(written)) => {
                    self.info_toast(self.lang.data_exported(written.songs, written.playlists));
                }
                AppEvent::DataExported(None) => {
                    self.warn_toast(self.s().notice_data_export_failed);
                }
                AppEvent::DataImported(Ok(result)) => {
                    let searches = self.take_in_searches(&result.searches);
                    self.info_toast(self.lang.data_import_result(
                        result.matched,
                        result.unmatched,
                        result.playlists,
                        searches,
                    ));
                    self.playlists_generation = self.core.library.playlists_generation();
                    self.playlists = self.core.library.playlists();
                }
                AppEvent::DataImported(Err(error)) => {
                    tracing::warn!("library data import failed: {error}");
                    self.warn_toast(if crate::library::backup::is_of_a_later_version(&error) {
                        self.s().notice_data_import_newer
                    } else {
                        self.s().notice_data_import_failed
                    });
                }
            }
        }
        let opened = app_hooks::take_opened_paths();
        if !opened.is_empty() {
            self.open_paths(opened);
        }
        let requests: Vec<UiRequest> = self
            .ui_requests
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default();
        for request in requests {
            self.handle_request(request);
        }
        // Closing the window hides it; the app keeps running and playing (docs/UX.md).
        // Where there is no Dock to bring it back from, closing the window quits.
        if ctx.input(|i| i.viewport().close_requested()) {
            self.persist_settings();
            if crate::platform::CLOSE_HIDES_WINDOW {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                window::hide_main_window();
            }
        }
    }

    /// The part of the per-frame bookkeeping that needs a visible frame's input.
    fn sync_frame(&mut self, ctx: &egui::Context) {
        self.art.begin_frame(ctx);
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if !dropped.is_empty() {
            self.open_paths(dropped);
        }
        if self
            .toast
            .as_ref()
            .is_some_and(|t| t.expires.is_some_and(|e| Instant::now() >= e))
        {
            self.toast = None;
        }
        if self.settings.right_panel != RightPanel::Queue {
            self.key_pane = KeyPane::List;
        }
    }

    /// Tracks the menu's "Song" commands act on: the selection of the list that has the
    /// keys, in display order, and the row the keyboard cursor is on.
    fn selected_tracks(&self) -> (Vec<TrackId>, Option<TrackId>) {
        if self.key_pane == KeyPane::Queue {
            let tracks: Vec<TrackId> = self
                .player
                .queue
                .iter()
                .filter(|q| self.queue_selection.contains(&q.id))
                .map(|q| q.track)
                .collect();
            let focus = self
                .queue_anchor
                .and_then(|a| self.player.queue.iter().find(|q| q.id == a))
                .map(|q| q.track)
                .or(tracks.first().copied());
            return (tracks, focus);
        }
        if matches!(
            self.nav,
            Nav::Albums | Nav::Artists | Nav::Artist(_) | Nav::Genres
        ) {
            return (Vec::new(), None);
        }
        let Some(state) = self.views.get(&self.nav) else {
            return (Vec::new(), None);
        };
        let id_of = |row: &Row| self.snapshot.tracks.get(row.idx as usize).map(|t| t.id);
        let tracks: Vec<TrackId> = state
            .rows
            .iter()
            .filter(|r| state.selection.contains(&r.key))
            .filter_map(id_of)
            .collect();
        let focus = state
            .cursor
            .or(state.anchor)
            .and_then(|key| state.rows.iter().find(|r| r.key == key))
            .and_then(id_of)
            .or(tracks.first().copied());
        (tracks, focus)
    }

    /// A "Song" menu command. Without a selection it applies to the playing track.
    fn song_command(&mut self, request: UiRequest) {
        let (mut tracks, mut focus) = self.selected_tracks();
        if tracks.is_empty()
            && let Some(current) = self.player.current
        {
            tracks.push(current.track_id);
            focus = Some(current.track_id);
        }
        let Some(focus) = focus.or(tracks.first().copied()) else {
            return;
        };
        let action = match request {
            UiRequest::SongPlay => Action::PlayTracks(tracks, 0),
            UiRequest::SongPlayNext => Action::PlayNext(tracks),
            UiRequest::SongAddToQueue => Action::Enqueue(tracks),
            UiRequest::SongToggleFavorite => {
                let favorite = self.snapshot.get(focus).is_some_and(|t| t.favorite);
                Action::SetFavorite(tracks, !favorite)
            }
            UiRequest::SongShowInFinder => Action::Reveal(focus),
            UiRequest::SongInfo => Action::ShowInfo(focus),
            _ => return,
        };
        self.apply(action);
    }

    /// Undoes "remove from playlist": the playlist gets its previous contents back, in
    /// one step (if that fails, it keeps what it has).
    fn restore_playlist(&mut self, id: i64, tracks: Vec<TrackId>) {
        self.edit(move |library| {
            library
                .replace_playlist_items(id, &tracks)
                .err()
                .map(AppEvent::EditFailed)
        });
    }

    /// Runs a library edit on the edit thread (see `background.rs`), after the ones
    /// before it. The event it returns, if any, is handled in a later frame.
    fn edit(&self, job: impl FnOnce(&Library) -> Option<AppEvent> + Send + 'static) {
        let library = self.core.library.clone();
        let core = self.core.clone();
        let events = self.events_tx.clone();
        self.edits.run(move || {
            if let Some(event) = job(&library) {
                let _ = events.send(event);
                core.request_repaint();
            }
        });
    }

    /// A failed library edit: what could not be done, or that it could not be saved.
    fn edit_error_toast(&mut self, error: &LibraryError) {
        match error {
            LibraryError::Db(_) | LibraryError::Io(_) => {
                tracing::warn!("a library edit could not be saved: {error}");
                self.warn_toast(self.s().notice_not_saved);
            }
            _ => self.library_error_toast(error),
        }
    }

    fn toggle_mute(&mut self) {
        let volume = self.player.volume;
        if volume > 0.001 {
            self.settings.volume_before_mute = Some(volume);
            self.core.player.send(Command::SetVolume(0.0));
        } else {
            let restore = self
                .settings
                .volume_before_mute
                .filter(|v| v.is_finite() && *v > 0.001)
                .unwrap_or(0.8);
            self.core.player.send(Command::SetVolume(restore.min(1.0)));
        }
    }

    fn handle_request(&mut self, request: UiRequest) {
        if self.phone_mode {
            return self.phone_request(request);
        }
        match request {
            UiRequest::Settings => self.open_settings(),
            UiRequest::OpenFiles => self.pick_files(),
            UiRequest::AddFolder => self.pick_folder(),
            UiRequest::NewPlaylist => {
                self.dialog = Some(Dialog::new_playlist(self.s().untitled_playlist, Vec::new()))
            }
            UiRequest::ImportPlaylist => self.pick_playlist_to_import(),
            UiRequest::Find => self.focus_search = true,
            UiRequest::GoToCurrent => self.go_to_current(),
            UiRequest::ToggleQueue => self.toggle_right_panel(RightPanel::Queue),
            UiRequest::ToggleInfo => self.toggle_right_panel(RightPanel::Info),
            UiRequest::ToggleMeter => self.settings.show_meter = !self.settings.show_meter,
            UiRequest::MiniPlayer => self.mini_toggle = true,
            UiRequest::UserGuide => self.open_guide(),
            UiRequest::Equalizer => {
                if self.eq_open {
                    self.eq_open = false;
                } else {
                    self.open_equalizer();
                }
            }
            UiRequest::ToggleMute => self.toggle_mute(),
            UiRequest::PlaylistRename | UiRequest::PlaylistExport | UiRequest::PlaylistDelete => {
                // The menu bar's way to what the playlist's right-click menu offers.
                let open = match self.nav {
                    Nav::Playlist(id) => self
                        .playlists
                        .iter()
                        .find(|p| p.id == id)
                        .map(|p| (id, p.name.clone())),
                    _ => None,
                };
                match (open, request) {
                    (None, _) => self.info_toast(self.s().notice_open_playlist_first),
                    (Some((id, name)), UiRequest::PlaylistRename) => {
                        self.renaming = Some((id, name, true));
                    }
                    (Some((id, name)), UiRequest::PlaylistExport) => {
                        self.export_playlist(id, &name);
                    }
                    (Some((id, name)), _) => {
                        self.dialog = Some(Dialog::DeletePlaylist(id, name));
                    }
                }
            }
            UiRequest::SongPlay
            | UiRequest::SongPlayNext
            | UiRequest::SongAddToQueue
            | UiRequest::SongToggleFavorite
            | UiRequest::SongShowInFinder
            | UiRequest::SongInfo => self.song_command(request),
        }
    }

    fn toggle_right_panel(&mut self, panel: RightPanel) {
        self.settings.right_panel = if self.settings.right_panel == panel {
            RightPanel::Hidden
        } else {
            panel
        };
        if panel == RightPanel::Queue {
            self.queue_scroll_to_current = true;
            self.queue_scroll_forced = true;
        }
    }

    // ----- navigation -----------------------------------------------------------------

    /// Opens `nav` with `query` in its search field.
    fn show_search(&mut self, nav: Nav, query: &str) {
        self.go(nav.clone());
        self.views.entry(nav).or_default().query = query.to_string();
    }

    fn go(&mut self, nav: Nav) {
        if nav == self.nav {
            return;
        }
        self.key_pane = KeyPane::List;
        self.info_track = None;
        let detail = matches!(nav, Nav::Album(_) | Nav::Artist(_) | Nav::Genre(_));
        // A smart playlist that is gone (renamed, deleted) has nothing to show.
        if let Nav::Smart(name) = &nav
            && !self
                .settings
                .smart_playlists
                .iter()
                .any(|p| &p.name == name)
        {
            return;
        }
        if detail {
            self.history.push(self.nav.clone());
        } else {
            self.history.clear();
        }
        self.nav = nav;
    }

    fn go_back(&mut self) {
        while let Some(previous) = self.history.pop() {
            // A smart playlist renamed or deleted since is passed over: going back to
            // it would show an empty list under a name that is no longer there.
            if let Nav::Smart(name) = &previous
                && !self
                    .settings
                    .smart_playlists
                    .iter()
                    .any(|p| &p.name == name)
            {
                continue;
            }
            self.nav = previous;
            return;
        }
    }

    /// Shows the playing track in the song list.
    fn go_to_current(&mut self) {
        let Some(current) = self.player.current else {
            return;
        };
        self.history.clear();
        self.nav = Nav::Songs;
        let state = self.views.entry(Nav::Songs).or_default();
        state.query.clear();
        state.built = None;
        state.selection = HashSet::from([current.track_id as u64]);
        state.anchor = Some(current.track_id as u64);
        state.cursor = Some(current.track_id as u64);
        self.key_pane = KeyPane::List;
        // The row index is resolved when the rows are rebuilt.
        state.scroll_to = Some(usize::MAX);
    }

    // ----- toasts ---------------------------------------------------------------------

    fn info_toast(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast {
            text: text.into(),
            warning: false,
            action: None,
            expires: Some(Instant::now() + Duration::from_secs(6)),
        });
    }

    fn warn_toast(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast {
            text: text.into(),
            warning: true,
            action: None,
            expires: Some(Instant::now() + Duration::from_secs(10)),
        });
    }

    fn show_notice(&mut self, notice: &Notice) {
        let s = self.s();
        match notice {
            Notice::TrackSkipped { track_id, kind } => {
                let title = self
                    .snapshot
                    .get(*track_id)
                    .map_or_else(|| "?".to_string(), |t| t.title.clone());
                let reason = match kind {
                    PlaybackErrorKind::FileMissing => s.skip_missing,
                    PlaybackErrorKind::FileUnreadable => s.skip_unreadable,
                    PlaybackErrorKind::Unsupported => s.skip_unsupported,
                    PlaybackErrorKind::Corrupt => s.skip_corrupt,
                };
                self.warn_toast(self.lang.skipped(&title, reason));
            }
            Notice::TooManyFailures => {
                self.alert = Some(Toast {
                    text: s.notice_too_many_failures.into(),
                    warning: true,
                    action: None,
                    expires: None,
                });
            }
            Notice::DeviceLost => {
                self.alert = Some(Toast {
                    text: s.notice_device_lost.into(),
                    warning: true,
                    action: Some((s.notice_device_lost_action.into(), ToastAction::RetryOutput)),
                    expires: None,
                });
            }
            Notice::OutputUnavailable(error) => {
                let selected_missing = matches!(error, crate::audio::OutputError::DeviceNotFound)
                    && self.settings.output_device.is_some();
                let (text, action) = if selected_missing {
                    (
                        s.notice_selected_device_missing,
                        (s.notice_open_settings, ToastAction::OpenPlaybackSettings),
                    )
                } else {
                    (
                        s.notice_no_output,
                        (s.notice_retry, ToastAction::RetryOutput),
                    )
                };
                self.alert = Some(Toast {
                    text: text.into(),
                    warning: true,
                    action: Some((action.0.into(), action.1)),
                    expires: None,
                });
            }
        }
    }

    fn library_error_toast(&mut self, error: &LibraryError) {
        let s = self.s();
        tracing::warn!("library operation failed: {error}");
        self.warn_toast(match error {
            LibraryError::AlreadyAdded => s.notice_folder_already_added,
            LibraryError::InsideExistingFolder(_) => s.notice_folder_inside_existing,
            LibraryError::ContainsExistingFolder(_) => s.notice_folder_contains_existing,
            LibraryError::NotADirectory => s.notice_not_a_folder,
            _ => s.notice_generic_error,
        });
    }

    // ----- files ----------------------------------------------------------------------

    /// Adds a music folder. The folder is looked at and written to the database on the
    /// edit thread (a drive that is slow to answer, a database that is busy): what came
    /// of it is told in a later frame.
    fn add_folder(&mut self, path: PathBuf) {
        self.edit(move |library| {
            Some(match library.add_folder(&path) {
                Ok(_) => AppEvent::FolderAdded(path),
                Err(error) => {
                    // On a phone the folder was linked as it was chosen: a link that
                    // nothing uses is dropped (not the one of a folder that is in the
                    // library already).
                    if !matches!(error, crate::library::LibraryError::AlreadyAdded) {
                        files::forget_linked_folder(&path);
                    }
                    AppEvent::EditFailed(error)
                }
            })
        });
    }

    /// Shows a file dialog. What the user chooses arrives as [`AppEvent::Picked`] (at
    /// once where the dialog blocks, later on an iPhone); nothing arrives if they cancel.
    fn pick(&self, purpose: PickFor, request: PickRequest) {
        let (events, core) = (self.events_tx.clone(), self.core.clone());
        files::pick(request, move |paths| {
            let _ = events.send(AppEvent::Picked(purpose, paths));
            core.request_repaint();
        });
    }

    /// What was chosen in a file dialog shown by [`Self::pick`].
    fn picked(&mut self, purpose: PickFor, mut paths: Vec<PathBuf>) {
        match purpose {
            // On a phone what was chosen has been copied into the app's own music
            // folder, which is in the library already: it only has to be read.
            PickFor::ImportMusic if crate::platform::PHONE => {
                if paths.is_empty() {
                    self.warn_toast(self.s().phone_import_failed);
                } else {
                    self.announce_scan = true;
                    self.core.library.rescan();
                }
            }
            // The phone screens on a desk: music is used where it is.
            PickFor::ImportMusic => self.open_paths(paths),
            PickFor::MusicFolder => {
                // On a phone the folder was linked as it was chosen: nothing comes
                // back if the system did not let it be kept.
                if paths.is_empty() && crate::platform::PHONE {
                    self.warn_toast(self.s().phone_link_failed);
                }
                for folder in paths {
                    self.add_folder(folder);
                }
            }
            PickFor::Open => self.open_paths(paths),
            PickFor::Correction => {
                if let Some(file) = paths.pop() {
                    self.import_correction(&file);
                }
            }
            PickFor::Room => {
                if let Some(file) = paths.pop() {
                    self.import_room(&file);
                }
            }
            PickFor::Playlist => {
                if let Some(file) = paths.pop() {
                    self.import_playlist(file);
                }
            }
            PickFor::AppleLibrary => {
                if let Some(file) = paths.pop() {
                    self.import_apple_library(file);
                }
            }
            PickFor::ExportData => {
                if let Some(dest) = paths.pop() {
                    self.write_library_data(dest, true);
                }
            }
            PickFor::ImportData => {
                if let Some(file) = paths.pop() {
                    self.import_library_data(file);
                }
            }
            PickFor::ExportPlaylist(id) => {
                if let Some(dest) = paths.pop() {
                    self.write_playlist(id, dest);
                }
            }
            PickFor::SaveLog => {
                if let Some(dest) = paths.pop() {
                    self.write_log(dest);
                }
            }
        }
    }

    fn pick_folder(&mut self) {
        let request = PickRequest::new(PickKind::Folder).title(self.s().dialog_choose_folder);
        self.pick(PickFor::MusicFolder, request);
    }

    /// On a phone: asks for music files, or for a folder of music, to bring into the
    /// app's folder.
    fn import_music(&mut self, kind: PickKind) {
        self.pick(PickFor::ImportMusic, PickRequest::new(kind));
    }

    /// On a phone: asks for a folder of music to use where it is (in iCloud Drive, on
    /// a drive that is plugged in), without a copy.
    fn link_folder(&mut self) {
        self.pick(
            PickFor::MusicFolder,
            PickRequest::new(PickKind::FolderInPlace),
        );
    }

    fn pick_files(&mut self) {
        let s = self.s();
        let request = PickRequest::new(PickKind::Files)
            .title(s.dialog_choose_files)
            .filter(s.dialog_filter_audio, AUDIO_EXTENSIONS)
            .filter(s.dialog_filter_playlist, PLAYLIST_EXTENSIONS);
        self.pick(PickFor::Open, request);
    }

    /// Asks for a correction file (Equalizer APO format) and imports it.
    fn pick_correction_file(&mut self) {
        let s = self.s();
        let request = PickRequest::new(PickKind::File)
            .title(s.correction_choose)
            .filter(s.correction_filter_text, &["txt"]);
        self.pick(PickFor::Correction, request);
    }

    /// Asks for the impulse response of a room correction and imports it.
    pub(super) fn pick_room_file(&mut self) {
        let s = self.s();
        let request = PickRequest::new(PickKind::File)
            .title(s.room_choose)
            .filter(s.room_filter_text, &["wav", "flac", "aiff", "aif", "wv"]);
        self.pick(PickFor::Room, request);
    }

    /// Reads a sound file as the impulse response of a room correction, keeps it in
    /// a file of the app's own and applies it. (Read here, on the window's thread:
    /// such a file is a second or two of sound.)
    pub(super) fn import_room(&mut self, file: &Path) {
        use crate::audio::room::{Impulse, Unusable};
        let s = self.s();
        let impulse = match Impulse::load(file) {
            Ok(impulse) => impulse,
            Err(Unusable::TooLong) => return self.warn_toast(s.room_err_long),
            Err(Unusable::Unreadable(_) | Unusable::Silent) => {
                return self.warn_toast(s.room_err_read);
            }
        };
        if let Err(error) = impulse.save(&self.core.paths.room_file()) {
            tracing::warn!("the room correction could not be kept: {error}");
            return self.warn_toast(s.room_err_keep);
        }
        self.settings.room = Some(app::settings::RoomSetting {
            name: impulse.name.clone(),
            enabled: true,
        });
        self.room_trim = Some(impulse.turned_down());
        self.core
            .player
            .send(Command::SetRoom(Some(Arc::new(impulse))));
    }

    /// Switches the room correction that was imported on or off.
    pub(super) fn set_room_enabled(&mut self, on: bool) {
        let Some(room) = self.settings.room.as_mut() else {
            return;
        };
        room.enabled = on;
        let impulse = on
            .then(|| crate::audio::room::Impulse::open(&self.core.paths.room_file()))
            .flatten();
        if on && impulse.is_none() {
            // What was kept is gone or is no longer what was written: nothing to apply.
            self.settings.room = None;
            self.room_trim = None;
            return self.warn_toast(self.s().room_err_read);
        }
        self.room_trim = impulse.as_ref().map(|impulse| impulse.turned_down());
        self.core
            .player
            .send(Command::SetRoom(impulse.map(Arc::new)));
    }

    /// Takes the room correction away, with the file it is kept in.
    pub(super) fn remove_room(&mut self) {
        if self.settings.room.take().is_some() {
            self.room_trim = None;
            let _ = std::fs::remove_file(self.core.paths.room_file());
            self.core.player.send(Command::SetRoom(None));
        }
    }

    /// Reads a correction file, keeps it as a profile named after the file and applies
    /// it. A file that is not understood in full is not imported at all.
    fn import_correction(&mut self, file: &Path) {
        use crate::audio::{PeqError, PeqProfile, peq};
        use std::io::Read;

        let s = self.s();
        // AutoEq names its files "<headphone> ParametricEQ.txt".
        let stem = file.file_stem().map(|n| n.to_string_lossy().into_owned());
        let name = stem
            .as_deref()
            .map(|n| n.trim_end_matches("ParametricEQ").trim())
            .filter(|n| !n.is_empty())
            .unwrap_or(s.correction)
            .to_string();
        // Opened once and read with a limit: the file is whatever the user picked.
        let mut bytes = Vec::new();
        let read = std::fs::File::open(file).and_then(|f| {
            if !f.metadata()?.is_file() {
                return Err(std::io::ErrorKind::InvalidInput.into());
            }
            f.take(peq::MAX_FILE_BYTES + 1).read_to_end(&mut bytes)
        });
        if read.is_err() || bytes.len() as u64 > peq::MAX_FILE_BYTES {
            self.warn_toast(s.correction_err_read);
            return;
        }
        let text = String::from_utf8_lossy(&bytes);
        let profile = match PeqProfile::parse(&name, &text) {
            Ok(profile) if !profile.name.is_empty() => profile,
            Ok(_) | Err(PeqError::Empty) => return self.warn_toast(s.correction_err_empty),
            Err(PeqError::TooManyFilters) => return self.warn_toast(s.correction_err_many),
            Err(PeqError::UnsupportedFilter(kind)) => {
                return self.warn_toast(self.lang.correction_unsupported(&kind));
            }
            Err(PeqError::Malformed(line)) => {
                return self.warn_toast(self.lang.correction_malformed(&line));
            }
        };
        // Imported again under the same name: the new values replace the old.
        let list = &mut self.settings.corrections;
        list.retain(|p| p.name != profile.name);
        if list.len() >= crate::app::settings::MAX_CORRECTIONS {
            list.remove(0);
        }
        list.push(profile.clone());
        self.settings.correction = Some(profile.name.clone());
        self.info_toast(self.lang.correction_imported(&profile.name));
        self.core.player.send(Command::SetCorrection(Some(profile)));
    }

    fn pick_playlist_to_import(&mut self) {
        let request = PickRequest::new(PickKind::File)
            .filter(self.s().dialog_filter_playlist, PLAYLIST_EXTENSIONS);
        self.pick(PickFor::Playlist, request);
    }

    /// Imports an M3U/M3U8 file as a playlist. Entries not yet in the library have their
    /// tags read, which can take a while on a slow drive, so this runs off the UI thread;
    /// the result arrives as an [`AppEvent`].
    fn import_playlist(&mut self, file: PathBuf) {
        let library = self.core.library.clone();
        let tx = self.events_tx.clone();
        let core = self.core.clone();
        let spawned = std::thread::Builder::new()
            .name("oneum-import".into())
            .spawn(move || {
                let _ = tx.send(AppEvent::PlaylistImported(library.import_m3u8(&file)));
                core.request_repaint();
            });
        if spawned.is_err() {
            self.warn_toast(self.s().notice_import_failed);
        }
    }

    /// Asks for an Apple Music library export.
    fn pick_apple_library(&mut self) {
        let request =
            PickRequest::new(PickKind::File).filter(self.s().dialog_filter_apple, &["xml"]);
        self.pick(PickFor::AppleLibrary, request);
    }

    /// Takes over an Apple Music library export, off the UI thread.
    fn import_apple_library(&mut self, file: PathBuf) {
        // The export names files where they are on the Mac it was made on. On a phone
        // the music is a copy of them: songs are found by the end of their path there.
        let elsewhere = self.phone_mode;
        let library = self.core.library.clone();
        let tx = self.events_tx.clone();
        let core = self.core.clone();
        let spawned = std::thread::Builder::new()
            .name("oneum-apple-import".into())
            .spawn(move || {
                let _ = tx.send(AppEvent::AppleImported(
                    library.import_apple_music_from(&file, elsewhere),
                ));
                core.request_repaint();
            });
        if spawned.is_err() {
            self.warn_toast(self.s().notice_apple_import_failed);
        }
    }

    /// The marked section of the current song in words, for the player bar and the
    /// phone's player: where it begins while only that is marked, and both ends once
    /// it is repeated.
    pub(super) fn section_words(&self) -> Option<String> {
        let section = self.player.section?;
        let from = crate::i18n::format_time(section.from_ms);
        Some(match section.to_ms {
            Some(to) => self
                .lang
                .section_repeating(&from, &crate::i18n::format_time(to)),
            None => self.lang.section_begun(&from),
        })
    }

    /// Begins making a correction profile on screen: from the profile that is selected
    /// (`from_selected`; one of the user's own is changed in place, one that comes with
    /// the app is the pattern of a new one), or from nothing but one flat band.
    pub(super) fn peq_edit_begin(&mut self, from_selected: bool) {
        use crate::audio::{PeqFilter, PeqKind, PeqProfile};
        let s = self.s();
        let own = self.settings.correction_is_imported();
        let pattern = self
            .settings
            .active_correction()
            .filter(|_| from_selected)
            .cloned();
        let (original, profile) = match pattern {
            Some(profile) if own => (Some(profile.name.clone()), profile),
            Some(profile) => {
                let name = format!("{} ({})", profile.name, s.correction_copy_mark);
                (None, PeqProfile { name, ..profile })
            }
            None => (
                None,
                PeqProfile {
                    name: s.correction_new_name.to_string(),
                    preamp_db: 0.0,
                    filters: vec![PeqFilter {
                        kind: PeqKind::Peak,
                        hz: 1_000.0,
                        gain_db: 0.0,
                        q: 1.0,
                    }],
                },
            ),
        };
        self.peq_edit = Some(PeqEdit {
            original,
            profile,
            selected_before: self.settings.correction.clone(),
            heard: None,
            shown: false,
        });
        self.peq_edit_listen();
    }

    /// What is being edited is what is heard: sent to the player when it changed.
    pub(super) fn peq_edit_listen(&mut self) {
        let Some(edit) = self.peq_edit.as_mut() else {
            return;
        };
        let now = edit.profile.clone().sanitized();
        if edit.heard.as_ref() != Some(&now) {
            edit.heard = Some(now.clone());
            self.core.player.send(Command::SetCorrection(Some(now)));
        }
    }

    /// Keeps the profile that was edited among the user's own, and selects it. One
    /// that was changed is replaced, and so is another of the name it is saved under
    /// (as a profile that is imported again replaces the one of its name).
    pub(super) fn peq_edit_save(&mut self) {
        let Some(edit) = self.peq_edit.take() else {
            return;
        };
        let mut profile = edit.profile.sanitized();
        if profile.name.is_empty() {
            profile.name = self.s().correction_new_name.to_string();
        }
        let list = &mut self.settings.corrections;
        list.retain(|p| p.name != profile.name && Some(&p.name) != edit.original.as_ref());
        if list.len() >= app::settings::MAX_CORRECTIONS {
            list.remove(0);
        }
        list.push(profile.clone());
        self.settings.correction = Some(profile.name.clone());
        self.info_toast(self.lang.correction_saved(&profile.name));
        self.core.player.send(Command::SetCorrection(Some(profile)));
    }

    /// Gives up what was edited: the profile that was selected before is heard again.
    pub(super) fn peq_edit_cancel(&mut self) {
        let Some(edit) = self.peq_edit.take() else {
            return;
        };
        self.settings.correction = edit.selected_before;
        self.core.player.send(Command::SetCorrection(
            self.settings.active_correction().cloned(),
        ));
    }

    /// Switches between the full window and the mini player: the window made as small
    /// as the player bar, which is all it shows then, and kept above other windows.
    fn toggle_mini(&mut self, ctx: &egui::Context) {
        use egui::ViewportCommand as Tell;
        match self.mini.take() {
            None => {
                let full = ctx
                    .input(|i| i.viewport().inner_rect)
                    .map_or(egui::vec2(1180.0, 760.0), |rect| rect.size());
                self.mini = Some(full);
                // What only the full window shows is put away.
                self.settings_open = false;
                self.eq_open = false;
                self.guide_open = false;
                self.dialog = None;
                self.folder_form = None;
                let size = egui::vec2(MINI_WIDTH, theme::PLAYER_BAR_HEIGHT);
                ctx.send_viewport_cmd(Tell::MinInnerSize(size));
                ctx.send_viewport_cmd(Tell::InnerSize(size));
                ctx.send_viewport_cmd(Tell::WindowLevel(egui::WindowLevel::AlwaysOnTop));
            }
            Some(full) => {
                ctx.send_viewport_cmd(Tell::WindowLevel(egui::WindowLevel::Normal));
                ctx.send_viewport_cmd(Tell::MinInnerSize(egui::vec2(760.0, 480.0)));
                ctx.send_viewport_cmd(Tell::InnerSize(full));
            }
        }
    }

    /// What a rating is called in a menu ("3 Stars"; "No Rating" for 0).
    pub(super) fn rating_words(&self, stars: u8) -> &'static str {
        let s = self.s();
        match stars {
            0 => s.rating_none,
            1 => s.rating_1,
            2 => s.rating_2,
            3 => s.rating_3,
            4 => s.rating_4,
            _ => s.rating_5,
        }
    }

    /// What the next step of marking a section does, as a menu says it.
    pub(super) fn section_step(&self) -> &'static str {
        match self.player.section {
            None => self.s().section_mark_start,
            Some(section) if section.to_ms.is_none() => self.s().section_mark_end,
            Some(_) => self.s().section_off,
        }
    }

    /// Asks where the library's own data is to be written (see `library::backup`).
    pub(super) fn pick_data_export(&mut self) {
        let request = PickRequest::new(PickKind::SaveAs(self.s().data_file_name.into()))
            .filter(self.s().dialog_filter_data, &["json"]);
        self.pick(PickFor::ExportData, request);
    }

    /// Asks for a file of library data to take in.
    pub(super) fn pick_data_import(&mut self) {
        let request =
            PickRequest::new(PickKind::File).filter(self.s().dialog_filter_data, &["json"]);
        self.pick(PickFor::ImportData, request);
    }

    /// Writes the library's own data to `dest`, with the saved searches the settings
    /// keep. On the edit thread: the destination may be slow to answer. With `hand_out`
    /// the written file is offered to the user where the system has them choose its
    /// place afterwards (a phone); a verification run reads it where it was written.
    fn write_library_data(&mut self, dest: PathBuf, hand_out: bool) {
        let searches: Vec<crate::library::backup::Search> = self
            .settings
            .smart_playlists
            .iter()
            .map(|search| crate::library::backup::Search {
                name: search.name.clone(),
                query: search.query.clone(),
            })
            .collect();
        self.edit(move |library| {
            let written = library
                .export_data(&dest, &searches)
                .inspect_err(|error| tracing::warn!("library data export failed: {error}"))
                .ok();
            if written.is_some() && hand_out {
                files::hand_out(&dest);
            }
            Some(AppEvent::DataExported(written))
        });
    }

    /// Takes in library data from `file`, on the edit thread.
    fn import_library_data(&mut self, file: PathBuf) {
        self.edit(move |library| Some(AppEvent::DataImported(library.import_data(&file))));
    }

    /// The saved searches that came with library data join those of the settings: one
    /// that is here already (same name, same search) is passed over, and a name that
    /// stands for another search here gets a mark. Returns how many were taken in.
    fn take_in_searches(&mut self, searches: &[crate::library::backup::Search]) -> usize {
        let mut taken = 0;
        for search in searches {
            let list = &mut self.settings.smart_playlists;
            // (As the settings keep them: one line, of a length that is kept.)
            let wanted =
                app::settings::tidy_text(&search.name, app::settings::MAX_SMART_NAME_CHARS);
            let query =
                app::settings::tidy_text(&search.query, app::settings::MAX_SMART_QUERY_CHARS);
            if wanted.is_empty() || query.is_empty() {
                continue;
            }
            // The search is here already, under any name: passed over.
            if list.iter().any(|own| own.query == query) {
                continue;
            }
            if list.len() >= app::settings::MAX_SMART_PLAYLISTS {
                break;
            }
            // A name that stands for another search here gets a number, the first
            // that is free ("Rock (2)", "Rock (3)").
            let mut name = wanted.clone();
            let mut number = 1u32;
            while list.iter().any(|own| own.name == name) {
                number += 1;
                name = format!("{wanted} ({number})");
            }
            list.push(app::settings::SmartPlaylist { name, query });
            taken += 1;
        }
        taken
    }

    fn export_playlist(&mut self, id: i64, name: &str) {
        let request = PickRequest::new(PickKind::SaveAs(format!("{name}.m3u8")))
            .filter(self.s().dialog_filter_playlist, &["m3u8"]);
        self.pick(PickFor::ExportPlaylist(id), request);
    }

    /// What earlier runs left of their errors, a few lines at most: how the system says
    /// they ended when that was not the listener's doing (Android: `exits.txt`, see
    /// `MainActivity.recordLastExits`), the app's own last panics from its log, and the
    /// last exception of the Java side. Shown on the About page of a phone, where
    /// nobody can look into the app's folders: it is what to pass on with a report.
    pub(super) fn past_errors(&mut self) -> &[String] {
        let log_dir = self.core.paths.log_dir.clone();
        self.past_errors.get_or_insert_with(|| {
            let short = |line: &str| line.chars().take(400).collect::<String>();
            let mut lines = Vec::new();
            if let Ok(text) = std::fs::read_to_string(log_dir.join("exits.txt")) {
                lines.extend(bad_exits(&text).into_iter().map(short));
            }
            if let Ok(bytes) = std::fs::read(log_dir.join("oneum.log")) {
                let text = String::from_utf8_lossy(&bytes);
                // What the app said of itself: a panic, or the graphics device failing.
                let said: Vec<&str> = text
                    .lines()
                    .filter(|l| l.contains("panic:") || l.contains(" ERROR graphics:"))
                    .collect();
                lines.extend(said.iter().rev().take(4).rev().map(|line| short(line)));
                lines.extend(memory_of_earlier_runs(&text).into_iter().map(short));
            }
            if let Ok(text) = std::fs::read_to_string(log_dir.join("crash-java.txt")) {
                // Of the last record: when, and what was thrown (not where from).
                let heads: Vec<&str> = text
                    .lines()
                    .filter(|l| !l.starts_with('\t') && !l.trim().is_empty())
                    .collect();
                lines.extend(heads.iter().rev().take(2).rev().map(|line| short(line)));
            }
            lines
        })
    }

    /// Puts `text` on the clipboard and says so.
    pub(super) fn copy_text(&mut self, ctx: &egui::Context, text: String) {
        if !crate::platform::copy_to_clipboard(&text) {
            ctx.copy_text(text);
        }
        self.info_toast(self.s().copied);
    }

    /// What is passed on when something went wrong, as one text for the clipboard:
    /// which app and what it draws with, how earlier runs ended (with what the system
    /// kept of a crash), what the log says of warnings, errors, the graphics and the
    /// memory, what the Java side wrote down, and the memory as it is now. More than
    /// the About page shows: a screen has room for a few lines, the clipboard for all.
    pub(super) fn error_report(&self) -> String {
        let log_dir = &self.core.paths.log_dir;
        let read = |name: &str| {
            std::fs::read(log_dir.join(name))
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default()
        };
        let mut about = vec![format!(
            "OnEum {} ({}), built {}",
            app::VERSION,
            std::env::consts::OS,
            app::BUILT
        )];
        if let Some(adapter) = renderer::adapter() {
            about.push(format!("drawn with: {adapter}"));
        }
        about.push(format!("graphics setting: {:?}", self.settings.graphics));
        if let Some(note) = app::relaunch::read(&self.core.paths.data_dir) {
            about.push(format!(
                "last start anew: at {} (seconds since 1970), device lost on {}",
                note.at,
                note.lost_on.unwrap_or("-")
            ));
        }
        // The thread that watches has the list of mappings; here only what is cheap.
        let memory = memory::latest().unwrap_or_else(memory::read);
        report_text(
            &about,
            &read("exits.txt"),
            &read("oneum.log"),
            &read("crash-java.txt"),
            &memory.to_string(),
        )
    }

    /// On a phone, where the app's folders cannot be looked into: asks where to keep a
    /// copy of the log.
    pub(super) fn save_log(&mut self) {
        let request = PickRequest::new(PickKind::SaveAs("OnEum-log.txt".into()));
        self.pick(PickFor::SaveLog, request);
    }

    /// Copies the log to the file the user is to keep.
    fn write_log(&mut self, dest: PathBuf) {
        let log = self.core.paths.log_dir.join("oneum.log");
        // Off the UI thread, after the edits before it: the place may be slow to answer.
        self.edit(move |_| {
            let copied = std::fs::read(&log).and_then(|mut text| {
                // Android: how earlier runs ended as the system tells it, and what the
                // Java side wrote down of a crash of its own (see `MainActivity`), go
                // with it.
                for (name, title) in [
                    ("exits.txt", "how earlier runs ended"),
                    ("crash-java.txt", "crashes of the Java side"),
                ] {
                    if let Ok(more) = std::fs::read(log.with_file_name(name)) {
                        text.extend_from_slice(format!("\n--- {title} ---\n").as_bytes());
                        text.extend_from_slice(&more);
                    }
                }
                std::fs::write(&dest, text)
            });
            match copied {
                Ok(()) => {
                    files::hand_out(&dest);
                    None
                }
                Err(error) => {
                    tracing::warn!("the log could not be copied: {error}");
                    Some(AppEvent::LogNotSaved)
                }
            }
        });
    }

    /// Shows `path` in the file manager. A phone's shows folders of shared storage
    /// only (the one a song is in): the user is told when it cannot show this one.
    pub(super) fn reveal_path(&mut self, path: &Path) {
        if !finder::reveal(path) && crate::platform::PHONE_FILE_MANAGER {
            self.warn_toast(self.s().notice_cannot_show_folder);
        }
    }

    /// Writes a playlist to the file the user chose for it.
    fn write_playlist(&mut self, id: i64, dest: PathBuf) {
        // Written on the edit thread: the destination may be slow to answer.
        self.edit(move |library| {
            let written = library
                .export_m3u8(id, &dest)
                .inspect_err(|error| tracing::warn!("playlist export failed: {error}"))
                .ok();
            if written.is_some() {
                files::hand_out(&dest);
            }
            Some(AppEvent::PlaylistExported(written))
        });
    }

    /// Handles paths from Finder, the Dock, drag-and-drop or the open panel: folders
    /// become music folders, playlists are imported, audio files are played.
    fn open_paths(&mut self, paths: Vec<PathBuf>) {
        let mut files = Vec::new();
        for path in paths {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase)
                .unwrap_or_default();
            if path.is_dir() {
                self.add_folder(path);
            } else if PLAYLIST_EXTENSIONS.contains(&ext.as_str()) {
                self.import_playlist(path);
            } else {
                files.push(path);
            }
        }
        if files.is_empty() {
            return;
        }
        // Reading tags touches the disk: do it off the UI thread.
        let library = self.core.library.clone();
        let tx = self.events_tx.clone();
        let core = self.core.clone();
        let spawned = std::thread::Builder::new()
            .name("oneum-open".into())
            .spawn(move || {
                let each = library.import_paths(&files);
                // A copy the app made of a file (Android: one it could not read in
                // place) that turns out to stand for no song is not kept.
                if let Some(copies) = crate::app::paths::AppPaths::imported_copies_dir() {
                    let unused: Vec<PathBuf> = files
                        .iter()
                        .zip(&each)
                        .filter(|(_, tracks)| tracks.is_empty())
                        .map(|(file, _)| file.clone())
                        .collect();
                    crate::app::delete_own_copies(&copies, &unused);
                }
                let ids = each.into_iter().flatten().collect();
                let _ = tx.send(AppEvent::FilesImported(ids));
                core.request_repaint();
            });
        if spawned.is_err() {
            self.warn_toast(self.s().notice_generic_error);
        }
    }

    // ----- actions --------------------------------------------------------------------

    fn apply(&mut self, action: Action) {
        // A media server's track is not in the library: what belongs to the library
        // (favourites, playlists, removal) is not done to it.
        let local = |tracks: Vec<TrackId>| -> Vec<TrackId> {
            tracks
                .into_iter()
                .filter(|id| !Library::is_remote(*id))
                .collect()
        };
        let had_remote = |tracks: &[TrackId]| tracks.iter().any(|id| Library::is_remote(*id));
        let action = match action {
            Action::AddToPlaylist(_, ref tracks)
            | Action::NewPlaylistWith(ref tracks)
            | Action::SetFavorite(ref tracks, _)
            | Action::SetRating(ref tracks, _)
            | Action::RemoveFromLibrary(ref tracks)
                if had_remote(tracks) =>
            {
                let action = match action {
                    Action::AddToPlaylist(id, tracks) => Action::AddToPlaylist(id, local(tracks)),
                    Action::NewPlaylistWith(tracks) => Action::NewPlaylistWith(local(tracks)),
                    Action::SetFavorite(tracks, on) => Action::SetFavorite(local(tracks), on),
                    Action::SetRating(tracks, stars) => Action::SetRating(local(tracks), stars),
                    Action::RemoveFromLibrary(tracks) => Action::RemoveFromLibrary(local(tracks)),
                    other => other,
                };
                let empty = match &action {
                    Action::AddToPlaylist(_, tracks)
                    | Action::NewPlaylistWith(tracks)
                    | Action::SetFavorite(tracks, _)
                    | Action::SetRating(tracks, _)
                    | Action::RemoveFromLibrary(tracks) => tracks.is_empty(),
                    _ => false,
                };
                if empty {
                    self.info_toast(self.s().notice_not_for_network);
                    return;
                }
                action
            }
            other => other,
        };
        match action {
            // "Play" on nothing (an empty playlist) must not stop what is playing.
            Action::PlayTracks(tracks, _) if tracks.is_empty() => {}
            Action::PlayTracks(tracks, start_index) => {
                self.core.player.send(Command::PlayTracks {
                    tracks,
                    start_index,
                });
            }
            Action::PlayNext(tracks) => self.core.player.send(Command::PlayNext(tracks)),
            Action::Enqueue(tracks) => self.core.player.send(Command::Enqueue(tracks)),
            // Library edits wait for the database (a scan holding it, another program
            // locking the file): on the edit thread, never in a frame.
            Action::AddToPlaylist(id, tracks) => self.edit(move |library| {
                Some(match library.add_to_playlist(id, &tracks) {
                    Ok(()) => AppEvent::AddedToPlaylist {
                        id,
                        count: tracks.len(),
                    },
                    Err(error) => AppEvent::EditFailed(error),
                })
            }),
            Action::NewPlaylistWith(tracks) => {
                self.dialog = Some(Dialog::new_playlist(self.s().untitled_playlist, tracks));
            }
            Action::SetFavorite(tracks, favorite) => self.edit(move |library| {
                library
                    .set_favorite(&tracks, favorite)
                    .err()
                    .map(AppEvent::EditFailed)
            }),
            Action::SetRating(tracks, stars) => self.edit(move |library| {
                library
                    .set_rating(&tracks, stars)
                    .err()
                    .map(AppEvent::EditFailed)
            }),
            Action::Reveal(id) => {
                if let Some(track) = self.snapshot.get(id).cloned() {
                    if crate::library::cue::sheet_of(&track.path).is_some() {
                        // Its audio file is found by reading the sheet: not in a frame.
                        let (tx, core, sheet_track) =
                            (self.events_tx.clone(), self.core.clone(), track.clone());
                        let spawned = std::thread::Builder::new()
                            .name("oneum-reveal".into())
                            .spawn(move || {
                                let file = sheet_track.audio_file().into_owned();
                                let _ = tx.send(AppEvent::Reveal(file));
                                core.request_repaint();
                            });
                        if spawned.is_err() {
                            // The sheet, then: it is beside its audio file.
                            self.reveal_path(track.file());
                        }
                    } else {
                        self.reveal_path(&track.path);
                    }
                }
            }
            Action::ShowInfo(id) => {
                self.info_track = Some(id);
                self.settings.right_panel = RightPanel::Info;
            }
            Action::RemoveFromPlaylist(id, items) => self.edit(move |library| {
                // Removing is a single key press, so it must be undoable. What it is
                // undone to is read here, after the edits made before this one.
                let before: Vec<TrackId> = library
                    .playlist_items(id)
                    .into_iter()
                    .map(|i| i.track_id)
                    .collect();
                Some(match library.remove_playlist_items(id, &items) {
                    Ok(()) => AppEvent::RemovedFromPlaylist { id, before },
                    Err(error) => AppEvent::EditFailed(error),
                })
            }),
            Action::MovePlaylistItems(id, items, to_index) => self.edit(move |library| {
                library
                    .move_playlist_items(id, &items, to_index)
                    .err()
                    .map(AppEvent::EditFailed)
            }),
            Action::RemoveFromLibrary(tracks) => {
                // Told once, when the dialog comes up: whether copies of the app's own
                // go with these songs (Android: music it could not read where it was).
                let own_copies =
                    crate::app::paths::AppPaths::imported_copies_dir().is_some_and(|copies| {
                        tracks.iter().any(|&id| {
                            self.snapshot
                                .get(id)
                                .is_some_and(|track| track.path.parent() == Some(copies.as_path()))
                        })
                    });
                self.dialog = Some(Dialog::RemoveTracks { tracks, own_copies });
            }
            Action::Go(nav) => self.go(nav),
            Action::ToggleColumn(kind) => {
                let columns = &mut self.settings.list_columns;
                let flag = match kind {
                    ColumnKind::Format => &mut self.settings.show_format_column,
                    ColumnKind::Year => &mut columns.year,
                    ColumnKind::Genre => &mut columns.genre,
                    ColumnKind::Plays => &mut columns.plays,
                    ColumnKind::Rating => &mut columns.rating,
                    ColumnKind::Added => &mut columns.added,
                    ColumnKind::LastPlayed => &mut columns.last_played,
                };
                *flag = !*flag;
            }
            Action::RenameSmart(name) => {
                if let Some(smart) = self
                    .settings
                    .smart_playlists
                    .iter()
                    .find(|p| p.name == name)
                {
                    self.dialog = Some(Dialog::smart_playlist(
                        &smart.name,
                        smart.query.clone(),
                        Some(name),
                    ));
                }
            }
            Action::TogglePin(pin) => {
                // Albums by the key they are shown under (one pinned before its tracks
                // were merged into another album holds an alias of its key).
                let snapshot = self.snapshot.clone();
                let shown = |pin: &Pin| shown_pin(&snapshot, pin);
                restate_pins(&mut self.settings.pinned, shown);
                let live = self.live_pins();
                toggle_pin(&mut self.settings.pinned, shown(&pin), |p| live.contains(p));
            }
            Action::DeleteSmart(name) => {
                self.settings
                    .pinned
                    .retain(|p| *p != Pin::Smart(name.clone()));
                self.settings.smart_playlists.retain(|p| p.name != name);
                self.views.remove(&Nav::Smart(name.clone()));
                if self.nav == Nav::Smart(name) {
                    self.go(Nav::Songs);
                }
            }
            Action::EditSmart(name) => {
                if let Some(query) = self
                    .settings
                    .smart_playlists
                    .iter()
                    .find(|p| p.name == name)
                    .map(|p| p.query.clone())
                {
                    self.show_search(Nav::Songs, &query);
                    self.focus_search = true;
                }
            }
            Action::Sort(key) => {
                let state = self.views.entry(self.nav.clone()).or_default();
                state.sort = match state.sort {
                    Some((current, true)) if current == key => Some((key, false)),
                    Some((current, false)) if current == key => None,
                    _ => Some((key, true)),
                };
            }
        }
    }

    fn track(&self, id: TrackId) -> Option<&Arc<Track>> {
        if Library::is_remote(id) {
            return self.remote.get(&id);
        }
        self.snapshot.get(id)
    }

    /// Remembers the window size, and saves the settings when they have changed (the
    /// size at most every few seconds).
    fn track_window_size(&mut self, ctx: &egui::Context) {
        // A phone's screen is not a window whose size is the user's choice, and not
        // every system tells a size at all (Android does not). Whether the settings
        // are saved must not hang on it: with an early return here for "no size",
        // nothing was ever saved on Android.
        // (Not the mini player's either: the size that is kept is the full window's.)
        if !self.phone_mode
            && self.mini.is_none()
            && let Some(rect) = ctx.input(|i| i.viewport().inner_rect)
        {
            let size = [rect.width().round(), rect.height().round()];
            if self.settings.window_size != Some(size) && size[0] >= 100.0 && size[1] >= 100.0 {
                self.settings.window_size = Some(size);
            }
        }
        if self.settings == self.saved_settings {
            self.note_unsaved_settings(false);
            return;
        }
        // Preference changes are saved at once; a window being resized only now and then.
        let only_size_changed = Settings {
            window_size: self.saved_settings.window_size,
            ..self.settings.clone()
        } == self.saved_settings;
        if only_size_changed {
            if self.last_settings_save.elapsed() > Duration::from_secs(2) {
                self.persist_settings();
            }
            return;
        }
        // Not in the middle of a drag (a slider passes through dozens of values on its
        // way) and not many times a second: every save is a file written and synced.
        const SETTLE: Duration = Duration::from_millis(250);
        let dragging = ctx.input(|i| i.pointer.any_down());
        if dragging || self.last_settings_save.elapsed() < SETTLE {
            // Not written yet: a phone's app that leaves the screen now writes it
            // then (see `Core::save_unsaved_settings`).
            self.note_unsaved_settings(true);
            ctx.request_repaint_after(SETTLE);
        } else {
            self.note_unsaved_settings(false);
            self.persist_settings();
        }
    }

    /// Tells the app's core whether the settings wait to be written, where something
    /// reads that: a phone's app saves them as it leaves the screen. A desktop app is
    /// not sent away, and is spared the bookkeeping.
    fn note_unsaved_settings(&self, unsaved: bool) {
        if crate::platform::PHONE {
            self.core
                .note_unsaved_settings(unsaved.then_some(&self.settings));
        }
    }

    /// Hands the settings to the writer thread if they changed since the last save (and
    /// are not already on their way). The file is written and synced there, not here.
    fn persist_settings(&mut self) {
        self.last_settings_save = Instant::now();
        if self.settings == self.saved_settings
            || self
                .settings_in_flight
                .as_ref()
                .is_some_and(|(_, sent)| *sent == self.settings)
        {
            return;
        }
        let bytes = match serde_json::to_vec_pretty(&self.settings) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!("could not encode settings: {e}");
                return;
            }
        };
        // The version's place among all that are written to the file, from any thread.
        self.settings_generation = crate::storage::settings::take_turn();
        let generation = self.settings_generation;
        self.settings_in_flight = Some((generation, self.settings.clone()));
        if !self.settings_writer.write(generation, bytes.clone()) {
            let result = crate::storage::settings::write_atomic_in_turn(
                &self.core.paths.settings_file(),
                &bytes,
                generation,
            );
            self.settings_written(generation, result);
        }
    }

    /// The writer's report on one version of the settings.
    fn settings_written(&mut self, generation: u64, result: std::io::Result<()>) {
        // A report on a version that a newer one replaced says nothing about the newer.
        if self
            .settings_in_flight
            .as_ref()
            .is_none_or(|(newest, _)| *newest != generation)
        {
            return;
        }
        let Some((_, settings)) = self.settings_in_flight.take() else {
            return;
        };
        match result {
            Ok(()) => self.saved_settings = settings,
            // Kept as unsaved, and tried again a little later (a full disk, a folder
            // made read-only): not every frame, and not given up on.
            Err(e) => {
                tracing::warn!("could not save settings: {e}");
                self.last_settings_save = Instant::now() + SETTINGS_RETRY;
            }
        }
    }

    fn refresh_devices(&mut self) {
        if self
            .devices_checked
            .is_none_or(|t| t.elapsed() > Duration::from_secs(2))
        {
            self.devices = crate::audio::output::list_output_devices();
            self.devices_checked = Some(Instant::now());
        }
    }

    // ----- keyboard -------------------------------------------------------------------

    fn global_shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.text_edit_focused();
        responder::set_text_input_active(typing);
        let was_typing = std::mem::replace(&mut self.typing, typing);
        self.widget_focused = !typing && ctx.memory(|m| m.focused()).is_some();
        if self.dialog.is_some() {
            return;
        }
        let (space, escape) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Space),
                i.key_pressed(egui::Key::Escape),
            )
        });
        // While letters are being typed to find a song, Space belongs to that title
        // ("come together"), not to play/pause.
        let finding = self.key_pane == KeyPane::List
            && self.views.get(&self.nav).is_some_and(|state| {
                !state.typeahead.is_empty()
                    && state
                        .typeahead_at
                        .is_some_and(|at| at.elapsed() < TYPEAHEAD_TIMEOUT)
            });
        // With a control focused, Space presses that control (egui does that itself).
        if space && !typing && !self.widget_focused && !finding {
            self.core.player.send(Command::TogglePlayPause);
        }
        if escape && self.phone_mode {
            // On the phone screens Escape is the system's Back. From a field that was
            // typed into, egui has taken the keyboard already (before the frame
            // starts): that is all the key does then.
            if !was_typing {
                self.phone_back(ctx);
            }
        } else if escape {
            // egui drops keyboard focus on Escape before the frame starts, so "was the
            // search field focused" has to come from the previous frame.
            if self.guide_open {
                self.guide_open = false;
            } else if self.eq_open {
                self.eq_open = false;
            } else if self.settings_open {
                self.settings_open = false;
            } else if self.search_had_focus
                && let Some(state) = self.views.get_mut(&self.nav)
            {
                state.query.clear();
            }
        }
    }

    /// A small badge following the pointer while songs are being dragged.
    fn drag_preview(&self, ctx: &egui::Context) {
        let Some(payload) = egui::DragAndDrop::payload::<DragTracks>(ctx) else {
            return;
        };
        let Some(pointer) = ctx.pointer_interact_pos() else {
            return;
        };
        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        let dark = ctx.global_style().visuals.dark_mode;
        let ink = if dark {
            egui::Color32::from_rgb(0x14, 0x14, 0x18)
        } else {
            egui::Color32::WHITE
        };
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Tooltip,
            egui::Id::unique("drag-preview"),
        ));
        let galley = painter.layout_no_wrap(
            self.lang.song_count(payload.tracks.len()),
            theme::semibold(12.0),
            ink,
        );
        let rect = egui::Rect::from_min_size(
            pointer + egui::vec2(14.0, 12.0),
            galley.size() + egui::vec2(16.0, 8.0),
        );
        painter.rect_filled(rect, 10.0, theme::accent(dark));
        painter.galley(rect.min + egui::vec2(8.0, 4.0), galley, ink);
    }

    /// While files are dragged over the window, shows where they will go.
    fn drop_overlay(&self, ctx: &egui::Context) {
        if ctx.input(|i| i.raw.hovered_files.is_empty()) {
            return;
        }
        let rect = ctx.content_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::unique("drop-overlay"),
        ));
        let dark = ctx.global_style().visuals.dark_mode;
        let accent = theme::accent(dark);
        painter.rect_filled(
            rect,
            0.0,
            egui::Color32::from_black_alpha(if dark { 150 } else { 90 }),
        );
        painter.rect_stroke(
            rect.shrink(14.0),
            14.0,
            egui::Stroke::new(2.5, accent),
            egui::StrokeKind::Inside,
        );
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            self.s().drop_hint,
            theme::semibold(20.0),
            egui::Color32::WHITE,
        );
    }

    /// Carries out the app-side steps of a verification script (see `automation`).
    fn run_automation(&mut self, ctx: &egui::Context) {
        let Some(automation) = self.automation.as_mut() else {
            return;
        };
        ctx.request_repaint_after(automation.poll_interval());
        let steps = std::mem::take(&mut automation.app_steps);
        for step in steps {
            match step {
                automation::AppStep::Shot(target) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                        egui::ScreenshotCallback::new(move |image| {
                            let [w, h] = image.size;
                            let bytes: Vec<u8> =
                                image.pixels.iter().flat_map(|p| p.to_array()).collect();
                            if let Some(buffer) =
                                image::RgbaImage::from_raw(w as u32, h as u32, bytes)
                            {
                                let tmp = target.with_extension("tmp.png");
                                if buffer.save(&tmp).is_ok() {
                                    let _ = std::fs::rename(&tmp, &target);
                                }
                            }
                        }),
                    ));
                }
                automation::AppStep::Dump(target) => {
                    let _ = std::fs::write(&target, self.state_json(ctx).to_string());
                    self.perf.max_frame_ms = 0.0;
                    self.perf.max_rows_build_ms = 0.0;
                }
                automation::AppStep::Open(path) => self.open_paths(vec![path]),
                automation::AppStep::Volume(v) => self.core.player.send(Command::SetVolume(v)),
                automation::AppStep::OsKey(code, cmd, shift, alt) => {
                    if !crate::platform::testing::post_key(code, cmd, shift, alt) {
                        tracing::warn!("oskey: no key window");
                    }
                }
                automation::AppStep::MenuKey(code, cmd, shift, alt) => {
                    let taken = crate::platform::testing::menu_key(code, cmd, shift, alt);
                    self.menu_keys.push(taken);
                }
                automation::AppStep::InputSource(id) => {
                    crate::platform::testing::select_input_source(&id);
                }
                automation::AppStep::Menu(id) => {
                    // The playback commands of the menu go straight to the player.
                    let playback = match id.as_str() {
                        "stop_after_current" => Some(MenuAction::StopAfterCurrent),
                        "repeat_section" => Some(MenuAction::RepeatSection),
                        "sleep_off" => Some(MenuAction::SleepTimer(0)),
                        "sleep_15" => Some(MenuAction::SleepTimer(15)),
                        "sleep_90" => Some(MenuAction::SleepTimer(90)),
                        "seek_forward" => Some(MenuAction::SeekForward),
                        "seek_back" => Some(MenuAction::SeekBack),
                        _ => None,
                    };
                    if let (Some(action), Some(dispatch)) = (playback, self.menu_dispatch.clone()) {
                        dispatch(action);
                        continue;
                    }
                    // Shuffling by stars, switched as the playback settings switch it.
                    if id == "shuffle_by_rating" {
                        self.settings.shuffle_by_rating = !self.settings.shuffle_by_rating;
                        self.core
                            .player
                            .send(Command::SetShuffleByRating(self.settings.shuffle_by_rating));
                        continue;
                    }
                    // Crossfade between songs, in seconds (as the playback settings set it).
                    if let Some(secs) = id.strip_prefix("crossfade ") {
                        if let Ok(secs) = secs.parse::<u8>() {
                            self.settings.crossfade_secs =
                                secs.min(app::settings::MAX_CROSSFADE_SECS);
                            self.core
                                .player
                                .send(Command::SetCrossfade(self.settings.crossfade_secs));
                        }
                        continue;
                    }
                    // Lyrics from the internet, on and off (as the settings switch
                    // them), and a stand-in for the lyrics service: a verification run
                    // asks nothing of the real one.
                    if id == "online_lyrics" {
                        self.set_online_lyrics(!self.settings.online_lyrics);
                        continue;
                    }
                    if let Some(base) = id.strip_prefix("lyrics_service ") {
                        self.lyrics_source = Arc::new(crate::library::online_lyrics::Source::new(
                            &self.core.paths.cache_dir,
                            base,
                        ));
                        self.extras = None;
                        continue;
                    }
                    // How the output is used, as the playback settings set it: a
                    // switch flipped (`output match_rate|exclusive|memory|upsample`),
                    // the upsampling filter (`filter sharp|slow|minimum`), and the
                    // device by a part of its name (`output_device <text>`, or
                    // `output_device default`).
                    if let Some(option) = id.strip_prefix("output ") {
                        let output = &mut self.settings.output;
                        match option {
                            "match_rate" => output.match_sample_rate = !output.match_sample_rate,
                            "exclusive" => output.exclusive = !output.exclusive,
                            "memory" => output.memory_playback = !output.memory_playback,
                            "upsample" => output.upsample = !output.upsample,
                            _ => {}
                        }
                        self.core
                            .player
                            .send(Command::SetOutput(self.settings.output));
                        continue;
                    }
                    if let Some(name) = id.strip_prefix("filter ") {
                        if let Some(filter) = crate::audio::upsample::Filter::named(name) {
                            self.settings.output.filter = filter;
                            self.core
                                .player
                                .send(Command::SetOutput(self.settings.output));
                        }
                        continue;
                    }
                    if let Some(name) = id.strip_prefix("output_device ") {
                        let devices = crate::audio::output::list_output_devices();
                        let wanted = (name != "default")
                            .then(|| devices.iter().find(|d| d.name.contains(name)))
                            .flatten()
                            .map(|device| device.id.clone());
                        if name == "default" || wanted.is_some() {
                            self.settings.output_device = wanted;
                            self.core.player.send(Command::SetOutputDevice(
                                self.settings.output_device.clone(),
                            ));
                        }
                        continue;
                    }
                    // Room correction, as its card does it: a file imported, the
                    // switch flipped, the filter taken away.
                    if let Some(path) = id.strip_prefix("room_import ") {
                        self.import_room(Path::new(path));
                        continue;
                    }
                    if id == "room_toggle" {
                        let on = self.settings.room.as_ref().is_some_and(|room| room.enabled);
                        self.set_room_enabled(!on);
                        continue;
                    }
                    if id == "room_remove" {
                        self.remove_room();
                        continue;
                    }
                    // Playback speed, in percent (as the playback settings set it).
                    if let Some(percent) = id.strip_prefix("speed ") {
                        if let Ok(percent) = percent.parse::<u16>() {
                            self.settings.speed_percent = crate::audio::pace::speed_of(percent);
                            self.core
                                .player
                                .send(Command::SetSpeed(self.settings.speed_percent));
                        }
                        continue;
                    }
                    // Stars for the song that is playing.
                    if let Some(stars) = id.strip_prefix("rate_current ") {
                        if let (Some(current), Ok(stars)) = (self.player.current, stars.parse()) {
                            self.apply(Action::SetRating(vec![current.track_id], stars));
                        }
                        continue;
                    }
                    // The library's own data, to a file and from one.
                    if let Some(path) = id.strip_prefix("data_export ") {
                        self.write_library_data(PathBuf::from(path), false);
                        continue;
                    }
                    if let Some(path) = id.strip_prefix("data_import ") {
                        self.import_library_data(PathBuf::from(path));
                        continue;
                    }
                    // Importing a correction file, and switching the correction off.
                    if let Some(path) = id.strip_prefix("correction_import ") {
                        self.import_correction(Path::new(path));
                        continue;
                    }
                    // Selecting a profile by name (as the pop-up in Settings does).
                    if let Some(name) = id.strip_prefix("correction_select ") {
                        // A name that is no profile leaves the selection as it was.
                        let previous = self.settings.correction.replace(name.to_string());
                        match self.settings.active_correction().cloned() {
                            Some(profile) => {
                                self.core.player.send(Command::SetCorrection(Some(profile)));
                            }
                            None => self.settings.correction = previous,
                        }
                        continue;
                    }
                    // Making a correction profile on screen, as its buttons do: begun
                    // anew, a band added, saved, given up.
                    if id == "correction_edit_new" {
                        self.peq_edit_begin(false);
                        continue;
                    }
                    if let Some(band) = id.strip_prefix("correction_edit_band ") {
                        let numbers: Vec<f32> = band
                            .split_whitespace()
                            .filter_map(|n| n.parse().ok())
                            .collect();
                        if let (Some(edit), [hz, gain_db, q]) =
                            (self.peq_edit.as_mut(), numbers.as_slice())
                        {
                            edit.profile.filters.push(crate::audio::PeqFilter {
                                kind: crate::audio::PeqKind::Peak,
                                hz: *hz,
                                gain_db: *gain_db,
                                q: *q,
                            });
                        }
                        self.peq_edit_listen();
                        continue;
                    }
                    if id == "correction_edit_save" {
                        self.peq_edit_save();
                        continue;
                    }
                    if id == "correction_edit_cancel" {
                        self.peq_edit_cancel();
                        continue;
                    }
                    if id == "correction_off" {
                        self.settings.correction = None;
                        self.core.player.send(Command::SetCorrection(None));
                        continue;
                    }
                    // The buttons of the loudness card in Settings.
                    match id.as_str() {
                        "loudness_start" => {
                            self.core.library.measure_loudness();
                            continue;
                        }
                        "loudness_stop" => {
                            self.core.library.cancel_loudness();
                            continue;
                        }
                        "loudness_forget" => {
                            self.core.library.forget_loudness();
                            continue;
                        }
                        "check_start" => {
                            self.core.library.check_files();
                            continue;
                        }
                        "check_show_damaged" => {
                            self.show_search(Nav::Songs, "check:damaged");
                            continue;
                        }
                        "go_folders" => {
                            self.go(Nav::Folder(PathBuf::new()));
                            continue;
                        }
                        "go_first_folder" => {
                            if let Some(root) = self.core.library.roots().first() {
                                self.go(Nav::Folder(root.path.clone()));
                            }
                            continue;
                        }
                        "go_stats" => {
                            self.go(Nav::Stats);
                            continue;
                        }
                        // The periods of the statistics page that are years of the
                        // calendar, and the one it opens with.
                        "stats_this_year" => {
                            self.stats_period = stats_view::Period::ThisYear;
                            continue;
                        }
                        "stats_last_year" => {
                            self.stats_period = stats_view::Period::LastYear;
                            continue;
                        }
                        "stats_month" => {
                            self.stats_period = stats_view::Period::Month;
                            continue;
                        }
                        "pin_first_album" => {
                            if let Some(album) = self.snapshot.albums().first() {
                                let pin = Pin::Album(album.key);
                                self.apply(Action::TogglePin(pin));
                            }
                            continue;
                        }
                        _ => {}
                    }
                    let request = match id.as_str() {
                        "song_play" => Some(UiRequest::SongPlay),
                        "song_play_next" => Some(UiRequest::SongPlayNext),
                        "song_add_to_queue" => Some(UiRequest::SongAddToQueue),
                        "song_toggle_favorite" => Some(UiRequest::SongToggleFavorite),
                        "song_info" => Some(UiRequest::SongInfo),
                        "toggle_mute" => Some(UiRequest::ToggleMute),
                        "playlist_rename" => Some(UiRequest::PlaylistRename),
                        "playlist_delete" => Some(UiRequest::PlaylistDelete),
                        "toggle_queue" => Some(UiRequest::ToggleQueue),
                        "toggle_info" => Some(UiRequest::ToggleInfo),
                        "toggle_meter" => Some(UiRequest::ToggleMeter),
                        "mini_player" => Some(UiRequest::MiniPlayer),
                        "equalizer" => Some(UiRequest::Equalizer),
                        "go_to_current" => Some(UiRequest::GoToCurrent),
                        "user_guide" => Some(UiRequest::UserGuide),
                        "settings" => Some(UiRequest::Settings),
                        _ => None,
                    };
                    match request {
                        Some(request) => self.handle_request(request),
                        None => tracing::warn!("menu: unknown id"),
                    }
                }
                automation::AppStep::Network(words) => self.network_script(&words),
                automation::AppStep::FailMeters => self.phone.fail_meters(),
                automation::AppStep::RestoreRemoved => self.core.library.restore_removed(),
                automation::AppStep::CopyReport => {
                    let report = self.error_report();
                    self.copy_text(ctx, report);
                }
                automation::AppStep::LoseDevice { told } => renderer::lose_device(told),
                automation::AppStep::GlMistake => renderer::ask_for_gl_mistake(),
                automation::AppStep::LoseSurface => renderer::lose_surface(),
                automation::AppStep::Activate => window::show_main_window(),
                automation::AppStep::Front => window::order_front_without_activating(),
                automation::AppStep::Back => window::send_back(),
                automation::AppStep::Hide => window::hide_main_window(),
                automation::AppStep::Resize(w, h) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(w, h)));
                }
                automation::AppStep::Quit => {
                    self.shutdown();
                    std::process::exit(0);
                }
            }
        }
    }

    /// Machine-readable summary of what the app currently shows (for verification scripts).
    fn state_json(&self, ctx: &egui::Context) -> serde_json::Value {
        let p = &self.player;
        let title = |id: TrackId| self.track(id).map(|t| t.title.clone());
        let view = self.views.get(&self.nav);
        let scan = self.core.library.scan_status();
        let mut state = serde_json::json!({
            "nav": format!("{:?}", self.nav),
            "library_tracks": self.snapshot.len(),
            "albums": self.snapshot.albums().len(),
            "artists": self.snapshot.artists().len(),
            "playlists": self.playlists.iter().map(|p| (p.name.clone(), p.item_count)).collect::<Vec<_>>(),
            "scan_running": scan.running,
            "rows": view.map_or(0, |v| v.rows.len()),
            "first_rows": view.map_or_else(Vec::new, |v| {
                v.rows.iter().take(8).filter_map(|r| match self.nav {
                    Nav::Albums | Nav::Artist(_) => self.snapshot.albums().get(r.idx as usize).map(|a| a.title.clone()),
                    Nav::Artists => self.snapshot.artists().get(r.idx as usize).map(|a| a.name.clone()),
                    Nav::Genres => self.genre_list.get(r.idx as usize).map(|g| g.name.clone()),
                    _ => self.snapshot.tracks.get(r.idx as usize).map(|t| t.title.clone()),
                }).collect()
            }),
            "query": view.map_or("", |v| v.query.as_str()),
            "selected": view.map_or(0, |v| v.selection.len()),
            "state": format!("{:?}", p.state),
            "current": p.current.and_then(|c| title(c.track_id)),
            "position_ms": p.position_now_ms(),
            "duration_ms": p.current.map(|c| c.duration_ms),
            "volume": p.volume,
            "shuffle": p.shuffle,
            "repeat": format!("{:?}", p.repeat),
            "queue": p.queue.iter().take(12).filter_map(|i| title(i.track)).collect::<Vec<_>>(),
            "queue_len": p.queue.len(),
            "queue_index": p.queue_index,
            "underruns": p.underruns,
            "start_latency_ms": p.last_start_latency_ms,
            "output_device": p.output_device,
            "output_rate": p.output_sample_rate,
            "source": p.source.as_ref().map(|s| format!("{} {}Hz {}ch", s.codec, s.sample_rate, s.channels)),
            "output_bits": p.output.as_ref().and_then(|o| o.hardware_bits).map(|(bits, _)| bits),
            "exclusive": p.output.as_ref().is_some_and(|o| o.exclusive),
            "in_memory": p.source.as_ref().is_some_and(|s| s.in_memory),
            "match_sample_rate": p.output_settings.match_sample_rate,
            "device_lost": p.device_lost,
            "toast": self.toast.as_ref().or(self.alert.as_ref()).map(|t| t.text.clone()),
            "dialog_open": self.dialog.is_some(),
            "settings_open": self.settings_open,
            "right_panel": format!("{:?}", self.settings.right_panel),
            "lang": format!("{:?}", self.lang),
            "dark": ctx.global_style().visuals.dark_mode,
            "text_focus": ctx.text_edit_focused(),
            "window_visible": window::is_main_window_visible(),
            "input_source": crate::platform::testing::current_input_source(),
            "pixels_per_point": ctx.pixels_per_point(),
            "window_size": ctx.input(|i| i.viewport().inner_rect).map(|r| [r.width(), r.height()]),
        });
        // (Added here: the list above is as long as the macro takes.)
        state["network"] = self.network_state();
        state["removed_songs"] = serde_json::json!(self.core.library.removed_count());
        state["memory"] = serde_json::json!(memory::read().to_string());
        state["adapter"] = serde_json::json!(renderer::adapter());
        state["graphics_errors"] = serde_json::json!(renderer::errors());
        state["section"] = serde_json::json!(p.section.map(|s| (s.from_ms, s.to_ms)));
        state["crossfade_secs"] = serde_json::json!(p.crossfade_secs);
        state["speed_percent"] = serde_json::json!(p.speed_percent);
        state["mini"] = serde_json::json!(self.mini.is_some());
        state["correction_editing"] = serde_json::json!(
            self.peq_edit
                .as_ref()
                .map(|edit| (edit.profile.name.clone(), edit.profile.filters.len()))
        );
        state["current_rating"] = serde_json::json!(
            p.current
                .and_then(|c| self.snapshot.get(c.track_id))
                .map(|t| t.rating)
        );
        state["current_path"] = serde_json::json!(
            p.current
                .and_then(|c| self.track(c.track_id))
                .map(|t| t.path.to_string_lossy().into_owned())
        );
        // Added piecewise: one big `json!` literal exceeds the macro recursion limit.
        let shown = self.toast.as_ref().or(self.alert.as_ref());
        state["toast_action"] = serde_json::json!(
            shown
                .and_then(|t| t.action.as_ref())
                .map(|(label, _)| label.clone())
        );
        state["alert"] = serde_json::json!(self.alert.as_ref().map(|t| t.text.clone()));
        state["key_pane"] = serde_json::json!(format!("{:?}", self.key_pane));
        state["renaming"] =
            serde_json::json!(self.renaming.as_ref().map(|(_, text, _)| text.clone()));
        state["queue_selected"] = serde_json::json!(self.queue_selection.len());
        state["settings_tab"] = serde_json::json!(format!("{:?}", self.settings_tab));
        state["show_remaining_time"] = serde_json::json!(self.settings.show_remaining_time);
        state["selected_titles"] = serde_json::json!(
            self.views
                .get(&self.nav)
                .map(|state| {
                    state
                        .rows
                        .iter()
                        .filter(|r| state.selection.contains(&r.key))
                        .filter_map(|r| self.snapshot.tracks.get(r.idx as usize))
                        .map(|t| t.title.clone())
                        .take(5)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        );
        state["theme"] = serde_json::json!(format!("{:?}", self.settings.theme));
        state["show_format_column"] = serde_json::json!(self.settings.show_format_column);
        state["replaygain"] = serde_json::json!(format!(
            "{:?} {:+.1} {}",
            self.settings.replaygain.mode,
            self.settings.replaygain.preamp_db,
            self.settings.replaygain.prevent_clipping
        ));
        state["guide_open"] = serde_json::json!(self.guide_open);
        state["eq_open"] = serde_json::json!(self.eq_open);
        state["radio"] = serde_json::json!(p.radio);
        state["stop_after_current"] = serde_json::json!(p.stop_after_current);
        state["sleep_in_s"] = serde_json::json!(
            p.sleep_at
                .map(|at| at.saturating_duration_since(Instant::now()).as_secs())
        );
        state["columns"] = serde_json::json!({
            "format": self.settings.show_format_column,
            "year": self.settings.list_columns.year,
            "genre": self.settings.list_columns.genre,
            "plays": self.settings.list_columns.plays,
            "added": self.settings.list_columns.added,
            "last_played": self.settings.list_columns.last_played,
        });
        state["smart_playlists"] = serde_json::json!(
            self.settings
                .smart_playlists
                .iter()
                .map(|p| (p.name.clone(), p.query.clone()))
                .collect::<Vec<_>>()
        );
        state["correction"] = serde_json::json!({
            "selected": self.settings.correction,
            "applied": p.correction,
            "profiles": self.settings.corrections.iter().map(|c| (c.name.clone(), c.filters.len())).collect::<Vec<_>>(),
            "builtin": crate::audio::peq::builtin_profiles().len(),
        });
        state["app_active"] = serde_json::json!(crate::platform::testing::app_is_active());
        state["menu_keys"] = serde_json::json!(self.menu_keys);
        let loudness = self.core.library.loudness_status();
        state["loudness"] = serde_json::json!({
            "running": loudness.running,
            "total": loudness.total,
            "done": loudness.done,
            "failed": loudness.failed,
            "measured": self.snapshot.tracks.iter().filter(|t| t.measured.is_some()).count(),
            "waiting": self
                .snapshot
                .tracks
                .iter()
                .filter(|t| t.available && t.wants_measuring())
                .count(),
        });
        let check = self.core.library.check_status();
        let checked: Vec<_> = self
            .snapshot
            .tracks
            .iter()
            .filter_map(|t| t.check)
            .collect();
        state["check"] = serde_json::json!({
            "running": check.running,
            "total": check.total,
            "done": check.done,
            "checked": checked.len(),
            "damaged": checked.iter().filter(|c| c.verdict == crate::model::CheckVerdict::Damaged).count(),
            "verified": checked.iter().filter(|c| c.verdict == crate::model::CheckVerdict::Verified).count(),
            "with_dr": checked.iter().filter(|c| c.dr.is_some()).count(),
        });
        state["pinned"] = serde_json::json!(self.settings.pinned.len());
        state["stats"] = serde_json::json!(self.stats_summary_for_dump());
        state["utc_offset_secs"] = serde_json::json!(crate::platform::clock::utc_offset_secs());
        state["subfolders"] = serde_json::json!(
            self.folder_children
                .as_ref()
                .map_or(0, |(_, _, entries)| entries.len())
        );
        state["shuffle_by_album"] = serde_json::json!(self.settings.shuffle_by_album);
        state["shuffle_by_rating"] = serde_json::json!(self.settings.shuffle_by_rating);
        let (textures, pending, uploads) = self.art.counts();
        state["art"] = serde_json::json!({
            "textures": textures,
            "pending": pending,
            "uploads": uploads,
        });
        if self.phone_mode {
            state["phone"] = self.phone.describe();
            let (full, safe) = (ctx.viewport_rect(), ctx.content_rect());
            state["screen"] = serde_json::json!({
                "full": [full.left(), full.top(), full.right(), full.bottom()],
                "safe": [safe.left(), safe.top(), safe.right(), safe.bottom()],
            });
        }
        state["crossfeed"] = serde_json::json!(format!("{:?}", p.crossfeed));
        state["rediscover"] = serde_json::json!(format!("{:?}", self.rediscover));
        let details = self.extras.as_ref().and_then(|e| e.details.as_ref());
        let lyrics = details.and_then(|d| d.lyrics.as_ref());
        state["lyrics"] = serde_json::json!({
            "chars": lyrics.map(|l| l.plain.chars().count()),
            "timed_lines": lyrics.and_then(|l| l.timed.as_ref()).map(Vec::len),
            "current_line": lyrics.and_then(|l| l.line_at(p.position_now_ms())),
            "online": self.settings.online_lyrics,
            "found_online": details.is_some_and(|d| d.lyrics_online),
            "seeking": self.seeking_lyrics(),
        });
        state["credits"] = serde_json::json!(details.map(|d| {
            d.credits
                .iter()
                .map(|c| format!("{:?}: {}", c.role, c.names.join(", ")))
                .collect::<Vec<_>>()
        }));
        state["versions"] = serde_json::json!(
            self.extras
                .as_ref()
                .map_or(0, panels::Extras::version_count)
        );
        state["eq"] = serde_json::json!({
            "enabled": self.settings.eq.enabled,
            "active": p.eq.is_active(),
            "preamp_db": self.settings.eq.preamp_db,
            "gains_db": self.settings.eq.gains_db,
            "player_gains_db": p.eq.gains_db,
            "preset": match equalizer::matching_preset(&self.settings.eq, &self.settings.eq_presets) {
                Some(equalizer::PresetRef::Builtin(i)) => crate::audio::eq::BUILTIN_PRESETS[i].id.to_string(),
                Some(equalizer::PresetRef::User(i)) => format!("user:{}", self.settings.eq_presets[i].name),
                None => "custom".to_string(),
            },
            "user_presets": self.settings.eq_presets.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
        });
        state["licenses_shown"] = serde_json::json!(self.settings_open && self.licenses_shown);
        state["player_stuck"] = serde_json::json!(self.player_stuck);
        state["spectrum_peak_hz"] = serde_json::json!(self.spectrum.loudest_band_hz());
        state["dither_bits"] = serde_json::json!(p.dither_bits);
        state["peak_limited_frames"] = serde_json::json!(p.peak_limited_frames);
        state["meter_visible"] = serde_json::json!(self.settings.show_meter);
        state["meter"] = serde_json::json!({
            "position": [self.meter.needles[0].position, self.meter.needles[1].position],
            "vu": [
                meter::reading(self.meter.needles[0].position),
                meter::reading(self.meter.needles[1].position),
            ],
            "peak": [self.meter.peak_lit(0), self.meter.peak_lit(1)],
        });
        // What is done to the samples of the song that plays, stage by stage.
        state["signal_path"] = serde_json::json!(self.signal_path().map(|path| {
            path.stages
                .iter()
                .map(|stage| format!("{}: {}", stage.name, stage.detail))
                .collect::<Vec<_>>()
        }));
        state["room"] = serde_json::json!({
            "kept": self.settings.room.as_ref().map(|room| room.name.clone()),
            "enabled": self.settings.room.as_ref().is_some_and(|room| room.enabled),
            "applied": p.room,
            "kept_in_file": self.core.paths.room_file().exists(),
        });
        state["output_options"] = serde_json::json!({
            "match_sample_rate": self.settings.output.match_sample_rate,
            "exclusive": self.settings.output.exclusive,
            "memory_playback": self.settings.output.memory_playback,
            "upsample": self.settings.output.upsample,
            "filter": format!("{:?}", self.settings.output.filter),
        });
        state["perf"] = serde_json::json!({
            "first_frame_ms": self.perf.first_frame_ms,
            "last_frame_ms": self.perf.last_frame_ms,
            "max_frame_ms": self.perf.max_frame_ms,
            "frames": self.perf.frames,
            "last_rows_build_ms": self.perf.last_rows_build_ms,
            "max_rows_build_ms": self.perf.max_rows_build_ms,
        });
        state
    }

    fn shutdown(&mut self) {
        if self.shut_down {
            return;
        }
        self.shut_down = true;
        // Edits that were made go in before the library closes.
        self.edits.finish(QUIT_PATIENCE);
        // The writer finishes the settings it has; whatever is still unsaved after that
        // (it failed, or the writer is stuck) is written here, the last thing it does.
        self.persist_settings();
        self.settings_writer.finish(QUIT_PATIENCE);
        while let Ok(event) = self.events_rx.try_recv() {
            if let AppEvent::SettingsWritten(generation, result) = event {
                self.settings_written(generation, result);
            }
        }
        if self.settings != self.saved_settings
            && let Err(e) = crate::storage::settings::save_json_atomic(
                &self.core.paths.settings_file(),
                &self.settings,
            )
        {
            tracing::warn!("could not save settings when quitting: {e}");
        }
        app::remove_media_session();
        self.menu = None;
        self.core.shutdown();
        tracing::info!("OnEum stopped");
    }
}

/// Whether music is on its way out (playing, or about to: loading, seeking).
fn sounding(state: PlayState) -> bool {
    matches!(
        state,
        PlayState::Playing | PlayState::Loading | PlayState::Seeking
    )
}

/// Whether the music was playing when the app last looked (`OneumApp::sync`): for an
/// end of drawing that the app itself does not get to see ([`lost_after_drawing`]).
#[cfg(target_os = "android")]
static WAS_PLAYING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A run that cannot draw any more (Android): the note for the run that follows is
/// written and the app is asked to be opened again in a new process, where the music
/// goes on. Where that is not to be (it happened twice in a row) or cannot be, a line
/// of the system says what happened. The caller saves what the app keeps and ends the
/// process.
#[cfg(target_os = "android")]
fn start_anew_after_loss(data_dir: &Path, playing: bool, s: &Strings) {
    use app::relaunch::Loss;
    let loss = app::relaunch::note_loss(
        data_dir,
        playing,
        renderer::backend_word(),
        app::relaunch::now(),
    );
    match loss {
        Loss::Again => {
            tracing::error!("graphics: lost again right after a new start; not starting anew");
            crate::platform::android::notice(s.graphics_lost_again);
        }
        Loss::StartAnew if crate::platform::android::open_again_after_exit() => {}
        Loss::StartAnew | Loss::Unwritten => {
            crate::platform::android::notice(s.graphics_lost_body);
        }
    }
}

/// The event loop ended on an error (Android). Under the UI framework's OpenGL
/// renderer that is how a run ends whose driver gives up while a frame is shown: no
/// error of OpenGL tells of it beforehand ([`renderer::note_gl_errors`]), the frame
/// cannot be shown and the framework stops. Once the window had been drawn, that is a
/// lost device like any other: what `OneumApp::graphics_lost` does is done here, for
/// an app that is gone already (it saved what it keeps when it was dropped). False
/// where nothing was ever drawn, a start that failed, which is for the caller to say.
#[cfg(target_os = "android")]
pub fn lost_after_drawing(data_dir: &Path, lang: Lang, error: &dyn std::fmt::Display) -> bool {
    if memory::frames() == 0 {
        return false;
    }
    let playing = WAS_PLAYING.load(std::sync::atomic::Ordering::Relaxed);
    tracing::error!(playing, "graphics: the device is lost: {error}");
    start_anew_after_loss(data_dir, playing, lang.strings());
    true
}

/// Of the system's list of how earlier runs ended (newest first, as `MainActivity`
/// writes it): the first three that ended badly. The newest crash the system kept a
/// record of is followed by the lines about it (the signal, where the thread was),
/// which begin with a tab in the list.
fn bad_exits(text: &str) -> Vec<&str> {
    const BAD: [&str; 4] = ["crash", "not answering", "signal", "low memory"];
    let mut lines = Vec::new();
    let (mut heads, mut detailed, mut taking) = (0, false, false);
    for line in text.lines() {
        if let Some(detail) = line.strip_prefix('\t') {
            if taking {
                detailed = true;
                lines.push(detail);
            }
            continue;
        }
        if heads == 3 {
            break;
        }
        taking = false;
        if BAD.iter().any(|word| line.contains(word)) {
            heads += 1;
            taking = !detailed;
            lines.push(line);
        }
    }
    lines
}

/// The side of the picture a phone keeps its glyphs in, at most (see `raw_input_hook`).
const PHONE_GLYPH_PICTURE_SIDE: usize = 4096;

/// The text of [`OneumApp::error_report`], from its parts. Of the log the last 80
/// lines that warn, report an error, tell of the graphics or the memory or begin a
/// run; of what the Java side wrote down the last 40 lines. Lines are cut at 600
/// characters and the whole at 60,000: a clipboard passes only so much between apps.
fn report_text(about: &[String], exits: &str, log: &str, java: &str, memory: &str) -> String {
    const WANTED: [&str; 6] = [
        " WARN ",
        " ERROR ",
        " memory: ",
        " graphics",
        "starting OnEum",
        "panic:",
    ];
    fn last(lines: Vec<&str>, count: usize) -> Vec<&str> {
        let from = lines.len().saturating_sub(count);
        lines[from..].to_vec()
    }
    let cut = |line: &str| line.chars().take(600).collect::<String>();
    let mut text = String::new();
    let mut part = |title: &str, lines: Vec<String>| {
        if lines.is_empty() {
            return;
        }
        text.push_str("--- ");
        text.push_str(title);
        text.push_str(" ---\n");
        for line in lines {
            text.push_str(&line);
            text.push('\n');
        }
    };
    part("the app", about.to_vec());
    part("how earlier runs ended", exits.lines().map(cut).collect());
    let said = log
        .lines()
        .filter(|line| WANTED.iter().any(|word| line.contains(word)))
        .collect();
    part("the log", last(said, 80).into_iter().map(cut).collect());
    part(
        "crashes of the Java side",
        last(java.lines().collect(), 40)
            .into_iter()
            .map(cut)
            .collect(),
    );
    part("memory now", vec![memory.to_string()]);
    if text.chars().count() > 60_000 {
        text = text.chars().take(60_000).collect();
        text.push_str("\n(cut here)\n");
    }
    text
}

/// Of the app's log: what the runs before the last start wrote of their memory, the
/// last line of each of the two latest ("memory: …", see `memory`). A run that ended
/// for lack of memory says with it what had grown.
fn memory_of_earlier_runs(log: &str) -> Vec<&str> {
    let mut last_of_run: Vec<&str> = Vec::new();
    let mut current: Option<&str> = None;
    for line in log.lines() {
        if line.contains("starting OnEum") {
            last_of_run.extend(current.take());
        } else if line.contains(" memory: ") {
            current = Some(line);
        }
    }
    // (`current` is what this run wrote: not an earlier one.)
    let from = last_of_run.len().saturating_sub(2);
    last_of_run.split_off(from)
}

fn report_open_problems(app: &mut OneumApp, report: &OpenReport) {
    if report.recovered_corrupt_db.is_some() {
        app.alert = Some(Toast {
            text: app.s().notice_db_recovered.into(),
            warning: true,
            action: None,
            expires: None,
        });
    }
}

/// Pins `pin`, or unpins it if it is pinned. Only pins whose things still exist
/// (`live`) count toward the limit: the others are not shown and cannot be unpinned,
/// so they are dropped when room is needed rather than holding places.
fn toggle_pin(pinned: &mut Vec<Pin>, pin: Pin, live: impl Fn(&Pin) -> bool) {
    if let Some(at) = pinned.iter().position(|p| *p == pin) {
        pinned.remove(at);
        return;
    }
    let kept: Vec<Pin> = pinned.iter().filter(|p| live(p)).cloned().collect();
    if kept.len() < MAX_PINS {
        *pinned = kept;
        pinned.push(pin);
    }
}

/// Makes the pin of `from` one of `to` (a smart playlist renamed). If `to` was pinned
/// already, the first of the two stays where it is and the other goes.
fn repoint_pin(pinned: &mut Vec<Pin>, from: &Pin, to: Pin) {
    restate_pins(
        pinned,
        |pin| if pin == from { to.clone() } else { pin.clone() },
    );
}

/// Puts every pin as `restate` says it (an album by the key it is shown under now), and
/// keeps only the first of pins that have become the same.
fn restate_pins(pinned: &mut Vec<Pin>, restate: impl Fn(&Pin) -> Pin) {
    let mut seen: Vec<Pin> = Vec::new();
    pinned.retain_mut(|pin| {
        *pin = restate(pin);
        let first = !seen.contains(pin);
        if first {
            seen.push(pin.clone());
        }
        first
    });
}

/// Turns a primary click with Control held into a secondary click (see
/// `raw_input_hook`).
fn control_click_is_secondary(events: &mut [egui::Event]) {
    for event in events {
        if let egui::Event::PointerButton {
            button, modifiers, ..
        } = event
            && *button == egui::PointerButton::Primary
            && modifiers.ctrl
            && !modifiers.mac_cmd
        {
            *button = egui::PointerButton::Secondary;
        }
    }
}

/// A pin as the library shows its thing now: an album by the key it is shown under.
fn shown_pin(snapshot: &LibrarySnapshot, pin: &Pin) -> Pin {
    match pin {
        Pin::Album(key) => Pin::Album(snapshot.shown_album_key(*key)),
        other => other.clone(),
    }
}

/// Where a pin leads.
fn pin_nav(pin: &Pin) -> Nav {
    match pin {
        Pin::Album(key) => Nav::Album(*key),
        Pin::Artist(name) => Nav::Artist(name.clone()),
        Pin::Genre(name) => Nav::Genre(name.clone()),
        Pin::Playlist(id) => Nav::Playlist(*id),
        Pin::Smart(name) => Nav::Smart(name.clone()),
    }
}

/// "Pin to Sidebar" (or "Unpin") in a context menu.
fn pin_menu_item(
    ui: &mut egui::Ui,
    s: &Strings,
    pinned: &[Pin],
    pin: Pin,
    actions: &mut Vec<Action>,
) {
    let is_pinned = pinned.contains(&pin);
    let label = if is_pinned { s.unpin } else { s.pin_to_sidebar };
    let room = is_pinned || pinned.len() < MAX_PINS;
    if ui.add_enabled(room, egui::Button::new(label)).clicked() {
        actions.push(Action::TogglePin(pin));
        ui.close();
    }
}

pub(crate) fn next_repeat(mode: RepeatMode) -> RepeatMode {
    match mode {
        RepeatMode::Off => RepeatMode::All,
        RepeatMode::All => RepeatMode::One,
        RepeatMode::One => RepeatMode::Off,
    }
}

impl eframe::App for OneumApp {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        // The on-screen keyboard of Android was put away by the user: the field that
        // was typed into lets go, so that a tap on it brings the keyboard back.
        #[cfg(target_os = "android")]
        if crate::platform::text_input::take_dismissed()
            && let Some(focused) = ctx.memory(|memory| memory.focused())
        {
            ctx.memory_mut(|memory| memory.surrender_focus(focused));
        }
        #[cfg(not(target_os = "android"))]
        let _ = ctx;
        // The picture all glyphs are kept in is as wide as the largest texture the
        // device takes (8192 pixels here) and doubles its height when it is full, up
        // to a square: 256 MB, sent whole to the graphics device at every doubling.
        // A phone gets a quarter of that side; a full picture is started afresh.
        if crate::platform::PHONE
            && let Some(side) = raw.max_texture_side.as_mut()
        {
            *side = (*side).min(PHONE_GLYPH_PICTURE_SIDE);
        }
        if let Some(automation) = self.automation.as_mut() {
            automation.step(raw);
        }
        // What the screen keeps for itself (status bar, home indicator), where the
        // system says so; see `platform::ios::scene`.
        // Whether the system is set to dark, where the window system does not tell.
        if let Some(dark) = window::system_is_dark() {
            raw.system_theme = Some(if dark {
                egui::Theme::Dark
            } else {
                egui::Theme::Light
            });
        }
        if let Some([top, left, bottom, right]) = window::safe_area_insets() {
            raw.safe_area_insets = Some(egui::SafeAreaInsets(egui::epaint::MarginF32 {
                left,
                right,
                top,
                bottom,
            }));
        }
        // Control-click is a secondary click on a Mac; the window system delivers it as a
        // primary click with Control held. (Only there: on Windows, Ctrl+click is how
        // more than one row is selected.)
        if cfg!(target_os = "macos") {
            control_click_is_secondary(&mut raw.events);
        }
        // Edit-menu actions arrive through the responder chain (see platform::macos::responder).
        for action in responder::drain() {
            let key = |key, shift| egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers {
                    shift,
                    command: true,
                    mac_cmd: true,
                    ..Default::default()
                },
            };
            raw.events.push(match action {
                EditAction::Cut => egui::Event::Cut,
                EditAction::Copy => egui::Event::Copy,
                EditAction::Paste => {
                    egui::Event::Paste(responder::pasteboard_text().unwrap_or_default())
                }
                EditAction::SelectAll => key(egui::Key::A, false),
                EditAction::Undo => key(egui::Key::Z, false),
                EditAction::Redo => key(egui::Key::Z, true),
            });
        }
        // A touch may change the field that is typed into (move its caret, empty it):
        // the on-screen keyboard of Android starts from nothing then.
        #[cfg(target_os = "android")]
        if raw.events.iter().any(|event| {
            matches!(
                event,
                egui::Event::PointerButton { pressed: true, .. }
                    | egui::Event::Touch {
                        phase: egui::TouchPhase::Start,
                        ..
                    }
            )
        }) {
            crate::platform::text_input::reset();
        }
        // The keys of a keyboard on Android that are the app's (its shortcuts).
        #[cfg(target_os = "android")]
        raw.events.extend(
            crate::platform::text_input::drain_keys()
                .iter()
                .filter_map(hardkeys::event),
        );
        // What the on-screen keyboard of Android typed (see platform::android::text_input).
        #[cfg(target_os = "android")]
        for typed in crate::platform::text_input::drain() {
            use crate::platform::text_input::Typed;
            let key = |key| egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            };
            raw.events.push(match typed {
                Typed::Text(text) => egui::Event::Text(text),
                Typed::Backspace => key(egui::Key::Backspace),
                Typed::Enter => key(egui::Key::Enter),
            });
        }
    }

    /// Runs for every pass, also while the window is hidden or fully covered (then
    /// without a UI pass): the close button, Finder opens and menu commands must not
    /// wait for the window to become visible again.
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.sync(ctx);
        self.run_automation(ctx);
        // The UI framework's OpenGL renderer does not say when the driver refused
        // something while the frame before was drawn: it is asked here.
        #[cfg(target_os = "android")]
        if let Some(gl) = frame.gl() {
            renderer::note_gl_errors(gl, memory::frames());
        }
        #[cfg(not(target_os = "android"))]
        let _ = frame;
        // Asked last, right before this pass is drawn: a device that went while the
        // frame before was drawn (or was taken away by a verification step just now)
        // must not be drawn with again.
        if renderer::device_lost() {
            self.graphics_lost();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let frame_started = Instant::now();
        if self.perf.first_frame_ms.is_none() {
            self.perf.first_frame_ms = Some(crate::app::process_uptime().as_secs_f64() * 1000.0);
        }
        let ctx = ui.ctx().clone();
        let (atlas, rows_used) = ctx.fonts(|fonts| {
            let [width, height] = fonts.font_image_size();
            // (Its fill is told as a share of the height it may grow to: its width.)
            let rows = fonts.font_atlas_fill_ratio() * width.max(height) as f32;
            ([width, height], rows as usize)
        });
        memory::note_frame(atlas, rows_used);
        if !self.phone_mode && std::mem::take(&mut self.mini_toggle) {
            self.toggle_mini(&ctx);
        }
        // The mini player shows the player bar and nothing else. What needs the whole
        // window (a question, Settings, the guide, the equalizer, a form, a message
        // with something to press) brings the whole window back: it would otherwise
        // be there unseen, and keys would go to it.
        if self.mini.is_some()
            && (self.dialog.is_some()
                || self.settings_open
                || self.eq_open
                || self.guide_open
                || self.folder_form.is_some()
                || self.alert.is_some())
        {
            self.toggle_mini(&ctx);
        }
        // A correction that is being made on screen is heard while it is made. Where
        // its editor is gone (Settings was closed), so is what was not saved of it.
        let editor_there = match self.phone_mode {
            true => self.phone.holds_settings(),
            false => self.settings_open,
        };
        if self.peq_edit.as_ref().is_some_and(|edit| edit.shown) && !editor_there {
            self.peq_edit_cancel();
        }
        self.sync_frame(&ctx);
        self.global_shortcuts(&ctx);
        if self.phone_mode {
            if crate::platform::PHONE_KEYBOARD {
                self.menu_shortcuts(&ctx);
            }
            self.phone_frame(ui);
        } else if self.mini.is_some() {
            // The mini player: the player bar is all there is.
            if crate::platform::MENU_IN_WINDOW {
                self.menu_shortcuts(&ctx);
            }
            self.player_bar(ui);
            egui::CentralPanel::default().show(ui, |_| {});
        } else {
            let mut actions: Vec<Action> = Vec::new();
            if crate::platform::MENU_IN_WINDOW {
                self.menu_shortcuts(&ctx);
            }
            if self.window_menu {
                self.menu_bar(ui);
            }
            self.top_bar(ui);
            self.player_bar(ui);
            self.meter_deck(ui);
            self.toast_bar(ui);
            self.sidebar(ui, &mut actions);
            self.right_panel(ui, &mut actions);
            egui::CentralPanel::default().show(ui, |ui| self.central(ui, &mut actions));

            self.drop_overlay(&ctx);
            self.drag_preview(&ctx);
            self.settings_window(&ctx);
            self.guide_window(&ctx);
            self.equalizer_window(&ctx);
            self.dialogs(&ctx);
            self.folder_form_window(&ctx);
            for action in actions {
                self.apply(action);
            }
        }

        // Keep the progress display moving while playing; otherwise draw only on demand.
        // (Not at all in the background: see `platform::lifecycle`.)
        if !crate::platform::lifecycle::in_foreground() {
            return;
        }
        if matches!(
            self.player.state,
            PlayState::Playing | PlayState::Loading | PlayState::Seeking
        ) {
            ctx.request_repaint_after(Duration::from_millis(250));
        } else if self.core.library.scan_status().running {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        if let Some(expires) = self.toast.as_ref().and_then(|t| t.expires) {
            ctx.request_repaint_after(
                expires.saturating_duration_since(Instant::now()) + Duration::from_millis(50),
            );
        }
        self.track_window_size(&ctx);
        let frame_ms = frame_started.elapsed().as_secs_f64() * 1000.0;
        self.perf.last_frame_ms = frame_ms;
        self.perf.max_frame_ms = self.perf.max_frame_ms.max(frame_ms);
        self.perf.frames += 1;
    }

    // (The framework hands its OpenGL context along where that renderer is built
    // in, which is on Android.)
    #[cfg(target_os = "android")]
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.shutdown();
    }

    #[cfg(not(target_os = "android"))]
    fn on_exit(&mut self) {
        self.shutdown();
    }
}

impl Drop for OneumApp {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_to_copy_has_every_part_that_has_something() {
        let about = ["OnEum 0.1.0 (android)".to_string()];
        let exits =
            "Sun Nov 15 20:20:31  crash (native)  crash\n\tSIGABRT, on thread \"android_main\"\n";
        let log = "t1  INFO starting OnEum version=\"0.1.0\"\n\
                   t2  INFO library opened tracks=1500\n\
                   t3  INFO graphics: the window is drawn with this adapter\n\
                   t4  WARN a file could not be read\n\
                   t5  INFO memory: mappings=3000\n\
                   t6 ERROR graphics: the device is lost: Out of memory\n";
        let text = report_text(&about, exits, log, "", "mappings=3100");
        assert_eq!(
            text,
            "--- the app ---\n\
             OnEum 0.1.0 (android)\n\
             --- how earlier runs ended ---\n\
             Sun Nov 15 20:20:31  crash (native)  crash\n\
             \tSIGABRT, on thread \"android_main\"\n\
             --- the log ---\n\
             t1  INFO starting OnEum version=\"0.1.0\"\n\
             t3  INFO graphics: the window is drawn with this adapter\n\
             t4  WARN a file could not be read\n\
             t5  INFO memory: mappings=3000\n\
             t6 ERROR graphics: the device is lost: Out of memory\n\
             --- memory now ---\n\
             mappings=3100\n"
        );
        // Nothing went wrong yet: the app and its memory are still told.
        let text = report_text(&about, "", "t  INFO library opened\n", "", "mappings=1");
        assert_eq!(
            text,
            "--- the app ---\nOnEum 0.1.0 (android)\n--- memory now ---\nmappings=1\n"
        );
        // Of a long log the last lines, and a long line cut.
        let long: String = (0..200)
            .map(|n| format!("t{n} ERROR {}\n", "x".repeat(900)))
            .collect();
        let text = report_text(&about, "", &long, "", "m");
        assert!(
            !text.contains("t119 ERROR")
                && text.contains("t120 ERROR")
                && text.contains("t199 ERROR")
        );
        assert!(text.lines().all(|line| line.chars().count() <= 600));
        assert!(text.chars().count() <= 60_100);
    }

    #[test]
    fn the_memory_shown_is_what_earlier_runs_wrote_last() {
        let log = "\
t1  INFO starting OnEum version=\"0.1.0\"\n\
t2  INFO memory: mappings=3000 first run, early\n\
t3  INFO memory: mappings=60000 first run, late\n\
t4  INFO starting OnEum version=\"0.1.0\"\n\
t5  INFO starting OnEum version=\"0.1.0\"\n\
t6 ERROR memory: mappings=65000 third run\n\
t7  INFO starting OnEum version=\"0.1.0\"\n\
t8  INFO memory: mappings=3100 this run\n";
        assert_eq!(
            memory_of_earlier_runs(log),
            [
                "t3  INFO memory: mappings=60000 first run, late",
                "t6 ERROR memory: mappings=65000 third run",
            ]
        );
        assert!(memory_of_earlier_runs("t  INFO starting OnEum\nt  INFO memory: now\n").is_empty());
    }

    /// The list `MainActivity` writes of how earlier runs ended, as the About page of
    /// a phone shows it.
    #[test]
    fn the_exits_shown_are_the_bad_ones_and_the_newest_crash_has_its_details() {
        let list = "Sun Nov 15 19:10:01  exit 0\n\
                    Sun Nov 15 19:09:00  crash (native)  crash\n\
                    \tSIGSEGV (SEGV_MAPERR) at 0x10, on thread \"oneum\"\n\
                    \t#00 libgpu.so+0x1234 (draw+12)\n\
                    Sun Nov 15 18:35:29  ended by the user\n\
                    Sun Nov 15 18:35:09  crash (native)  crash\n\
                    \t#00 libother.so+0x10\n\
                    Sun Nov 15 18:00:58  not answering\n\
                    Sun Nov 15 17:00:00  low memory\n";
        assert_eq!(
            bad_exits(list),
            [
                "Sun Nov 15 19:09:00  crash (native)  crash",
                "SIGSEGV (SEGV_MAPERR) at 0x10, on thread \"oneum\"",
                "#00 libgpu.so+0x1234 (draw+12)",
                "Sun Nov 15 18:35:09  crash (native)  crash",
                "Sun Nov 15 18:00:58  not answering",
            ]
        );
        // A crash the system kept nothing of: the details of the next one are shown.
        let list = "a  crash (native)  crash\nb  crash (native)  crash\n\t#00 lib.so+0x1\n";
        assert_eq!(
            bad_exits(list),
            [
                "a  crash (native)  crash",
                "b  crash (native)  crash",
                "#00 lib.so+0x1"
            ]
        );
        assert!(bad_exits("x  exit 0\ny  ended by the user\n").is_empty());
    }

    /// Pins of things that are gone (a deleted playlist) do not hold places: when the
    /// visible pins leave room, a new pin takes it and the gone ones are dropped.
    #[test]
    fn pins_of_gone_things_make_room() {
        let mut pinned: Vec<Pin> = (0..MAX_PINS as i64).map(Pin::Playlist).collect();
        // All twelve exist: no room.
        toggle_pin(&mut pinned, Pin::Genre("Jazz".into()), |_| true);
        assert_eq!(pinned.len(), MAX_PINS);
        assert!(!pinned.contains(&Pin::Genre("Jazz".into())));
        // Playlists 0 and 1 were deleted: room, and they go.
        let gone = [Pin::Playlist(0), Pin::Playlist(1)];
        toggle_pin(&mut pinned, Pin::Genre("Jazz".into()), |p| {
            !gone.contains(p)
        });
        assert_eq!(pinned.len(), MAX_PINS - 1);
        assert!(pinned.contains(&Pin::Genre("Jazz".into())));
        assert!(!pinned.iter().any(|p| gone.contains(p)));
        // Toggling a pinned thing unpins it.
        toggle_pin(&mut pinned, Pin::Genre("Jazz".into()), |_| true);
        assert!(!pinned.contains(&Pin::Genre("Jazz".into())));
    }

    /// On a Mac a click with Control held is a secondary click; with Command held (or
    /// with nothing) it stays what it is. (The conversion is applied on macOS only: on
    /// Windows, Ctrl+click selects.)
    #[test]
    fn a_control_click_becomes_a_secondary_click() {
        let click = |ctrl: bool, mac_cmd: bool| egui::Event::PointerButton {
            pos: egui::pos2(10.0, 10.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers {
                ctrl,
                mac_cmd,
                ..Default::default()
            },
        };
        let mut events = vec![click(true, false), click(false, true), click(false, false)];
        control_click_is_secondary(&mut events);
        let buttons: Vec<egui::PointerButton> = events
            .iter()
            .filter_map(|event| match event {
                egui::Event::PointerButton { button, .. } => Some(*button),
                _ => None,
            })
            .collect();
        assert_eq!(
            buttons,
            [
                egui::PointerButton::Secondary,
                egui::PointerButton::Primary,
                egui::PointerButton::Primary
            ]
        );
    }

    /// A smart playlist renamed to the name of another pinned one: one pin for it, at
    /// the first of the two places.
    #[test]
    fn a_renamed_smart_playlist_is_pinned_once() {
        let smart = |name: &str| Pin::Smart(name.into());
        let mut pinned = vec![smart("B"), Pin::Album(7), smart("A")];
        repoint_pin(&mut pinned, &smart("A"), smart("B"));
        assert_eq!(pinned, [smart("B"), Pin::Album(7)]);
        let mut pinned = vec![Pin::Album(7), smart("A"), Pin::Playlist(1)];
        repoint_pin(&mut pinned, &smart("A"), smart("C"));
        assert_eq!(pinned, [Pin::Album(7), smart("C"), Pin::Playlist(1)]);
    }

    /// An album pinned under a key that has since become an alias (its tracks merged
    /// into another album) is the same pin as that album's.
    #[test]
    fn an_album_pinned_before_a_merge_is_pinned_once() {
        use crate::library::album_key;
        use crate::model::test_track;
        let folder = "/music/Sessions";
        let mut tagged = test_track(1);
        tagged.path = std::path::PathBuf::from(format!("{folder}/01.flac"));
        tagged.album = "Sessions".into();
        tagged.album_artist = "Band".into();
        tagged.album_key = album_key("Sessions", "Band", false, &tagged.path);
        let mut untagged = test_track(2);
        untagged.path = std::path::PathBuf::from(format!("{folder}/02.flac"));
        untagged.album = "Sessions".into();
        untagged.album_key = album_key("Sessions", "", false, &untagged.path);
        let (alias, shown) = (untagged.album_key, tagged.album_key);
        let snapshot = LibrarySnapshot::from_tracks(vec![tagged, untagged]);
        assert_eq!(snapshot.shown_album_key(alias), shown);

        let mut pinned = vec![Pin::Album(alias), Pin::Playlist(1), Pin::Album(shown)];
        restate_pins(&mut pinned, |pin| shown_pin(&snapshot, pin));
        assert_eq!(pinned, [Pin::Album(shown), Pin::Playlist(1)]);
        // Toggling the album as its menu offers it unpins it.
        toggle_pin(
            &mut pinned,
            shown_pin(&snapshot, &Pin::Album(shown)),
            |_| true,
        );
        assert_eq!(pinned, [Pin::Playlist(1)]);
    }
}
