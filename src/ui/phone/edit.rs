//! Changing what a list shows and holds, on a phone: the order of its songs, several
//! songs at once, and a row carried to another place with a finger.
//!
//! What a desk does with column headers, ⌘-clicks and dragging rows.
use super::super::icons::{self, Icon};
use super::super::{Action, Nav, OneumApp, Row, theme};
use super::{Reorder, SIDE, Sheet, font, held, pill_button};
use crate::i18n::Strings;
use crate::library::SortKey;
use crate::model::TrackId;
use eframe::egui::{self, Align, Align2, Layout, Rect, RichText, Sense, Ui, pos2, vec2};

/// The orders a list of songs can be put in, with what each is called.
fn sort_choices(s: &Strings) -> [(SortKey, &'static str); 9] {
    [
        (SortKey::Title, s.col_title),
        (SortKey::Artist, s.col_artist),
        (SortKey::Album, s.col_album),
        (SortKey::Year, s.col_year),
        (SortKey::Duration, s.col_duration),
        (SortKey::PlayCount, s.info_plays),
        (SortKey::DateAdded, s.phone_sort_added),
        (SortKey::LastPlayed, s.col_last_played),
        (SortKey::Format, s.col_format),
    ]
}

/// The direction an order starts in when it is chosen: what one looks for first is at
/// the top (the songs played most, the ones added last), names from their beginning.
fn starts_ascending(key: SortKey) -> bool {
    !matches!(
        key,
        SortKey::PlayCount | SortKey::DateAdded | SortKey::LastPlayed
    )
}

/// Whether the listener chooses the order of this list's songs. Not where the order is
/// the list's own (an album, a playlist, the "listen again" lists), as on a desk.
pub(super) fn sortable(nav: &Nav) -> bool {
    matches!(
        nav,
        Nav::Songs | Nav::Favorites | Nav::Genre(_) | Nav::Smart(_) | Nav::Folder(_)
    )
}

/// Whether several songs of this list can be chosen at once.
pub(super) fn selectable(nav: &Nav) -> bool {
    matches!(
        nav,
        Nav::Songs
            | Nav::Favorites
            | Nav::Genre(_)
            | Nav::Smart(_)
            | Nav::Folder(_)
            | Nav::Album(_)
            | Nav::Playlist(_)
            | Nav::Recent
    )
}

/// Where a row that was at `from` and is let go of in the gap `to` (0 is before the
/// first row) ends up in the list with it taken out. `None` if that is where it is.
pub(super) fn drop_place(from: usize, to: usize) -> Option<(usize, usize)> {
    if to == from || to == from + 1 {
        return None;
    }
    Some((from, if to > from { to - 1 } else { to }))
}

/// What marks a line of a sheet as the choice that is on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    Chosen,
    Ascending,
    Descending,
}

/// A line of a sheet that is one of several choices: what it is called and, at its
/// right, the mark of the one that is on. True when it was tapped.
fn choice_item(ui: &mut Ui, label: &str, mark: Option<Mark>) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 50.0), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::RadioButton, true, mark.is_some(), label)
    });
    if held(ui, &response) {
        ui.painter()
            .rect_filled(rect, 0.0, ui.visuals().widgets.hovered.weak_bg_fill);
    }
    let accent = theme::accent(ui.visuals().dark_mode);
    let color = if mark.is_some() {
        accent
    } else {
        ui.visuals().strong_text_color()
    };
    ui.painter().text(
        pos2(rect.left() + SIDE + 6.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font(17.0),
        color,
    );
    let at = pos2(rect.right() - SIDE - 14.0, rect.center().y);
    match mark {
        Some(Mark::Chosen) => icons::paint(
            ui.painter(),
            Rect::from_center_size(at, egui::Vec2::splat(20.0)),
            Icon::Check,
            accent,
        ),
        Some(direction) => {
            let arrow = if direction == Mark::Ascending {
                "▲"
            } else {
                "▼"
            };
            ui.painter()
                .text(at, Align2::CENTER_CENTER, arrow, font(14.0), accent);
        }
        None => {}
    }
    response.clicked()
}

impl OneumApp {
    /// Leaves the choosing of several songs, with nothing chosen.
    pub(super) fn phone_end_selection(&mut self) {
        if let Some(nav) = self.phone.selecting.take()
            && let Some(state) = self.views.get_mut(&nav)
        {
            state.selection.clear();
        }
    }

    /// The line above a list's songs: how many there are and how long they play, and at
    /// its right the ways to put them in another order and to choose several of them.
    pub(super) fn phone_list_tools(&mut self, ui: &mut Ui, nav: &Nav, rows: &[Row]) {
        let s = self.s();
        let selecting = self.phone.selecting.as_ref() == Some(nav);
        let accent = theme::accent(ui.visuals().dark_mode);
        let (chosen, total) = self
            .views
            .get(nav)
            .map_or((0, 0), |state| (state.selection.len(), state.total_ms));
        let text = if selecting {
            self.lang.selected_count(chosen)
        } else {
            // Added up when the rows were made, not in every frame.
            format!(
                "{} · {}",
                self.lang.song_count(rows.len()),
                self.lang.total_time(total)
            )
        };
        let mut begin = false;
        let mut end = false;
        let mut sort = false;
        ui.horizontal(|ui| {
            ui.add_space(SIDE);
            ui.label(RichText::new(text).font(font(13.0)).color(theme::dim(ui)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(SIDE - 6.0);
                ui.spacing_mut().item_spacing.x = 6.0;
                // Words to tap, each with room for a finger around it.
                let button = |ui: &mut Ui, label: &str| {
                    let text = RichText::new(label)
                        .font(theme::semibold(14.5))
                        .color(accent);
                    let galley = egui::WidgetText::from(text).into_galley(
                        ui,
                        Some(egui::TextWrapMode::Extend),
                        f32::INFINITY,
                        egui::TextStyle::Button,
                    );
                    let size = vec2(galley.size().x + 16.0, 36.0);
                    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
                    response
                        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
                    let color = if held(ui, &response) {
                        accent.gamma_multiply(0.6)
                    } else {
                        accent
                    };
                    ui.painter().galley(
                        pos2(
                            rect.center().x - galley.size().x / 2.0,
                            rect.center().y - galley.size().y / 2.0,
                        ),
                        galley,
                        color,
                    );
                    response.clicked()
                };
                if selecting {
                    end = button(ui, s.phone_done);
                } else {
                    if selectable(nav) {
                        begin = button(ui, s.phone_select);
                    }
                    if sortable(nav) {
                        sort = button(ui, s.phone_sort);
                    }
                }
            });
        });
        if begin {
            self.phone_end_selection();
            self.phone.selecting = Some(nav.clone());
        }
        if end {
            self.phone_end_selection();
        }
        if sort {
            self.phone.sheet = Some(Sheet::Sort(nav.clone()));
        }
    }

    /// In the place of "Play" and "Shuffle" while songs are being chosen: what to do
    /// with the chosen ones, and all of them at once.
    pub(super) fn phone_selection_bar(&mut self, ui: &mut Ui, nav: &Nav, rows: &[Row]) {
        let s = self.s();
        let chosen = self.views.get(nav).map_or(0, |state| state.selection.len());
        let all = !rows.is_empty() && chosen >= rows.len();
        let mut act = false;
        let mut toggle = false;
        ui.horizontal(|ui| {
            ui.add_space(SIDE);
            let width = (ui.available_width() - SIDE - 10.0) / 2.0;
            ui.spacing_mut().item_spacing.x = 10.0;
            act = pill_button(ui, Icon::More, s.phone_selected_songs, width, true).clicked();
            let label = if all {
                s.phone_deselect
            } else {
                s.menu_select_all
            };
            toggle = pill_button(ui, Icon::Check, label, width, false).clicked();
        });
        if act {
            if chosen == 0 {
                self.info_toast(s.phone_nothing_selected);
            } else {
                self.phone.sheet = Some(Sheet::Selection(nav.clone()));
            }
        }
        if toggle {
            let state = self.views.entry(nav.clone()).or_default();
            if all {
                state.selection.clear();
            } else {
                state.selection = rows.iter().map(|row| row.key).collect();
            }
        }
    }

    /// The songs chosen in the list of `nav`, in the order the list shows them, with
    /// the keys of their rows (in a playlist: its items).
    fn phone_chosen(&self, nav: &Nav) -> (Vec<TrackId>, Vec<u64>) {
        let mut tracks = Vec::new();
        let mut keys = Vec::new();
        if let Some(state) = self.views.get(nav) {
            for row in &state.rows {
                if state.selection.contains(&row.key)
                    && let Some(track) = self.snapshot.tracks.get(row.idx as usize)
                {
                    tracks.push(track.id);
                    keys.push(row.key);
                }
            }
        }
        (tracks, keys)
    }

    /// The sheet of what can be done with the chosen songs. True when it is done with.
    pub(super) fn phone_selection_items(
        &mut self,
        ui: &mut Ui,
        nav: &Nav,
        actions: &mut Vec<Action>,
    ) -> bool {
        let s = self.s();
        let (tracks, keys) = self.phone_chosen(nav);
        if tracks.is_empty() {
            return true;
        }
        Self::sheet_heading(ui, &self.lang.selected_count(tracks.len()), "");
        if Self::sheet_item(ui, Icon::Play, s.play_now, false) {
            actions.push(Action::PlayTracks(tracks, 0));
            self.phone_end_selection();
            return true;
        }
        if Self::sheet_item(ui, Icon::Play, s.play_next, false) {
            actions.push(Action::PlayNext(tracks));
            self.phone_end_selection();
            return true;
        }
        if Self::sheet_item(ui, Icon::Queue, s.add_to_queue, false) {
            self.info_toast(self.lang.added_to_queue(tracks.len()));
            actions.push(Action::Enqueue(tracks));
            self.phone_end_selection();
            return true;
        }
        if Self::sheet_item(ui, Icon::Playlist, s.add_to_playlist, false) {
            self.phone.sheet = Some(Sheet::AddToPlaylist(tracks));
            self.phone_end_selection();
            return false;
        }
        // All of them favourites already: the heart takes them out again.
        let all_favorite = tracks
            .iter()
            .all(|id| self.snapshot.get(*id).is_some_and(|track| track.favorite));
        let (icon, label) = if all_favorite {
            (Icon::HeartFilled, s.remove_favorite)
        } else {
            (Icon::Heart, s.add_favorite)
        };
        if Self::sheet_item(ui, icon, label, false) {
            actions.push(Action::SetFavorite(tracks, !all_favorite));
            self.phone_end_selection();
            return true;
        }
        match nav {
            Nav::Playlist(id) => {
                if Self::sheet_item(ui, Icon::Trash, s.remove_from_playlist, true) {
                    let items = keys.into_iter().map(|key| key as i64).collect();
                    actions.push(Action::RemoveFromPlaylist(*id, items));
                    self.phone_end_selection();
                    return true;
                }
            }
            _ => {
                if Self::sheet_item(ui, Icon::Trash, s.remove_from_library, true) {
                    actions.push(Action::RemoveFromLibrary(tracks));
                    self.phone_end_selection();
                    return true;
                }
            }
        }
        false
    }

    /// The sheet of the orders a list can be put in. True when one was chosen.
    pub(super) fn phone_sort_items(&mut self, ui: &mut Ui, nav: &Nav) -> bool {
        let s = self.s();
        Self::sheet_heading(ui, s.phone_sort, &self.phone_nav_title(nav));
        let current = self.views.get(nav).and_then(|state| state.sort);
        let mut chosen: Option<Option<(SortKey, bool)>> = None;
        if choice_item(
            ui,
            s.phone_sort_default,
            current.is_none().then_some(Mark::Chosen),
        ) {
            chosen = Some(None);
        }
        for (key, label) in sort_choices(s) {
            let mark = match current {
                Some((on, true)) if on == key => Some(Mark::Ascending),
                Some((on, false)) if on == key => Some(Mark::Descending),
                _ => None,
            };
            if choice_item(ui, label, mark) {
                // The order that is on turns around; another one starts where one
                // looks first.
                chosen = Some(Some(match current {
                    Some((on, ascending)) if on == key => (key, !ascending),
                    _ => (key, starts_ascending(key)),
                }));
            }
        }
        match chosen {
            Some(sort) => {
                self.views.entry(nav.clone()).or_default().sort = sort;
                true
            }
            None => false,
        }
    }

    /// The handle at the right of a row by which it is carried to another place. Taking
    /// hold of it begins that; [`Self::follow_reorder`] goes on with it.
    pub(super) fn reorder_grip(&mut self, ui: &mut Ui, list: &'static str, index: usize, at: Rect) {
        let s = self.s();
        let response = ui.interact(at, ui.scope_id().with("grip"), Sense::drag());
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, s.phone_reorder));
        let carried = self
            .phone
            .reorder
            .is_some_and(|carried| carried.list == list && carried.from == index);
        let color = if carried {
            theme::accent(ui.visuals().dark_mode)
        } else {
            ui.visuals().weak_text_color()
        };
        icons::paint(
            ui.painter(),
            Rect::from_center_size(at.center(), egui::Vec2::splat(20.0)),
            Icon::Grip,
            color,
        );
        if response.drag_started() && self.phone.reorder.is_none() {
            self.phone.reorder = Some(Reorder {
                list,
                from: index,
                to: index,
            });
        }
    }

    /// Whether row `index` of `list` is the one being carried.
    pub(super) fn is_carried(&self, list: &'static str, index: usize) -> bool {
        self.phone
            .reorder
            .is_some_and(|carried| carried.list == list && carried.from == index)
    }

    /// Goes on with a row of `list` that is being carried: marks the gap it would go
    /// to, moves the list along when the finger is near its upper or lower end, and
    /// when the finger lets go says where the row was and where it goes in the list with
    /// it taken out. `rows` is where the rows are (all of them, also those out of
    /// view), `height` that of one, `count` how many there are.
    pub(super) fn follow_reorder(
        &mut self,
        ui: &mut Ui,
        list: &'static str,
        rows: Rect,
        height: f32,
        count: usize,
    ) -> Option<(usize, usize)> {
        let carried = self.phone.reorder.filter(|carried| carried.list == list)?;
        let (at, down) = ui.input(|i| (i.pointer.latest_pos(), i.pointer.primary_down()));
        if !down {
            self.phone.reorder = None;
            return drop_place(carried.from, carried.to.min(count));
        }
        let at = at?;
        let to = (((at.y - rows.top()) / height) + 0.5)
            .floor()
            .clamp(0.0, count as f32) as usize;
        self.phone.reorder = Some(Reorder { to, ..carried });
        // Near an end of what is in view the list moves along, so that a row can be
        // carried further than the screen shows.
        const ZONE: f32 = 64.0;
        const STEP: f32 = 9.0;
        let view = ui.clip_rect();
        let step = if at.y < view.top() + ZONE {
            STEP
        } else if at.y > view.bottom() - ZONE {
            -STEP
        } else {
            0.0
        };
        if step != 0.0 {
            ui.scroll_with_delta_animation(vec2(0.0, step), egui::style::ScrollAnimation::none());
        }
        // (Before the first row the mark is kept inside what is drawn.)
        let y = (rows.top() + to as f32 * height).max(view.top() + 2.0);
        let accent = theme::accent(ui.visuals().dark_mode);
        ui.painter().hline(
            rows.left() + SIDE..=rows.right() - SIDE,
            y,
            egui::Stroke::new(2.5, accent),
        );
        ui.painter()
            .circle_filled(pos2(rows.left() + SIDE, y), 4.5, accent);
        ui.ctx().request_repaint();
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_let_go_of_where_it_is_does_not_move() {
        // The gaps on either side of row 3 are 3 and 4.
        assert_eq!(drop_place(3, 3), None);
        assert_eq!(drop_place(3, 4), None);
    }

    #[test]
    fn a_row_goes_to_the_gap_it_is_let_go_of_in() {
        // Up: the gap's number is the place.
        assert_eq!(drop_place(3, 0), Some((3, 0)));
        assert_eq!(drop_place(3, 2), Some((3, 2)));
        // Down: the row itself no longer counts among those before the gap.
        assert_eq!(drop_place(3, 5), Some((3, 4)));
        assert_eq!(drop_place(0, 6), Some((0, 5)));
    }

    #[test]
    fn the_lists_whose_order_is_their_own_are_not_sorted() {
        assert!(sortable(&Nav::Songs));
        assert!(sortable(&Nav::Genre("Jazz".into())));
        assert!(!sortable(&Nav::Playlist(1)));
        assert!(!sortable(&Nav::Album(1)));
        assert!(!sortable(&Nav::Recent));
        // … but several of their songs can still be chosen.
        assert!(selectable(&Nav::Playlist(1)));
        assert!(selectable(&Nav::Album(1)));
        assert!(!selectable(&Nav::Albums));
    }

    #[test]
    fn every_order_of_the_sheet_is_named_and_starts_where_one_looks_first() {
        let s = &crate::i18n::KO;
        let choices = sort_choices(s);
        for (index, (key, label)) in choices.iter().enumerate() {
            assert!(!label.is_empty());
            assert!(
                choices[..index].iter().all(|(other, _)| other != key),
                "{key:?} twice"
            );
        }
        assert!(starts_ascending(SortKey::Title));
        assert!(!starts_ascending(SortKey::PlayCount));
        assert!(!starts_ascending(SortKey::DateAdded));
    }
}
