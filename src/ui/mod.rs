//! The egui user interface. Layout and behaviour follow `docs/UX.md`.
//!
//! The UI thread only draws and handles input: it reads immutable snapshots of the
//! library and the player and sends commands. Decoding, scanning and image processing
//! happen on other threads.
mod art;
mod automation;
mod dialogs;
mod icons;
mod lists;
mod panels;
mod player_bar;
mod settings_window;
pub mod theme;
mod widgets;

use crate::app::settings::{RightPanel, Settings};
use crate::app::{self, Core};
use crate::audio::{OutputDeviceInfo, PlaybackErrorKind};
use crate::i18n::{Lang, Strings};
use crate::library::{LibraryError, LibrarySnapshot, OpenReport, PlaylistInfo, SortKey};
use crate::model::{Track, TrackId};
use crate::platform::macos::menu::{AppMenu, MenuAction};
use crate::platform::macos::responder::{self, EditAction};
use crate::platform::macos::{app_hooks, finder, window};
use crate::playback::{Command, Notice, PlayState, PlayerSnapshot, RepeatMode};
use art::ArtCache;
use dialogs::Dialog;
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "wav", "aif", "aiff", "m4a", "mp4", "m4b", "ogg", "oga",
];
const PLAYLIST_EXTENSIONS: &[&str] = &["m3u8", "m3u"];
const VOLUME_STEP: f32 = 0.05;

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
    rows: Vec<Row>,
    /// Inputs `rows` was built from: library generation, playlists generation, query, sort.
    built: Option<RowsStamp>,
    scroll_to: Option<usize>,
    total_ms: u64,
}

/// Things decided while drawing that are carried out after the frame's borrows end.
enum Action {
    PlayTracks(Vec<TrackId>, usize),
    PlayNext(Vec<TrackId>),
    Enqueue(Vec<TrackId>),
    AddToPlaylist(i64, Vec<TrackId>),
    NewPlaylistWith(Vec<TrackId>),
    SetFavorite(Vec<TrackId>, bool),
    Reveal(TrackId),
    ShowInfo(TrackId),
    RemoveFromPlaylist(i64, Vec<i64>),
    MovePlaylistItems(i64, Vec<i64>, usize),
    RemoveFromLibrary(Vec<TrackId>),
    Go(Nav),
    Sort(SortKey),
}

/// A message shown in the strip above the player bar.
struct Toast {
    text: String,
    warning: bool,
    action: Option<(String, ToastAction)>,
    /// `None` stays until dismissed.
    expires: Option<Instant>,
}

#[derive(Clone, Copy)]
enum ToastAction {
    RetryOutput,
    OpenPlaybackSettings,
}

/// Results of background work started by the UI.
enum AppEvent {
    FilesImported(Vec<TrackId>),
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
    UserGuide,
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
    playlists: Vec<PlaylistInfo>,
    playlists_generation: u64,
    player: PlayerSnapshot,
    last_notice_seq: u64,
    scan_was_running: bool,
    announce_scan: bool,

    nav: Nav,
    history: Vec<Nav>,
    views: HashMap<Nav, ListState>,
    focus_search: bool,
    /// Whether the search field had keyboard focus at the end of the previous frame.
    search_had_focus: bool,
    info_track: Option<TrackId>,
    /// Queue panel selection (queue item ids) and drag state.
    queue_selection: HashSet<u64>,
    queue_anchor: Option<u64>,
    queue_scroll_to_current: bool,
    seek_drag: Option<f64>,

    art: ArtCache,
    toast: Option<Toast>,
    dialog: Option<Dialog>,
    renaming: Option<(i64, String, bool)>,
    settings_open: bool,
    settings_tab: settings_window::Tab,
    devices: Vec<OutputDeviceInfo>,
    devices_checked: Option<Instant>,

    menu: Option<AppMenu>,
    automation: Option<automation::Automation>,
    ui_requests: Arc<Mutex<Vec<UiRequest>>>,
    events_tx: Sender<AppEvent>,
    events_rx: Receiver<AppEvent>,
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
        theme::install(&ctx);
        theme::apply_choice(&ctx, settings.theme);

        let repaint_ctx = ctx.clone();
        core.set_repaint(move || repaint_ctx.request_repaint());
        let bridge_ctx = ctx.clone();
        if !responder::install(move || bridge_ctx.request_repaint()) {
            tracing::warn!(
                "edit-menu bridge unavailable: copy/paste shortcuts may not reach text fields"
            );
        }
        if !app_hooks::install_now() {
            tracing::warn!("Finder open events are unavailable");
        }
        app::install_media_session(&core);

        let (events_tx, events_rx) = channel();
        let player = core.player.snapshot();
        let mut this = Self {
            perf: Perf::default(),
            art: ArtCache::new(core.library.clone(), ctx.clone()),
            snapshot: core.library.snapshot(),
            lib_generation: core.library.generation(),
            playlists: core.library.playlists(),
            playlists_generation: core.library.playlists_generation(),
            last_notice_seq: player.notice_seq,
            player,
            scan_was_running: false,
            announce_scan: false,
            nav: Nav::Songs,
            history: Vec::new(),
            views: HashMap::new(),
            focus_search: false,
            search_had_focus: false,
            info_track: None,
            queue_selection: HashSet::new(),
            queue_anchor: None,
            queue_scroll_to_current: true,
            seek_drag: None,
            toast: None,
            dialog: None,
            renaming: None,
            settings_open: false,
            settings_tab: settings_window::Tab::General,
            devices: Vec::new(),
            devices_checked: None,
            menu: None,
            automation: automation::Automation::from_env(),
            ui_requests: Arc::new(Mutex::new(Vec::new())),
            events_tx,
            events_rx,
            shut_down: false,
            saved_settings: settings.clone(),
            settings,
            lang,
            system_prefers_korean,
            core,
        };
        this.install_menu(&ctx);
        report_open_problems(&mut this, &open_report);
        this
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
        self.menu = AppMenu::install(self.s(), app::VERSION, move |action| {
            // Playback commands work even while the window is hidden; everything else
            // needs the window, so it is brought back first.
            let request = match action {
                MenuAction::PlayPause => return player.send(Command::TogglePlayPause),
                MenuAction::Next => return player.send(Command::Next),
                MenuAction::Previous => return player.send(Command::Previous),
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
                MenuAction::UserGuide => UiRequest::UserGuide,
            };
            if let Ok(mut queue) = requests.lock() {
                queue.push(request);
            }
            window::show_main_window();
            ctx.request_repaint();
        });
    }

    // ----- state synchronisation ------------------------------------------------------

    fn sync(&mut self, ctx: &egui::Context) {
        self.art.begin_frame(ctx);
        let generation = self.core.library.generation();
        if generation != self.lib_generation {
            self.lib_generation = generation;
            self.snapshot = self.core.library.snapshot();
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
        if self.player.current.map(|c| c.item_id) != previous_track {
            self.queue_scroll_to_current = true;
        }
        if self.player.notice_seq != self.last_notice_seq {
            self.last_notice_seq = self.player.notice_seq;
            if let Some(notice) = self.player.notice.clone() {
                self.show_notice(&notice);
            }
        }
        // A lost-device toast is obsolete once playback is running again.
        if !self.player.device_lost
            && self
                .toast
                .as_ref()
                .is_some_and(|t| matches!(t.action, Some((_, ToastAction::RetryOutput))))
            && self.player.state == PlayState::Playing
        {
            self.toast = None;
        }

        let scan = self.core.library.scan_status();
        // Only scans the user asked for are announced; background rescans stay quiet.
        if self.scan_was_running && !scan.running && std::mem::take(&mut self.announce_scan) {
            let text = self
                .lang
                .scan_finished(self.snapshot.len(), scan.unsupported, scan.errors);
            self.info_toast(text);
        }
        self.scan_was_running = scan.running;

        while let Ok(event) = self.events_rx.try_recv() {
            match event {
                AppEvent::FilesImported(ids) if ids.is_empty() => {
                    self.warn_toast(self.s().notice_no_playable_files)
                }
                AppEvent::FilesImported(ids) => {
                    self.core.player.send(Command::PlayTracks {
                        tracks: ids,
                        start_index: 0,
                    });
                }
            }
        }
        let opened = app_hooks::take_opened_paths();
        if !opened.is_empty() {
            self.open_paths(opened);
        }
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
        let requests: Vec<UiRequest> = self
            .ui_requests
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default();
        for request in requests {
            self.handle_request(request);
        }
        if self
            .toast
            .as_ref()
            .is_some_and(|t| t.expires.is_some_and(|e| Instant::now() >= e))
        {
            self.toast = None;
        }

        // Closing the window hides it; the app keeps running and playing (docs/UX.md).
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.persist_settings();
            window::hide_main_window();
        }
    }

    fn handle_request(&mut self, request: UiRequest) {
        match request {
            UiRequest::Settings => self.settings_open = true,
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
            UiRequest::UserGuide => {
                let guide = self.core.paths.data_dir.join("USER_GUIDE.md");
                let bundled = bundled_resource("USER_GUIDE.md");
                let target = bundled.filter(|p| p.exists()).unwrap_or(guide);
                if target.exists() {
                    finder::reveal(&target);
                }
            }
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
        }
    }

    // ----- navigation -----------------------------------------------------------------

    fn go(&mut self, nav: Nav) {
        if nav == self.nav {
            return;
        }
        let detail = matches!(nav, Nav::Album(_) | Nav::Artist(_));
        if detail {
            self.history.push(self.nav.clone());
        } else {
            self.history.clear();
        }
        self.nav = nav;
    }

    fn go_back(&mut self) {
        if let Some(previous) = self.history.pop() {
            self.nav = previous;
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
                self.toast = Some(Toast {
                    text: s.notice_too_many_failures.into(),
                    warning: true,
                    action: None,
                    expires: None,
                });
            }
            Notice::DeviceLost => {
                self.toast = Some(Toast {
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
                self.toast = Some(Toast {
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

    fn add_folder(&mut self, path: PathBuf) {
        match self.core.library.add_folder(&path) {
            Ok(_) => {
                self.announce_scan = true;
                let name = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                self.info_toast(self.lang.folder_added(&name));
            }
            Err(error) => self.library_error_toast(&error),
        }
    }

    fn pick_folder(&mut self) {
        if let Some(folder) = rfd::FileDialog::new()
            .set_title(self.s().dialog_choose_folder)
            .pick_folder()
        {
            self.add_folder(folder);
        }
    }

    fn pick_files(&mut self) {
        let s = self.s();
        let picked = rfd::FileDialog::new()
            .set_title(s.dialog_choose_files)
            .add_filter(s.dialog_filter_audio, AUDIO_EXTENSIONS)
            .add_filter(s.dialog_filter_playlist, PLAYLIST_EXTENSIONS)
            .pick_files();
        if let Some(files) = picked {
            self.open_paths(files);
        }
    }

    fn pick_playlist_to_import(&mut self) {
        let s = self.s();
        let picked = rfd::FileDialog::new()
            .add_filter(s.dialog_filter_playlist, PLAYLIST_EXTENSIONS)
            .pick_file();
        if let Some(file) = picked {
            self.import_playlist(file);
        }
    }

    fn import_playlist(&mut self, file: PathBuf) {
        match self.core.library.import_m3u8(&file) {
            Ok(result) => {
                self.info_toast(self.lang.playlist_import_result(
                    &result.name,
                    result.matched,
                    result.missing.len(),
                ));
                self.go(Nav::Playlist(result.playlist_id));
            }
            Err(error) => {
                tracing::warn!("playlist import failed: {error}");
                self.warn_toast(self.s().notice_import_failed);
            }
        }
    }

    fn export_playlist(&mut self, id: i64, name: &str) {
        let picked = rfd::FileDialog::new()
            .set_file_name(format!("{name}.m3u8"))
            .add_filter(self.s().dialog_filter_playlist, &["m3u8"])
            .save_file();
        let Some(dest) = picked else { return };
        match self.core.library.export_m3u8(id, &dest) {
            Ok(count) => self.info_toast(self.lang.playlist_exported(count)),
            Err(error) => {
                tracing::warn!("playlist export failed: {error}");
                self.warn_toast(self.s().notice_export_failed);
            }
        }
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
                let ids = library.import_files(&files);
                let _ = tx.send(AppEvent::FilesImported(ids));
                core.request_repaint();
            });
        if spawned.is_err() {
            self.warn_toast(self.s().notice_generic_error);
        }
    }

    // ----- actions --------------------------------------------------------------------

    fn apply(&mut self, action: Action) {
        let library = self.core.library.clone();
        match action {
            Action::PlayTracks(tracks, start_index) => {
                self.core.player.send(Command::PlayTracks {
                    tracks,
                    start_index,
                });
            }
            Action::PlayNext(tracks) => self.core.player.send(Command::PlayNext(tracks)),
            Action::Enqueue(tracks) => self.core.player.send(Command::Enqueue(tracks)),
            Action::AddToPlaylist(id, tracks) => {
                if let Err(error) = library.add_to_playlist(id, &tracks) {
                    self.library_error_toast(&error);
                }
            }
            Action::NewPlaylistWith(tracks) => {
                self.dialog = Some(Dialog::new_playlist(self.s().untitled_playlist, tracks));
            }
            Action::SetFavorite(tracks, favorite) => library.set_favorite(&tracks, favorite),
            Action::Reveal(id) => {
                if let Some(track) = self.snapshot.get(id) {
                    finder::reveal(&track.path);
                }
            }
            Action::ShowInfo(id) => {
                self.info_track = Some(id);
                self.settings.right_panel = RightPanel::Info;
            }
            Action::RemoveFromPlaylist(id, items) => {
                if let Err(error) = library.remove_playlist_items(id, &items) {
                    self.library_error_toast(&error);
                }
            }
            Action::MovePlaylistItems(id, items, to_index) => {
                if let Err(error) = library.move_playlist_items(id, &items, to_index) {
                    self.library_error_toast(&error);
                }
            }
            Action::RemoveFromLibrary(tracks) => self.dialog = Some(Dialog::RemoveTracks(tracks)),
            Action::Go(nav) => self.go(nav),
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
        self.snapshot.get(id)
    }

    fn persist_settings(&mut self) {
        if self.settings != self.saved_settings {
            if let Err(e) = crate::storage::settings::save_json_atomic(
                &self.core.paths.settings_file(),
                &self.settings,
            ) {
                tracing::warn!("could not save settings: {e}");
            }
            self.saved_settings = self.settings.clone();
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
        if self.dialog.is_some() {
            return;
        }
        let (space, escape) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Space),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if space && !typing {
            self.core.player.send(Command::TogglePlayPause);
        }
        if escape {
            // egui drops keyboard focus on Escape before the frame starts, so "was the
            // search field focused" has to come from the previous frame.
            if self.settings_open {
                self.settings_open = false;
            } else if self.search_had_focus
                && let Some(state) = self.views.get_mut(&self.nav)
            {
                state.query.clear();
            }
        }
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
                automation::AppStep::OsKey(code, cmd, shift) => {
                    if !crate::platform::macos::testing::post_key(code, cmd, shift) {
                        tracing::warn!("oskey: no key window");
                    }
                }
                automation::AppStep::InputSource(id) => {
                    crate::platform::macos::testing::select_input_source(&id);
                }
                automation::AppStep::Activate => window::show_main_window(),
                automation::AppStep::Hide => window::hide_main_window(),
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
        let title = |id: TrackId| self.snapshot.get(id).map(|t| t.title.clone());
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
            "output_device": p.output_device,
            "output_rate": p.output_sample_rate,
            "source": p.source.as_ref().map(|s| format!("{} {}Hz {}ch", s.codec, s.sample_rate, s.channels)),
            "device_lost": p.device_lost,
            "toast": self.toast.as_ref().map(|t| t.text.clone()),
            "dialog_open": self.dialog.is_some(),
            "settings_open": self.settings_open,
            "right_panel": format!("{:?}", self.settings.right_panel),
            "lang": format!("{:?}", self.lang),
            "dark": ctx.global_style().visuals.dark_mode,
            "text_focus": ctx.text_edit_focused(),
            "window_visible": window::is_main_window_visible(),
            "input_source": crate::platform::macos::testing::current_input_source(),
            "pixels_per_point": ctx.pixels_per_point(),
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
        self.persist_settings();
        app::remove_media_session();
        self.menu = None;
        self.core.shutdown();
        tracing::info!("OnEum stopped");
    }
}

fn report_open_problems(app: &mut OneumApp, report: &OpenReport) {
    if report.recovered_corrupt_db.is_some() {
        app.toast = Some(Toast {
            text: app.s().notice_db_recovered.into(),
            warning: true,
            action: None,
            expires: None,
        });
    }
}

pub(crate) fn next_repeat(mode: RepeatMode) -> RepeatMode {
    match mode {
        RepeatMode::Off => RepeatMode::All,
        RepeatMode::All => RepeatMode::One,
        RepeatMode::One => RepeatMode::Off,
    }
}

/// Path of a file in the app bundle's `Resources` folder, when running from a bundle.
pub(crate) fn bundled_resource(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.parent()?.join("Resources").join(name))
}

impl eframe::App for OneumApp {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        if let Some(automation) = self.automation.as_mut() {
            automation.step(raw);
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
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let frame_started = Instant::now();
        if self.perf.first_frame_ms.is_none() {
            self.perf.first_frame_ms = Some(crate::app::process_uptime().as_secs_f64() * 1000.0);
        }
        let ctx = ui.ctx().clone();
        self.sync(&ctx);
        self.global_shortcuts(&ctx);
        let mut actions: Vec<Action> = Vec::new();

        self.top_bar(ui);
        self.player_bar(ui);
        self.toast_bar(ui);
        self.sidebar(ui, &mut actions);
        self.right_panel(ui, &mut actions);
        egui::CentralPanel::default().show(ui, |ui| self.central(ui, &mut actions));

        self.settings_window(&ctx);
        self.dialogs(&ctx);
        for action in actions {
            self.apply(action);
        }

        // Keep the progress display moving while playing; otherwise draw only on demand.
        if matches!(
            self.player.state,
            PlayState::Playing | PlayState::Loading | PlayState::Seeking
        ) {
            ctx.request_repaint_after(Duration::from_millis(250));
        } else if self.core.library.scan_status().running {
            ctx.request_repaint_after(Duration::from_millis(500));
        } else if let Some(expires) = self.toast.as_ref().and_then(|t| t.expires) {
            ctx.request_repaint_after(
                expires.saturating_duration_since(Instant::now()) + Duration::from_millis(50),
            );
        }
        self.persist_settings();
        let frame_ms = frame_started.elapsed().as_secs_f64() * 1000.0;
        self.perf.last_frame_ms = frame_ms;
        self.perf.max_frame_ms = self.perf.max_frame_ms.max(frame_ms);
        self.perf.frames += 1;
        self.run_automation(&ctx);
    }

    fn on_exit(&mut self) {
        self.shutdown();
    }
}

impl Drop for OneumApp {
    fn drop(&mut self) {
        self.shutdown();
    }
}
