//! The phone screens: one narrow screen that is touched.
//!
//! The same app as on a desk (the same library and player snapshots, the same actions),
//! laid out for a phone: tabs at the bottom, a stack of pages in each, the player as a
//! bar above the tabs that opens to the full screen, and sheets from below for what a
//! right-click offers on a desk. Rows are tall enough to hit with a finger, and nothing
//! depends on a pointer hovering or on a keyboard.
//!
//! The module is compiled everywhere: `ONEUM_PHONE=1` shows these screens in a window on
//! a Mac, which is how they are worked on and checked without a phone.
use super::dialogs::Dialog;
use super::icons::{self, Icon};
use super::lists::{self, Rediscover};
use super::{Action, Nav, OneumApp, Row, ToastAction, next_repeat, theme, widgets};
use crate::i18n::format_time;
use crate::model::{Track, TrackId};
use crate::playback::{Command, PlayState, RepeatMode};
use eframe::egui::{
    self, Align, Align2, Color32, FontFamily, FontId, Layout, Rect, RichText, Sense, Ui, UiBuilder,
    pos2, vec2,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Height of the bar at the top of a page: where one is and the way back.
const HEADER: f32 = 50.0;
/// Height of the tab bar, above the home indicator.
const TAB_BAR: f32 = 54.0;
/// Height of the bar of the playing song above the tabs.
const MINI: f32 = 62.0;
/// A row with a cover and two lines of text.
const ROW: f32 = 62.0;
/// A row with one line of text and an icon.
const MENU_ROW: f32 = 54.0;
/// Space kept free at the sides of a page.
const SIDE: f32 = 16.0;
/// The smallest thing a finger is asked to hit.
const TOUCH: f32 = 44.0;

fn font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tab {
    Library,
    Search,
    Playlists,
    Settings,
}

const TABS: [Tab; 4] = [Tab::Library, Tab::Search, Tab::Playlists, Tab::Settings];

/// A page above the first one of a tab.
#[derive(Debug, Clone, PartialEq)]
enum Page {
    /// A view of the library, as on a desk.
    List(Nav),
    Settings(SettingsPage),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsPage {
    Library,
    Playback,
    Equalizer,
    General,
    Guide,
    About,
}

/// What the search looks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
enum SearchKind {
    #[default]
    Songs,
    Albums,
    Artists,
}

/// What a sheet that came up from below is about.
#[derive(Debug, Clone, PartialEq)]
enum Sheet {
    /// A song: in a playlist (playlist, item) or in the queue (item), if it was
    /// reached there.
    Track {
        track: TrackId,
        playlist: Option<(i64, i64)>,
        queue_item: Option<u64>,
    },
    /// Which playlist to add these songs to.
    AddToPlaylist(Vec<TrackId>),
    Album(u64),
    Playlist(i64),
    Smart(String),
    /// The sleep timer's choices.
    Sleep,
    Info(TrackId),
}

/// The state of the phone screens.
pub(super) struct Phone {
    tab: Tab,
    /// The pages above the first one of each tab, in the order of [`TABS`].
    stacks: [Vec<Page>; 4],
    player_open: bool,
    queue_open: bool,
    sheet: Option<Sheet>,
    query: String,
    search_kind: SearchKind,
    search_rows: Vec<Row>,
    /// What `search_rows` was made from: library and playlist generations, query, kind.
    search_built: Option<(u64, u64, String, SearchKind)>,
    focus_search: bool,
    /// The view the rest of the app last asked for (`OneumApp::go`), as it was followed.
    seen_nav: Nav,
}

impl Phone {
    pub(super) fn new() -> Self {
        Self {
            tab: Tab::Library,
            stacks: Default::default(),
            player_open: false,
            queue_open: false,
            sheet: None,
            query: String::new(),
            search_kind: SearchKind::default(),
            search_rows: Vec::new(),
            search_built: None,
            focus_search: false,
            seen_nav: Nav::Songs,
        }
    }

    fn stack(&mut self) -> &mut Vec<Page> {
        &mut self.stacks[TABS.iter().position(|t| *t == self.tab).unwrap_or(0)]
    }

    fn page(&self) -> Option<&Page> {
        self.stacks[TABS.iter().position(|t| *t == self.tab).unwrap_or(0)].last()
    }

    /// What is on screen, for verification dumps.
    pub(super) fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "tab": format!("{:?}", self.tab),
            "page": self.page().map(|page| format!("{page:?}")),
            "depth": self.stacks.iter().map(Vec::len).collect::<Vec<_>>(),
            "player_open": self.player_open,
            "queue_open": self.queue_open,
            "sheet": self.sheet.as_ref().map(|sheet| format!("{sheet:?}")),
            "query": self.query,
            "search_rows": self.search_rows.len(),
        })
    }
}

/// Lays out `count` rows of `height` below what is there and calls `row` for those in
/// view, each with a `Ui` of its own.
///
/// `list` names the list: two lists on one page (the folders of a folder above its
/// songs, playlists above smart playlists) must not share the ids of their rows.
fn virtual_rows(
    ui: &mut Ui,
    list: &'static str,
    count: usize,
    height: f32,
    mut row: impl FnMut(&mut Ui, usize),
) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, count as f32 * height), Sense::hover());
    let clip = ui.clip_rect();
    let first = ((clip.top() - rect.top()) / height).floor().max(0.0) as usize;
    let last = (((clip.bottom() - rect.top()) / height).ceil().max(0.0) as usize).min(count);
    for index in first..last {
        let row_rect = Rect::from_min_size(
            pos2(rect.left(), rect.top() + index as f32 * height),
            vec2(width, height),
        );
        let mut child = ui.new_child(
            UiBuilder::new()
                .id_salt((list, index))
                .max_rect(row_rect)
                .layout(Layout::left_to_right(Align::Center)),
        );
        row(&mut child, index);
    }
}

/// A row that can be tapped: the response of the whole row, and whether the finger
/// rests on it.
fn row_response(ui: &mut Ui, id_salt: &'static str) -> egui::Response {
    let rect = ui.max_rect();
    let response = ui.interact(rect, ui.scope_id().with(id_salt), Sense::click());
    if response.is_pointer_button_down_on() {
        ui.painter()
            .rect_filled(rect, 0.0, ui.visuals().widgets.hovered.weak_bg_fill);
    }
    response
}

/// The hairline below a row, starting at `left`.
fn row_line(ui: &Ui, left: f32) {
    let rect = ui.max_rect();
    ui.painter().hline(
        left..=rect.right(),
        rect.bottom() - 0.5,
        egui::Stroke::new(1.0, widgets::divider_color(ui)),
    );
}

/// Two lines of text in `rect`, each cut off with an ellipsis: a title and what is
/// said below it.
fn two_lines(ui: &mut Ui, rect: Rect, title: RichText, below: Option<RichText>) {
    let lines = if below.is_some() { 2.0 } else { 1.0 };
    let block = 21.0 + (lines - 1.0) * 19.0;
    let text_rect = Rect::from_min_max(
        pos2(rect.left(), rect.center().y - block / 2.0),
        pos2(rect.right().max(rect.left() + 20.0), rect.bottom()),
    );
    let mut text = ui.new_child(
        UiBuilder::new()
            .max_rect(text_rect)
            .layout(Layout::top_down(Align::Min)),
    );
    text.spacing_mut().item_spacing.y = 1.0;
    text.add(egui::Label::new(title).truncate());
    if let Some(below) = below {
        text.add(egui::Label::new(below).truncate());
    }
}

/// A round "more" button at the right end of a row. True when it was tapped.
fn more_button(ui: &mut Ui, label: &str, rect: Rect) -> bool {
    let button = Rect::from_center_size(
        pos2(rect.right() - SIDE - 14.0, rect.center().y),
        egui::Vec2::splat(TOUCH),
    );
    let response = ui.interact(button, ui.scope_id().with("more"), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    let color = if response.is_pointer_button_down_on() {
        ui.visuals().strong_text_color()
    } else {
        theme::dim(ui)
    };
    icons::paint(
        ui.painter(),
        Rect::from_center_size(button.center(), egui::Vec2::splat(22.0)),
        Icon::More,
        color,
    );
    response.clicked()
}

/// A filled button with an icon and a word, as wide as `width`.
fn pill_button(ui: &mut Ui, icon: Icon, label: &str, width: f32, filled: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 46.0), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    let dark = ui.visuals().dark_mode;
    let accent = theme::accent(dark);
    let (fill, ink) = if filled {
        let ink = if dark {
            Color32::from_rgb(0x14, 0x14, 0x18)
        } else {
            Color32::WHITE
        };
        (accent, ink)
    } else {
        (ui.visuals().faint_bg_color, accent)
    };
    let fill = if response.is_pointer_button_down_on() {
        fill.gamma_multiply(0.8)
    } else {
        fill
    };
    ui.painter().rect_filled(rect, 12.0, fill);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), theme::semibold(16.0), ink);
    let content = 18.0 + 8.0 + galley.size().x;
    let left = rect.center().x - content / 2.0;
    icons::paint(
        ui.painter(),
        Rect::from_min_size(pos2(left, rect.center().y - 9.0), egui::Vec2::splat(18.0)),
        icon,
        ink,
    );
    ui.painter().galley(
        pos2(left + 26.0, rect.center().y - galley.size().y / 2.0),
        galley,
        ink,
    );
    response
}

/// What a song row says below its title.
fn artist_line<'a>(track: &'a Track, unknown: &'a str) -> &'a str {
    if track.artist.is_empty() {
        unknown
    } else {
        &track.artist
    }
}

impl OneumApp {
    /// One frame of the phone screens.
    pub(super) fn phone_frame(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.phone_follow_nav();
        let full = ctx.viewport_rect();
        let safe = ctx.content_rect();
        ui.painter().rect_filled(full, 0.0, ui.visuals().panel_fill);
        let mut actions: Vec<Action> = Vec::new();

        if self.player.current.is_none() {
            self.phone.player_open = false;
        }
        let mut above_bars = safe.bottom();
        // The on-screen keyboard covers the lower end of the screen: while it is up
        // the page ends above it, and the bars it would cover are left out.
        let keyboard = crate::platform::window::keyboard_height();
        if self.phone.player_open {
            self.phone_player(ui, safe, &mut actions);
        } else {
            let tab_bar = Rect::from_min_max(pos2(full.left(), safe.bottom() - TAB_BAR), full.max);
            let mini = (self.player.current.is_some() && keyboard <= 0.0).then(|| {
                Rect::from_min_max(
                    pos2(full.left(), tab_bar.top() - MINI),
                    pos2(full.right(), tab_bar.top()),
                )
            });
            let content_bottom = if keyboard > 0.0 {
                full.bottom() - keyboard
            } else {
                mini.map_or(tab_bar.top(), |mini| mini.top())
            };
            above_bars = content_bottom;
            let header = Rect::from_min_max(safe.min, pos2(safe.right(), safe.top() + HEADER));
            let content = Rect::from_min_max(
                pos2(safe.left(), header.bottom()),
                pos2(safe.right(), content_bottom),
            );
            self.phone_page(ui, header, content, &mut actions);
            if let Some(mini) = mini {
                self.phone_mini_player(ui, mini);
            }
            if keyboard <= 0.0 {
                self.phone_tab_bar(ui, tab_bar);
            }
        }
        self.phone_toast(&ctx, safe, above_bars);
        self.phone_sheet(&ctx, &mut actions);
        self.dialogs(&ctx);
        for action in actions {
            match action {
                Action::Go(nav) => self.phone_open(nav),
                Action::ShowInfo(track) => self.phone.sheet = Some(Sheet::Info(track)),
                other => self.apply(other),
            }
        }
    }

    /// Shows `nav` as a page above the ones of the tab it belongs to.
    fn phone_open(&mut self, nav: Nav) {
        self.phone.player_open = false;
        self.phone.sheet = None;
        if matches!(nav, Nav::Playlist(_) | Nav::Smart(_)) {
            self.phone.tab = Tab::Playlists;
        } else if self.phone.tab == Tab::Settings || self.phone.tab == Tab::Playlists {
            self.phone.tab = Tab::Library;
        }
        let page = Page::List(nav.clone());
        let stack = self.phone.stack();
        if stack.last() != Some(&page) {
            stack.push(page);
        }
        self.nav = nav.clone();
        self.phone.seen_nav = nav;
    }

    /// Follows what the rest of the app did to `nav` (a playlist was made or imported:
    /// it is shown; one was deleted: its page goes).
    fn phone_follow_nav(&mut self) {
        let playlists: Vec<i64> = self.playlists.iter().map(|p| p.id).collect();
        let smart: Vec<&str> = self
            .settings
            .smart_playlists
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        for stack in &mut self.phone.stacks {
            stack.retain(|page| match page {
                Page::List(Nav::Playlist(id)) => playlists.contains(id),
                Page::List(Nav::Smart(name)) => smart.contains(&name.as_str()),
                _ => true,
            });
        }
        if self.nav == self.phone.seen_nav {
            return;
        }
        let nav = self.nav.clone();
        self.phone.seen_nav = nav.clone();
        // "All songs" is where the desk goes when the open view is gone: here that
        // page has gone already.
        if nav != Nav::Songs {
            self.phone_open(nav);
        }
    }

    // ----- pages ------------------------------------------------------------------------

    fn phone_page(&mut self, ui: &mut Ui, header: Rect, content: Rect, actions: &mut Vec<Action>) {
        let s = self.s();
        let page = self.phone.page().cloned();
        let title: String = match &page {
            None => match self.phone.tab {
                Tab::Library => s.phone_library.into(),
                Tab::Search => s.search_label.into(),
                Tab::Playlists => s.playlists.into(),
                Tab::Settings => s.settings.into(),
            },
            Some(Page::List(nav)) => self.phone_nav_title(nav),
            Some(Page::Settings(page)) => match page {
                SettingsPage::Library => s.phone_library,
                SettingsPage::Playback => s.settings_playback,
                SettingsPage::Equalizer => s.equalizer,
                SettingsPage::General => s.settings_general,
                SettingsPage::Guide => s.menu_user_guide,
                SettingsPage::About => s.settings_about,
            }
            .into(),
        };

        // The bar at the top: the way back, where one is, and what can be done here.
        let mut left = header.left() + SIDE;
        if page.is_some() {
            let back = Rect::from_min_size(
                pos2(header.left() + 4.0, header.center().y - TOUCH / 2.0),
                egui::Vec2::splat(TOUCH),
            );
            let response = ui.interact(back, ui.scope_id().with("phone-back"), Sense::click());
            response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, s.back));
            icons::paint(
                ui.painter(),
                Rect::from_center_size(back.center(), egui::Vec2::splat(22.0)),
                Icon::Back,
                theme::accent(ui.visuals().dark_mode),
            );
            if response.clicked() {
                self.phone.stack().pop();
                return;
            }
            left = back.right();
        }
        let mut right = header.right() - 4.0;
        let more: Option<Sheet> = match &page {
            Some(Page::List(Nav::Playlist(id))) => Some(Sheet::Playlist(*id)),
            Some(Page::List(Nav::Smart(name))) => Some(Sheet::Smart(name.clone())),
            Some(Page::List(Nav::Album(key))) => Some(Sheet::Album(*key)),
            _ => None,
        };
        if let Some(sheet) = more {
            let button = Rect::from_min_size(
                pos2(right - TOUCH, header.center().y - TOUCH / 2.0),
                egui::Vec2::splat(TOUCH),
            );
            let response = ui.interact(
                button,
                ui.scope_id().with("phone-page-more"),
                Sense::click(),
            );
            response
                .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, s.phone_more));
            icons::paint(
                ui.painter(),
                Rect::from_center_size(button.center(), egui::Vec2::splat(22.0)),
                Icon::More,
                theme::accent(ui.visuals().dark_mode),
            );
            if response.clicked() {
                self.phone.sheet = Some(sheet);
            }
            right = button.left();
        } else if page.is_none() && self.phone.tab == Tab::Playlists {
            let button = Rect::from_min_size(
                pos2(right - TOUCH, header.center().y - TOUCH / 2.0),
                egui::Vec2::splat(TOUCH),
            );
            let response = ui.interact(
                button,
                ui.scope_id().with("phone-new-playlist"),
                Sense::click(),
            );
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, true, s.new_playlist)
            });
            icons::paint(
                ui.painter(),
                Rect::from_center_size(button.center(), egui::Vec2::splat(22.0)),
                Icon::Plus,
                theme::accent(ui.visuals().dark_mode),
            );
            if response.clicked() {
                self.dialog = Some(Dialog::new_playlist(s.untitled_playlist, Vec::new()));
            }
            right = button.left();
        }
        let size = if page.is_none() { 26.0 } else { 18.0 };
        let title_rect =
            Rect::from_min_max(pos2(left, header.top()), pos2(right - 4.0, header.bottom()));
        let mut title_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(title_rect)
                .layout(Layout::left_to_right(Align::Center)),
        );
        title_ui.add(
            egui::Label::new(
                RichText::new(title)
                    .font(theme::semibold(size))
                    .color(ui.visuals().strong_text_color()),
            )
            .truncate(),
        );

        let mut body = ui.new_child(
            UiBuilder::new()
                .id_salt("phone-body")
                .max_rect(content)
                .layout(Layout::top_down(Align::Min)),
        );
        body.set_clip_rect(content);
        match page {
            None => match self.phone.tab {
                Tab::Library => self.phone_library(&mut body, actions),
                Tab::Search => self.phone_search(&mut body, actions),
                Tab::Playlists => self.phone_playlists(&mut body, actions),
                Tab::Settings => self.phone_settings(&mut body),
            },
            Some(Page::List(nav)) => self.phone_list(&mut body, &nav, actions),
            Some(Page::Settings(page)) => self.phone_settings_page(&mut body, page),
        }
    }

    fn phone_nav_title(&self, nav: &Nav) -> String {
        let s = self.s();
        match nav {
            Nav::Songs => s.phone_songs.into(),
            Nav::Albums => s.albums.into(),
            Nav::Artists => s.artists.into(),
            Nav::Genres => s.genres.into(),
            Nav::Favorites => s.favorites.into(),
            Nav::Recent => s.recently_played.into(),
            Nav::Stats => s.stats.into(),
            Nav::Album(key) => self
                .snapshot
                .album_index(*key)
                .and_then(|i| self.snapshot.albums().get(i as usize))
                .map(|album| lists::album_title(album, &self.snapshot, s.unknown_album))
                .unwrap_or(s.unknown_album)
                .to_string(),
            Nav::Artist(name) if name.is_empty() => s.unknown_artist.into(),
            Nav::Artist(name) | Nav::Genre(name) | Nav::Smart(name) => name.clone(),
            Nav::Playlist(id) => self
                .playlists
                .iter()
                .find(|p| p.id == *id)
                .map(|p| p.name.clone())
                .unwrap_or_default(),
            Nav::Folder(folder) => folder.file_name().map_or_else(
                || s.folders.to_string(),
                |n| n.to_string_lossy().into_owned(),
            ),
        }
    }

    /// The first page of the library tab: the ways into the library.
    fn phone_library(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let s = self.s();
        let scan = self.core.library.scan_status();
        if self.snapshot.is_empty() && !scan.running {
            self.phone_empty_library(ui);
            return;
        }
        // The folder view starts in the music folder itself when there is only one.
        let roots = self.core.library.roots();
        let folders = match roots.as_slice() {
            [only] => Nav::Folder(only.path.clone()),
            _ => Nav::Folder(std::path::PathBuf::new()),
        };
        let favorites = self.snapshot.tracks.iter().filter(|t| t.favorite).count();
        let entries: [(Icon, &str, Option<usize>, Nav); 8] = [
            (
                Icon::Note,
                s.phone_songs,
                Some(self.snapshot.len()),
                Nav::Songs,
            ),
            (
                Icon::Album,
                s.albums,
                Some(self.snapshot.albums().len()),
                Nav::Albums,
            ),
            (
                Icon::Person,
                s.artists,
                Some(self.snapshot.artists().len()),
                Nav::Artists,
            ),
            (Icon::Tag, s.genres, None, Nav::Genres),
            (Icon::Heart, s.favorites, Some(favorites), Nav::Favorites),
            (Icon::Clock, s.recently_played, None, Nav::Recent),
            (Icon::Folder, s.folders, None, folders),
            (Icon::Meter, s.stats, None, Nav::Stats),
        ];
        egui::ScrollArea::vertical()
            .id_salt("phone-library")
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                virtual_rows(ui, "library", entries.len(), MENU_ROW, |ui, index| {
                    let (icon, label, count, nav) = &entries[index];
                    let count = count.map(crate::i18n::group_digits);
                    if self.phone_menu_row(ui, *icon, label, count.as_deref(), true) {
                        actions.push(Action::Go(nav.clone()));
                    }
                });
                if scan.running {
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        ui.add_space(SIDE);
                        ui.add(egui::Spinner::new().size(16.0));
                        ui.label(
                            RichText::new(self.lang.scan_progress(scan.files_found))
                                .font(font(14.0))
                                .color(theme::dim(ui)),
                        );
                    });
                }
            });
    }

    /// A row with an icon, a word, a number at the right, and an arrow if it leads on.
    fn phone_menu_row(
        &self,
        ui: &mut Ui,
        icon: Icon,
        label: &str,
        detail: Option<&str>,
        leads_on: bool,
    ) -> bool {
        let rect = ui.max_rect();
        let response = row_response(ui, "menu");
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
        let dark = ui.visuals().dark_mode;
        icons::paint(
            ui.painter(),
            Rect::from_center_size(
                pos2(rect.left() + SIDE + 12.0, rect.center().y),
                egui::Vec2::splat(22.0),
            ),
            icon,
            theme::accent(dark),
        );
        let mut right = rect.right() - SIDE;
        if leads_on {
            icons::paint(
                ui.painter(),
                Rect::from_center_size(pos2(right - 6.0, rect.center().y), egui::Vec2::splat(14.0)),
                Icon::Forward,
                ui.visuals().weak_text_color(),
            );
            right -= 22.0;
        }
        if let Some(detail) = detail {
            let galley = ui.painter().layout_no_wrap(
                detail.to_string(),
                font(15.0),
                ui.visuals().weak_text_color(),
            );
            right -= galley.size().x;
            ui.painter().galley(
                pos2(right, rect.center().y - galley.size().y / 2.0),
                galley,
                ui.visuals().weak_text_color(),
            );
            right -= 8.0;
        }
        let text = Rect::from_min_max(
            pos2(rect.left() + SIDE + 40.0, rect.top()),
            pos2(right, rect.bottom()),
        );
        two_lines(
            ui,
            text,
            RichText::new(label)
                .font(font(17.0))
                .color(ui.visuals().strong_text_color()),
            None,
        );
        row_line(ui, rect.left() + SIDE + 40.0);
        response.clicked()
    }

    /// Nothing in the library: where music goes and how it gets there.
    fn phone_empty_library(&mut self, ui: &mut Ui) {
        let s = self.s();
        let mut import_files = false;
        let mut import_folder = false;
        let mut rescan = false;
        egui::ScrollArea::vertical()
            .id_salt("phone-empty")
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space((ui.available_height() * 0.10).max(16.0));
                    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(64.0), Sense::hover());
                    icons::paint(
                        ui.painter(),
                        rect,
                        Icon::Note,
                        theme::accent(ui.visuals().dark_mode),
                    );
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new(s.phone_empty_title)
                            .font(theme::semibold(22.0))
                            .color(ui.visuals().strong_text_color()),
                    );
                    ui.add_space(6.0);
                    let width = (ui.available_width() - 2.0 * 28.0).max(120.0);
                    ui.allocate_ui(vec2(width, 0.0), |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(s.phone_empty_body)
                                    .font(font(15.0))
                                    .color(theme::dim(ui)),
                            )
                            .wrap()
                            .halign(Align::Center),
                        );
                    });
                    ui.add_space(22.0);
                    let width = (ui.available_width() - 2.0 * 28.0).clamp(160.0, 320.0);
                    import_files =
                        pill_button(ui, Icon::Plus, s.phone_import_files, width, true).clicked();
                    ui.add_space(10.0);
                    import_folder =
                        pill_button(ui, Icon::Folder, s.phone_import_folder, width, false)
                            .clicked();
                    ui.add_space(10.0);
                    rescan = pill_button(ui, Icon::Repeat, s.rescan, width, false).clicked();
                });
            });
        if import_files {
            self.import_music(crate::platform::PickKind::Files);
        }
        if import_folder {
            self.import_music(crate::platform::PickKind::Folder);
        }
        if rescan {
            self.announce_scan = true;
            self.core.library.rescan();
        }
    }

    /// A view of the library.
    fn phone_list(&mut self, ui: &mut Ui, nav: &Nav, actions: &mut Vec<Action>) {
        self.nav = nav.clone();
        self.phone.seen_nav = nav.clone();
        self.ensure_rows(nav);
        let rows = self
            .views
            .get_mut(nav)
            .map(|state| std::mem::take(&mut state.rows))
            .unwrap_or_default();
        match nav {
            Nav::Stats => {
                egui::ScrollArea::vertical()
                    .id_salt("phone-stats")
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.add_space(SIDE - 4.0);
                            ui.vertical(|ui| {
                                ui.set_width(ui.available_width() - SIDE);
                                self.stats_view(ui, actions);
                            });
                        });
                    });
            }
            Nav::Albums | Nav::Artist(_) => {
                egui::ScrollArea::vertical()
                    .id_salt(("phone-list", nav))
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        if rows.is_empty() {
                            self.empty_list_message(ui, nav, false);
                        }
                        self.phone_album_grid(ui, &rows, actions);
                    });
            }
            Nav::Artists => {
                egui::ScrollArea::vertical()
                    .id_salt(("phone-list", nav))
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        if rows.is_empty() {
                            self.empty_list_message(ui, nav, false);
                        }
                        self.phone_artist_rows(ui, &rows, actions);
                    });
            }
            Nav::Genres => {
                egui::ScrollArea::vertical()
                    .id_salt(("phone-list", nav))
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        if rows.is_empty() {
                            self.empty_list_message(ui, nav, false);
                        }
                        virtual_rows(ui, "genres", rows.len(), MENU_ROW, |ui, index| {
                            let Some(genre) = self.genre_list.get(rows[index].idx as usize) else {
                                return;
                            };
                            let count = self.lang.song_count(genre.songs);
                            if self.phone_menu_row(ui, Icon::Tag, &genre.name, Some(&count), true) {
                                actions.push(Action::Go(Nav::Genre(genre.name.clone())));
                            }
                        });
                    });
            }
            _ => {
                egui::ScrollArea::vertical()
                    .id_salt(("phone-list", nav))
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        self.phone_list_head(ui, nav, &rows, actions);
                        self.phone_track_rows(ui, nav, &rows, actions);
                        ui.add_space(12.0);
                    });
            }
        }
        if let Some(state) = self.views.get_mut(nav) {
            state.rows = rows;
        }
    }

    /// What is above the songs of a list: the album, the folders inside a folder, the
    /// choice of a "listen again" list, and the buttons that start playing.
    fn phone_list_head(&mut self, ui: &mut Ui, nav: &Nav, rows: &[Row], actions: &mut Vec<Action>) {
        let s = self.s();
        match nav {
            Nav::Album(key) => self.phone_album_head(ui, *key),
            Nav::Recent => {
                ui.add_space(6.0);
                egui::ScrollArea::horizontal()
                    .id_salt("phone-rediscover")
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.add_space(SIDE);
                            widgets::segmented(
                                ui,
                                "phone-rediscover",
                                &mut self.rediscover,
                                &[
                                    (Rediscover::Played, s.rediscover_played),
                                    (Rediscover::Added, s.rediscover_added),
                                    (Rediscover::MostPlayed, s.rediscover_most),
                                    (Rediscover::Forgotten, s.rediscover_forgotten),
                                ],
                            );
                            ui.add_space(SIDE);
                        });
                    });
                ui.add_space(8.0);
            }
            Nav::Folder(folder) => {
                let current = self
                    .folder_children
                    .as_ref()
                    .is_some_and(|(of, generation, _)| {
                        of == folder && *generation == self.lib_generation
                    });
                if !current {
                    let roots: Vec<std::path::PathBuf> = self
                        .core
                        .library
                        .roots()
                        .into_iter()
                        .map(|root| root.path)
                        .collect();
                    let entries = lists::folder_entries(&self.snapshot.tracks, folder, &roots);
                    self.folder_children = Some((folder.clone(), self.lib_generation, entries));
                }
                let entries = self
                    .folder_children
                    .as_ref()
                    .map(|(_, _, entries)| entries.clone())
                    .unwrap_or_default();
                virtual_rows(ui, "folders", entries.len(), MENU_ROW, |ui, index| {
                    let entry = &entries[index];
                    let count = self.lang.song_count(entry.songs);
                    if self.phone_menu_row(ui, Icon::Folder, &entry.name, Some(&count), true) {
                        actions.push(Action::Go(Nav::Folder(entry.path.clone())));
                    }
                });
            }
            _ => {}
        }
        if rows.is_empty() {
            let folders_inside = matches!(nav, Nav::Folder(_))
                && self
                    .folder_children
                    .as_ref()
                    .is_some_and(|(_, _, entries)| !entries.is_empty());
            if !folders_inside {
                self.empty_list_message(ui, nav, false);
            }
            return;
        }
        // Play, and play in a shuffled order.
        ui.add_space(10.0);
        let mut play = false;
        let mut shuffle = false;
        ui.horizontal(|ui| {
            ui.add_space(SIDE);
            let width = (ui.available_width() - SIDE - 10.0) / 2.0;
            ui.spacing_mut().item_spacing.x = 10.0;
            play = pill_button(ui, Icon::Play, s.play, width, true).clicked();
            shuffle = pill_button(ui, Icon::Shuffle, s.shuffle, width, false).clicked();
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(SIDE);
            let total: u64 = rows
                .iter()
                .map(|row| self.snapshot.tracks[row.idx as usize].duration_ms)
                .sum();
            ui.label(
                RichText::new(format!(
                    "{} · {}",
                    self.lang.song_count(rows.len()),
                    self.lang.total_time(total)
                ))
                .font(font(13.0))
                .color(theme::dim(ui)),
            );
        });
        ui.add_space(6.0);
        if play || shuffle {
            let tracks: Vec<TrackId> = rows
                .iter()
                .map(|row| self.snapshot.tracks[row.idx as usize].id)
                .collect();
            let start = if shuffle {
                fastrand::usize(..tracks.len())
            } else {
                0
            };
            self.core.player.send(Command::SetShuffle(shuffle));
            actions.push(Action::PlayTracks(tracks, start));
        }
    }

    /// The top of an album's page: its cover and what it is.
    fn phone_album_head(&mut self, ui: &mut Ui, key: u64) {
        let s = self.s();
        let Some(album) = self
            .snapshot
            .album_index(key)
            .and_then(|i| self.snapshot.albums().get(i as usize))
            .cloned()
        else {
            return;
        };
        let title = lists::album_title(&album, &self.snapshot, s.unknown_album).to_string();
        let side = (ui.available_width() * 0.56).clamp(140.0, 280.0);
        ui.add_space(8.0);
        ui.vertical_centered(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(side), Sense::hover());
            let texture = self.art.get(album.art_id.as_deref(), true);
            widgets::art(ui, rect, texture, 12.0);
            ui.add_space(12.0);
            let width = ui.available_width() - 2.0 * SIDE;
            ui.allocate_ui(vec2(width, 0.0), |ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(title)
                            .font(theme::semibold(20.0))
                            .color(ui.visuals().strong_text_color()),
                    )
                    .wrap()
                    .halign(Align::Center),
                );
                let artist = if album.artist.is_empty() {
                    s.unknown_artist
                } else {
                    &album.artist
                };
                let line = match album.year {
                    Some(year) => format!("{artist} · {year}"),
                    None => artist.to_string(),
                };
                ui.add(
                    egui::Label::new(RichText::new(line).font(font(15.0)).color(theme::dim(ui)))
                        .wrap()
                        .halign(Align::Center),
                );
            });
        });
    }

    /// The song rows of a list.
    fn phone_track_rows(
        &mut self,
        ui: &mut Ui,
        nav: &Nav,
        rows: &[Row],
        actions: &mut Vec<Action>,
    ) {
        let s = self.s();
        let playing = self.player.current.map(|c| c.track_id);
        let in_album = matches!(nav, Nav::Album(_));
        let playlist = match nav {
            Nav::Playlist(id) => Some(*id),
            _ => None,
        };
        let snapshot = self.snapshot.clone();
        virtual_rows(ui, "songs", rows.len(), ROW, |ui, index| {
            let row = rows[index];
            let Some(track) = snapshot.tracks.get(row.idx as usize) else {
                return;
            };
            let rect = ui.max_rect();
            let response = row_response(ui, "song");
            response
                .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &track.title));
            let dark = ui.visuals().dark_mode;
            let is_playing = playing == Some(track.id);
            let mut left = rect.left() + SIDE;
            if in_album {
                // The place on the album instead of the cover they all share.
                let number = track.track_no.map(|n| n.to_string()).unwrap_or_default();
                let color = if is_playing {
                    theme::accent(dark)
                } else {
                    ui.visuals().weak_text_color()
                };
                ui.painter().text(
                    pos2(left + 14.0, rect.center().y),
                    Align2::CENTER_CENTER,
                    number,
                    font(15.0),
                    color,
                );
                left += 36.0;
            } else {
                let art = Rect::from_min_size(
                    pos2(left, rect.center().y - 23.0),
                    egui::Vec2::splat(46.0),
                );
                let texture = self.art.get(track.art_id.as_deref(), false);
                widgets::art(ui, art, texture, 6.0);
                left = art.right() + 12.0;
            }
            let title_color = if !track.available {
                ui.visuals().weak_text_color()
            } else if is_playing {
                theme::accent(dark)
            } else {
                ui.visuals().strong_text_color()
            };
            let below = if in_album {
                format!(
                    "{} · {}",
                    artist_line(track, s.unknown_artist),
                    format_time(track.duration_ms)
                )
            } else if track.album.is_empty() {
                artist_line(track, s.unknown_artist).to_string()
            } else {
                format!("{} — {}", artist_line(track, s.unknown_artist), track.album)
            };
            let text = Rect::from_min_max(
                pos2(left, rect.top()),
                pos2(rect.right() - SIDE - TOUCH, rect.bottom()),
            );
            two_lines(
                ui,
                text,
                RichText::new(&track.title)
                    .font(font(16.5))
                    .color(title_color),
                Some(RichText::new(below).font(font(13.5)).color(theme::dim(ui))),
            );
            row_line(ui, left);
            let sheet = Sheet::Track {
                track: track.id,
                playlist: playlist.map(|id| (id, row.key as i64)),
                queue_item: None,
            };
            if more_button(ui, s.phone_more, rect) || response.long_touched() {
                self.phone.sheet = Some(sheet);
            } else if response.clicked() {
                let tracks: Vec<TrackId> = rows
                    .iter()
                    .map(|row| snapshot.tracks[row.idx as usize].id)
                    .collect();
                actions.push(Action::PlayTracks(tracks, index));
            }
        });
    }

    /// Albums, two to a row.
    fn phone_album_grid(&mut self, ui: &mut Ui, rows: &[Row], actions: &mut Vec<Action>) {
        let s = self.s();
        let width = ui.available_width();
        let columns = ((width / 190.0).floor() as usize).max(2);
        let gap = 14.0;
        let cell = (width - 2.0 * SIDE - gap * (columns as f32 - 1.0)) / columns as f32;
        let height = cell + 54.0;
        let snapshot = self.snapshot.clone();
        ui.add_space(8.0);
        virtual_rows(
            ui,
            "albums",
            rows.len().div_ceil(columns),
            height,
            |ui, line| {
                let rect = ui.max_rect();
                for column in 0..columns {
                    let Some(row) = rows.get(line * columns + column) else {
                        break;
                    };
                    let Some(album) = snapshot.albums().get(row.idx as usize) else {
                        continue;
                    };
                    let cell_rect = Rect::from_min_size(
                        pos2(
                            rect.left() + SIDE + column as f32 * (cell + gap),
                            rect.top(),
                        ),
                        vec2(cell, height),
                    );
                    let title = lists::album_title(album, &snapshot, s.unknown_album);
                    let response = ui.interact(
                        cell_rect,
                        ui.scope_id().with(("album", album.key)),
                        Sense::click(),
                    );
                    response
                        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, title));
                    let art = Rect::from_min_size(cell_rect.min, egui::Vec2::splat(cell));
                    let texture = self.art.get(album.art_id.as_deref(), true);
                    widgets::art(ui, art, texture, 10.0);
                    if response.is_pointer_button_down_on() {
                        ui.painter()
                            .rect_filled(art, 10.0, Color32::from_black_alpha(60));
                    }
                    let text = Rect::from_min_max(
                        pos2(cell_rect.left() + 2.0, art.bottom() + 4.0),
                        pos2(cell_rect.right() - 2.0, cell_rect.bottom()),
                    );
                    let mut text_ui = ui.new_child(
                        UiBuilder::new()
                            .id_salt(("album-text", album.key))
                            .max_rect(text)
                            .layout(Layout::top_down(Align::Min)),
                    );
                    text_ui.spacing_mut().item_spacing.y = 0.0;
                    text_ui.add(
                        egui::Label::new(
                            RichText::new(title)
                                .font(theme::semibold(14.5))
                                .color(ui.visuals().strong_text_color()),
                        )
                        .truncate(),
                    );
                    let artist = if album.artist.is_empty() {
                        s.unknown_artist
                    } else {
                        &album.artist
                    };
                    text_ui.add(
                        egui::Label::new(
                            RichText::new(artist).font(font(13.0)).color(theme::dim(ui)),
                        )
                        .truncate(),
                    );
                    if response.long_touched() {
                        self.phone.sheet = Some(Sheet::Album(album.key));
                    } else if response.clicked() {
                        actions.push(Action::Go(Nav::Album(album.key)));
                    }
                }
            },
        );
        ui.add_space(12.0);
    }

    fn phone_artist_rows(&mut self, ui: &mut Ui, rows: &[Row], actions: &mut Vec<Action>) {
        let s = self.s();
        let snapshot = self.snapshot.clone();
        virtual_rows(ui, "artists", rows.len(), MENU_ROW + 4.0, |ui, index| {
            let Some(artist) = snapshot.artists().get(rows[index].idx as usize) else {
                return;
            };
            let name = if artist.name.is_empty() {
                s.unknown_artist
            } else {
                &artist.name
            };
            let rect = ui.max_rect();
            let response = row_response(ui, "artist");
            response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, name));
            let circle = pos2(rect.left() + SIDE + 20.0, rect.center().y);
            ui.painter()
                .circle_filled(circle, 20.0, ui.visuals().faint_bg_color);
            icons::paint(
                ui.painter(),
                Rect::from_center_size(circle, egui::Vec2::splat(18.0)),
                Icon::Person,
                ui.visuals().weak_text_color(),
            );
            let text = Rect::from_min_max(
                pos2(circle.x + 32.0, rect.top()),
                pos2(rect.right() - SIDE - 20.0, rect.bottom()),
            );
            let summary = self
                .lang
                .artist_summary(artist.albums.len(), artist.track_count);
            two_lines(
                ui,
                text,
                RichText::new(name)
                    .font(font(16.5))
                    .color(ui.visuals().strong_text_color()),
                Some(
                    RichText::new(summary)
                        .font(font(13.5))
                        .color(theme::dim(ui)),
                ),
            );
            icons::paint(
                ui.painter(),
                Rect::from_center_size(
                    pos2(rect.right() - SIDE - 6.0, rect.center().y),
                    egui::Vec2::splat(14.0),
                ),
                Icon::Forward,
                ui.visuals().weak_text_color(),
            );
            row_line(ui, circle.x + 32.0);
            if response.clicked() {
                actions.push(Action::Go(Nav::Artist(artist.name.clone())));
            }
        });
    }

    /// The search tab: a field, what to look for, and what was found.
    fn phone_search(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let s = self.s();
        ui.add_space(4.0);
        let mut clear = false;
        // The field: a rounded box with the looking glass, the text and a way to empty it.
        let (outer, _) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::hover());
        let field = outer.shrink2(vec2(SIDE, 0.0));
        ui.painter()
            .rect_filled(field, 12.0, ui.visuals().faint_bg_color);
        let mut field_ui = ui.new_child(
            UiBuilder::new()
                .id_salt("phone-search-field")
                .max_rect(field.shrink2(vec2(10.0, 0.0)))
                .layout(Layout::left_to_right(Align::Center)),
        );
        {
            let ui = &mut field_ui;
            let (icon, _) = ui.allocate_exact_size(egui::Vec2::splat(18.0), Sense::hover());
            icons::paint(
                ui.painter(),
                icon,
                Icon::Search,
                ui.visuals().weak_text_color(),
            );
            let room = ui.available_width()
                - if self.phone.query.is_empty() {
                    0.0
                } else {
                    40.0
                };
            let edit = egui::TextEdit::singleline(&mut self.phone.query)
                .hint_text(s.search_hint)
                .font(font(17.0))
                .frame(egui::Frame::NONE)
                .desired_width(room);
            let response = ui.add(edit);
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::TextInput, true, s.search_label)
            });
            if std::mem::take(&mut self.phone.focus_search) {
                response.request_focus();
            }
            if !self.phone.query.is_empty()
                && widgets::icon_button(ui, Icon::Close, s.clear_search, 32.0, false).clicked()
            {
                clear = true;
            }
        }
        if clear {
            self.phone.query.clear();
            self.phone.focus_search = true;
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(SIDE);
            widgets::segmented(
                ui,
                "phone-search-kind",
                &mut self.phone.search_kind,
                &[
                    (SearchKind::Songs, s.phone_songs),
                    (SearchKind::Albums, s.albums),
                    (SearchKind::Artists, s.artists),
                ],
            );
        });
        ui.add_space(8.0);

        let query = self.phone.query.trim().to_string();
        if query.is_empty() {
            self.phone.search_rows.clear();
            self.phone.search_built = None;
            widgets::empty_state(ui, Icon::Search, s.search_label, s.phone_search_prompt);
            return;
        }
        let kind = self.phone.search_kind;
        let stamp = (
            self.lib_generation,
            self.playlists_generation,
            query.clone(),
            kind,
        );
        if self.phone.search_built.as_ref() != Some(&stamp) {
            let nav = match kind {
                SearchKind::Songs => Nav::Songs,
                SearchKind::Albums => Nav::Albums,
                SearchKind::Artists => Nav::Artists,
            };
            self.phone.search_rows = self.rows_for(&nav, &query);
            self.phone.search_built = Some(stamp);
        }
        let rows = std::mem::take(&mut self.phone.search_rows);
        if rows.is_empty() {
            widgets::empty_state(ui, Icon::Search, s.empty_search, s.empty_search_hint);
        } else {
            egui::ScrollArea::vertical()
                .id_salt(("phone-search", kind))
                .auto_shrink(false)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    match kind {
                        SearchKind::Songs => self.phone_track_rows(ui, &Nav::Songs, &rows, actions),
                        SearchKind::Albums => self.phone_album_grid(ui, &rows, actions),
                        SearchKind::Artists => self.phone_artist_rows(ui, &rows, actions),
                    }
                    ui.add_space(12.0);
                });
        }
        self.phone.search_rows = rows;
    }

    /// The playlists tab.
    fn phone_playlists(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let s = self.s();
        let playlists = self.playlists.clone();
        let smart = self.settings.smart_playlists.clone();
        if playlists.is_empty() && smart.is_empty() {
            widgets::empty_state(
                ui,
                Icon::Playlist,
                s.phone_no_playlists,
                s.phone_no_playlists_hint,
            );
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("phone-playlists")
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                virtual_rows(ui, "playlists", playlists.len(), MENU_ROW, |ui, index| {
                    let playlist = &playlists[index];
                    let rect = ui.max_rect();
                    let count = self.lang.song_count(playlist.item_count);
                    let tapped =
                        self.phone_menu_row(ui, Icon::Playlist, &playlist.name, Some(&count), true);
                    // A long press offers what the page's own "more" does.
                    let held = ui
                        .interact(rect, ui.scope_id().with("menu"), Sense::click())
                        .long_touched();
                    if held {
                        self.phone.sheet = Some(Sheet::Playlist(playlist.id));
                    } else if tapped {
                        actions.push(Action::Go(Nav::Playlist(playlist.id)));
                    }
                });
                if !smart.is_empty() {
                    ui.add_space(18.0);
                    ui.horizontal(|ui| {
                        ui.add_space(SIDE);
                        ui.label(
                            RichText::new(s.phone_smart)
                                .font(theme::semibold(13.0))
                                .color(theme::dim(ui)),
                        );
                    });
                    ui.add_space(4.0);
                    virtual_rows(ui, "smart", smart.len(), MENU_ROW, |ui, index| {
                        let playlist = &smart[index];
                        if self.phone_menu_row(ui, Icon::Search, &playlist.name, None, true) {
                            actions.push(Action::Go(Nav::Smart(playlist.name.clone())));
                        }
                    });
                }
            });
    }

    /// The settings tab: the groups of settings.
    fn phone_settings(&mut self, ui: &mut Ui) {
        let s = self.s();
        let entries: [(Icon, &str, SettingsPage); 6] = [
            (Icon::Folder, s.phone_library, SettingsPage::Library),
            (Icon::Play, s.settings_playback, SettingsPage::Playback),
            (Icon::Equalizer, s.equalizer, SettingsPage::Equalizer),
            (Icon::Gear, s.settings_general, SettingsPage::General),
            (Icon::Keyboard, s.menu_user_guide, SettingsPage::Guide),
            (Icon::Info, s.settings_about, SettingsPage::About),
        ];
        let mut open = None;
        egui::ScrollArea::vertical()
            .id_salt("phone-settings")
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                virtual_rows(ui, "settings", entries.len(), MENU_ROW, |ui, index| {
                    let (icon, label, page) = entries[index];
                    if self.phone_menu_row(ui, icon, label, None, true) {
                        open = Some(page);
                    }
                });
            });
        if let Some(page) = open {
            self.phone.stack().push(Page::Settings(page));
        }
    }

    fn phone_settings_page(&mut self, ui: &mut Ui, page: SettingsPage) {
        let s = self.s();
        if page == SettingsPage::Library {
            self.phone_library_settings(ui);
            return;
        }
        if page == SettingsPage::Guide {
            egui::ScrollArea::vertical()
                .id_salt("phone-guide")
                .auto_shrink(false)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.add_space(SIDE);
                        ui.vertical(|ui| {
                            ui.set_width(ui.available_width() - SIDE);
                            self.guide_body(ui, None);
                        });
                    });
                });
            return;
        }
        let before = self.settings.eq;
        egui::ScrollArea::both()
            .id_salt(("phone-settings-page", page as u8))
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_space(SIDE - 4.0);
                    ui.vertical(|ui| {
                        let width = ui.available_width() - SIDE;
                        match page {
                            SettingsPage::Playback => {
                                ui.set_width(width);
                                self.settings_playback(ui);
                            }
                            SettingsPage::General => {
                                ui.set_width(width);
                                self.settings_general(ui);
                            }
                            SettingsPage::About => {
                                ui.set_width(width);
                                self.settings_about(ui);
                            }
                            SettingsPage::Equalizer => {
                                ui.horizontal(|ui| {
                                    widgets::toggle(ui, &mut self.settings.eq.enabled, s.equalizer);
                                    let state = if self.settings.eq.enabled {
                                        s.eq_on
                                    } else {
                                        s.eq_off
                                    };
                                    ui.label(RichText::new(state).color(theme::dim(ui)));
                                });
                                ui.add_space(12.0);
                                // The sliders need the width they have on a desk; the
                                // page scrolls sideways to them.
                                ui.set_width(width.max(super::equalizer::WIDTH - 40.0));
                                self.equalizer_body(ui);
                            }
                            SettingsPage::Library | SettingsPage::Guide => {}
                        }
                    });
                });
                ui.add_space(16.0);
            });
        self.settings.eq = self.settings.eq.sanitized();
        if self.settings.eq != before {
            self.core.player.send(Command::SetEq(self.settings.eq));
        }
    }

    /// Where the music is and how more gets in.
    fn phone_library_settings(&mut self, ui: &mut Ui) {
        let s = self.s();
        let scan = self.core.library.scan_status();
        let mut import_files = false;
        let mut import_folder = false;
        let mut rescan = false;
        egui::ScrollArea::vertical()
            .id_salt("phone-library-settings")
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_space(SIDE);
                    ui.vertical(|ui| {
                        ui.set_width(ui.available_width() - SIDE);
                        widgets::card(ui, |ui| {
                            widgets::setting_row(
                                ui,
                                s.phone_music_place,
                                Some(s.phone_music_place_desc),
                                true,
                                |_| {},
                            );
                            widgets::row_divider(ui);
                            let songs = self.lang.song_count(self.snapshot.len());
                            let status = if scan.running {
                                self.lang.scan_progress(scan.files_found)
                            } else {
                                songs
                            };
                            widgets::setting_row(
                                ui,
                                s.rescan,
                                Some(s.phone_rescan_desc),
                                true,
                                |ui| {
                                    if scan.running {
                                        ui.add(egui::Spinner::new().size(16.0));
                                    } else if ui.button(s.rescan).clicked() {
                                        rescan = true;
                                    }
                                    ui.label(RichText::new(status).small().color(theme::dim(ui)));
                                },
                            );
                        });
                        ui.add_space(14.0);
                        widgets::card(ui, |ui| {
                            widgets::setting_row(
                                ui,
                                s.phone_import_files,
                                Some(s.phone_import_desc),
                                true,
                                |ui| {
                                    if ui.button(s.phone_import_files).clicked() {
                                        import_files = true;
                                    }
                                },
                            );
                            widgets::row_divider(ui);
                            widgets::setting_row(ui, s.phone_import_folder, None, true, |ui| {
                                if ui.button(s.phone_import_folder).clicked() {
                                    import_folder = true;
                                }
                            });
                            widgets::row_divider(ui);
                            widgets::setting_row(ui, s.playlists, None, true, |ui| {
                                if ui.button(s.import_m3u8).clicked() {
                                    self.pick_playlist_to_import();
                                }
                            });
                        });
                    });
                });
            });
        if import_files {
            self.import_music(crate::platform::PickKind::Files);
        }
        if import_folder {
            self.import_music(crate::platform::PickKind::Folder);
        }
        if rescan {
            self.announce_scan = true;
            self.core.library.rescan();
        }
    }

    // ----- the bars at the bottom -------------------------------------------------------

    fn phone_tab_bar(&mut self, ui: &mut Ui, rect: Rect) {
        let s = self.s();
        ui.painter().rect_filled(rect, 0.0, theme::chrome_fill(ui));
        ui.painter().hline(
            rect.x_range(),
            rect.top(),
            egui::Stroke::new(1.0, widgets::divider_color(ui)),
        );
        let dark = ui.visuals().dark_mode;
        let width = rect.width() / TABS.len() as f32;
        for (index, tab) in TABS.into_iter().enumerate() {
            let (icon, label) = match tab {
                Tab::Library => (Icon::Note, s.phone_library),
                Tab::Search => (Icon::Search, s.search_label),
                Tab::Playlists => (Icon::Playlist, s.playlists),
                Tab::Settings => (Icon::Gear, s.settings),
            };
            let cell = Rect::from_min_size(
                pos2(rect.left() + index as f32 * width, rect.top()),
                vec2(width, TAB_BAR),
            );
            let response = ui.interact(
                cell,
                ui.scope_id().with(("phone-tab", index)),
                Sense::click(),
            );
            let selected = self.phone.tab == tab;
            response
                .widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, true, selected, label));
            let color = if selected {
                theme::accent(dark)
            } else {
                ui.visuals().weak_text_color()
            };
            icons::paint(
                ui.painter(),
                Rect::from_center_size(
                    pos2(cell.center().x, cell.top() + 19.0),
                    egui::Vec2::splat(22.0),
                ),
                icon,
                color,
            );
            ui.painter().text(
                pos2(cell.center().x, cell.top() + 42.0),
                Align2::CENTER_CENTER,
                label,
                font(11.0),
                color,
            );
            if response.clicked() {
                if selected {
                    // The tab that is open already: back to its first page.
                    self.phone.stack().clear();
                } else {
                    self.phone.tab = tab;
                }
                if tab == Tab::Search && self.phone.query.is_empty() {
                    self.phone.focus_search = true;
                }
            }
        }
    }

    /// The bar of the playing song: tap it for the whole player.
    fn phone_mini_player(&mut self, ui: &mut Ui, rect: Rect) {
        let s = self.s();
        let Some(current) = self.player.current else {
            return;
        };
        let track = self.track(current.track_id).cloned();
        ui.painter().rect_filled(rect, 0.0, theme::chrome_fill(ui));
        ui.painter().hline(
            rect.x_range(),
            rect.top(),
            egui::Stroke::new(1.0, widgets::divider_color(ui)),
        );
        // How far the song is, as a line along the top.
        let dark = ui.visuals().dark_mode;
        if current.duration_ms > 0 {
            let done =
                (self.player.position_now_ms() as f32 / current.duration_ms as f32).clamp(0.0, 1.0);
            ui.painter().rect_filled(
                Rect::from_min_size(rect.min, vec2(rect.width() * done, 2.0)),
                0.0,
                theme::accent(dark),
            );
        }
        let response = ui.interact(rect, ui.scope_id().with("phone-mini"), Sense::click());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::Button, true, s.phone_now_playing)
        });
        let art = Rect::from_min_size(
            pos2(rect.left() + SIDE, rect.center().y - 22.0),
            egui::Vec2::splat(44.0),
        );
        let texture = self
            .art
            .get(track.as_ref().and_then(|t| t.art_id.as_deref()), false);
        widgets::art(ui, art, texture, 6.0);

        let playing = matches!(
            self.player.state,
            PlayState::Playing | PlayState::Loading | PlayState::Seeking
        );
        let next = Rect::from_center_size(
            pos2(rect.right() - SIDE - 16.0, rect.center().y),
            egui::Vec2::splat(TOUCH),
        );
        let play = next.translate(vec2(-TOUCH - 4.0, 0.0));
        let text = Rect::from_min_max(
            pos2(art.right() + 12.0, rect.top()),
            pos2(play.left() - 8.0, rect.bottom()),
        );
        let (title, artist) = match &track {
            Some(track) => (
                track.title.clone(),
                artist_line(track, s.unknown_artist).to_string(),
            ),
            None => (s.nothing_playing.to_string(), String::new()),
        };
        two_lines(
            ui,
            text,
            RichText::new(title)
                .font(theme::semibold(15.0))
                .color(ui.visuals().strong_text_color()),
            Some(RichText::new(artist).font(font(13.0)).color(theme::dim(ui))),
        );
        let mut tapped_button = false;
        for (button, icon, label, command) in [
            (
                play,
                if playing { Icon::Pause } else { Icon::Play },
                if playing { s.pause } else { s.play },
                Command::TogglePlayPause,
            ),
            (next, Icon::Next, s.next_track, Command::Next),
        ] {
            let button_response = ui.interact(
                button,
                ui.scope_id().with(("phone-mini-button", label)),
                Sense::click(),
            );
            button_response
                .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
            let color = if button_response.is_pointer_button_down_on() {
                theme::accent(dark)
            } else {
                ui.visuals().strong_text_color()
            };
            icons::paint(
                ui.painter(),
                Rect::from_center_size(button.center(), egui::Vec2::splat(24.0)),
                icon,
                color,
            );
            if button_response.clicked() {
                self.core.player.send(command);
                tapped_button = true;
            }
        }
        if response.clicked() && !tapped_button {
            self.phone.player_open = true;
            self.phone.queue_open = false;
        }
    }

    // ----- the player -------------------------------------------------------------------

    /// The player on the whole screen.
    fn phone_player(&mut self, ui: &mut Ui, safe: Rect, actions: &mut Vec<Action>) {
        let s = self.s();
        let Some(current) = self.player.current else {
            return;
        };
        let track = self.track(current.track_id).cloned();
        let dark = ui.visuals().dark_mode;

        // The bar at the top: put the player away, and the queue.
        let header = Rect::from_min_max(safe.min, pos2(safe.right(), safe.top() + HEADER));
        let close = Rect::from_min_size(
            pos2(header.left() + 6.0, header.center().y - TOUCH / 2.0),
            egui::Vec2::splat(TOUCH),
        );
        let close_response = ui.interact(
            close,
            ui.scope_id().with("phone-player-close"),
            Sense::click(),
        );
        close_response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, s.close));
        icons::paint(
            ui.painter(),
            Rect::from_center_size(close.center(), egui::Vec2::splat(24.0)),
            Icon::Down,
            ui.visuals().strong_text_color(),
        );
        if close_response.clicked() {
            self.phone.player_open = false;
            return;
        }
        let queue = Rect::from_min_size(
            pos2(
                header.right() - 6.0 - TOUCH,
                header.center().y - TOUCH / 2.0,
            ),
            egui::Vec2::splat(TOUCH),
        );
        let queue_response = ui.interact(
            queue,
            ui.scope_id().with("phone-player-queue"),
            Sense::click(),
        );
        queue_response
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, s.show_queue));
        icons::paint(
            ui.painter(),
            Rect::from_center_size(queue.center(), egui::Vec2::splat(24.0)),
            Icon::Queue,
            if self.phone.queue_open {
                theme::accent(dark)
            } else {
                ui.visuals().strong_text_color()
            },
        );
        if queue_response.clicked() {
            self.phone.queue_open = !self.phone.queue_open;
        }
        let heading = if self.phone.queue_open {
            s.show_queue
        } else {
            s.phone_now_playing
        };
        ui.painter().text(
            header.center(),
            Align2::CENTER_CENTER,
            heading,
            theme::semibold(16.0),
            ui.visuals().strong_text_color(),
        );

        let body = Rect::from_min_max(pos2(safe.left(), header.bottom()), safe.max);
        if self.phone.queue_open {
            let mut queue_ui = ui.new_child(
                UiBuilder::new()
                    .id_salt("phone-queue")
                    .max_rect(body)
                    .layout(Layout::top_down(Align::Min)),
            );
            queue_ui.set_clip_rect(body);
            self.phone_queue(&mut queue_ui);
            return;
        }

        // Below the cover: the song, the seek bar, the buttons, the volume.
        let controls = 292.0;
        let side = (body.width() - 2.0 * 28.0)
            .min(body.height() - controls - 16.0)
            .max(120.0);
        let art = Rect::from_center_size(
            pos2(body.center().x, body.top() + 8.0 + side / 2.0),
            egui::Vec2::splat(side),
        );
        let texture = self
            .art
            .get(track.as_ref().and_then(|t| t.art_id.as_deref()), true);
        widgets::art(ui, art, texture, 14.0);

        let column = Rect::from_min_max(
            pos2(body.left() + 28.0, art.bottom() + 16.0),
            pos2(body.right() - 28.0, body.bottom()),
        );
        let mut column_ui = ui.new_child(
            UiBuilder::new()
                .id_salt("phone-player-controls")
                .max_rect(column)
                .layout(Layout::top_down(Align::Min)),
        );
        let ui = &mut column_ui;
        ui.spacing_mut().item_spacing.y = 0.0;

        // Title and artist, the heart and "more" beside them.
        let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::hover());
        let more = Rect::from_center_size(
            pos2(line.right() - TOUCH / 2.0 + 8.0, line.center().y),
            egui::Vec2::splat(TOUCH),
        );
        let heart = more.translate(vec2(-TOUCH, 0.0));
        let text = Rect::from_min_max(line.min, pos2(heart.left() - 6.0, line.bottom()));
        let (title, below) = match &track {
            Some(track) => {
                let artist = artist_line(track, s.unknown_artist);
                let below = if track.album.is_empty() {
                    artist.to_string()
                } else {
                    format!("{artist} — {}", track.album)
                };
                (track.title.clone(), below)
            }
            None => (String::new(), String::new()),
        };
        let text_response = ui.interact(
            text,
            ui.scope_id().with("phone-player-text"),
            Sense::click(),
        );
        two_lines(
            ui,
            text,
            RichText::new(title)
                .font(theme::semibold(20.0))
                .color(ui.visuals().strong_text_color()),
            Some(RichText::new(below).font(font(15.0)).color(theme::dim(ui))),
        );
        if let Some(track) = &track {
            if text_response.clicked() {
                actions.push(Action::Go(Nav::Album(track.album_key)));
            }
            let heart_response = ui.interact(
                heart,
                ui.scope_id().with("phone-player-heart"),
                Sense::click(),
            );
            let label = if track.favorite {
                s.remove_favorite
            } else {
                s.add_favorite
            };
            heart_response
                .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
            icons::paint(
                ui.painter(),
                Rect::from_center_size(heart.center(), egui::Vec2::splat(24.0)),
                if track.favorite {
                    Icon::HeartFilled
                } else {
                    Icon::Heart
                },
                if track.favorite {
                    theme::accent(dark)
                } else {
                    ui.visuals().text_color()
                },
            );
            if heart_response.clicked() {
                actions.push(Action::SetFavorite(vec![track.id], !track.favorite));
            }
            let more_response = ui.interact(
                more,
                ui.scope_id().with("phone-player-more"),
                Sense::click(),
            );
            more_response
                .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, s.phone_more));
            icons::paint(
                ui.painter(),
                Rect::from_center_size(more.center(), egui::Vec2::splat(24.0)),
                Icon::More,
                ui.visuals().text_color(),
            );
            if more_response.clicked() {
                self.phone.sheet = Some(Sheet::Track {
                    track: track.id,
                    playlist: None,
                    queue_item: None,
                });
            }
        }
        ui.add_space(10.0);
        self.phone_seek_bar(ui);
        ui.add_space(8.0);
        self.phone_transport(ui);
        ui.add_space(14.0);
        self.phone_volume(ui);
    }

    /// The seek bar with the time gone and the time left.
    fn phone_seek_bar(&mut self, ui: &mut Ui) {
        let s = self.s();
        let current = self.player.current;
        let duration_s = current.map_or(0.0, |c| c.duration_ms as f64 / 1000.0);
        let live_s = self.player.position_now_ms() as f64 / 1000.0;
        // After a seek the snapshot still shows the old position for a moment; keep the
        // thumb where the finger put it until the player has caught up.
        if let Some((target, since)) = self.seek_pending
            && ((live_s - target).abs() < 1.5 || since.elapsed() > Duration::from_millis(1500))
        {
            self.seek_pending = None;
        }
        let pending = self.seek_pending.map(|(target, _)| target);
        let mut shown = self
            .seek_drag
            .or(pending)
            .unwrap_or(live_s)
            .min(duration_s.max(0.0));
        let enabled = current.is_some() && duration_s > 0.0;
        ui.scope(|ui| {
            ui.spacing_mut().slider_width = ui.available_width();
            ui.spacing_mut().interact_size.y = 30.0;
            let slider =
                egui::Slider::new(&mut shown, 0.0..=duration_s.max(0.001)).show_value(false);
            let response = ui.add_enabled(enabled, slider);
            response.widget_info(|| egui::WidgetInfo::slider(true, shown, s.seek_bar));
            if response.dragged() {
                self.seek_drag = Some(shown);
            } else if response.drag_stopped() || response.changed() {
                self.core
                    .player
                    .send(Command::SeekTo((shown * 1000.0) as u64));
                self.seek_drag = None;
                self.seek_pending = Some((shown, Instant::now()));
            }
        });
        let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
        let dim = theme::dim(ui);
        ui.painter().text(
            line.left_center(),
            Align2::LEFT_CENTER,
            format_time((shown * 1000.0) as u64),
            font(12.5),
            dim,
        );
        let total_ms = current.map_or(0, |c| c.duration_ms);
        let left = total_ms.saturating_sub((shown * 1000.0) as u64);
        ui.painter().text(
            line.right_center(),
            Align2::RIGHT_CENTER,
            format!("-{}", format_time(left)),
            font(12.5),
            dim,
        );
    }

    /// Shuffle, previous, play, next, repeat.
    fn phone_transport(&mut self, ui: &mut Ui) {
        let s = self.s();
        let player = self.core.player.clone();
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 76.0), Sense::hover());
        let dark = ui.visuals().dark_mode;
        let playing = matches!(
            self.player.state,
            PlayState::Playing | PlayState::Loading | PlayState::Seeking
        );
        let step = rect.width() / 5.0;
        let centre =
            |index: usize| pos2(rect.left() + step * (index as f32 + 0.5), rect.center().y);
        let (repeat_icon, repeat_label) = match self.player.repeat {
            RepeatMode::Off => (Icon::Repeat, s.repeat_off),
            RepeatMode::All => (Icon::Repeat, s.repeat_all),
            RepeatMode::One => (Icon::RepeatOne, s.repeat_one),
        };
        let buttons: [(Icon, &str, f32, bool, Command); 4] = [
            (
                Icon::Shuffle,
                if self.player.shuffle {
                    s.shuffle_on
                } else {
                    s.shuffle
                },
                24.0,
                self.player.shuffle,
                Command::SetShuffle(!self.player.shuffle),
            ),
            (
                Icon::Previous,
                s.previous_track,
                34.0,
                false,
                Command::Previous,
            ),
            (Icon::Next, s.next_track, 34.0, false, Command::Next),
            (
                repeat_icon,
                repeat_label,
                24.0,
                self.player.repeat != RepeatMode::Off,
                Command::SetRepeat(next_repeat(self.player.repeat)),
            ),
        ];
        for (index, (icon, label, size, active, command)) in buttons.into_iter().enumerate() {
            let slot = if index >= 2 { index + 1 } else { index };
            let hit = Rect::from_center_size(centre(slot), egui::Vec2::splat(TOUCH + 8.0));
            let response = ui.interact(
                hit,
                ui.scope_id().with(("phone-transport", slot)),
                Sense::click(),
            );
            response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
            let color = if active {
                theme::accent(dark)
            } else if response.is_pointer_button_down_on() {
                theme::dim(ui)
            } else {
                ui.visuals().strong_text_color()
            };
            icons::paint(
                ui.painter(),
                Rect::from_center_size(hit.center(), egui::Vec2::splat(size)),
                icon,
                color,
            );
            if response.clicked() {
                player.send(command);
            }
        }
        // Play or pause, in the middle.
        let hit = Rect::from_center_size(centre(2), egui::Vec2::splat(72.0));
        let label = if playing { s.pause } else { s.play };
        let response = ui.interact(hit, ui.scope_id().with("phone-play"), Sense::click());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
        let fill = if response.is_pointer_button_down_on() {
            theme::accent(dark).gamma_multiply(0.8)
        } else {
            theme::accent(dark)
        };
        ui.painter().circle_filled(hit.center(), 34.0, fill);
        let ink = if dark {
            Color32::from_rgb(0x14, 0x14, 0x18)
        } else {
            Color32::WHITE
        };
        icons::paint(
            ui.painter(),
            Rect::from_center_size(hit.center(), egui::Vec2::splat(30.0)),
            if playing { Icon::Pause } else { Icon::Play },
            ink,
        );
        if response.clicked() {
            player.send(Command::TogglePlayPause);
        }
    }

    /// The app's own volume (the buttons of the phone set the phone's).
    fn phone_volume(&mut self, ui: &mut Ui) {
        let s = self.s();
        let mut volume = self.player.volume;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let (icon, _) = ui.allocate_exact_size(egui::Vec2::splat(20.0), Sense::hover());
            icons::paint(
                ui.painter(),
                icon,
                if volume <= 0.001 {
                    Icon::VolumeMuted
                } else {
                    Icon::Volume
                },
                theme::dim(ui),
            );
            ui.spacing_mut().slider_width = ui.available_width() - 4.0;
            ui.spacing_mut().interact_size.y = 28.0;
            let response = ui.add(egui::Slider::new(&mut volume, 0.0..=1.0).show_value(false));
            response.widget_info(|| {
                egui::WidgetInfo::slider(true, f64::from(volume), s.phone_app_volume)
            });
            if response.changed() {
                self.core.player.send(Command::SetVolume(volume));
                self.player.volume = volume;
            }
        });
        // What is set for this song or this evening, said in words.
        let mut notes: Vec<String> = Vec::new();
        if self.player.stop_after_current {
            notes.push(s.stop_after_short.to_string());
        }
        if let Some(at) = self.player.sleep_at {
            let left = at.saturating_duration_since(Instant::now()).as_secs();
            notes.push(format!(
                "{} {}:{:02}",
                s.menu_sleep_timer,
                left / 60,
                left % 60
            ));
            ui.ctx().request_repaint_after(Duration::from_millis(500));
        }
        if !notes.is_empty() {
            ui.add_space(8.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(notes.join(" · "))
                        .font(font(13.0))
                        .color(theme::accent(ui.visuals().dark_mode)),
                );
            });
        }
    }

    /// The queue, in the player.
    fn phone_queue(&mut self, ui: &mut Ui) {
        let s = self.s();
        let queue = self.player.queue.clone();
        let current = self.player.current.map(|c| c.item_id);
        if queue.is_empty() {
            widgets::empty_state(ui, Icon::Queue, s.empty_queue, "");
            return;
        }
        let mut clear = false;
        ui.horizontal(|ui| {
            ui.add_space(SIDE);
            ui.label(
                RichText::new(self.lang.song_count(queue.len()))
                    .font(font(13.5))
                    .color(theme::dim(ui)),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(SIDE);
                if ui
                    .add(egui::Button::new(
                        RichText::new(s.clear_queue).font(font(14.0)),
                    ))
                    .clicked()
                {
                    clear = true;
                }
            });
        });
        ui.add_space(4.0);
        let snapshot = self.snapshot.clone();
        let scroll_to = std::mem::take(&mut self.queue_scroll_forced)
            .then(|| current.and_then(|id| queue.iter().position(|item| item.id == id)))
            .flatten();
        let mut area = egui::ScrollArea::vertical()
            .id_salt("phone-queue-list")
            .auto_shrink(false);
        if let Some(index) = scroll_to {
            area = area.vertical_scroll_offset((index as f32 * ROW - ROW).max(0.0));
        }
        area.show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            virtual_rows(ui, "queue", queue.len(), ROW, |ui, index| {
                let item = queue[index];
                let Some(track) = snapshot.get(item.track) else {
                    return;
                };
                let rect = ui.max_rect();
                let response = row_response(ui, "queue");
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::Role::Button, true, &track.title)
                });
                let dark = ui.visuals().dark_mode;
                let is_current = current == Some(item.id);
                if is_current {
                    ui.painter().rect_filled(
                        rect,
                        0.0,
                        theme::accent(dark).gamma_multiply(if dark { 0.16 } else { 0.10 }),
                    );
                }
                let art = Rect::from_min_size(
                    pos2(rect.left() + SIDE, rect.center().y - 23.0),
                    egui::Vec2::splat(46.0),
                );
                let texture = self.art.get(track.art_id.as_deref(), false);
                widgets::art(ui, art, texture, 6.0);
                let text = Rect::from_min_max(
                    pos2(art.right() + 12.0, rect.top()),
                    pos2(rect.right() - SIDE - TOUCH, rect.bottom()),
                );
                let color = if is_current {
                    theme::accent(dark)
                } else {
                    ui.visuals().strong_text_color()
                };
                two_lines(
                    ui,
                    text,
                    RichText::new(&track.title).font(font(16.5)).color(color),
                    Some(
                        RichText::new(artist_line(track, s.unknown_artist))
                            .font(font(13.5))
                            .color(theme::dim(ui)),
                    ),
                );
                row_line(ui, art.right() + 12.0);
                if more_button(ui, s.phone_more, rect) || response.long_touched() {
                    self.phone.sheet = Some(Sheet::Track {
                        track: track.id,
                        playlist: None,
                        queue_item: Some(item.id),
                    });
                } else if response.clicked() {
                    self.core.player.send(Command::JumpTo(item.id));
                }
            });
            ui.add_space(12.0);
        });
        if clear {
            self.core.player.send(Command::ClearQueue);
        }
    }

    // ----- sheets and messages ----------------------------------------------------------

    /// A message, above the bars at the bottom.
    fn phone_toast(&mut self, ctx: &egui::Context, safe: Rect, above: f32) {
        let showing_transient = self.toast.is_some();
        let Some(toast) = self.toast.as_ref().or(self.alert.as_ref()) else {
            return;
        };
        let s = self.s();
        let dark = ctx.global_style().visuals.dark_mode;
        let fill = match (toast.warning, dark) {
            (true, true) => Color32::from_rgb(0x4A, 0x39, 0x1A),
            (true, false) => Color32::from_rgb(0xFF, 0xF1, 0xD0),
            (false, true) => Color32::from_rgb(0x30, 0x30, 0x38),
            (false, false) => Color32::from_rgb(0xEC, 0xEC, 0xF2),
        };
        let mut dismiss = false;
        let mut run: Option<ToastAction> = None;
        let width = safe.width() - 2.0 * 12.0;
        egui::Area::new(egui::Id::unique("phone-toast"))
            .order(egui::Order::Foreground)
            .pivot(Align2::CENTER_BOTTOM)
            .fixed_pos(pos2(safe.center().x, above - 8.0))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(fill)
                    .corner_radius(12.0)
                    .inner_margin(egui::Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.set_width(width - 24.0);
                        ui.horizontal(|ui| {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if widgets::icon_button(ui, Icon::Close, s.dismiss, 32.0, false)
                                    .clicked()
                                {
                                    dismiss = true;
                                }
                                if let Some((label, action)) = &toast.action
                                    && ui.button(label).clicked()
                                {
                                    run = Some(action.clone());
                                }
                                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&toast.text).font(font(14.0)),
                                        )
                                        .wrap(),
                                    );
                                });
                            });
                        });
                    });
            });
        if let Some(action) = run {
            match action {
                ToastAction::RetryOutput => self.core.player.send(Command::RetryOutput),
                ToastAction::OpenPlaybackSettings => {
                    self.phone.player_open = false;
                    self.phone.tab = Tab::Settings;
                    let stack = self.phone.stack();
                    stack.clear();
                    stack.push(Page::Settings(SettingsPage::Playback));
                }
                ToastAction::RestorePlaylist(id, tracks) => self.restore_playlist(id, tracks),
            }
            dismiss = true;
        }
        if dismiss {
            if showing_transient {
                self.toast = None;
            } else {
                self.alert = None;
            }
        }
    }

    /// The sheet that is up, if one is.
    fn phone_sheet(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(sheet) = self.phone.sheet.clone() else {
            return;
        };
        let full = ctx.viewport_rect();
        let safe = ctx.content_rect();
        let dark = ctx.global_style().visuals.dark_mode;
        // What is behind is dimmed; a tap there puts the sheet away.
        let backdrop = egui::Area::new(egui::Id::unique("phone-sheet-backdrop"))
            .order(egui::Order::Middle)
            .fixed_pos(full.min)
            .show(ctx, |ui| {
                let response = ui.allocate_rect(full, Sense::click());
                ui.painter().rect_filled(
                    full,
                    0.0,
                    Color32::from_black_alpha(if dark { 150 } else { 90 }),
                );
                response.clicked()
            })
            .inner;
        let fill = if dark {
            Color32::from_rgb(0x26, 0x26, 0x2C)
        } else {
            Color32::WHITE
        };
        let mut close = backdrop;
        let below_safe = full.bottom() - safe.bottom();
        egui::Area::new(egui::Id::unique("phone-sheet"))
            .order(egui::Order::Foreground)
            .pivot(Align2::CENTER_BOTTOM)
            .fixed_pos(pos2(full.center().x, full.bottom()))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(fill)
                    .corner_radius(egui::CornerRadius {
                        nw: 18,
                        ne: 18,
                        sw: 0,
                        se: 0,
                    })
                    .show(ui, |ui| {
                        ui.set_width(full.width());
                        ui.spacing_mut().item_spacing.y = 0.0;
                        // The handle at the top.
                        let (handle, _) =
                            ui.allocate_exact_size(vec2(full.width(), 18.0), Sense::hover());
                        ui.painter().rect_filled(
                            Rect::from_center_size(handle.center(), vec2(38.0, 5.0)),
                            3.0,
                            widgets::divider_color(ui),
                        );
                        let limit = (safe.height() * 0.72).max(200.0);
                        egui::ScrollArea::vertical()
                            .id_salt("phone-sheet-items")
                            .max_height(limit)
                            .show(ui, |ui| {
                                ui.set_width(full.width());
                                if self.phone_sheet_items(ui, &sheet, actions) {
                                    close = true;
                                }
                            });
                        ui.add_space(8.0 + below_safe);
                    });
            });
        // Another sheet may have been put up from this one: only this one goes.
        if close && self.phone.sheet.as_ref() == Some(&sheet) {
            self.phone.sheet = None;
        }
    }

    /// A line of a sheet: an icon and what it does. True when it was tapped.
    fn sheet_item(ui: &mut Ui, icon: Icon, label: &str, warning: bool) -> bool {
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::click());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
        if response.is_pointer_button_down_on() {
            ui.painter()
                .rect_filled(rect, 0.0, ui.visuals().widgets.hovered.weak_bg_fill);
        }
        let color = if warning {
            Color32::from_rgb(0xE0, 0x55, 0x55)
        } else {
            ui.visuals().strong_text_color()
        };
        icons::paint(
            ui.painter(),
            Rect::from_center_size(
                pos2(rect.left() + SIDE + 14.0, rect.center().y),
                egui::Vec2::splat(22.0),
            ),
            icon,
            if warning {
                color
            } else {
                theme::accent(ui.visuals().dark_mode)
            },
        );
        ui.painter().text(
            pos2(rect.left() + SIDE + 44.0, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            font(17.0),
            color,
        );
        response.clicked()
    }

    /// The heading of a sheet: what it is about.
    fn sheet_heading(ui: &mut Ui, title: &str, below: &str) {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::hover());
        let text = Rect::from_min_max(
            pos2(rect.left() + SIDE + 4.0, rect.top()),
            pos2(rect.right() - SIDE, rect.bottom()),
        );
        two_lines(
            ui,
            text,
            RichText::new(title)
                .font(theme::semibold(16.0))
                .color(ui.visuals().strong_text_color()),
            (!below.is_empty())
                .then(|| RichText::new(below).font(font(13.5)).color(theme::dim(ui))),
        );
        ui.painter().hline(
            rect.x_range(),
            rect.bottom(),
            egui::Stroke::new(1.0, widgets::divider_color(ui)),
        );
    }

    /// The lines of `sheet`. True when the sheet is done with.
    fn phone_sheet_items(&mut self, ui: &mut Ui, sheet: &Sheet, actions: &mut Vec<Action>) -> bool {
        let s = self.s();
        match sheet {
            Sheet::Track {
                track,
                playlist,
                queue_item,
            } => {
                let Some(track) = self.snapshot.get(*track).cloned() else {
                    return true;
                };
                Self::sheet_heading(ui, &track.title, artist_line(&track, s.unknown_artist));
                let ids = vec![track.id];
                if Self::sheet_item(ui, Icon::Play, s.play_next, false) {
                    actions.push(Action::PlayNext(ids));
                    return true;
                }
                if Self::sheet_item(ui, Icon::Queue, s.add_to_queue, false) {
                    actions.push(Action::Enqueue(ids));
                    self.info_toast(self.lang.added_to_queue(1));
                    return true;
                }
                if Self::sheet_item(ui, Icon::Playlist, s.add_to_playlist, false) {
                    self.phone.sheet = Some(Sheet::AddToPlaylist(ids));
                    return false;
                }
                let (icon, label) = if track.favorite {
                    (Icon::HeartFilled, s.remove_favorite)
                } else {
                    (Icon::Heart, s.add_favorite)
                };
                if Self::sheet_item(ui, icon, label, false) {
                    actions.push(Action::SetFavorite(ids, !track.favorite));
                    return true;
                }
                if Self::sheet_item(ui, Icon::Album, s.go_to_album, false) {
                    actions.push(Action::Go(Nav::Album(track.album_key)));
                    return true;
                }
                let artist = if track.album_artist.is_empty() {
                    track.artist.clone()
                } else {
                    track.album_artist.clone()
                };
                if self.snapshot.artists().iter().any(|a| a.name == artist)
                    && Self::sheet_item(ui, Icon::Person, s.go_to_artist, false)
                {
                    actions.push(Action::Go(Nav::Artist(artist)));
                    return true;
                }
                if Self::sheet_item(ui, Icon::Info, s.song_info, false) {
                    self.phone.sheet = Some(Sheet::Info(track.id));
                    return false;
                }
                if self.player.current.map(|c| c.track_id) == Some(track.id) {
                    let on = self.player.stop_after_current;
                    if Self::sheet_item(ui, Icon::Pause, s.menu_stop_after_current, on) {
                        self.core.player.send(Command::SetStopAfterCurrent(!on));
                        return true;
                    }
                    if Self::sheet_item(ui, Icon::Clock, s.menu_sleep_timer, false) {
                        self.phone.sheet = Some(Sheet::Sleep);
                        return false;
                    }
                }
                if let Some(item) = queue_item
                    && Self::sheet_item(ui, Icon::Trash, s.remove_from_queue, true)
                {
                    self.core.player.send(Command::RemoveItems(vec![*item]));
                    return true;
                }
                if let Some((playlist, item)) = playlist
                    && Self::sheet_item(ui, Icon::Trash, s.remove_from_playlist, true)
                {
                    actions.push(Action::RemoveFromPlaylist(*playlist, vec![*item]));
                    return true;
                }
                if queue_item.is_none()
                    && playlist.is_none()
                    && Self::sheet_item(ui, Icon::Trash, s.remove_from_library, true)
                {
                    actions.push(Action::RemoveFromLibrary(vec![track.id]));
                    return true;
                }
                false
            }
            Sheet::AddToPlaylist(tracks) => {
                Self::sheet_heading(ui, s.add_to_playlist, &self.lang.song_count(tracks.len()));
                if Self::sheet_item(ui, Icon::Plus, s.new_playlist, false) {
                    actions.push(Action::NewPlaylistWith(tracks.clone()));
                    return true;
                }
                for playlist in self.playlists.clone() {
                    if Self::sheet_item(ui, Icon::Playlist, &playlist.name, false) {
                        actions.push(Action::AddToPlaylist(playlist.id, tracks.clone()));
                        return true;
                    }
                }
                false
            }
            Sheet::Album(key) => {
                let Some(album) = self
                    .snapshot
                    .album_index(*key)
                    .and_then(|i| self.snapshot.albums().get(i as usize))
                    .cloned()
                else {
                    return true;
                };
                let title = lists::album_title(&album, &self.snapshot, s.unknown_album).to_string();
                Self::sheet_heading(ui, &title, &album.artist);
                let tracks: Vec<TrackId> = album
                    .tracks
                    .iter()
                    .filter_map(|&i| self.snapshot.tracks.get(i as usize).map(|t| t.id))
                    .collect();
                if Self::sheet_item(ui, Icon::Play, s.play_album, false) {
                    actions.push(Action::PlayTracks(tracks, 0));
                    return true;
                }
                if Self::sheet_item(ui, Icon::Play, s.play_next, false) {
                    actions.push(Action::PlayNext(tracks));
                    return true;
                }
                if Self::sheet_item(ui, Icon::Queue, s.add_to_queue, false) {
                    self.info_toast(self.lang.added_to_queue(tracks.len()));
                    actions.push(Action::Enqueue(tracks));
                    return true;
                }
                if Self::sheet_item(ui, Icon::Playlist, s.add_to_playlist, false) {
                    self.phone.sheet = Some(Sheet::AddToPlaylist(tracks));
                    return false;
                }
                if self
                    .snapshot
                    .artists()
                    .iter()
                    .any(|a| a.name == album.artist)
                    && Self::sheet_item(ui, Icon::Person, s.go_to_artist, false)
                {
                    actions.push(Action::Go(Nav::Artist(album.artist.clone())));
                    return true;
                }
                false
            }
            Sheet::Playlist(id) => {
                let Some(playlist) = self.playlists.iter().find(|p| p.id == *id).cloned() else {
                    return true;
                };
                Self::sheet_heading(
                    ui,
                    &playlist.name,
                    &self.lang.song_count(playlist.item_count),
                );
                if Self::sheet_item(ui, Icon::Keyboard, s.rename, false) {
                    self.dialog = Some(Dialog::RenamePlaylist {
                        id: playlist.id,
                        name: playlist.name.clone(),
                        focus: true,
                    });
                    return true;
                }
                if Self::sheet_item(ui, Icon::Folder, s.export_m3u8, false) {
                    self.export_playlist(playlist.id, &playlist.name);
                    return true;
                }
                if Self::sheet_item(ui, Icon::Trash, s.delete, true) {
                    self.dialog = Some(Dialog::DeletePlaylist(playlist.id, playlist.name.clone()));
                    return true;
                }
                false
            }
            Sheet::Smart(name) => {
                let query = self
                    .settings
                    .smart_playlists
                    .iter()
                    .find(|p| &p.name == name)
                    .map(|p| p.query.clone())
                    .unwrap_or_default();
                Self::sheet_heading(ui, name, &query);
                if Self::sheet_item(ui, Icon::Keyboard, s.rename, false) {
                    actions.push(Action::RenameSmart(name.clone()));
                    return true;
                }
                if Self::sheet_item(ui, Icon::Trash, s.delete, true) {
                    actions.push(Action::DeleteSmart(name.clone()));
                    return true;
                }
                false
            }
            Sheet::Sleep => {
                Self::sheet_heading(ui, s.menu_sleep_timer, "");
                for (minutes, label) in [
                    (15, s.sleep_15),
                    (30, s.sleep_30),
                    (45, s.sleep_45),
                    (60, s.sleep_60),
                    (90, s.sleep_90),
                ] {
                    if Self::sheet_item(ui, Icon::Clock, label, false) {
                        self.core
                            .player
                            .send(Command::SetSleepTimer(Some(Duration::from_secs(
                                minutes * 60,
                            ))));
                        return true;
                    }
                }
                if self.player.sleep_at.is_some()
                    && Self::sheet_item(ui, Icon::Close, s.sleep_off, true)
                {
                    self.core.player.send(Command::SetSleepTimer(None));
                    return true;
                }
                false
            }
            Sheet::Info(track) => {
                let Some(track) = self.snapshot.get(*track).cloned() else {
                    return true;
                };
                Self::sheet_heading(ui, &track.title, artist_line(&track, s.unknown_artist));
                self.phone_info_lines(ui, &track);
                false
            }
        }
    }

    /// What the library knows about a song, line by line.
    fn phone_info_lines(&self, ui: &mut Ui, track: &Arc<Track>) {
        let s = self.s();
        let mut lines: Vec<(&str, String)> = Vec::new();
        if !track.album.is_empty() {
            lines.push((s.col_album, track.album.clone()));
        }
        if !track.album_artist.is_empty() && track.album_artist != track.artist {
            lines.push((s.info_album_artist, track.album_artist.clone()));
        }
        if let Some(year) = track.year {
            lines.push((s.info_year, year.to_string()));
        }
        if !track.genre.is_empty() {
            lines.push((s.info_genre, track.genre.clone()));
        }
        lines.push((s.col_duration, format_time(track.duration_ms)));
        let mut format = track.codec.clone();
        if track.sample_rate > 0 {
            format.push_str(&format!(
                " · {:.1} kHz",
                f64::from(track.sample_rate) / 1000.0
            ));
        }
        if let Some(bits) = track.bit_depth {
            format.push_str(&format!(" · {bits} bit"));
        }
        if let Some(kbps) = track.bitrate_kbps {
            format.push_str(&format!(" · {kbps} kbps"));
        }
        lines.push((s.info_format, format));
        lines.push((s.info_plays, track.play_count.to_string()));
        let file = track
            .file()
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        lines.push((s.info_file, file));
        for (name, value) in lines {
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
            ui.painter().text(
                pos2(rect.left() + SIDE + 4.0, rect.center().y),
                Align2::LEFT_CENTER,
                name,
                font(14.0),
                theme::dim(ui),
            );
            let value_rect = Rect::from_min_max(
                pos2(rect.left() + 120.0, rect.top()),
                pos2(rect.right() - SIDE, rect.bottom()),
            );
            let mut value_ui = ui.new_child(
                UiBuilder::new()
                    .id_salt(name)
                    .max_rect(value_rect)
                    .layout(Layout::right_to_left(Align::Center)),
            );
            value_ui.add(
                egui::Label::new(
                    RichText::new(value)
                        .font(font(15.0))
                        .color(ui.visuals().strong_text_color()),
                )
                .truncate(),
            );
        }
    }
}
