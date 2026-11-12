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
    /// Row the keyboard cursor is on; a shift-range spans from `anchor` to here.
    cursor: Option<u64>,
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
    ToggleMute,
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
    playlists: Vec<PlaylistInfo>,
    playlists_generation: u64,
    player: PlayerSnapshot,
    last_notice_seq: u64,
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
    seek_drag: Option<f64>,
    /// A seek that was sent but is not yet reflected in the player snapshot:
    /// `(target seconds, when)`. The thumb stays there instead of jumping back.
    seek_pending: Option<(f64, Instant)>,
    last_settings_save: Instant,

    art: ArtCache,
    /// Transient message.
    toast: Option<Toast>,
    /// Message that stays until dismissed or resolved (lost device, …). A transient
    /// message is shown in front of it for its few seconds and never replaces it.
    alert: Option<Toast>,
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
            seek_drag: None,
            seek_pending: None,
            last_settings_save: Instant::now(),
            toast: None,
            alert: None,
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
                MenuAction::ToggleMute => UiRequest::ToggleMute,
                MenuAction::SongPlay => UiRequest::SongPlay,
                MenuAction::SongPlayNext => UiRequest::SongPlayNext,
                MenuAction::SongAddToQueue => UiRequest::SongAddToQueue,
                MenuAction::SongToggleFavorite => UiRequest::SongToggleFavorite,
                MenuAction::SongShowInFinder => UiRequest::SongShowInFinder,
                MenuAction::SongInfo => UiRequest::SongInfo,
            };
            if let Ok(mut queue) = requests.lock() {
                queue.push(request);
            }
            window::show_main_window();
            ctx.request_repaint();
        });
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
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.persist_settings();
            window::hide_main_window();
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
        if matches!(self.nav, Nav::Albums | Nav::Artists | Nav::Artist(_)) {
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

    /// Undoes "remove from playlist": the playlist gets its previous contents back.
    fn restore_playlist(&mut self, id: i64, tracks: &[TrackId]) {
        let library = self.core.library.clone();
        let current: Vec<i64> = library
            .playlist_items(id)
            .iter()
            .map(|i| i.item_id)
            .collect();
        let result = library
            .remove_playlist_items(id, &current)
            .and_then(|()| library.add_to_playlist(id, tracks));
        if let Err(error) = result {
            self.library_error_toast(&error);
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
            UiRequest::ToggleMute => self.toggle_mute(),
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

    fn go(&mut self, nav: Nav) {
        if nav == self.nav {
            return;
        }
        self.key_pane = KeyPane::List;
        self.info_track = None;
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
                // Removing is a single key press, so it must be undoable.
                let before: Vec<TrackId> = library
                    .playlist_items(id)
                    .into_iter()
                    .map(|i| i.track_id)
                    .collect();
                match library.remove_playlist_items(id, &items) {
                    Ok(()) => {
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
                    Err(error) => self.library_error_toast(&error),
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

    /// Remembers the window size (saved with the other settings, at most every few seconds).
    fn track_window_size(&mut self, ctx: &egui::Context) {
        let Some(rect) = ctx.input(|i| i.viewport().inner_rect) else {
            return;
        };
        let size = [rect.width().round(), rect.height().round()];
        if self.settings.window_size != Some(size) && size[0] >= 100.0 && size[1] >= 100.0 {
            self.settings.window_size = Some(size);
        }
        if self.settings == self.saved_settings {
            return;
        }
        // Preference changes are saved at once; a window being resized only now and then.
        let only_size_changed = Settings {
            window_size: self.saved_settings.window_size,
            ..self.settings.clone()
        } == self.saved_settings;
        if !only_size_changed || self.last_settings_save.elapsed() > Duration::from_secs(2) {
            self.persist_settings();
        }
    }

    fn persist_settings(&mut self) {
        self.last_settings_save = Instant::now();
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
        self.typing = typing;
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
        // With a control focused, Space presses that control (egui does that itself).
        if space && !typing && !self.widget_focused {
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
                automation::AppStep::OsKey(code, cmd, shift) => {
                    if !crate::platform::macos::testing::post_key(code, cmd, shift) {
                        tracing::warn!("oskey: no key window");
                    }
                }
                automation::AppStep::InputSource(id) => {
                    crate::platform::macos::testing::select_input_source(&id);
                }
                automation::AppStep::Menu(id) => {
                    let request = match id.as_str() {
                        "song_play" => Some(UiRequest::SongPlay),
                        "song_play_next" => Some(UiRequest::SongPlayNext),
                        "song_add_to_queue" => Some(UiRequest::SongAddToQueue),
                        "song_toggle_favorite" => Some(UiRequest::SongToggleFavorite),
                        "song_info" => Some(UiRequest::SongInfo),
                        "toggle_mute" => Some(UiRequest::ToggleMute),
                        "toggle_queue" => Some(UiRequest::ToggleQueue),
                        "toggle_info" => Some(UiRequest::ToggleInfo),
                        "go_to_current" => Some(UiRequest::GoToCurrent),
                        _ => None,
                    };
                    match request {
                        Some(request) => self.handle_request(request),
                        None => tracing::warn!("menu: unknown id"),
                    }
                }
                automation::AppStep::Activate => window::show_main_window(),
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
            "start_latency_ms": p.last_start_latency_ms,
            "output_device": p.output_device,
            "output_rate": p.output_sample_rate,
            "source": p.source.as_ref().map(|s| format!("{} {}Hz {}ch", s.codec, s.sample_rate, s.channels)),
            "device_lost": p.device_lost,
            "toast": self.toast.as_ref().or(self.alert.as_ref()).map(|t| t.text.clone()),
            "dialog_open": self.dialog.is_some(),
            "settings_open": self.settings_open,
            "right_panel": format!("{:?}", self.settings.right_panel),
            "lang": format!("{:?}", self.lang),
            "dark": ctx.global_style().visuals.dark_mode,
            "text_focus": ctx.text_edit_focused(),
            "window_visible": window::is_main_window_visible(),
            "input_source": crate::platform::macos::testing::current_input_source(),
            "pixels_per_point": ctx.pixels_per_point(),
            "window_size": ctx.input(|i| i.viewport().inner_rect).map(|r| [r.width(), r.height()]),
        });
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
        app.alert = Some(Toast {
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

    /// Runs for every pass, also while the window is hidden or fully covered (then
    /// without a UI pass): the close button, Finder opens and menu commands must not
    /// wait for the window to become visible again.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.sync(ctx);
        self.run_automation(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let frame_started = Instant::now();
        if self.perf.first_frame_ms.is_none() {
            self.perf.first_frame_ms = Some(crate::app::process_uptime().as_secs_f64() * 1000.0);
        }
        let ctx = ui.ctx().clone();
        self.sync_frame(&ctx);
        self.global_shortcuts(&ctx);
        let mut actions: Vec<Action> = Vec::new();

        self.top_bar(ui);
        self.player_bar(ui);
        self.toast_bar(ui);
        self.sidebar(ui, &mut actions);
        self.right_panel(ui, &mut actions);
        egui::CentralPanel::default().show(ui, |ui| self.central(ui, &mut actions));

        self.drop_overlay(&ctx);
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

    fn on_exit(&mut self) {
        self.shutdown();
    }
}

impl Drop for OneumApp {
    fn drop(&mut self) {
        self.shutdown();
    }
}
