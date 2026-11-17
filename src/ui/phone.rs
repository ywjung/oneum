//! The phone screens: one narrow screen that is touched.
//!
//! The same app as on a desk (the same library and player snapshots, the same actions),
//! laid out for a phone: tabs at the bottom, a stack of pages in each, the player as a
//! bar above the tabs that opens to the full screen, and sheets from below for what a
//! right-click offers on a desk. Rows are tall enough to hit with a finger, and nothing
//! depends on a pointer hovering or on a keyboard (where a phone passes on the keys of
//! one, the shortcuts of the menus work: [`OneumApp::phone_request`]).
//!
//! The module is compiled everywhere: `ONEUM_PHONE=1` shows these screens in a window on
//! a Mac, which is how they are worked on and checked without a phone.
use super::dialogs::Dialog;
use super::icons::{self, Icon};
use super::lists::{self, Rediscover};
use super::{
    Action, Nav, OneumApp, Row, ToastAction, UiRequest, meter, next_repeat, spectrum, theme,
    widgets,
};
use crate::app::settings::{MAX_PINS, Pin};
use crate::i18n::format_time;
use crate::model::{Track, TrackId};
use crate::playback::{Command, PlayState, RepeatMode};
use eframe::egui::{
    self, Align, Align2, Color32, FontFamily, FontId, Layout, Rect, RichText, Sense, Ui, UiBuilder,
    pos2, vec2,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod edit;
mod info;

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
    /// The keys of a keyboard, where the phone passes them on.
    Shortcuts,
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
    /// The stars of these songs.
    Rating(Vec<TrackId>),
    Info(TrackId),
    Artist(String),
    Genre(String),
    /// The orders the songs of this list can be put in.
    Sort(Nav),
    /// What can be done with the songs chosen in this list.
    Selection(Nav),
    /// What can be done with the entries chosen in the queue.
    QueueSelection,
}

/// A row that is being carried to another place in its list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Reorder {
    /// Which list: its name as given to [`virtual_rows`].
    list: &'static str,
    /// Where the row was, and the gap it would go to if it were let go of now (0 is
    /// before the first row).
    from: usize,
    to: usize,
}

/// What the player shows in the place of the cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum PlayerView {
    #[default]
    Cover,
    /// The VU meters and the spectrum, with where the sound comes from and goes to.
    Meters,
    Lyrics,
    Queue,
}

/// A change of page that is being shown: the new page slides in over the old one, or
/// off it when going back.
struct Transition {
    /// The other page of the two: the one left behind going forward, the one that is
    /// leaving going back. `None` is the first page of the tab.
    other: Option<Page>,
    forward: bool,
    started: Instant,
    /// How far it had come when it began (a page dragged back and let go is part of
    /// the way already).
    from: f32,
}

/// How long a page takes to slide in or out, in seconds.
const TRANSITION: f32 = 0.30;
/// How long a sheet takes to go back down, in seconds.
const SHEET_FALL: f32 = 0.20;
/// The most a sheet is wide (in points).
const SHEET_WIDTH: f32 = 520.0;
/// How long the player and a sheet take to come up.
const RISE: f32 = 0.30;

/// Fast at first, slowing into place.
fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

/// The state of the phone screens.
pub(super) struct Phone {
    tab: Tab,
    /// The pages above the first one of each tab, in the order of [`TABS`].
    stacks: [Vec<Page>; 4],
    player_open: bool,
    player_view: PlayerView,
    sheet: Option<Sheet>,
    query: String,
    search_kind: SearchKind,
    search_rows: Vec<Row>,
    /// What `search_rows` was made from: library and playlist generations, query, kind.
    search_built: Option<(u64, u64, String, SearchKind)>,
    focus_search: bool,
    /// How far the search results were scrolled when last drawn.
    search_offset: f32,
    /// The view the rest of the app last asked for (`OneumApp::go`), as it was followed.
    seen_nav: Nav,
    /// How many favourites the library has: (library generation, count).
    favorites: Option<(u64, usize)>,
    /// What went wrong drawing the meters, once it has: they are not tried again in
    /// this run (see `phone_meters_guarded`).
    meters_failed: Option<String>,
    /// A verification script asked for the meters to fail once (`fail meters`), to see
    /// that the app survives it.
    fail_meters: bool,
    transition: Option<Transition>,
    /// How far the open page has been pulled back from the left edge, in points, and
    /// whether a finger is still on it.
    back_drag: f32,
    back_dragging: bool,
    /// How far the player has been pulled down.
    player_drag: f32,
    player_dragging: bool,
    /// When the sheet that is up came up, how tall it was when last drawn, and how far
    /// it has been pulled down.
    sheet_since: Option<Instant>,
    sheet_height: f32,
    sheet_drag: f32,
    sheet_dragging: bool,
    /// Since when the sheet has been on its way down, and which one that was.
    sheet_leaving: Option<(Instant, Sheet)>,
    /// The line of the lyrics that was current when they were last drawn.
    lyrics_line: Option<usize>,
    /// The list in which several songs are being chosen.
    selecting: Option<Nav>,
    reorder: Option<Reorder>,
    /// The smart playlist whose search was put into the search field to be changed:
    /// saving the search offers its name again.
    editing_smart: Option<String>,
    /// The list whose own search field is shown (it is also shown while it holds a
    /// search), and whether that field is to get the keyboard.
    searching: Option<Nav>,
    focus_list_search: bool,
    /// A song to bring into view in the list of this view when it is next drawn.
    reveal: Option<(Nav, TrackId)>,
    /// Several entries of the queue are being chosen.
    queue_selecting: bool,
    /// Whether music folders can be read (Android's "all files access"), and when that
    /// was last asked of the system.
    storage_access: Option<(Instant, bool)>,
}

impl Phone {
    /// See the field of that name.
    pub(super) fn fail_meters(&mut self) {
        self.fail_meters = true;
    }

    pub(super) fn new() -> Self {
        Self {
            tab: Tab::Library,
            stacks: Default::default(),
            player_open: false,
            player_view: PlayerView::default(),
            sheet: None,
            query: String::new(),
            search_kind: SearchKind::default(),
            search_rows: Vec::new(),
            search_built: None,
            focus_search: false,
            search_offset: 0.0,
            seen_nav: Nav::Songs,
            favorites: None,
            meters_failed: None,
            fail_meters: false,
            transition: None,
            back_drag: 0.0,
            back_dragging: false,
            player_drag: 0.0,
            player_dragging: false,
            sheet_since: None,
            sheet_height: 0.0,
            sheet_drag: 0.0,
            sheet_dragging: false,
            sheet_leaving: None,
            lyrics_line: None,
            selecting: None,
            reorder: None,
            editing_smart: None,
            searching: None,
            focus_list_search: false,
            reveal: None,
            queue_selecting: false,
            storage_access: None,
        }
    }

    fn tab_index(&self) -> usize {
        TABS.iter().position(|t| *t == self.tab).unwrap_or(0)
    }

    fn stack(&mut self) -> &mut Vec<Page> {
        let index = self.tab_index();
        &mut self.stacks[index]
    }

    fn page(&self) -> Option<&Page> {
        self.stacks[self.tab_index()].last()
    }

    /// Puts `page` above the ones of the open tab; it slides in.
    fn push(&mut self, page: Page) {
        let was = self.page().cloned();
        if was.as_ref() == Some(&page) {
            return;
        }
        self.stack().push(page);
        self.selecting = None;
        self.reorder = None;
        self.searching = None;
        self.transition = Some(Transition {
            other: was,
            forward: true,
            started: Instant::now(),
            from: 0.0,
        });
    }

    /// Goes back a page; the one that is left slides off. `from` is how far it has
    /// been pulled off already (0 for the back button).
    fn pop(&mut self, from: f32) {
        if let Some(page) = self.stack().pop() {
            self.transition = Some(Transition {
                other: Some(page),
                forward: false,
                started: Instant::now(),
                from: from.clamp(0.0, 1.0),
            });
        }
        self.back_drag = 0.0;
        self.back_dragging = false;
        self.selecting = None;
        self.reorder = None;
        self.searching = None;
    }

    /// Shows another tab (or the first page of the one that is open).
    fn show_tab(&mut self, tab: Tab) {
        if self.tab == tab {
            self.stack().clear();
        } else {
            self.tab = tab;
        }
        self.transition = None;
        self.back_drag = 0.0;
        self.back_dragging = false;
        self.selecting = None;
        self.reorder = None;
    }

    /// What is on screen, for verification dumps.
    pub(super) fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "tab": format!("{:?}", self.tab),
            "page": self.page().map(|page| format!("{page:?}")),
            "depth": self.stacks.iter().map(Vec::len).collect::<Vec<_>>(),
            "player_open": self.player_open,
            "player_view": format!("{:?}", self.player_view),
            "queue_open": self.player_view == PlayerView::Queue,
            "sheet": self.sheet.as_ref().map(|sheet| format!("{sheet:?}")),
            "query": self.query,
            "search_rows": self.search_rows.len(),
            "moving": self.transition.is_some(),
            "selecting": self.selecting.as_ref().map(|nav| format!("{nav:?}")),
            "searching": self.searching.as_ref().map(|nav| format!("{nav:?}")),
            "queue_selecting": self.queue_selecting,
            "carrying": self.reorder.map(|carried| (carried.from, carried.to)),
        })
    }
}

/// Lays out `count` rows of `height` below what is there and calls `row` for those in
/// view, each with a `Ui` of its own.
///
/// `list` names the list: two lists on one page (the folders of a folder above its
/// songs, playlists above smart playlists) must not share the ids of their rows.
/// Returns where the rows are, all of them (also those out of view).
fn virtual_rows(
    ui: &mut Ui,
    list: &'static str,
    count: usize,
    height: f32,
    mut row: impl FnMut(&mut Ui, usize),
) -> Rect {
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
    rect
}

/// What a page or a sheet that was pulled aside and let go has left of `pulled` in this
/// frame on its way back to where it was (asks for the next frame until it is there).
fn settle(ctx: &egui::Context, pulled: f32) -> f32 {
    let dt = ctx.input(|i| i.stable_dt).min(0.05);
    let left = pulled * (-dt * 18.0).exp();
    if left < 0.5 {
        0.0
    } else {
        ctx.request_repaint();
        left
    }
}

/// Darkens `rect` a little: a page that another one lies over.
fn shade(ui: &Ui, rect: Rect, amount: f32) {
    let alpha = (amount.clamp(0.0, 1.0) * 255.0) as u8;
    if alpha > 0 {
        ui.painter()
            .rect_filled(rect, 0.0, Color32::from_black_alpha(alpha));
    }
}

/// A button of the bar at the top of a page or of the player: an icon in a square a
/// finger can hit. True when it was tapped.
fn bar_button(
    ui: &mut Ui,
    id_salt: &'static str,
    centre: egui::Pos2,
    icon: Icon,
    label: &str,
    color: Color32,
) -> bool {
    let rect = Rect::from_center_size(centre, egui::Vec2::splat(TOUCH));
    let response = ui.interact(rect, ui.scope_id().with(id_salt), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    let color = if held(ui, &response) {
        color.gamma_multiply(0.6)
    } else {
        color
    };
    icons::paint(
        ui.painter(),
        Rect::from_center_size(centre, egui::Vec2::splat(23.0)),
        icon,
        color,
    );
    response.clicked()
}

/// True while a finger rests on what `response` is of: after a moment, because a touch
/// that begins a scroll must not light a row up, and only until the finger moves away.
fn held(ui: &Ui, response: &egui::Response) -> bool {
    if !response.is_pointer_button_down_on() {
        return false;
    }
    let (origin, at, since, now) = ui.input(|i| {
        (
            i.pointer.press_origin(),
            i.pointer.interact_pos(),
            i.pointer.press_start_time(),
            i.time,
        )
    });
    let moved = matches!((origin, at), (Some(a), Some(b)) if (b - a).length() > 6.0);
    let rested = since.is_some_and(|since| now - since > 0.07);
    if !moved && !rested {
        ui.ctx().request_repaint_after(Duration::from_millis(40));
    }
    !moved && rested
}

/// A row that can be tapped: the response of the whole row, and whether the finger
/// rests on it.
fn row_response(ui: &mut Ui, id_salt: &'static str) -> egui::Response {
    let rect = ui.max_rect();
    let response = ui.interact(rect, ui.scope_id().with(id_salt), Sense::click());
    if held(ui, &response) {
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

/// A small heading above a group of rows.
fn small_heading(ui: &mut Ui, text: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    ui.painter().text(
        pos2(rect.left() + SIDE, rect.bottom() - 9.0),
        Align2::LEFT_CENTER,
        text,
        theme::semibold(13.0),
        theme::dim(ui),
    );
}

/// The sheet of what can be done with what a pin leads to.
fn pin_sheet(pin: &Pin) -> Sheet {
    match pin {
        Pin::Album(key) => Sheet::Album(*key),
        Pin::Artist(name) => Sheet::Artist(name.clone()),
        Pin::Genre(name) => Sheet::Genre(name.clone()),
        Pin::Playlist(id) => Sheet::Playlist(*id),
        Pin::Smart(name) => Sheet::Smart(name.clone()),
    }
}

/// The circle at the left of a row while songs are being chosen: empty, or filled
/// with a tick.
fn choice_circle(ui: &Ui, centre: egui::Pos2, on: bool) {
    let accent = theme::accent(ui.visuals().dark_mode);
    if on {
        ui.painter().circle_filled(centre, 11.0, accent);
        let ink = if ui.visuals().dark_mode {
            Color32::from_rgb(0x14, 0x14, 0x18)
        } else {
            Color32::WHITE
        };
        icons::paint(
            ui.painter(),
            Rect::from_center_size(centre, egui::Vec2::splat(14.0)),
            Icon::Check,
            ink,
        );
    } else {
        ui.painter().circle_stroke(
            centre,
            11.0,
            egui::Stroke::new(1.5, ui.visuals().weak_text_color()),
        );
    }
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
        // The system's Back (a gesture or a button on Android).
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::BrowserBack)) {
            self.phone_back(&ctx);
        }
        let full = ctx.viewport_rect();
        let safe = ctx.content_rect();
        ui.painter().rect_filled(full, 0.0, ui.visuals().panel_fill);
        let mut actions: Vec<Action> = Vec::new();

        if self.player.current.is_none() {
            self.phone.player_open = false;
        }
        // How far up the player is: 0 put away, 1 over the whole screen.
        let up = ctx.animate_bool_with_time_and_easing(
            egui::Id::unique("phone-player-up"),
            self.phone.player_open,
            RISE,
            ease_out,
        );
        let mut above_bars = safe.bottom();
        // The on-screen keyboard covers the lower end of the screen: while it is up
        // the page ends above it, and the bars it would cover are left out.
        let keyboard = crate::platform::window::keyboard_height();
        // Under the player once it is all the way up, nothing needs drawing.
        if up < 1.0 || self.phone.player_drag > 0.0 {
            // Both bars run from edge to edge; what is on them keeps to the safe part
            // (a phone on its side has its camera at one end).
            let tab_bar = Rect::from_min_max(pos2(full.left(), safe.bottom() - TAB_BAR), full.max);
            let playing = self.player.current.is_some() && keyboard <= 0.0;
            // A screen that lies on its side has little height: there the song that
            // plays shares the line of the tabs instead of lying above it.
            let beside = playing && full.width() > full.height();
            let split = safe.left() + safe.width() * 0.5;
            let tabs = if beside {
                Rect::from_min_max(
                    pos2(safe.left(), tab_bar.top()),
                    pos2(split, tab_bar.bottom()),
                )
            } else {
                Rect::from_x_y_ranges(safe.x_range(), tab_bar.y_range())
            };
            let mini = playing.then(|| {
                let rect = if beside {
                    Rect::from_min_max(
                        pos2(split, tab_bar.top()),
                        pos2(full.right(), tab_bar.top() + TAB_BAR),
                    )
                } else {
                    Rect::from_min_max(
                        pos2(full.left(), tab_bar.top() - MINI),
                        pos2(full.right(), tab_bar.top()),
                    )
                };
                let inner = Rect::from_min_max(
                    pos2(rect.left().max(safe.left()), rect.top()),
                    pos2(safe.right(), rect.bottom()),
                );
                (rect, inner)
            });
            let content_bottom = if keyboard > 0.0 {
                full.bottom() - keyboard
            } else if beside {
                tab_bar.top()
            } else {
                mini.map_or(tab_bar.top(), |(mini, _)| mini.top())
            };
            above_bars = content_bottom;
            let pages = Rect::from_min_max(safe.min, pos2(safe.right(), content_bottom));
            self.phone_pages(ui, pages, &mut actions);
            if keyboard <= 0.0 {
                self.phone_tab_bar(ui, tab_bar, tabs);
            }
            if let Some((mini, inner)) = mini {
                self.phone_mini_player(ui, mini, inner);
                if beside {
                    ui.painter().vline(
                        split,
                        mini.y_range(),
                        egui::Stroke::new(1.0, widgets::divider_color(ui)),
                    );
                }
            }
        }
        let meters = up > 0.0 && self.phone.player_view == PlayerView::Meters;
        if up > 0.0 {
            // Let go of without closing: it settles back.
            if !self.phone.player_dragging && self.phone.player_drag > 0.0 {
                self.phone.player_drag = settle(&ctx, self.phone.player_drag);
            }
            let offset = (1.0 - up) * full.height() + self.phone.player_drag * up;
            // What is behind the player goes dark as it comes up.
            shade(ui, full, 0.4 * (1.0 - offset / full.height()));
            self.phone_player(ui, full, safe, offset, &mut actions);
            above_bars = safe.bottom();
        } else {
            self.phone.player_drag = 0.0;
            self.phone.player_dragging = false;
        }
        if !(self.phone.player_open && self.phone.player_view == PlayerView::Queue)
            && self.phone.queue_selecting
        {
            // Choosing entries of the queue ends with the queue being put away.
            self.phone.queue_selecting = false;
            self.queue_selection.clear();
        }
        if !meters {
            // Needles and columns fall back to rest while nobody looks at them.
            self.meter.suspend();
            self.spectrum.suspend();
        }
        self.phone_toast(&ctx, safe, above_bars);
        self.phone_sheet(&ctx, &mut actions);
        self.dialogs(&ctx);
        for action in actions {
            match action {
                Action::Go(nav) => self.phone_open(nav),
                Action::ShowInfo(track) => self.phone.sheet = Some(Sheet::Info(track)),
                // On a desk the search of a smart playlist goes into the search field
                // of the song list; here, into the search tab.
                Action::EditSmart(name) => {
                    if let Some(query) = self
                        .settings
                        .smart_playlists
                        .iter()
                        .find(|p| p.name == name)
                        .map(|p| p.query.clone())
                    {
                        self.phone_show_search(&query);
                        self.phone.editing_smart = Some(name);
                    }
                }
                other => self.apply(other),
            }
        }
    }

    /// What a command of the menus does on these screens. It comes from a key of a
    /// keyboard (where the phone passes those on), or from the menu bar of a Mac on
    /// which these screens are tried out.
    pub(super) fn phone_request(&mut self, request: UiRequest) {
        let current = self.player.current.map(|current| current.track_id);
        match request {
            UiRequest::Settings => self.phone_show_settings(None),
            UiRequest::Equalizer => self.phone_show_settings(Some(SettingsPage::Equalizer)),
            UiRequest::UserGuide => self.phone_show_settings(Some(SettingsPage::Guide)),
            UiRequest::Find => {
                self.phone.player_open = false;
                if self.phone.tab != Tab::Search {
                    self.phone.show_tab(Tab::Search);
                }
                self.phone.stack().clear();
                self.phone.focus_search = true;
            }
            UiRequest::NewPlaylist => {
                self.phone.player_open = false;
                if self.phone.tab != Tab::Playlists {
                    self.phone.show_tab(Tab::Playlists);
                }
                self.dialog = Some(Dialog::new_playlist(self.s().untitled_playlist, Vec::new()));
            }
            UiRequest::OpenFiles => {
                if crate::platform::COPIES_MUSIC_IN {
                    self.import_music(crate::platform::PickKind::Files);
                } else {
                    self.pick_files();
                }
            }
            UiRequest::AddFolder => {
                if self.storage_access() {
                    self.link_folder();
                } else {
                    self.warn_toast(self.s().phone_storage_missing);
                }
            }
            UiRequest::ImportPlaylist => self.pick_playlist_to_import(),
            UiRequest::GoToCurrent => {
                if let Some(track) = current {
                    if let Some(state) = self.views.get_mut(&Nav::Songs) {
                        state.query.clear();
                    }
                    self.phone.player_open = false;
                    self.phone.reveal = Some((Nav::Songs, track));
                    self.phone_open(Nav::Songs);
                }
            }
            UiRequest::ToggleQueue => self.phone_toggle_player(PlayerView::Queue),
            UiRequest::ToggleMeter => self.phone_toggle_player(PlayerView::Meters),
            // (A phone has no window to make small.)
            UiRequest::MiniPlayer => {}
            UiRequest::ToggleInfo | UiRequest::SongInfo => {
                if let Some(track) = current {
                    let sheet = Sheet::Info(track);
                    if self.phone.sheet.as_ref() != Some(&sheet) {
                        self.phone.sheet = Some(sheet);
                    } else if self.phone.sheet_leaving.is_none() {
                        self.phone.sheet_leaving = Some((Instant::now(), sheet));
                    }
                }
            }
            UiRequest::ToggleMute => self.toggle_mute(),
            // No key chooses songs in a list here: these are about the song that plays.
            UiRequest::SongToggleFavorite => {
                if let Some(track) = current {
                    let favorite = self.snapshot.get(track).is_some_and(|t| t.favorite);
                    self.apply(Action::SetFavorite(vec![track], !favorite));
                }
            }
            UiRequest::SongShowInFinder => {
                if let Some(track) = current {
                    self.apply(Action::Reveal(track));
                }
            }
            // What these do on a desk (to the playlist that is open, to the songs
            // chosen in a list) is in the sheets of these screens; no key asks for them.
            UiRequest::PlaylistRename
            | UiRequest::PlaylistExport
            | UiRequest::PlaylistDelete
            | UiRequest::SongPlay
            | UiRequest::SongPlayNext
            | UiRequest::SongAddToQueue => {}
        }
    }

    /// Shows the settings tab: its first page, or `page` (put away again if it is the
    /// one that is open, as the same key closes a window on a desk).
    fn phone_show_settings(&mut self, page: Option<SettingsPage>) {
        self.phone.player_open = false;
        if self.phone.tab != Tab::Settings {
            self.phone.show_tab(Tab::Settings);
        }
        match page.map(Page::Settings) {
            None => self.phone.stack().clear(),
            Some(page) if self.phone.page() == Some(&page) => self.phone.pop(0.0),
            Some(page) => {
                self.phone.stack().clear();
                self.phone.push(page);
            }
        }
    }

    /// Opens the player with `view` in the place of the cover; the cover comes back if
    /// that is what it shows already. Nothing without a song.
    fn phone_toggle_player(&mut self, view: PlayerView) {
        if self.player.current.is_none() {
            return;
        }
        if self.phone.player_open && self.phone.player_view == view {
            self.phone.player_view = PlayerView::Cover;
        } else {
            self.phone.player_open = true;
            self.phone.player_view = view;
            if view == PlayerView::Queue {
                self.queue_scroll_forced = true;
            }
        }
        self.phone.lyrics_line = None;
    }

    /// Shows the songs that `query` finds, in the search tab.
    fn phone_show_search(&mut self, query: &str) {
        self.phone.player_open = false;
        self.phone.show_tab(Tab::Search);
        self.phone.stack().clear();
        self.phone.query = query.to_string();
        self.phone.search_kind = SearchKind::Songs;
        self.phone.editing_smart = None;
    }

    /// The system's Back: what lies topmost goes first (a dialog, a sheet, the keyboard,
    /// the player), then what a list is in the middle of (choosing songs, its search),
    /// then the page, then the tab. On the first page of the first tab the app itself
    /// steps back behind what was there before, and plays on.
    pub(super) fn phone_back(&mut self, ctx: &egui::Context) {
        if self.dialog.is_some() {
            self.dialog = None;
        } else if let Some(sheet) = self.phone.sheet.clone() {
            if self.phone.sheet_leaving.is_none() {
                self.phone.sheet_leaving = Some((Instant::now(), sheet));
            }
        } else if let Some(focused) = ctx.memory(|memory| memory.focused()) {
            // A field that is typed into gives up the keyboard.
            ctx.memory_mut(|memory| memory.surrender_focus(focused));
        } else if self.phone.player_open {
            if self.phone.queue_selecting {
                self.phone.queue_selecting = false;
                self.queue_selection.clear();
            } else {
                self.phone.player_open = false;
            }
        } else if self.phone.selecting.is_some() {
            self.phone_end_selection();
        } else if let Some(nav) = self.phone.searching.take() {
            if let Some(state) = self.views.get_mut(&nav) {
                state.query.clear();
            }
        } else if matches!(self.phone.page(), Some(Page::List(Nav::Network))) && self.network.up() {
            // Out of the folder that is open on a media server, one at a time; from
            // the list of servers Back leaves the page like any other.
        } else if self.phone.page().is_some() {
            self.phone.pop(0.0);
        } else if self.phone.tab != Tab::Library {
            self.phone.show_tab(Tab::Library);
        } else {
            crate::platform::window::send_back();
        }
    }

    /// Shows `nav` as a page above the ones of the tab it belongs to.
    fn phone_open(&mut self, nav: Nav) {
        self.phone.player_open = false;
        // A sheet that is up goes back down (rather than vanishing) while the page
        // comes in.
        if let Some(sheet) = self.phone.sheet.clone()
            && self.phone.sheet_leaving.is_none()
        {
            self.phone.sheet_leaving = Some((Instant::now(), sheet));
        }
        if matches!(nav, Nav::Playlist(_) | Nav::Smart(_)) {
            if self.phone.tab != Tab::Playlists {
                self.phone.show_tab(Tab::Playlists);
            }
        } else if self.phone.tab == Tab::Settings || self.phone.tab == Tab::Playlists {
            self.phone.show_tab(Tab::Library);
        }
        self.phone.push(Page::List(nav.clone()));
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

    /// The pages of the open tab in `area`: the open one, and while it slides in or out
    /// (or is pulled back with a finger) the other one beside it.
    fn phone_pages(&mut self, ui: &mut Ui, area: Rect, actions: &mut Vec<Action>) {
        let ctx = ui.ctx().clone();
        let tab = self.phone.tab_index();
        let depth = self.phone.stacks[tab].len();
        let page = self.phone.page().cloned();
        let width = area.width();
        // Pulled back and let go without going back: it settles where it was.
        if !self.phone.back_dragging && self.phone.back_drag > 0.0 {
            self.phone.back_drag = settle(&ctx, self.phone.back_drag);
        }
        // Where a page change that is under way has come to.
        let mut moving: Option<(Option<Page>, bool, f32)> = None;
        if let Some(transition) = &self.phone.transition {
            let time = transition.started.elapsed().as_secs_f32() / TRANSITION;
            if time >= 1.0 {
                self.phone.transition = None;
            } else {
                let t = transition.from + (1.0 - transition.from) * ease_out(time);
                moving = Some((transition.other.clone(), transition.forward, t));
                ctx.request_repaint();
            }
        }
        // The page underneath is a third of the way to the side and a little darker.
        let under = |t: f32| area.translate(vec2(-0.3 * width * t, 0.0));
        let ground = Rect::from_min_max(pos2(area.left(), ctx.viewport_rect().top()), area.max);
        match moving {
            Some((other, true, t)) => {
                self.phone_page(
                    ui,
                    (tab, depth.saturating_sub(1)),
                    &other,
                    under(t),
                    area,
                    actions,
                );
                shade(ui, ground, 0.12 * t);
                let over = area.translate(vec2(width * (1.0 - t), 0.0));
                self.phone_page(ui, (tab, depth), &page, over, area, actions);
            }
            Some((other, false, t)) => {
                self.phone_page(ui, (tab, depth), &page, under(1.0 - t), area, actions);
                shade(ui, ground, 0.12 * (1.0 - t));
                let over = area.translate(vec2(width * t, 0.0));
                self.phone_page(ui, (tab, depth + 1), &other, over, area, actions);
            }
            None if self.phone.back_drag > 0.0 && depth > 0 => {
                let t = (self.phone.back_drag / width).clamp(0.0, 1.0);
                let beneath = depth
                    .checked_sub(2)
                    .and_then(|index| self.phone.stacks[tab].get(index))
                    .cloned();
                self.phone_page(
                    ui,
                    (tab, depth - 1),
                    &beneath,
                    under(1.0 - t),
                    area,
                    actions,
                );
                shade(ui, ground, 0.12 * (1.0 - t));
                let over = area.translate(vec2(self.phone.back_drag, 0.0));
                self.phone_page(ui, (tab, depth), &page, over, area, actions);
            }
            None => self.phone_page(ui, (tab, depth), &page, area, area, actions),
        }

        // A finger pulling from the left edge takes the page back with it. This is read
        // from the pointer itself: a widget over the page would keep taps from what is
        // under it, and one below it would lose the pull to the list.
        let depth = self.phone.stacks[tab].len();
        let free = depth > 0
            && self.phone.transition.is_none()
            && self.phone.sheet.is_none()
            && !self.phone.player_open
            && self.dialog.is_none();
        let (origin, at, down, speed) = ui.input(|i| {
            (
                i.pointer.press_origin(),
                i.pointer.latest_pos(),
                i.pointer.primary_down(),
                i.pointer.velocity().x,
            )
        });
        if !free {
            self.phone.back_dragging = false;
            self.phone.back_drag = 0.0;
        } else if down {
            if let (Some(origin), Some(at)) = (origin, at) {
                let pulled = at - origin;
                let from_edge = origin.x <= area.left() + 24.0 && area.contains(origin);
                if !self.phone.back_dragging
                    && from_edge
                    && pulled.x > 10.0
                    && pulled.x > pulled.y.abs()
                {
                    self.phone.back_dragging = true;
                }
                if self.phone.back_dragging {
                    self.phone.back_drag = pulled.x.clamp(0.0, width);
                    ctx.request_repaint();
                }
            }
        } else if self.phone.back_dragging {
            self.phone.back_dragging = false;
            if self.phone.back_drag > width * 0.33 || speed > 500.0 {
                let from = self.phone.back_drag / width;
                self.phone.pop(from);
            }
        }
    }

    /// One page, drawn in `area` (which may be partly off the screen while the page
    /// slides) and cut off at `clip`. `place` is the tab and how deep in its stack the
    /// page is: what the page keeps (how far a list is scrolled) is kept under that, so
    /// it is the same whether the page is the open one or the one beside it.
    fn phone_page(
        &mut self,
        ui: &mut Ui,
        place: (usize, usize),
        page: &Option<Page>,
        area: Rect,
        clip: Rect,
        actions: &mut Vec<Action>,
    ) {
        let s = self.s();
        let mut page_ui = ui.new_child(
            UiBuilder::new()
                .id_salt(("phone-page", place.0, place.1))
                .max_rect(area),
        );
        page_ui.set_clip_rect(area.intersect(clip));
        let ui = &mut page_ui;
        // The page's ground reaches up behind the status bar.
        let top = ui.ctx().viewport_rect().top();
        let ground = Rect::from_min_max(pos2(area.left(), top), area.max);
        let ground_clip = Rect::from_min_max(pos2(clip.left(), top), clip.max);
        let painter = ui.painter_at(ground_clip);
        painter.rect_filled(ground, 0.0, ui.visuals().panel_fill);
        if area.left() > clip.left() + 0.5 {
            // The edge of a page that lies over another: a soft shadow to its left.
            for step in 0..6 {
                let x = area.left() - step as f32 * 3.0;
                painter.rect_filled(
                    Rect::from_min_max(pos2(x - 3.0, top), pos2(x, area.bottom())),
                    0.0,
                    Color32::from_black_alpha(14_u8.saturating_sub(step * 2)),
                );
            }
        }
        let header = Rect::from_min_max(area.min, pos2(area.right(), area.top() + HEADER));
        let content = Rect::from_min_max(pos2(area.left(), header.bottom()), area.max);
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
                SettingsPage::Shortcuts => s.settings_shortcuts,
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
                // On a media server: out of the folder that is open, one at a time
                // (as the system's Back does), and out of the page from the list of
                // servers.
                let in_server = matches!(page, Some(Page::List(Nav::Network)));
                if !(in_server && self.network.up()) {
                    self.phone.pop(0.0);
                }
                return;
            }
            left = back.right();
        }
        let mut right = header.right() - 4.0;
        // A list can be searched where it is, as the search field of a desk does for
        // the view that is open.
        let searchable = match &page {
            Some(Page::List(nav)) => edit::searchable(nav).then_some(nav),
            _ => None,
        };
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
        if let Some(nav) = searchable {
            let centre = pos2(right - TOUCH / 2.0, header.center().y);
            let on = self.phone.searching.as_ref() == Some(nav);
            let color = if on {
                ui.visuals().strong_text_color()
            } else {
                theme::accent(ui.visuals().dark_mode)
            };
            if bar_button(
                ui,
                "phone-page-search",
                centre,
                Icon::Search,
                s.phone_search_here,
                color,
            ) {
                if on {
                    // Put away, and with it what it was looking for.
                    self.phone.searching = None;
                    if let Some(state) = self.views.get_mut(nav) {
                        state.query.clear();
                    }
                } else {
                    self.phone.searching = Some(nav.clone());
                    self.phone.focus_list_search = true;
                }
            }
            right -= TOUCH;
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
        body.set_clip_rect(content.intersect(clip));
        if let Some(nav) = searchable {
            self.phone_list_search(&mut body, nav);
        }
        match page.clone() {
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
            // The folder that is open on a media server, if one is.
            Nav::Network => self
                .network
                .place()
                .map_or_else(|| s.network.into(), |(here, _)| here),
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
        // Counted once for each state of the library, not in every frame.
        let generation = self.lib_generation;
        let favorites = match self.phone.favorites {
            Some((counted_for, count)) if counted_for == generation => count,
            _ => {
                let count = self.snapshot.tracks.iter().filter(|t| t.favorite).count();
                self.phone.favorites = Some((generation, count));
                count
            }
        };
        let entries: [(Icon, &str, Option<usize>, Nav); 9] = [
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
            (Icon::Network, s.network, None, Nav::Network),
        ];
        // What was pinned comes first, as at the top of the sidebar on a desk.
        let pins: Vec<(Pin, Icon, String)> = self
            .live_pins()
            .into_iter()
            .filter_map(|pin| {
                let (icon, name) = self.pin_target(&pin)?;
                Some((pin, icon, name))
            })
            .collect();
        egui::ScrollArea::vertical()
            .id_salt("phone-library")
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                if !pins.is_empty() {
                    small_heading(ui, s.pinned);
                    virtual_rows(ui, "pins", pins.len(), MENU_ROW, |ui, index| {
                        let (pin, icon, name) = &pins[index];
                        let rect = ui.max_rect();
                        let tapped = self.phone_menu_row(ui, *icon, name, None, true);
                        // A long press offers what can be done with it (unpinning too).
                        let pressed = ui
                            .interact(rect, ui.scope_id().with("menu"), Sense::click())
                            .long_touched();
                        if pressed {
                            self.phone.sheet = Some(pin_sheet(pin));
                        } else if tapped {
                            actions.push(Action::Go(super::pin_nav(pin)));
                        }
                    });
                    ui.add_space(14.0);
                }
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
    /// Whether music folders can be read. Where that takes a right of the system's
    /// (Android), it is asked again about once a second while a page shows it: the
    /// user gives the right in the system's settings and comes back.
    fn storage_access(&mut self) -> bool {
        if !crate::platform::STORAGE_NEEDS_ACCESS {
            return true;
        }
        match self.phone.storage_access {
            Some((asked, access)) if asked.elapsed() < Duration::from_secs(1) => access,
            _ => {
                let access = crate::platform::files::has_storage_access();
                self.phone.storage_access = Some((Instant::now(), access));
                access
            }
        }
    }

    fn phone_empty_library(&mut self, ui: &mut Ui) {
        let s = self.s();
        // Where a folder is copied into the app's own (an iPhone) music is imported;
        // elsewhere folders are added and read where they are.
        let copies = crate::platform::COPIES_MUSIC_IN;
        let access = self.storage_access();
        let mut import_files = false;
        let mut import_folder = false;
        let mut add_folder = false;
        let mut allow = false;
        let mut rescan = false;
        let mut network = false;
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
                                RichText::new(if copies {
                                    s.phone_empty_body
                                } else if access {
                                    s.phone_empty_body_folders
                                } else {
                                    s.phone_storage_needed
                                })
                                .font(font(15.0))
                                .color(theme::dim(ui)),
                            )
                            .wrap()
                            .halign(Align::Center),
                        );
                    });
                    ui.add_space(22.0);
                    let width = (ui.available_width() - 2.0 * 28.0).clamp(160.0, 320.0);
                    if copies {
                        import_files =
                            pill_button(ui, Icon::Plus, s.phone_import_files, width, true)
                                .clicked();
                        ui.add_space(10.0);
                        import_folder =
                            pill_button(ui, Icon::Folder, s.phone_import_folder, width, false)
                                .clicked();
                    } else if access {
                        add_folder = pill_button(ui, Icon::Folder, s.phone_add_folder, width, true)
                            .clicked();
                    } else {
                        allow = pill_button(ui, Icon::Check, s.phone_storage_allow, width, true)
                            .clicked();
                    }
                    ui.add_space(10.0);
                    rescan = pill_button(ui, Icon::Repeat, s.rescan, width, false).clicked();
                    // Music need not be on the phone: a media server of the network
                    // has it too, and this page is all there is of the library tab
                    // while the library is empty.
                    ui.add_space(10.0);
                    network =
                        pill_button(ui, Icon::Network, s.network_open, width, false).clicked();
                });
            });
        if network {
            self.phone.push(Page::List(Nav::Network));
        }
        if add_folder {
            self.link_folder();
        }
        if allow {
            crate::platform::files::ask_storage_access();
        }
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
                // The view scrolls by itself; here it only gets the page's margins.
                let inside = ui.max_rect().shrink2(vec2(SIDE - 4.0, 0.0));
                let mut stats_ui = ui.new_child(
                    UiBuilder::new()
                        .id_salt("phone-stats")
                        .max_rect(inside)
                        .layout(Layout::top_down(Align::Min)),
                );
                self.stats_view(&mut stats_ui, actions);
            }
            Nav::Network => {
                let inside = ui.max_rect().shrink2(vec2(SIDE - 4.0, 0.0));
                let mut network_ui = ui.new_child(
                    UiBuilder::new()
                        .id_salt("phone-network")
                        .max_rect(inside)
                        .layout(Layout::top_down(Align::Min)),
                );
                self.network_view(&mut network_ui, actions);
            }
            Nav::Albums | Nav::Artist(_) => {
                egui::ScrollArea::vertical()
                    .id_salt(("phone-list", nav))
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        if rows.is_empty() {
                            let searched = self
                                .views
                                .get(nav)
                                .is_some_and(|state| !state.query.trim().is_empty());
                            self.empty_list_message(ui, nav, searched);
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
                            let searched = self
                                .views
                                .get(nav)
                                .is_some_and(|state| !state.query.trim().is_empty());
                            self.empty_list_message(ui, nav, searched);
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
                            let rect = ui.max_rect();
                            let tapped =
                                self.phone_menu_row(ui, Icon::Tag, &genre.name, Some(&count), true);
                            let pressed = ui
                                .interact(rect, ui.scope_id().with("menu"), Sense::click())
                                .long_touched();
                            if pressed {
                                self.phone.sheet = Some(Sheet::Genre(genre.name.clone()));
                            } else if tapped {
                                actions.push(Action::Go(Nav::Genre(genre.name.clone())));
                            }
                        });
                    });
            }
            _ => {
                let mut area = egui::ScrollArea::vertical()
                    .id_salt(("phone-list", nav))
                    .auto_shrink(false);
                // A song that was asked for is brought into view once, with a row or
                // two before it (what is above the rows is about as tall as that).
                if self.phone.reveal.as_ref().is_some_and(|(of, _)| of == nav)
                    && let Some((_, wanted)) = self.phone.reveal.take()
                    && let Some(index) = rows.iter().position(|row| {
                        self.snapshot
                            .tracks
                            .get(row.idx as usize)
                            .is_some_and(|track| track.id == wanted)
                    })
                {
                    area = area.vertical_scroll_offset(index as f32 * ROW);
                }
                area.show(ui, |ui| {
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
                let searched = self
                    .views
                    .get(nav)
                    .is_some_and(|state| !state.query.trim().is_empty());
                self.empty_list_message(ui, nav, searched);
            }
            return;
        }
        // Play, and play in a shuffled order; while songs are being chosen, what to do
        // with those instead.
        ui.add_space(10.0);
        let mut play = false;
        let mut shuffle = false;
        if self.phone.selecting.as_ref() == Some(nav) {
            self.phone_selection_bar(ui, nav, rows);
        } else {
            ui.horizontal(|ui| {
                ui.add_space(SIDE);
                let width = (ui.available_width() - SIDE - 10.0) / 2.0;
                ui.spacing_mut().item_spacing.x = 10.0;
                play = pill_button(ui, Icon::Play, s.play, width, true).clicked();
                shuffle = pill_button(ui, Icon::Shuffle, s.shuffle, width, false).clicked();
            });
        }
        self.phone_list_tools(ui, nav, rows);
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
        // While songs are being chosen a tap chooses, and the rows offer nothing else.
        let mut chosen = (self.phone.selecting.as_ref() == Some(nav)).then(|| {
            self.views
                .get_mut(nav)
                .map(|state| std::mem::take(&mut state.selection))
                .unwrap_or_default()
        });
        // The songs of a playlist are in the order they were put in: a row can be
        // carried to another place (not while another order is shown).
        let reorderable = playlist.is_some()
            && chosen.is_none()
            && self
                .views
                .get(nav)
                .is_none_or(|state| state.sort.is_none() && state.query.is_empty());
        let all_rows = virtual_rows(ui, "songs", rows.len(), ROW, |ui, index| {
            let row = rows[index];
            let Some(track) = snapshot.tracks.get(row.idx as usize) else {
                return;
            };
            let rect = ui.max_rect();
            let response = row_response(ui, "song");
            response
                .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &track.title));
            let dark = ui.visuals().dark_mode;
            if self.is_carried("songs", index) {
                ui.painter().rect_filled(
                    rect,
                    0.0,
                    theme::accent(dark).gamma_multiply(if dark { 0.20 } else { 0.12 }),
                );
            }
            let is_playing = playing == Some(track.id);
            let mut left = rect.left() + SIDE;
            if let Some(chosen) = &chosen {
                choice_circle(
                    ui,
                    pos2(left + 12.0, rect.center().y),
                    chosen.contains(&row.key),
                );
                left += 36.0;
            }
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
            // At the right: "more", and before it the handle a row is carried by.
            let mut right = if chosen.is_some() {
                rect.right() - SIDE
            } else {
                rect.right() - SIDE - TOUCH
            };
            if reorderable {
                let grip =
                    Rect::from_min_max(pos2(right - 34.0, rect.top()), pos2(right, rect.bottom()));
                self.reorder_grip(ui, "songs", index, grip);
                right = grip.left();
            }
            let text = Rect::from_min_max(pos2(left, rect.top()), pos2(right, rect.bottom()));
            two_lines(
                ui,
                text,
                RichText::new(&track.title)
                    .font(font(16.5))
                    .color(title_color),
                Some(RichText::new(below).font(font(13.5)).color(theme::dim(ui))),
            );
            row_line(ui, left);
            if let Some(chosen) = &mut chosen {
                if response.clicked() && !chosen.remove(&row.key) {
                    chosen.insert(row.key);
                }
                return;
            }
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
        if let Some(chosen) = chosen
            && let Some(state) = self.views.get_mut(nav)
        {
            state.selection = chosen;
        }
        if let Some(id) = playlist
            && let Some((from, to)) = self.follow_reorder(ui, "songs", all_rows, ROW, rows.len())
            && let Some(row) = rows.get(from)
        {
            actions.push(Action::MovePlaylistItems(id, vec![row.key as i64], to));
        }
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
                    let texture = self
                        .art
                        .get_sized(album.art_id.as_deref(), super::art::ArtSize::Grid);
                    widgets::art(ui, art, texture, 10.0);
                    if held(ui, &response) {
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
            if response.long_touched() {
                self.phone.sheet = Some(Sheet::Artist(artist.name.clone()));
            } else if response.clicked() {
                actions.push(Action::Go(Nav::Artist(artist.name.clone())));
            }
        });
    }

    /// The search tab: a field, what to look for, and what was found.
    fn phone_search(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let s = self.s();
        // What is shown here are songs of the whole library: a list that is opened
        // from here (a search kept as a smart playlist) is another view than this,
        // also when it was the one looked at last.
        self.nav = Nav::Songs;
        self.phone.seen_nav = Nav::Songs;
        ui.add_space(4.0);
        let mut clear = false;
        let mut save = false;
        // A search for songs can be kept as a smart playlist, as on a desk.
        let savable =
            self.phone.search_kind == SearchKind::Songs && !self.phone.query.trim().is_empty();
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
                }
                - if savable { 40.0 } else { 0.0 };
            let edit = egui::TextEdit::singleline(&mut self.phone.query)
                .hint_text(s.search_hint)
                .font(font(17.0))
                .frame(egui::Frame::NONE)
                .align(Align2::LEFT_CENTER)
                .desired_width(room);
            let response = ui.add(edit);
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::TextInput, true, s.search_label)
            });
            if std::mem::take(&mut self.phone.focus_search) {
                response.request_focus();
            }
            if savable && widgets::icon_button(ui, Icon::Plus, s.smart_save, 32.0, false).clicked()
            {
                save = true;
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
        if self.phone.query.is_empty() {
            self.phone.editing_smart = None;
        }
        if save {
            use crate::app::settings::{MAX_SMART_QUERY_CHARS, tidy_text};
            let query = tidy_text(&self.phone.query, MAX_SMART_QUERY_CHARS);
            // The search of a smart playlist that was brought here to be changed is
            // offered its name again: saved under it, it takes the old one's place.
            let name = self
                .phone
                .editing_smart
                .clone()
                .filter(|name| {
                    self.settings
                        .smart_playlists
                        .iter()
                        .any(|p| &p.name == name)
                })
                .unwrap_or_else(|| query.clone());
            self.dialog = Some(Dialog::smart_playlist(&name, query, None));
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
            egui::ScrollArea::vertical()
                .id_salt("phone-search-tips")
                .auto_shrink(false)
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(36.0);
                        let (rect, _) =
                            ui.allocate_exact_size(egui::Vec2::splat(44.0), Sense::hover());
                        icons::paint(
                            ui.painter(),
                            rect,
                            Icon::Search,
                            ui.visuals().weak_text_color(),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(s.phone_search_prompt)
                                .font(font(16.0))
                                .color(ui.visuals().strong_text_color()),
                        );
                    });
                    // How a search is narrowed: what a pointer resting on the field
                    // says on a desk.
                    ui.add_space(22.0);
                    ui.horizontal(|ui| {
                        ui.add_space(SIDE + 6.0);
                        ui.vertical(|ui| {
                            ui.set_width(ui.available_width() - SIDE - 6.0);
                            ui.spacing_mut().item_spacing.y = 8.0;
                            for line in s.phone_search_tips.lines() {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(line).font(font(13.5)).color(theme::dim(ui)),
                                    )
                                    .wrap(),
                                );
                            }
                        });
                    });
                    ui.add_space(16.0);
                });
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
            let shown = egui::ScrollArea::vertical()
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
            // Moving the results with a finger puts the keyboard away: it covers half
            // of them.
            let offset = shown.state.clamped_offset().y;
            if (offset - self.phone.search_offset).abs() > 2.0 && ui.input(|i| i.pointer.any_down())
            {
                ui.memory_mut(|memory| {
                    if let Some(focused) = memory.focused() {
                        memory.surrender_focus(focused);
                    }
                });
            }
            self.phone.search_offset = offset;
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
                    ui.add_space(8.0);
                    small_heading(ui, s.phone_smart);
                    virtual_rows(ui, "smart", smart.len(), MENU_ROW, |ui, index| {
                        let playlist = &smart[index];
                        let rect = ui.max_rect();
                        let tapped =
                            self.phone_menu_row(ui, Icon::Search, &playlist.name, None, true);
                        let pressed = ui
                            .interact(rect, ui.scope_id().with("menu"), Sense::click())
                            .long_touched();
                        if pressed {
                            self.phone.sheet = Some(Sheet::Smart(playlist.name.clone()));
                        } else if tapped {
                            actions.push(Action::Go(Nav::Smart(playlist.name.clone())));
                        }
                    });
                }
            });
    }

    /// The settings tab: the groups of settings.
    fn phone_settings(&mut self, ui: &mut Ui) {
        let s = self.s();
        let mut entries = vec![
            (Icon::Folder, s.phone_library, SettingsPage::Library),
            (Icon::Play, s.settings_playback, SettingsPage::Playback),
            (Icon::Equalizer, s.equalizer, SettingsPage::Equalizer),
            (Icon::Gear, s.settings_general, SettingsPage::General),
            (Icon::Keyboard, s.menu_user_guide, SettingsPage::Guide),
            (Icon::Info, s.settings_about, SettingsPage::About),
        ];
        if crate::platform::PHONE_KEYBOARD {
            // The keyboard is the sign of its keys there; the guide gets words.
            entries[4].0 = Icon::Lyrics;
            entries.insert(
                4,
                (
                    Icon::Keyboard,
                    s.settings_shortcuts,
                    SettingsPage::Shortcuts,
                ),
            );
        }
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
            self.phone.push(Page::Settings(page));
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
        egui::ScrollArea::vertical()
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
                            SettingsPage::Shortcuts => {
                                ui.set_width(width);
                                self.phone_settings_shortcuts(ui);
                            }
                            SettingsPage::Equalizer => {
                                ui.set_width(width);
                                widgets::card(ui, |ui| {
                                    let state = if self.settings.eq.enabled {
                                        s.eq_on
                                    } else {
                                        s.eq_off
                                    };
                                    widgets::setting_row(
                                        ui,
                                        s.equalizer,
                                        Some(state),
                                        true,
                                        |ui| {
                                            widgets::toggle(
                                                ui,
                                                &mut self.settings.eq.enabled,
                                                s.equalizer,
                                            );
                                        },
                                    );
                                });
                                ui.add_space(14.0);
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
        let mut restore_removed = false;
        let mut show_damaged = false;
        let mut import_apple = false;
        let (mut export_data, mut import_data) = (false, false);
        let mut link_folder = false;
        let mut open_files = false;
        let mut show_folder: Option<std::path::PathBuf> = None;
        let mut unlink: Option<(i64, String)> = None;
        // The folders that are used where they are: every music folder but the app's
        // own.
        let copies = crate::platform::COPIES_MUSIC_IN;
        let access = self.storage_access();
        let mut allow = false;
        let own = copies
            .then(crate::app::paths::AppPaths::phone_music_dir)
            .flatten();
        let linked: Vec<_> = self
            .core
            .library
            .roots()
            .into_iter()
            .filter(|root| Some(&root.path) != own.as_ref())
            .collect();
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
                            if copies {
                                widgets::setting_row(
                                    ui,
                                    s.phone_music_place,
                                    Some(s.phone_music_place_desc),
                                    true,
                                    |_| {},
                                );
                                widgets::row_divider(ui);
                            } else if !access {
                                widgets::setting_row(
                                    ui,
                                    s.phone_storage_row,
                                    Some(s.phone_storage_missing),
                                    true,
                                    |ui| {
                                        allow = ui.button(s.phone_storage_allow).clicked();
                                    },
                                );
                                widgets::row_divider(ui);
                            }
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
                            // Songs that were removed stay out of every scan: this is
                            // the way back for them. Always there, also with nothing to
                            // bring back: a row that comes and goes is not found.
                            let removed = self.core.library.removed_count();
                            widgets::row_divider(ui);
                            widgets::setting_row(
                                ui,
                                s.restore_removed,
                                Some(s.restore_removed_desc),
                                true,
                                |ui| {
                                    let button = egui::Button::new(s.restore_removed_button);
                                    if ui
                                        .add_enabled(removed > 0 && !scan.running, button)
                                        .clicked()
                                    {
                                        restore_removed = true;
                                    }
                                    ui.label(
                                        RichText::new(self.lang.song_count(removed))
                                            .small()
                                            .color(theme::dim(ui)),
                                    );
                                },
                            );
                        });
                        ui.add_space(14.0);
                        widgets::card(ui, |ui| {
                            if copies {
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
                            } else {
                                // Music is used where it is: single files can be
                                // played without their folder, as on a desk.
                                widgets::setting_row(
                                    ui,
                                    s.phone_open_files,
                                    Some(s.phone_open_files_desc),
                                    true,
                                    |ui| {
                                        if ui.button(s.phone_open_files).clicked() {
                                            open_files = true;
                                        }
                                    },
                                );
                                widgets::row_divider(ui);
                            }
                            widgets::setting_row(ui, s.playlists, None, true, |ui| {
                                if ui.button(s.import_m3u8).clicked() {
                                    self.pick_playlist_to_import();
                                }
                            });
                        });
                        // Folders that are read where they are.
                        ui.add_space(14.0);
                        widgets::section_title(
                            ui,
                            if copies {
                                s.phone_linked_folders
                            } else {
                                s.phone_music_folders
                            },
                        );
                        widgets::card(ui, |ui| {
                            for root in &linked {
                                let name = root.path.file_name().map_or_else(
                                    || root.path.display().to_string(),
                                    |name| name.to_string_lossy().into_owned(),
                                );
                                let state = if root.online {
                                    self.lang.song_count(root.track_count)
                                } else {
                                    s.folder_offline.to_string()
                                };
                                ui.push_id(("phone-linked", root.id), |ui| {
                                    widgets::setting_row(ui, &name, Some(&state), true, |ui| {
                                        let label = if copies {
                                            s.phone_unlink
                                        } else {
                                            s.remove_folder
                                        };
                                        if ui.button(label).clicked() {
                                            unlink =
                                                Some((root.id, root.path.display().to_string()));
                                        }
                                        if crate::platform::PHONE_FILE_MANAGER
                                            && ui.button(s.phone_show_folder).clicked()
                                        {
                                            show_folder = Some(root.path.clone());
                                        }
                                    });
                                });
                                widgets::row_divider(ui);
                            }
                            let (add, about) = if copies {
                                (s.phone_link_folder, Some(s.phone_link_folder_desc))
                            } else {
                                (s.phone_add_folder, None)
                            };
                            widgets::setting_row(ui, add, about, true, |ui| {
                                if ui.button(add).clicked() {
                                    link_folder = true;
                                }
                            });
                        });
                        widgets::section_note(
                            ui,
                            if copies {
                                s.phone_linked_hint
                            } else {
                                s.phone_folders_hint
                            },
                        );
                        // Reading every file to the end to see that it is whole, as on
                        // a desk. The damaged ones are shown in the search tab.
                        ui.add_space(18.0);
                        show_damaged = self.check_card(ui);
                        // What Apple Music knew: play counts, favourites, playlists.
                        ui.add_space(14.0);
                        widgets::section_title(ui, s.import_title);
                        widgets::card(ui, |ui| {
                            widgets::setting_row(
                                ui,
                                s.apple_import_row,
                                Some(s.apple_import_desc),
                                true,
                                |ui| {
                                    import_apple = ui.button(s.apple_import_button).clicked();
                                },
                            );
                        });
                        widgets::section_note(ui, s.phone_apple_import_hint);
                        // The library's own data, to keep or to take to another device.
                        ui.add_space(14.0);
                        widgets::section_title(ui, s.phone_data_title);
                        widgets::card(ui, |ui| {
                            widgets::setting_row(
                                ui,
                                s.data_export_row,
                                Some(s.data_export_desc),
                                true,
                                |ui| {
                                    export_data = ui.button(s.data_export_button).clicked();
                                },
                            );
                            widgets::row_divider(ui);
                            widgets::setting_row(
                                ui,
                                s.data_import_row,
                                Some(s.phone_data_import_desc),
                                true,
                                |ui| {
                                    import_data = ui.button(s.apple_import_button).clicked();
                                },
                            );
                        });
                        widgets::section_note(ui, s.phone_data_hint);
                    });
                });
                ui.add_space(16.0);
            });
        if show_damaged {
            self.phone_show_search("check:damaged");
        }
        if import_apple {
            self.pick_apple_library();
        }
        if export_data {
            self.pick_data_export();
        }
        if import_data {
            self.pick_data_import();
        }
        if restore_removed {
            self.core.library.restore_removed();
        }
        if import_files {
            self.import_music(crate::platform::PickKind::Files);
        }
        if import_folder {
            self.import_music(crate::platform::PickKind::Folder);
        }
        if link_folder {
            self.link_folder();
        }
        if open_files {
            self.pick_files();
        }
        if let Some(folder) = show_folder {
            self.reveal_path(&folder);
        }
        if allow {
            crate::platform::files::ask_storage_access();
        }
        if let Some((id, path)) = unlink {
            self.dialog = Some(super::dialogs::Dialog::RemoveFolder(id, path));
        }
        if rescan {
            // A drive that was plugged in since the app started: its linked folders
            // can be read again from here on.
            crate::platform::files::reach_linked_folders();
            self.announce_scan = true;
            self.core.library.rescan();
        }
    }

    // ----- the bars at the bottom -------------------------------------------------------

    /// The tabs: `rect` is the bar, `inner` the part of it they share out.
    fn phone_tab_bar(&mut self, ui: &mut Ui, rect: Rect, inner: Rect) {
        let s = self.s();
        ui.painter().rect_filled(rect, 0.0, theme::chrome_fill(ui));
        ui.painter().hline(
            rect.x_range(),
            rect.top(),
            egui::Stroke::new(1.0, widgets::divider_color(ui)),
        );
        let dark = ui.visuals().dark_mode;
        let width = inner.width() / TABS.len() as f32;
        for (index, tab) in TABS.into_iter().enumerate() {
            let (icon, label) = match tab {
                Tab::Library => (Icon::Note, s.phone_library),
                Tab::Search => (Icon::Search, s.search_label),
                Tab::Playlists => (Icon::Playlist, s.playlists),
                Tab::Settings => (Icon::Gear, s.settings),
            };
            let cell = Rect::from_min_size(
                pos2(inner.left() + index as f32 * width, rect.top()),
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
                self.phone.show_tab(tab);
            }
        }
    }

    /// The bar of the playing song: tap it for the whole player.
    /// The song that plays, as a bar: `rect` is the bar, `inner` the part of it that
    /// what is on it keeps to.
    fn phone_mini_player(&mut self, ui: &mut Ui, rect: Rect, inner: Rect) {
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
            pos2(inner.left() + SIDE, rect.center().y - 22.0),
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
            pos2(inner.right() - SIDE - 16.0, rect.center().y),
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
            self.phone.player_view = PlayerView::Cover;
        }
    }

    // ----- the player -------------------------------------------------------------------

    /// The player on the whole screen, `offset` points below where it rests (while it
    /// comes up, goes away or is pulled down).
    fn phone_player(
        &mut self,
        ui: &mut Ui,
        full: Rect,
        safe: Rect,
        offset: f32,
        actions: &mut Vec<Action>,
    ) {
        let s = self.s();
        let Some(current) = self.player.current else {
            return;
        };
        let track = self.track(current.track_id).cloned();
        let down = vec2(0.0, offset);
        let area = full.translate(down);
        let safe = safe.translate(down);
        let mut player_ui = ui.new_child(UiBuilder::new().id_salt("phone-player").max_rect(area));
        player_ui.set_clip_rect(full);
        let ui = &mut player_ui;
        let dark = ui.visuals().dark_mode;
        // On its way: rounded at the top, over what is behind it.
        let corner = if offset > 0.5 { 20 } else { 0 };
        ui.painter().rect_filled(
            area,
            egui::CornerRadius {
                nw: corner,
                ne: corner,
                sw: 0,
                se: 0,
            },
            ui.visuals().panel_fill,
        );

        let header = Rect::from_min_max(safe.min, pos2(safe.right(), safe.top() + HEADER));
        let body = Rect::from_min_max(pos2(safe.left(), header.bottom()), safe.max);
        // Below the cover: the song, the seek bar, the buttons, the volume.
        let controls = 292.0;
        // A screen that is wider than it is tall (a phone on its side) has no room for
        // them below the cover: there the cover stands at the left and they beside it.
        let wide = body.width() > body.height() * 1.25;
        let (art, column) = if wide {
            let side = (body.height() - 2.0 * 14.0)
                .min(body.width() * 0.42)
                .max(120.0);
            let art = Rect::from_center_size(
                pos2(body.left() + 28.0 + side / 2.0, body.center().y),
                egui::Vec2::splat(side),
            );
            let top = (body.center().y - controls / 2.0).max(body.top());
            let column = Rect::from_min_max(
                pos2(art.right() + 28.0, top),
                pos2(body.right() - 28.0, body.bottom()),
            );
            (art, column)
        } else {
            let side = (body.width() - 2.0 * 28.0)
                .min(body.height() - controls - 16.0)
                .max(120.0);
            // What the screen has to spare is shared out above and below the cover.
            let spare = (body.height() - controls - 16.0 - side).max(0.0);
            let art = Rect::from_center_size(
                pos2(body.center().x, body.top() + 8.0 + spare * 0.3 + side / 2.0),
                egui::Vec2::splat(side),
            );
            let column = Rect::from_min_max(
                pos2(body.left() + 28.0, art.bottom() + 16.0 + spare * 0.3),
                pos2(body.right() - 28.0, body.bottom()),
            );
            (art, column)
        };

        // Pulled down by its bar (or by the cover), the player goes away. This lies
        // under the buttons of the bar: a sensor for dragging that lay over them would
        // keep taps from them.
        let mut grip = Rect::from_min_max(full.min.max(area.min), header.max);
        if matches!(
            self.phone.player_view,
            PlayerView::Cover | PlayerView::Meters
        ) {
            grip = grip.union(art);
        }
        let pull = ui.interact(grip, ui.scope_id().with("phone-player-pull"), Sense::drag());
        if pull.dragged() {
            self.phone.player_dragging = true;
            self.phone.player_drag = (self.phone.player_drag + pull.drag_delta().y).max(0.0);
        }
        if pull.drag_stopped() {
            self.phone.player_dragging = false;
            let flung = ui.input(|i| i.pointer.velocity().y) > 700.0;
            if self.phone.player_drag > 110.0 || flung {
                self.phone.player_open = false;
            }
        }

        // The bar at the top: put the player away, and what to show.
        let strong = ui.visuals().strong_text_color();
        if bar_button(
            ui,
            "phone-player-close",
            pos2(header.left() + 6.0 + TOUCH / 2.0, header.center().y),
            Icon::Down,
            s.close,
            strong,
        ) {
            self.phone.player_open = false;
        }
        let view = self.phone.player_view;
        let choices = [
            (
                PlayerView::Queue,
                Icon::Queue,
                s.show_queue,
                "phone-player-queue",
            ),
            (
                PlayerView::Meters,
                Icon::Meter,
                s.show_meter,
                "phone-player-meters",
            ),
            (
                PlayerView::Lyrics,
                Icon::Lyrics,
                s.info_lyrics,
                "phone-player-lyrics",
            ),
        ];
        for (index, (choice, icon, label, id_salt)) in choices.into_iter().enumerate() {
            let centre = pos2(
                header.right() - 6.0 - TOUCH / 2.0 - index as f32 * (TOUCH - 2.0),
                header.center().y,
            );
            let color = if view == choice {
                theme::accent(dark)
            } else {
                strong
            };
            if bar_button(ui, id_salt, centre, icon, label, color) {
                // A second tap brings the cover back.
                self.phone.player_view = if view == choice {
                    PlayerView::Cover
                } else {
                    choice
                };
                if self.phone.player_view == PlayerView::Queue {
                    self.queue_scroll_forced = true;
                }
                self.phone.lyrics_line = None;
            }
        }
        let view = self.phone.player_view;
        let heading = match view {
            PlayerView::Cover => s.phone_now_playing,
            PlayerView::Queue => s.show_queue,
            PlayerView::Meters => s.show_meter,
            PlayerView::Lyrics => s.info_lyrics,
        };
        // To the left of the three buttons, as far as there is room.
        let heading_right = header.right() - 6.0 - 3.0 * (TOUCH - 2.0) - 6.0;
        let heading_left = header.left() + 6.0 + TOUCH + 4.0;
        let mut heading_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(Rect::from_min_max(
                    pos2(heading_left, header.top()),
                    pos2(heading_right.max(heading_left + 20.0), header.bottom()),
                ))
                .layout(Layout::left_to_right(Align::Center)),
        );
        heading_ui.add(
            egui::Label::new(
                RichText::new(heading)
                    .font(theme::semibold(17.0))
                    .color(strong),
            )
            .truncate(),
        );

        if view == PlayerView::Queue {
            let mut queue_ui = ui.new_child(
                UiBuilder::new()
                    .id_salt("phone-queue")
                    .max_rect(body)
                    .layout(Layout::top_down(Align::Min)),
            );
            queue_ui.set_clip_rect(body.intersect(full));
            self.phone_queue(&mut queue_ui);
            return;
        }
        match view {
            PlayerView::Meters => self.phone_meters_guarded(ui, art),
            PlayerView::Lyrics => self.phone_lyrics(ui, art, track.as_ref(), full),
            PlayerView::Cover | PlayerView::Queue => {
                let texture = self
                    .art
                    .get(track.as_ref().and_then(|t| t.art_id.as_deref()), true);
                widgets::art(ui, art, texture, 14.0);
            }
        }

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

    /// The meters, kept from taking the whole app down with them. On one phone the app
    /// ended whenever this view was shown, for a reason that could not be seen from
    /// outside (it does not happen on an emulator). A panic in drawing them is caught
    /// here: the panic hook has written it to the log, and the view says what it was
    /// in the meters' place, so that it can be read off the screen and passed on.
    fn phone_meters_guarded(&mut self, ui: &mut Ui, rect: Rect) {
        if self.phone.meters_failed.is_none() {
            let drawn = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if std::mem::take(&mut self.phone.fail_meters) {
                    panic!("asked for by a verification script");
                }
                self.phone_meters(ui, rect);
            }));
            if let Err(payload) = drawn {
                // What the panic hook noted (with the place); else the message alone.
                let message = crate::app::last_panic().unwrap_or_else(|| {
                    payload
                        .downcast_ref::<&str>()
                        .map(|text| (*text).to_owned())
                        .or_else(|| payload.downcast_ref::<String>().cloned())
                        .unwrap_or_default()
                });
                self.phone.meters_failed = Some(message);
            }
        }
        let Some(message) = self.phone.meters_failed.clone() else {
            return;
        };
        let s = self.s();
        ui.painter()
            .rect_filled(rect, 14.0, meter::deck_fill(ui.visuals().dark_mode));
        let mut told = ui.new_child(
            UiBuilder::new()
                .id_salt("phone-meters-failed")
                .max_rect(rect.shrink(16.0))
                .layout(Layout::top_down(Align::Min)),
        );
        told.label(RichText::new(s.meter_failed).color(theme::dim(ui)));
        told.add_space(8.0);
        told.add(egui::Label::new(RichText::new(&message).monospace().small()).wrap());
        told.add_space(8.0);
        if told
            .add(egui::Button::new(s.copy_short).min_size(vec2(0.0, 30.0)))
            .clicked()
        {
            self.copy_text(ui.ctx(), message);
        }
    }

    /// The VU meters, the spectrum and where the sound comes from and goes to, in the
    /// place of the cover.
    fn phone_meters(&mut self, ui: &mut Ui, rect: Rect) {
        let s = self.s();
        let dark = ui.visuals().dark_mode;
        ui.painter().rect_filled(rect, 14.0, meter::deck_fill(dark));
        let inner = rect.shrink(12.0);
        // The needles follow the music only while it is sounding; otherwise they fall
        // back to rest.
        let playing = self.player.state == PlayState::Playing;
        let now = Instant::now();
        let levels = playing.then(|| self.core.player.levels());
        let mut busy = self.meter.update(levels, now);

        let gap = 10.0;
        let dial_width = (inner.width() - gap) / 2.0;
        let dial_height = dial_width / meter::DIAL_ASPECT;
        let names = [s.meter_left_name, s.meter_right_name];
        let letters = [s.meter_left, s.meter_right];
        for channel in 0..2 {
            let dial = Rect::from_min_size(
                pos2(
                    inner.left() + channel as f32 * (dial_width + gap),
                    inner.top(),
                ),
                vec2(dial_width, dial_height),
            );
            let response = ui.interact(
                dial,
                ui.scope_id().with(("phone-vu", channel)),
                Sense::hover(),
            );
            response
                .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Meter, true, names[channel]));
            meter::dial(
                ui.painter(),
                dial,
                self.meter.needles[channel].position,
                self.meter.peak_lit(channel),
                letters[channel],
                channel == 1,
            );
        }

        // At the bottom: the file, and what it is played through.
        let lines: Vec<String> = match self.signal_path() {
            Some(path) => {
                let mut source = format!("{} · {}", path.codec, path.source_format);
                if path.in_memory {
                    source.push_str(" · ");
                    source.push_str(s.info_memory);
                }
                let mut output = format!("{} · {}", path.device, path.format);
                for stage in &path.stages {
                    output.push_str(" · ");
                    output.push_str(&stage.name);
                }
                vec![source, output]
            }
            None => Vec::new(),
        };
        let readout = lines.len() as f32 * 18.0;
        let mut y = inner.bottom() - readout;
        for (index, line) in lines.iter().enumerate() {
            let line_rect = Rect::from_min_max(
                pos2(inner.left() + 2.0, y),
                pos2(inner.right() - 2.0, y + 18.0),
            );
            let mut line_ui = ui.new_child(
                UiBuilder::new()
                    .id_salt(("phone-meter-line", index))
                    .max_rect(line_rect)
                    .layout(Layout::left_to_right(Align::Center)),
            );
            line_ui.add(
                egui::Label::new(RichText::new(line).font(font(12.0)).color(theme::dim(ui)))
                    .truncate(),
            );
            y += 18.0;
        }

        // Between them: the spectrum, as tall as there is room for.
        let spectrum_rect = Rect::from_min_max(
            pos2(inner.left(), inner.top() + dial_height + 10.0),
            pos2(inner.right(), inner.bottom() - readout - 8.0),
        );
        if spectrum_rect.height() >= 48.0 {
            let levels = playing.then(|| {
                let mut samples = [0f32; spectrum::MAX_SAMPLES];
                self.core.player.scope_samples(&mut samples);
                let rate = self.player.output_sample_rate.unwrap_or(0);
                self.spectrum.analyse(&samples, rate)
            });
            busy |= self.spectrum.update(levels, now);
            let response = ui.interact(
                spectrum_rect,
                ui.scope_id().with("phone-spectrum"),
                Sense::hover(),
            );
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Meter, true, s.spectrum_name)
            });
            spectrum::draw(
                ui.painter(),
                spectrum_rect,
                &self.spectrum.bars,
                &self.spectrum.caps,
            );
        } else {
            self.spectrum.suspend();
        }
        if busy {
            // Only while something moves.
            ui.ctx().request_repaint_after(Duration::from_millis(16));
        }
    }

    /// The words of the song in the place of the cover; with time stamps, the line
    /// that is being sung is brought to the middle and a tap on a line goes there.
    fn phone_lyrics(&mut self, ui: &mut Ui, rect: Rect, track: Option<&Arc<Track>>, clip: Rect) {
        let s = self.s();
        ui.painter()
            .rect_filled(rect, 14.0, ui.visuals().faint_bg_color);
        let Some(track) = track else {
            return;
        };
        let details = self.extras_of(track);
        let inside = rect.shrink2(vec2(16.0, 12.0));
        let mut words_ui = ui.new_child(
            UiBuilder::new()
                .id_salt("phone-lyrics")
                .max_rect(inside)
                .layout(Layout::top_down(Align::Min)),
        );
        words_ui.set_clip_rect(inside.intersect(clip));
        let ui = &mut words_ui;
        // Still being read: nothing to say yet.
        let Some(details) = details else {
            return;
        };
        let Some(lyrics) = details.lyrics.as_ref() else {
            widgets::empty_state(ui, Icon::Lyrics, s.phone_no_lyrics, s.phone_no_lyrics_hint);
            return;
        };
        let position = self.player.position_now_ms();
        let current = lyrics.line_at(position);
        let line_changed = self.phone.lyrics_line != current;
        self.phone.lyrics_line = current;
        let dim = theme::dim(ui);
        let strong = ui.visuals().strong_text_color();
        let accent = theme::accent(ui.visuals().dark_mode);
        let mut seek = None;
        egui::ScrollArea::vertical()
            .id_salt("phone-lyrics-words")
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 9.0;
                ui.set_width(ui.available_width());
                let Some(lines) = lyrics.timed.as_ref() else {
                    ui.add(
                        egui::Label::new(
                            RichText::new(&lyrics.plain).font(font(17.0)).color(strong),
                        )
                        .wrap(),
                    );
                    return;
                };
                for (index, line) in lines.iter().enumerate() {
                    // A stamp without words is a pause in the singing.
                    let words = if line.text.is_empty() {
                        "♪"
                    } else {
                        &line.text
                    };
                    let is_current = current == Some(index);
                    let sung = current.is_some_and(|now| index < now);
                    let text = if is_current {
                        RichText::new(words)
                            .font(theme::semibold(20.0))
                            .color(accent)
                    } else if sung {
                        RichText::new(words).font(font(17.0)).color(dim)
                    } else {
                        RichText::new(words)
                            .font(font(17.0))
                            .color(strong.gamma_multiply(0.85))
                    };
                    let response = ui.add(egui::Label::new(text).wrap().sense(Sense::click()));
                    if is_current && line_changed {
                        response.scroll_to_me(Some(Align::Center));
                    }
                    let mut tapped = response.clicked();
                    // A translation or pronunciation of the same moment, under it.
                    for also in &line.also {
                        let color = if is_current {
                            accent.gamma_multiply(0.75)
                        } else {
                            dim
                        };
                        tapped |= ui
                            .add(
                                egui::Label::new(RichText::new(also).font(font(14.0)).color(color))
                                    .wrap()
                                    .sense(Sense::click()),
                            )
                            .clicked();
                    }
                    if tapped {
                        seek = Some(u64::from(line.at_ms));
                    }
                }
            });
        if let Some(position) = seek {
            self.core.player.send(Command::SeekTo(position));
        }
        // Wake up for the next line rather than for the next quarter of a second.
        if self.player.state == PlayState::Playing
            && let Some(lines) = lyrics.timed.as_ref()
            && let Some(line) = lines.get(current.map_or(0, |now| now + 1))
        {
            let wait = u64::from(line.at_ms)
                .saturating_sub(position)
                .clamp(20, 1000);
            ui.ctx().request_repaint_after(Duration::from_millis(wait));
        }
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
            // The section that is marked for repeating: its ends, on the rail.
            if let Some(section) = self.player.section
                && duration_s > 0.0
            {
                super::player_bar::section_marks(ui, response.rect, section, duration_s);
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
        // At the right: the whole length or what is left of it; a tap changes which.
        let total_ms = current.map_or(0, |c| c.duration_ms);
        let remaining = self.settings.show_remaining_time;
        let text = if remaining {
            let left = total_ms.saturating_sub((shown * 1000.0) as u64);
            format!("-{}", format_time(left))
        } else {
            format_time(total_ms)
        };
        ui.painter().text(
            line.right_center(),
            Align2::RIGHT_CENTER,
            text,
            font(12.5),
            dim,
        );
        let hit = Rect::from_min_max(
            pos2(line.right() - 64.0, line.top() - 8.0),
            pos2(line.right(), line.bottom() + 8.0),
        );
        let response = ui.interact(hit, ui.scope_id().with("phone-time"), Sense::click());
        let label = if remaining {
            s.phone_show_total
        } else {
            s.phone_show_left
        };
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
        if response.clicked() {
            self.settings.show_remaining_time = !remaining;
        }
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
        // The two at the ends are switches. What each stands at is said in words below
        // it, and one that is on stands on a tinted ground; one that is off is grey.
        // (By colour alone, dark for off and the accent for on, the two were hard to
        // tell apart, and a phone has no pointer to hover for the words.)
        let buttons: [(Icon, &str, f32, Option<bool>, Command); 4] = [
            (
                Icon::Shuffle,
                if self.player.shuffle {
                    s.shuffle_on
                } else {
                    s.shuffle_off
                },
                24.0,
                Some(self.player.shuffle),
                Command::SetShuffle(!self.player.shuffle),
            ),
            (
                Icon::Previous,
                s.previous_track,
                34.0,
                None,
                Command::Previous,
            ),
            (Icon::Next, s.next_track, 34.0, None, Command::Next),
            (
                repeat_icon,
                repeat_label,
                24.0,
                Some(self.player.repeat != RepeatMode::Off),
                Command::SetRepeat(next_repeat(self.player.repeat)),
            ),
        ];
        for (index, (icon, label, size, switch, command)) in buttons.into_iter().enumerate() {
            let slot = if index >= 2 { index + 1 } else { index };
            let hit = Rect::from_center_size(centre(slot), egui::Vec2::splat(TOUCH + 8.0));
            let response = ui.interact(
                hit,
                ui.scope_id().with(("phone-transport", slot)),
                Sense::click(),
            );
            response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
            let pressed = response.is_pointer_button_down_on();
            let (at, color) = match switch {
                Some(on) => {
                    let color = match (on, pressed) {
                        (true, _) => theme::accent(dark),
                        (false, true) => ui.visuals().text_color(),
                        (false, false) => theme::dim(ui),
                    };
                    let at = hit.center() - vec2(0.0, 9.0);
                    if on {
                        ui.painter().rect_filled(
                            Rect::from_center_size(at, vec2(48.0, 32.0)),
                            10.0,
                            theme::accent(dark).gamma_multiply(0.16),
                        );
                    }
                    ui.painter().text(
                        pos2(hit.center().x, hit.center().y + 19.0),
                        Align2::CENTER_CENTER,
                        label,
                        font(11.0),
                        color,
                    );
                    (at, color)
                }
                None if pressed => (hit.center(), theme::dim(ui)),
                None => (hit.center(), ui.visuals().strong_text_color()),
            };
            icons::paint(
                ui.painter(),
                Rect::from_center_size(at, egui::Vec2::splat(size)),
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
        let mut mute = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let (icon, _) = ui.allocate_exact_size(egui::Vec2::splat(20.0), Sense::hover());
            // The speaker silences the app, and brings the sound back as it was.
            let speaker = ui.interact(
                icon.expand(12.0),
                ui.scope_id().with("phone-mute"),
                Sense::click(),
            );
            let label = if volume <= 0.001 { s.unmute } else { s.mute };
            speaker.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
            mute = speaker.clicked();
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
        if mute {
            self.toggle_mute();
        }
        // What is set for this song or this evening, said in words; a tap on one
        // switches it off, as on a desk.
        let accent = theme::accent(ui.visuals().dark_mode);
        let note = |ui: &mut Ui, text: String| {
            ui.add(
                egui::Label::new(RichText::new(text).font(font(13.5)).color(accent))
                    .sense(Sense::click()),
            )
            .clicked()
        };
        let section = self.section_words();
        let paced = self.player.speed_percent != 100;
        if self.player.stop_after_current
            || self.player.sleep_at.is_some()
            || section.is_some()
            || paced
        {
            ui.add_space(6.0);
        }
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            if let Some(words) = section
                && note(ui, words)
            {
                self.core.player.send(Command::ClearSection);
            }
            if self.player.stop_after_current && note(ui, s.stop_after_short.to_string()) {
                self.core.player.send(Command::SetStopAfterCurrent(false));
            }
            // Another speed than the songs' own: a tap sets it back.
            let speed = crate::i18n::Lang::speed_label(self.player.speed_percent);
            if paced && note(ui, format!("{} {speed}", s.speed)) {
                self.settings.speed_percent = 100;
                self.core.player.send(Command::SetSpeed(100));
            }
            if let Some(at) = self.player.sleep_at {
                let left = at.saturating_duration_since(Instant::now()).as_secs();
                let text = format!("{} {}:{:02}", s.menu_sleep_timer, left / 60, left % 60);
                if note(ui, text) {
                    self.core.player.send(Command::SetSleepTimer(None));
                }
                ui.ctx().request_repaint_after(Duration::from_millis(500));
            }
        });
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
        let mut keep = false;
        // While entries are being chosen a tap chooses, and the bar offers what to do
        // with the chosen ones.
        self.queue_selection
            .retain(|id| queue.iter().any(|item| item.id == *id));
        let selecting = self.phone.queue_selecting;
        let accent = theme::accent(ui.visuals().dark_mode);
        let mut toggle_selecting = false;
        let mut act = false;
        ui.horizontal(|ui| {
            ui.add_space(SIDE);
            let count = if selecting {
                self.lang.selected_count(self.queue_selection.len())
            } else {
                self.lang.song_count(queue.len())
            };
            ui.label(RichText::new(count).font(font(13.5)).color(theme::dim(ui)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(SIDE - 6.0);
                ui.spacing_mut().item_spacing.x = 6.0;
                if selecting {
                    toggle_selecting = edit::word_button(ui, s.phone_done, accent);
                    act = edit::word_button(ui, s.phone_selected_songs, accent);
                    return;
                }
                toggle_selecting = edit::word_button(ui, s.phone_select, accent);
                ui.add_space(4.0);
                if ui
                    .add(egui::Button::new(
                        RichText::new(s.clear_queue).font(font(14.0)),
                    ))
                    .clicked()
                {
                    clear = true;
                }
                // The queue as it is, kept as a playlist.
                let save = ui.add(egui::Button::new(
                    RichText::new(s.queue_save).font(font(14.0)),
                ));
                save.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::Role::Button, true, s.queue_save_hint)
                });
                if save.clicked() {
                    keep = true;
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
            let all_rows = virtual_rows(ui, "queue", queue.len(), ROW, |ui, index| {
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
                let chosen = self.queue_selection.contains(&item.id);
                if selecting {
                    if response.clicked() && !self.queue_selection.remove(&item.id) {
                        self.queue_selection.insert(item.id);
                    }
                    if is_current {
                        ui.painter().rect_filled(
                            rect,
                            0.0,
                            theme::accent(dark).gamma_multiply(if dark { 0.16 } else { 0.10 }),
                        );
                    }
                } else if self.is_carried("queue", index) {
                    ui.painter().rect_filled(
                        rect,
                        0.0,
                        theme::accent(dark).gamma_multiply(if dark { 0.28 } else { 0.18 }),
                    );
                } else if is_current {
                    ui.painter().rect_filled(
                        rect,
                        0.0,
                        theme::accent(dark).gamma_multiply(if dark { 0.16 } else { 0.10 }),
                    );
                }
                let mut left = rect.left() + SIDE;
                if selecting {
                    choice_circle(ui, pos2(left + 12.0, rect.center().y), chosen);
                    left += 36.0;
                }
                let art = Rect::from_min_size(
                    pos2(left, rect.center().y - 23.0),
                    egui::Vec2::splat(46.0),
                );
                let texture = self.art.get(track.art_id.as_deref(), false);
                widgets::art(ui, art, texture, 6.0);
                // The handle by which the row is carried to another place in the queue.
                let grip = Rect::from_min_max(
                    pos2(rect.right() - SIDE - TOUCH - 34.0, rect.top()),
                    pos2(rect.right() - SIDE - TOUCH, rect.bottom()),
                );
                let text_right = if selecting {
                    rect.right() - SIDE
                } else {
                    self.reorder_grip(ui, "queue", index, grip);
                    grip.left()
                };
                let text = Rect::from_min_max(
                    pos2(art.right() + 12.0, rect.top()),
                    pos2(text_right, rect.bottom()),
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
                if selecting {
                    return;
                }
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
            if let Some((from, to_index)) =
                self.follow_reorder(ui, "queue", all_rows, ROW, queue.len())
                && let Some(item) = queue.get(from)
            {
                self.core.player.send(Command::MoveItems {
                    ids: vec![item.id],
                    to_index,
                });
            }
            ui.add_space(12.0);
        });
        if clear {
            self.core.player.send(Command::ClearQueue);
        }
        if keep {
            let tracks = queue.iter().map(|item| item.track).collect();
            self.dialog = Some(Dialog::new_playlist(s.untitled_playlist, tracks));
        }
        if toggle_selecting {
            self.phone.queue_selecting = !selecting;
            self.queue_selection.clear();
        }
        if act {
            if self.queue_selection.is_empty() {
                self.info_toast(s.phone_nothing_selected);
            } else {
                self.phone.sheet = Some(Sheet::QueueSelection);
            }
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

    /// The sheet that is up, if one is. It comes up from below, and goes back down at
    /// a tap outside it, when it is pulled down by its handle, or once a line of it was
    /// chosen.
    fn phone_sheet(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(sheet) = self.phone.sheet.clone() else {
            self.phone.sheet_since = None;
            self.phone.sheet_leaving = None;
            self.phone.sheet_drag = 0.0;
            return;
        };
        let full = ctx.viewport_rect();
        let safe = ctx.content_rect();
        let dark = ctx.global_style().visuals.dark_mode;
        if self
            .phone
            .sheet_leaving
            .as_ref()
            .is_some_and(|(_, leaving)| *leaving != sheet)
        {
            // Another sheet was asked for while one was going: that one comes up.
            self.phone.sheet_leaving = None;
            self.phone.sheet_since = Some(Instant::now());
            self.phone.sheet_drag = 0.0;
        }
        let since = *self.phone.sheet_since.get_or_insert_with(Instant::now);
        let mut up = ease_out(since.elapsed().as_secs_f32() / RISE);
        if up < 1.0 {
            ctx.request_repaint();
        }
        // On its way down: nothing on it answers any more, and then it is gone.
        let leaving = match &self.phone.sheet_leaving {
            Some((started, _)) => {
                let gone = started.elapsed().as_secs_f32() / SHEET_FALL;
                if gone >= 1.0 {
                    self.phone.sheet = None;
                    self.phone.sheet_since = None;
                    self.phone.sheet_leaving = None;
                    self.phone.sheet_drag = 0.0;
                    return;
                }
                up *= 1.0 - gone * gone;
                ctx.request_repaint();
                true
            }
            None => false,
        };
        // What is behind is dimmed; a tap there puts the sheet away. (Neither area is
        // kept inside the safe area: the dimming covers the whole screen and the sheet
        // starts below its lower edge.)
        let dim = (if dark { 150.0 } else { 90.0 }) * up;
        let backdrop = egui::Area::new(egui::Id::unique("phone-sheet-backdrop"))
            .order(egui::Order::Middle)
            .constrain(false)
            .fade_in(false)
            .fixed_pos(full.min)
            .show(ctx, |ui| {
                ui.set_clip_rect(full);
                let response = ui.allocate_rect(full, Sense::click());
                ui.painter()
                    .rect_filled(full, 0.0, Color32::from_black_alpha(dim as u8));
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
        // Below the screen's edge by what has not come up yet and what it was pulled.
        let height = if self.phone.sheet_height > 0.0 {
            self.phone.sheet_height
        } else {
            420.0
        };
        let lowered = (1.0 - up) * height + self.phone.sheet_drag;
        // As wide as the screen when that is narrow; on a wide one (a phone on its
        // side) a sheet of that width would be mostly empty.
        let width = safe.width().min(SHEET_WIDTH);
        let shown = egui::Area::new(egui::Id::unique("phone-sheet"))
            .order(egui::Order::Foreground)
            .constrain(false)
            .fade_in(false)
            .pivot(Align2::CENTER_BOTTOM)
            .fixed_pos(pos2(safe.center().x, full.bottom() + lowered))
            .show(ctx, |ui| {
                ui.set_clip_rect(full);
                if leaving {
                    ui.visuals_mut().disabled_alpha = 1.0;
                    ui.disable();
                }
                egui::Frame::new()
                    .fill(fill)
                    .corner_radius(egui::CornerRadius {
                        nw: 18,
                        ne: 18,
                        sw: 0,
                        se: 0,
                    })
                    .show(ui, |ui| {
                        ui.set_width(width);
                        ui.spacing_mut().item_spacing.y = 0.0;
                        // The handle at the top: pull it down and the sheet goes.
                        let (handle, pull) =
                            ui.allocate_exact_size(vec2(width, 26.0), Sense::drag());
                        ui.painter().rect_filled(
                            Rect::from_center_size(handle.center(), vec2(38.0, 5.0)),
                            3.0,
                            widgets::divider_color(ui),
                        );
                        if pull.dragged() {
                            self.phone.sheet_dragging = true;
                            self.phone.sheet_drag =
                                (self.phone.sheet_drag + pull.drag_delta().y).max(0.0);
                        }
                        if pull.drag_stopped() {
                            self.phone.sheet_dragging = false;
                            let flung = ui.input(|i| i.pointer.velocity().y) > 600.0;
                            if self.phone.sheet_drag > 70.0 || flung {
                                close = true;
                            }
                        }
                        let limit = (safe.height() * 0.72).max(200.0);
                        egui::ScrollArea::vertical()
                            .id_salt("phone-sheet-items")
                            .max_height(limit)
                            .show(ui, |ui| {
                                ui.set_width(width);
                                if self.phone_sheet_items(ui, &sheet, actions) {
                                    close = true;
                                }
                            });
                        ui.add_space(8.0 + below_safe);
                    });
            });
        self.phone.sheet_height = shown.response.rect.height();
        // Let go of without going away: it comes back up.
        if !self.phone.sheet_dragging && !leaving && !close && self.phone.sheet_drag > 0.0 {
            self.phone.sheet_drag = settle(ctx, self.phone.sheet_drag);
        }
        // Another sheet may have been put up from this one: only this one goes.
        if close && !leaving && self.phone.sheet.as_ref() == Some(&sheet) {
            self.phone.sheet_leaving = Some((Instant::now(), sheet));
            self.phone.sheet_dragging = false;
            ctx.request_repaint();
        }
    }

    /// A line of a sheet: an icon and what it does. True when it was tapped.
    fn sheet_item(ui: &mut Ui, icon: Icon, label: &str, warning: bool) -> bool {
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::click());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
        if held(ui, &response) {
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
                // The stars it has, and the way to give it others.
                if Self::sheet_item(ui, Icon::Tag, self.rating_words(track.rating), false) {
                    self.phone.sheet = Some(Sheet::Rating(ids));
                    return false;
                }
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
                // The folder the file is in, in the phone's file manager.
                if crate::platform::PHONE_FILE_MANAGER
                    && Self::sheet_item(ui, Icon::Folder, s.show_in_finder, false)
                {
                    actions.push(Action::Reveal(track.id));
                    return true;
                }
                if self.player.current.map(|c| c.track_id) == Some(track.id) {
                    // Where it is among all songs (on a desk: a click on its title).
                    if Self::sheet_item(ui, Icon::Note, s.phone_show_in_songs, false) {
                        if let Some(state) = self.views.get_mut(&Nav::Songs) {
                            state.query.clear();
                        }
                        self.phone.reveal = Some((Nav::Songs, track.id));
                        actions.push(Action::Go(Nav::Songs));
                        return true;
                    }
                    let on = self.player.stop_after_current;
                    if Self::sheet_item(ui, Icon::Pause, s.menu_stop_after_current, on) {
                        self.core.player.send(Command::SetStopAfterCurrent(!on));
                        return true;
                    }
                    // A section of the song to play over and over: where it begins,
                    // where it ends, and off again, one step at each choice.
                    let repeating = self
                        .player
                        .section
                        .is_some_and(|section| section.to_ms.is_some());
                    if Self::sheet_item(ui, Icon::Repeat, self.section_step(), repeating) {
                        self.core.player.send(Command::MarkSection);
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
                self.sheet_pin_item(ui, Pin::Album(*key), actions)
            }
            Sheet::Artist(name) => {
                let Some(artist) = self.snapshot.artists().iter().find(|a| &a.name == name) else {
                    return true;
                };
                let summary = self
                    .lang
                    .artist_summary(artist.albums.len(), artist.track_count);
                let shown = if name.is_empty() {
                    s.unknown_artist
                } else {
                    name.as_str()
                };
                Self::sheet_heading(ui, shown, &summary);
                if let Some(close) =
                    self.sheet_play_items(ui, actions, |app| app.phone_artist_tracks(name))
                {
                    return close;
                }
                self.sheet_pin_item(ui, Pin::Artist(name.clone()), actions)
            }
            Sheet::Genre(name) => {
                Self::sheet_heading(ui, name, s.genres);
                if let Some(close) = self.sheet_play_items(ui, actions, |app| {
                    let rows = app.rows_for(&Nav::Genre(name.clone()), "");
                    rows.iter()
                        .filter_map(|row| app.snapshot.tracks.get(row.idx as usize))
                        .map(|track| track.id)
                        .collect()
                }) {
                    return close;
                }
                self.sheet_pin_item(ui, Pin::Genre(name.clone()), actions)
            }
            Sheet::Sort(nav) => self.phone_sort_items(ui, nav),
            Sheet::Selection(nav) => self.phone_selection_items(ui, nav, actions),
            Sheet::QueueSelection => self.phone_queue_selection_items(ui),
            Sheet::Playlist(id) => {
                let Some(playlist) = self.playlists.iter().find(|p| p.id == *id).cloned() else {
                    return true;
                };
                Self::sheet_heading(
                    ui,
                    &playlist.name,
                    &self.lang.song_count(playlist.item_count),
                );
                let id = playlist.id;
                if let Some(close) = self.sheet_play_items(ui, actions, |app| {
                    app.core
                        .library
                        .playlist_items(id)
                        .into_iter()
                        .map(|item| item.track_id)
                        .collect()
                }) {
                    return close;
                }
                if self.sheet_pin_item(ui, Pin::Playlist(id), actions) {
                    return true;
                }
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
                if let Some(close) = self.sheet_play_items(ui, actions, |app| {
                    app.snapshot
                        .search(&query)
                        .into_iter()
                        .filter_map(|index| app.snapshot.tracks.get(index as usize))
                        .map(|track| track.id)
                        .collect()
                }) {
                    return close;
                }
                if self.sheet_pin_item(ui, Pin::Smart(name.clone()), actions) {
                    return true;
                }
                if Self::sheet_item(ui, Icon::Search, s.smart_edit, false) {
                    actions.push(Action::EditSmart(name.clone()));
                    return true;
                }
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
            Sheet::Rating(tracks) => {
                Self::sheet_heading(ui, s.col_rating, "");
                // (Of several songs, the first says what they have.)
                let current = tracks
                    .first()
                    .and_then(|id| self.snapshot.get(*id))
                    .map_or(0, |track| track.rating);
                let mut chosen = None;
                ui.vertical_centered(|ui| {
                    ui.add_space(6.0);
                    chosen = widgets::stars(ui, current, 38.0, Some(s.col_rating));
                    ui.add_space(16.0);
                });
                if current > 0 && Self::sheet_item(ui, Icon::Close, s.rating_none, true) {
                    chosen = Some(0);
                }
                match chosen {
                    Some(stars) => {
                        actions.push(Action::SetRating(tracks.clone(), stars));
                        true
                    }
                    None => false,
                }
            }
            Sheet::Info(track) => {
                let Some(track) = self.snapshot.get(*track).cloned() else {
                    return true;
                };
                Self::sheet_heading(ui, &track.title, artist_line(&track, s.unknown_artist));
                self.phone_info(ui, &track, actions)
            }
        }
    }

    /// The lines of a sheet that play or queue a group of songs: play, play next, add
    /// to the queue, add to a playlist. Which songs is worked out only when one of the
    /// lines is tapped. `None` while none is; else whether the sheet is done with.
    fn sheet_play_items(
        &mut self,
        ui: &mut Ui,
        actions: &mut Vec<Action>,
        tracks: impl FnOnce(&mut Self) -> Vec<TrackId>,
    ) -> Option<bool> {
        let s = self.s();
        let play = Self::sheet_item(ui, Icon::Play, s.play_now, false);
        let next = Self::sheet_item(ui, Icon::Play, s.play_next, false);
        let enqueue = Self::sheet_item(ui, Icon::Queue, s.add_to_queue, false);
        let add = Self::sheet_item(ui, Icon::Playlist, s.add_to_playlist, false);
        if !(play || next || enqueue || add) {
            return None;
        }
        let tracks = tracks(self);
        if tracks.is_empty() {
            return Some(true);
        }
        if play {
            actions.push(Action::PlayTracks(tracks, 0));
        } else if next {
            actions.push(Action::PlayNext(tracks));
        } else if enqueue {
            self.info_toast(self.lang.added_to_queue(tracks.len()));
            actions.push(Action::Enqueue(tracks));
        } else {
            self.phone.sheet = Some(Sheet::AddToPlaylist(tracks));
            return Some(false);
        }
        Some(true)
    }

    /// "Pin to Library" in a sheet, or "Unpin" for what is pinned. True when it was
    /// tapped.
    fn sheet_pin_item(&mut self, ui: &mut Ui, pin: Pin, actions: &mut Vec<Action>) -> bool {
        let s = self.s();
        let live = self.live_pins();
        let pinned = live.contains(&super::shown_pin(&self.snapshot, &pin));
        let label = if pinned { s.unpin } else { s.phone_pin };
        if !Self::sheet_item(ui, Icon::Pin, label, false) {
            return false;
        }
        if !pinned && live.len() >= MAX_PINS {
            self.warn_toast(s.phone_pins_full);
        } else {
            actions.push(Action::TogglePin(pin));
        }
        true
    }

    /// The songs of an artist, album by album.
    fn phone_artist_tracks(&self, name: &str) -> Vec<TrackId> {
        let snapshot = &self.snapshot;
        snapshot
            .artists()
            .iter()
            .find(|artist| artist.name == name)
            .map(|artist| {
                artist
                    .albums
                    .iter()
                    .filter_map(|&index| snapshot.albums().get(index as usize))
                    .flat_map(|album| album.tracks.iter())
                    .filter_map(|&index| snapshot.tracks.get(index as usize))
                    .map(|track| track.id)
                    .collect()
            })
            .unwrap_or_default()
    }
}
