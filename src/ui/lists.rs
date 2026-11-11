//! The central area: song tables, album grid, artist list and their detail views.
use super::icons::Icon;
use super::{Action, ListState, Nav, OneumApp, Row, theme, widgets};
use crate::i18n::format_time;
use crate::library::{Library, LibrarySnapshot, SortKey};
use crate::model::TrackId;
use crate::playback::PlayState;
use eframe::egui::{self, Align, Layout, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};
use std::collections::{HashMap, HashSet};

const GRID_CELL: f32 = 172.0;
const GRID_ART: f32 = 148.0;
const GRID_ROW: f32 = 206.0;

#[derive(Clone, Copy, Default)]
struct TableOptions {
    track_no: bool,
    hide_album: bool,
    /// Rows are playlist items (keys are item ids) of this playlist.
    playlist: Option<i64>,
    /// Keep the given order unless the user sorts by a column.
    natural_order: bool,
}

fn build_rows(
    snap: &LibrarySnapshot,
    library: &Library,
    nav: &Nav,
    query: &str,
    sort: Option<(SortKey, bool)>,
) -> Vec<Row> {
    let q = LibrarySnapshot::prepare_query(query);
    let track_row = |idx: u32| Row {
        key: snap.tracks[idx as usize].id as u64,
        idx,
    };
    let matches = |idx: u32| q.is_empty() || snap.matches(idx, &q);
    let mut rows: Vec<Row> = match nav {
        Nav::Songs => {
            if q.is_empty() {
                (0..snap.len() as u32).map(track_row).collect()
            } else {
                snap.search(query).into_iter().map(track_row).collect()
            }
        }
        Nav::Favorites => (0..snap.len() as u32)
            .filter(|&i| snap.tracks[i as usize].favorite && matches(i))
            .map(track_row)
            .collect(),
        Nav::Recent => library
            .recent_plays(300)
            .into_iter()
            .filter_map(|id| snap.index_of(id))
            .filter(|&i| matches(i))
            .map(track_row)
            .collect(),
        Nav::Album(key) => snap
            .album_index(*key)
            .and_then(|i| snap.albums().get(i as usize))
            .map(|album| {
                album
                    .tracks
                    .iter()
                    .copied()
                    .filter(|&i| matches(i))
                    .map(track_row)
                    .collect()
            })
            .unwrap_or_default(),
        Nav::Playlist(id) => library
            .playlist_items(*id)
            .into_iter()
            .filter_map(|item| {
                snap.index_of(item.track_id).map(|idx| Row {
                    key: item.item_id as u64,
                    idx,
                })
            })
            .filter(|row| matches(row.idx))
            .collect(),
        Nav::Albums => snap
            .albums()
            .iter()
            .enumerate()
            .filter(|(_, album)| q.is_empty() || album.tracks.iter().any(|&t| snap.matches(t, &q)))
            .map(|(i, album)| Row {
                key: album.key,
                idx: i as u32,
            })
            .collect(),
        Nav::Artists => snap
            .artists()
            .iter()
            .enumerate()
            .filter(|(_, artist)| {
                q.is_empty()
                    || artist.albums.iter().any(|&a| {
                        snap.albums()
                            .get(a as usize)
                            .is_some_and(|al| al.tracks.iter().any(|&t| snap.matches(t, &q)))
                    })
            })
            .map(|(i, _)| Row {
                key: i as u64,
                idx: i as u32,
            })
            .collect(),
        Nav::Artist(name) => snap
            .artists()
            .iter()
            .find(|a| &a.name == name)
            .map(|artist| {
                artist
                    .albums
                    .iter()
                    .filter_map(|&a| {
                        snap.albums().get(a as usize).map(|album| Row {
                            key: album.key,
                            idx: a,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
    };

    let is_track_list = matches!(
        nav,
        Nav::Songs | Nav::Favorites | Nav::Recent | Nav::Album(_) | Nav::Playlist(_)
    );
    let effective_sort = match (sort, nav) {
        (Some(s), _) => Some(s),
        (None, Nav::Songs | Nav::Favorites) => Some((SortKey::Default, true)),
        _ => None,
    };
    if is_track_list && let Some((key, ascending)) = effective_sort {
        // Sort distinct track indices once, then order rows by rank (rows may repeat a track).
        let mut indices: Vec<u32> = rows.iter().map(|r| r.idx).collect();
        indices.sort_unstable();
        indices.dedup();
        snap.sort(&mut indices, key, ascending);
        let rank: HashMap<u32, usize> = indices
            .into_iter()
            .enumerate()
            .map(|(r, i)| (i, r))
            .collect();
        rows.sort_by_key(|r| rank.get(&r.idx).copied().unwrap_or(usize::MAX));
    }
    rows
}

fn selected_tracks(snap: &LibrarySnapshot, rows: &[Row], selection: &HashSet<u64>) -> Vec<TrackId> {
    rows.iter()
        .filter(|r| selection.contains(&r.key))
        .map(|r| snap.tracks[r.idx as usize].id)
        .collect()
}

fn click_select(
    state_rows: &[Row],
    selection: &mut HashSet<u64>,
    anchor: &mut Option<u64>,
    index: usize,
    modifiers: egui::Modifiers,
) {
    let Some(row) = state_rows.get(index) else {
        return;
    };
    if modifiers.command {
        if !selection.remove(&row.key) {
            selection.insert(row.key);
        }
        *anchor = Some(row.key);
    } else if modifiers.shift {
        let from = anchor
            .and_then(|a| state_rows.iter().position(|r| r.key == a))
            .unwrap_or(index);
        let (lo, hi) = (from.min(index), from.max(index));
        selection.clear();
        selection.extend(state_rows[lo..=hi].iter().map(|r| r.key));
    } else {
        selection.clear();
        selection.insert(row.key);
        *anchor = Some(row.key);
    }
}

impl OneumApp {
    /// Rebuilds the row list of `nav` when the library, the query or the sort changed.
    fn ensure_rows(&mut self, nav: &Nav) {
        let extra = match nav {
            Nav::Recent => self.player.current.map_or(0, |c| c.item_id),
            _ => 0,
        };
        let stamp_generation = self
            .lib_generation
            .wrapping_add(extra.wrapping_mul(0x9E37_79B9));
        let playlists_generation = self.playlists_generation;
        let state = self.views.entry(nav.clone()).or_default();
        let current = state.built.as_ref().is_some_and(|(g, p, q, s)| {
            *g == stamp_generation
                && *p == playlists_generation
                && *q == state.query
                && *s == state.sort
        });
        if current {
            return;
        }
        let rows = build_rows(
            &self.snapshot,
            &self.core.library,
            nav,
            &state.query,
            state.sort,
        );
        let keys: HashSet<u64> = rows.iter().map(|r| r.key).collect();
        state.selection.retain(|k| keys.contains(k));
        if state.anchor.is_some_and(|a| !keys.contains(&a)) {
            state.anchor = None;
        }
        if state.scroll_to == Some(usize::MAX) {
            state.scroll_to = state
                .anchor
                .and_then(|a| rows.iter().position(|r| r.key == a));
        }
        state.total_ms = if matches!(nav, Nav::Albums | Nav::Artists | Nav::Artist(_)) {
            0
        } else {
            rows.iter()
                .map(|r| self.snapshot.tracks[r.idx as usize].duration_ms)
                .sum()
        };
        state.rows = rows;
        state.built = Some((
            stamp_generation,
            playlists_generation,
            state.query.clone(),
            state.sort,
        ));
    }

    pub(super) fn central(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let nav = self.nav.clone();
        let library_empty = self.snapshot.is_empty() && self.core.library.roots().is_empty();
        if library_empty
            && !matches!(nav, Nav::Playlist(_))
            && !self.core.library.scan_status().running
        {
            self.empty_library(ui);
            return;
        }
        self.ensure_rows(&nav);
        match &nav {
            Nav::Songs => {
                self.track_list(ui, &nav, TableOptions::default(), actions);
            }
            Nav::Favorites => {
                self.view_title(ui, self.s().favorites, None);
                self.track_list(ui, &nav, TableOptions::default(), actions);
            }
            Nav::Recent => {
                self.view_title(ui, self.s().recently_played, None);
                self.track_list(
                    ui,
                    &nav,
                    TableOptions {
                        natural_order: true,
                        ..Default::default()
                    },
                    actions,
                );
            }
            Nav::Playlist(id) => {
                let name = self
                    .playlists
                    .iter()
                    .find(|p| p.id == *id)
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
                self.view_title(ui, &name, None);
                let options = TableOptions {
                    playlist: Some(*id),
                    natural_order: true,
                    ..Default::default()
                };
                self.track_list(ui, &nav, options, actions);
            }
            Nav::Albums => self.album_grid(ui, &nav, actions),
            Nav::Album(key) => self.album_detail(ui, &nav, *key, actions),
            Nav::Artists => self.artist_list(ui, &nav, actions),
            Nav::Artist(name) => {
                let summary = self
                    .snapshot
                    .artists()
                    .iter()
                    .find(|a| &a.name == name)
                    .map(|a| self.lang.artist_summary(a.albums.len(), a.track_count));
                let title = if name.is_empty() {
                    self.s().unknown_artist
                } else {
                    name.as_str()
                };
                self.view_title(ui, title, summary.as_deref());
                self.album_grid(ui, &nav, actions);
            }
        }
    }

    fn view_title(&self, ui: &mut Ui, title: &str, subtitle: Option<&str>) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.add(egui::Label::new(RichText::new(title).heading().strong()).truncate());
            if let Some(subtitle) = subtitle {
                ui.label(RichText::new(subtitle).color(theme::dim(ui)));
            }
        });
        ui.add_space(4.0);
    }

    fn empty_library(&mut self, ui: &mut Ui) {
        let s = self.s();
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.22).max(24.0));
            let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(64.0), Sense::hover());
            super::icons::paint(
                ui.painter(),
                rect,
                Icon::Note,
                theme::accent(ui.visuals().dark_mode),
            );
            ui.add_space(10.0);
            ui.label(RichText::new(s.empty_library_title).size(22.0).strong());
            ui.add_space(4.0);
            ui.label(RichText::new(s.empty_library_body).color(theme::dim(ui)));
            ui.add_space(18.0);
            let dark = ui.visuals().dark_mode;
            let ink = if dark {
                egui::Color32::from_rgb(0x14, 0x14, 0x18)
            } else {
                egui::Color32::WHITE
            };
            let button = egui::Button::new(
                RichText::new(s.add_music_folder)
                    .size(16.0)
                    .strong()
                    .color(ink),
            )
            .fill(theme::accent(dark))
            .min_size(egui::vec2(220.0, 44.0));
            if ui.add(button).clicked() {
                self.pick_folder();
            }
            ui.add_space(8.0);
            if ui.link(s.open_files).clicked() {
                self.pick_files();
            }
        });
    }

    fn empty_list_message(&self, ui: &mut Ui, nav: &Nav, has_query: bool) {
        let s = self.s();
        let (icon, title, body) = if has_query {
            (Icon::Search, s.empty_search, s.empty_search_hint)
        } else {
            match nav {
                Nav::Favorites => (Icon::Heart, s.empty_favorites, s.empty_favorites_hint),
                Nav::Recent => (Icon::Note, s.empty_recent, ""),
                Nav::Playlist(_) => (Icon::Queue, s.empty_playlist, s.empty_playlist_hint),
                _ => (Icon::Note, s.empty_search, ""),
            }
        };
        widgets::empty_state(ui, icon, title, body);
    }

    /// Arrow keys, Return, ⌘A and Delete for the focused list.
    fn list_keys(
        &self,
        ui: &Ui,
        state: &mut ListState,
        options: TableOptions,
        actions: &mut Vec<Action>,
    ) {
        if ui.ctx().text_edit_focused()
            || self.dialog.is_some()
            || self.settings_open
            || state.rows.is_empty()
        {
            return;
        }
        let (down, up, enter, select_all, delete, shift) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowDown) && !i.modifiers.command,
                i.key_pressed(egui::Key::ArrowUp) && !i.modifiers.command,
                i.key_pressed(egui::Key::Enter),
                i.modifiers.command && i.key_pressed(egui::Key::A),
                i.key_pressed(egui::Key::Backspace) || i.key_pressed(egui::Key::Delete),
                i.modifiers.shift,
            )
        });
        if select_all {
            state.selection = state.rows.iter().map(|r| r.key).collect();
        }
        if down || up {
            let cursor = state
                .anchor
                .and_then(|a| state.rows.iter().position(|r| r.key == a));
            let next = match (cursor, down) {
                (Some(i), true) => (i + 1).min(state.rows.len() - 1),
                (Some(i), false) => i.saturating_sub(1),
                (None, true) => 0,
                (None, false) => state.rows.len() - 1,
            };
            let key = state.rows[next].key;
            if !shift {
                state.selection.clear();
            }
            state.selection.insert(key);
            state.anchor = Some(key);
            state.scroll_to = Some(next);
        }
        if enter
            && let Some(index) = state
                .anchor
                .and_then(|a| state.rows.iter().position(|r| r.key == a))
        {
            let tracks = state
                .rows
                .iter()
                .map(|r| self.snapshot.tracks[r.idx as usize].id)
                .collect();
            actions.push(Action::PlayTracks(tracks, index));
        }
        if delete
            && let Some(playlist) = options.playlist
            && !state.selection.is_empty()
        {
            actions.push(Action::RemoveFromPlaylist(
                playlist,
                state.selection.iter().map(|k| *k as i64).collect(),
            ));
        }
    }

    fn track_list(
        &mut self,
        ui: &mut Ui,
        nav: &Nav,
        options: TableOptions,
        actions: &mut Vec<Action>,
    ) {
        let mut state = self.views.remove(nav).unwrap_or_default();
        if state.rows.is_empty() {
            self.empty_list_message(ui, nav, !state.query.is_empty());
            self.views.insert(nav.clone(), state);
            return;
        }
        self.list_keys(ui, &mut state, options, actions);
        let this = &*self;
        let s = this.s();
        let snap = this.snapshot.clone();
        let current_track = this.player.current.map(|c| c.track_id);
        let is_playing = this.player.state == PlayState::Playing;
        let show_format = this.settings.show_format_column;
        let scroll_to = state.scroll_to.take();
        let sort = state.sort;
        let sortable = !options.natural_order || options.playlist.is_none();
        let accent = theme::accent(ui.visuals().dark_mode);
        let dim = theme::dim(ui);

        // Footer first so the table can use the remaining height.
        egui::Panel::bottom(("list-footer", nav.clone()))
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let mut text = format!(
                        "{} · {}",
                        this.lang.song_count(state.rows.len()),
                        this.lang.total_time(state.total_ms)
                    );
                    if state.selection.len() > 1 {
                        text = format!(
                            "{} · {text}",
                            this.lang.selected_count(state.selection.len())
                        );
                    }
                    ui.label(RichText::new(text).small().color(dim));
                });
            });

        let ListState {
            rows,
            selection,
            anchor,
            ..
        } = &mut state;
        let rows: &[Row] = rows;
        // Column widths are derived from the available width every frame, so the table
        // always fits, also in a small window or next to the side panel.
        let column_count = 4
            + usize::from(options.track_no)
            + usize::from(!options.hide_album)
            + usize::from(show_format);
        let fixed = 26.0
            + 58.0
            + if options.track_no { 32.0 } else { 0.0 }
            + if show_format { 64.0 } else { 0.0 };
        let gaps =
            ui.spacing().item_spacing.x * column_count as f32 + ui.spacing().scroll.bar_width + 6.0;
        let flexible = (ui.available_width() - fixed - gaps).max(180.0);
        let (title_w, artist_w, album_w) = if options.hide_album {
            (flexible * 0.62, flexible * 0.38, 0.0)
        } else {
            (flexible * 0.44, flexible * 0.26, flexible * 0.30)
        };
        let mut table = TableBuilder::new(ui)
            .id_salt(("tracks", nav.clone()))
            .striped(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .min_scrolled_height(0.0)
            .column(Column::exact(26.0));
        if options.track_no {
            table = table.column(Column::exact(32.0));
        }
        table = table.column(Column::exact(title_w).clip(true));
        table = table.column(Column::exact(artist_w).clip(true));
        if !options.hide_album {
            table = table.column(Column::exact(album_w).clip(true));
        }
        table = table.column(Column::exact(58.0));
        if show_format {
            table = table.column(Column::exact(64.0));
        }
        if let Some(row) = scroll_to.filter(|r| *r < rows.len()) {
            table = table.scroll_to_row(row, None);
        }

        let header_cell =
            |ui: &mut Ui, text: &str, key: Option<SortKey>, actions: &mut Vec<Action>| {
                let arrow = match (sort, key) {
                    (Some((k, asc)), Some(key)) if k == key => {
                        if asc {
                            " ▲"
                        } else {
                            " ▼"
                        }
                    }
                    _ => "",
                };
                let label = RichText::new(format!("{text}{arrow}"))
                    .small()
                    .strong()
                    .color(dim);
                match key.filter(|_| sortable) {
                    Some(key) => {
                        if ui
                            .add(egui::Label::new(label).sense(Sense::click()).truncate())
                            .clicked()
                        {
                            actions.push(Action::Sort(key));
                        }
                    }
                    None => {
                        ui.add(egui::Label::new(label).truncate());
                    }
                }
            };

        table
            .header(22.0, |mut header| {
                header.col(|_| {});
                if options.track_no {
                    header
                        .col(|ui| header_cell(ui, s.col_track_no, Some(SortKey::TrackNo), actions));
                }
                header.col(|ui| header_cell(ui, s.col_title, Some(SortKey::Title), actions));
                header.col(|ui| header_cell(ui, s.col_artist, Some(SortKey::Artist), actions));
                if !options.hide_album {
                    header.col(|ui| header_cell(ui, s.col_album, Some(SortKey::Album), actions));
                }
                header.col(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        header_cell(ui, s.col_duration, Some(SortKey::Duration), actions);
                    });
                });
                if show_format {
                    header.col(|ui| header_cell(ui, s.col_format, Some(SortKey::Format), actions));
                }
            })
            .body(|body| {
                body.rows(theme::ROW_HEIGHT, rows.len(), |mut row| {
                    let index = row.index();
                    let Row { key, idx } = rows[index];
                    let track = &snap.tracks[idx as usize];
                    let selected = selection.contains(&key);
                    row.set_selected(selected);
                    let is_current = current_track == Some(track.id);
                    let text_color = |ui: &Ui| {
                        if !track.available {
                            dim
                        } else if is_current {
                            accent
                        } else {
                            ui.visuals().text_color()
                        }
                    };

                    row.col(|ui| {
                        let hovered_row = ui.ctx().pointer_hover_pos().is_some_and(|p| {
                            ui.max_rect().y_range().contains(p.y) && ui.clip_rect().contains(p)
                        });
                        if track.favorite || hovered_row {
                            let (icon, label) = if track.favorite {
                                (Icon::HeartFilled, s.remove_favorite)
                            } else {
                                (Icon::Heart, s.add_favorite)
                            };
                            if widgets::icon_button(ui, icon, label, 20.0, track.favorite).clicked()
                            {
                                actions.push(Action::SetFavorite(vec![track.id], !track.favorite));
                            }
                        }
                    });
                    if options.track_no {
                        row.col(|ui| {
                            let n = track.track_no.map(|n| n.to_string()).unwrap_or_default();
                            ui.label(RichText::new(n).color(dim));
                        });
                    }
                    row.col(|ui| {
                        if is_current {
                            let (rect, _) =
                                ui.allocate_exact_size(egui::Vec2::splat(12.0), Sense::hover());
                            let icon = if is_playing { Icon::Play } else { Icon::Pause };
                            super::icons::paint(ui.painter(), rect, icon, accent);
                        } else if !track.available {
                            let (rect, response) =
                                ui.allocate_exact_size(egui::Vec2::splat(13.0), Sense::hover());
                            super::icons::paint(ui.painter(), rect, Icon::Warning, dim);
                            response.on_hover_text(s.info_missing);
                        }
                        let mut title = RichText::new(&track.title).color(text_color(ui));
                        if is_current {
                            title = title.strong();
                        }
                        widgets::truncated(ui, title);
                    });
                    row.col(|ui| {
                        let artist = if track.artist.is_empty() {
                            s.unknown_artist
                        } else {
                            track.artist.as_str()
                        };
                        widgets::truncated(ui, RichText::new(artist).color(text_color(ui)));
                    });
                    if !options.hide_album {
                        row.col(|ui| {
                            widgets::truncated(
                                ui,
                                RichText::new(&track.album).color(text_color(ui)),
                            );
                        });
                    }
                    row.col(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(6.0);
                            ui.label(RichText::new(format_time(track.duration_ms)).color(dim));
                        });
                    });
                    if show_format {
                        row.col(|ui| {
                            ui.label(RichText::new(&track.codec).small().color(dim));
                        });
                    }

                    let response = row.response();
                    if response.double_clicked() {
                        let tracks = rows
                            .iter()
                            .map(|r| snap.tracks[r.idx as usize].id)
                            .collect();
                        actions.push(Action::PlayTracks(tracks, index));
                    } else if response.clicked() {
                        let modifiers = response.ctx.input(|i| i.modifiers);
                        click_select(rows, selection, anchor, index, modifiers);
                    }
                    if response.secondary_clicked() && !selection.contains(&key) {
                        selection.clear();
                        selection.insert(key);
                        *anchor = Some(key);
                    }
                    response.context_menu(|ui| {
                        this.track_menu(ui, rows, selection, index, options, actions);
                    });
                });
            });
        self.views.insert(nav.clone(), state);
    }

    /// Context menu for the selected rows (`clicked` is the row under the pointer).
    fn track_menu(
        &self,
        ui: &mut Ui,
        rows: &[Row],
        selection: &HashSet<u64>,
        clicked: usize,
        options: TableOptions,
        actions: &mut Vec<Action>,
    ) {
        let s = self.s();
        let snap = &self.snapshot;
        let mut tracks = selected_tracks(snap, rows, selection);
        let clicked_track = snap.tracks[rows[clicked].idx as usize].clone();
        if tracks.is_empty() {
            tracks.push(clicked_track.id);
        }
        ui.set_min_width(210.0);
        if ui.button(s.play_now).clicked() {
            actions.push(Action::PlayTracks(tracks.clone(), 0));
            ui.close();
        }
        if ui.button(s.play_next).clicked() {
            actions.push(Action::PlayNext(tracks.clone()));
            ui.close();
        }
        if ui.button(s.add_to_queue).clicked() {
            actions.push(Action::Enqueue(tracks.clone()));
            ui.close();
        }
        ui.separator();
        ui.menu_button(s.add_to_playlist, |ui| {
            if ui.button(format!("{}…", s.new_playlist)).clicked() {
                actions.push(Action::NewPlaylistWith(tracks.clone()));
                ui.close();
            }
            if !self.playlists.is_empty() {
                ui.separator();
            }
            for playlist in &self.playlists {
                if ui.button(&playlist.name).clicked() {
                    actions.push(Action::AddToPlaylist(playlist.id, tracks.clone()));
                    ui.close();
                }
            }
        });
        let favorite = clicked_track.favorite;
        if ui
            .button(if favorite {
                s.remove_favorite
            } else {
                s.add_favorite
            })
            .clicked()
        {
            actions.push(Action::SetFavorite(tracks.clone(), !favorite));
            ui.close();
        }
        ui.separator();
        if ui.button(s.show_in_finder).clicked() {
            actions.push(Action::Reveal(clicked_track.id));
            ui.close();
        }
        if ui.button(s.song_info).clicked() {
            actions.push(Action::ShowInfo(clicked_track.id));
            ui.close();
        }
        if let Some(playlist) = options.playlist {
            let items: Vec<i64> = rows
                .iter()
                .filter(|r| selection.contains(&r.key))
                .map(|r| r.key as i64)
                .collect();
            let first = rows
                .iter()
                .position(|r| selection.contains(&r.key))
                .unwrap_or(clicked);
            ui.separator();
            if ui
                .add_enabled(first > 0, egui::Button::new(s.move_up))
                .clicked()
            {
                actions.push(Action::MovePlaylistItems(
                    playlist,
                    items.clone(),
                    first - 1,
                ));
                ui.close();
            }
            if ui
                .add_enabled(
                    first + items.len() < rows.len(),
                    egui::Button::new(s.move_down),
                )
                .clicked()
            {
                actions.push(Action::MovePlaylistItems(
                    playlist,
                    items.clone(),
                    first + 1,
                ));
                ui.close();
            }
            if ui.button(s.remove_from_playlist).clicked() {
                actions.push(Action::RemoveFromPlaylist(playlist, items));
                ui.close();
            }
        }
        // Only individually opened files can be removed here; tracks inside a music
        // folder would simply come back on the next scan.
        let loose: Vec<TrackId> = tracks
            .iter()
            .copied()
            .filter(|id| snap.get(*id).is_some_and(|t| t.root_id.is_none()))
            .collect();
        if !loose.is_empty() && loose.len() == tracks.len() {
            ui.separator();
            if ui.button(s.remove_from_library).clicked() {
                actions.push(Action::RemoveFromLibrary(loose));
                ui.close();
            }
        }
    }

    fn album_grid(&mut self, ui: &mut Ui, nav: &Nav, actions: &mut Vec<Action>) {
        let state = self.views.remove(nav).unwrap_or_default();
        if state.rows.is_empty() {
            self.empty_list_message(ui, nav, !state.query.is_empty());
            self.views.insert(nav.clone(), state);
            return;
        }
        let s = self.s();
        let snap = self.snapshot.clone();
        let dim = theme::dim(ui);
        let columns = ((ui.available_width() / GRID_CELL).floor() as usize).max(1);
        let grid_rows = state.rows.len().div_ceil(columns);
        egui::ScrollArea::vertical()
            .id_salt(("albums", nav.clone()))
            .auto_shrink([false, false])
            .show_rows(ui, GRID_ROW, grid_rows, |ui, range| {
                for grid_row in range {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        for column in 0..columns {
                            let Some(row) = state.rows.get(grid_row * columns + column) else {
                                break;
                            };
                            let Some(album) = snap.albums().get(row.idx as usize) else {
                                continue;
                            };
                            let (cell, response) = ui.allocate_exact_size(
                                egui::vec2(GRID_CELL, GRID_ROW),
                                Sense::click(),
                            );
                            let title = if album.title.is_empty() {
                                s.unknown_album
                            } else {
                                album.title.as_str()
                            };
                            let artist = if album.artist.is_empty() {
                                s.unknown_artist
                            } else {
                                album.artist.as_str()
                            };
                            let name = format!("{title}, {artist}");
                            response.widget_info(|| {
                                egui::WidgetInfo::labeled(egui::Role::Button, true, &name)
                            });
                            if !ui.is_rect_visible(cell) {
                                continue;
                            }
                            if response.hovered() || response.has_focus() {
                                ui.painter().rect_filled(
                                    cell.shrink(2.0),
                                    8.0,
                                    ui.visuals().faint_bg_color,
                                );
                            }
                            let art_rect = egui::Rect::from_min_size(
                                cell.min + egui::vec2((GRID_CELL - GRID_ART) / 2.0, 8.0),
                                egui::Vec2::splat(GRID_ART),
                            );
                            let texture = self.art.get(album.art_id.as_deref(), false);
                            widgets::art(ui, art_rect, texture, 6.0);
                            let text_rect = egui::Rect::from_min_max(
                                egui::pos2(art_rect.left(), art_rect.bottom() + 6.0),
                                egui::pos2(art_rect.right(), cell.bottom() - 4.0),
                            );
                            let mut text_ui = ui.new_child(
                                egui::UiBuilder::new()
                                    .max_rect(text_rect)
                                    .layout(Layout::top_down(Align::Min)),
                            );
                            text_ui.spacing_mut().item_spacing.y = 1.0;
                            widgets::truncated(&mut text_ui, RichText::new(title).strong());
                            widgets::truncated(
                                &mut text_ui,
                                RichText::new(artist).small().color(dim),
                            );
                            let album_tracks = || {
                                album
                                    .tracks
                                    .iter()
                                    .map(|&i| snap.tracks[i as usize].id)
                                    .collect::<Vec<_>>()
                            };
                            if response.double_clicked() {
                                actions.push(Action::PlayTracks(album_tracks(), 0));
                            } else if response.clicked() {
                                actions.push(Action::Go(Nav::Album(album.key)));
                            }
                            response.context_menu(|ui| {
                                if ui.button(s.play_album).clicked() {
                                    actions.push(Action::PlayTracks(album_tracks(), 0));
                                    ui.close();
                                }
                                if ui.button(s.play_next).clicked() {
                                    actions.push(Action::PlayNext(album_tracks()));
                                    ui.close();
                                }
                                if ui.button(s.add_to_queue).clicked() {
                                    actions.push(Action::Enqueue(album_tracks()));
                                    ui.close();
                                }
                            });
                        }
                    });
                }
            });
        self.views.insert(nav.clone(), state);
    }

    fn album_detail(&mut self, ui: &mut Ui, nav: &Nav, key: u64, actions: &mut Vec<Action>) {
        let s = self.s();
        let snap = self.snapshot.clone();
        let Some(album) = snap
            .album_index(key)
            .and_then(|i| snap.albums().get(i as usize))
        else {
            // The album disappeared (rescan): fall back to the grid.
            actions.push(Action::Go(Nav::Albums));
            return;
        };
        let dim = theme::dim(ui);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(132.0), Sense::hover());
            let texture = self.art.get(album.art_id.as_deref(), true);
            widgets::art(ui, rect, texture, 8.0);
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.add_space(14.0);
                let title = if album.title.is_empty() {
                    s.unknown_album
                } else {
                    album.title.as_str()
                };
                ui.add(egui::Label::new(RichText::new(title).heading().strong()).truncate());
                let artist = if album.artist.is_empty() {
                    s.unknown_artist
                } else {
                    album.artist.as_str()
                };
                if ui.link(RichText::new(artist).size(15.0)).clicked() {
                    actions.push(Action::Go(Nav::Artist(album.artist.clone())));
                }
                let mut meta = Vec::new();
                if let Some(year) = album.year {
                    meta.push(year.to_string());
                }
                meta.push(self.lang.song_count(album.tracks.len()));
                meta.push(self.lang.total_time(album.duration_ms));
                ui.label(RichText::new(meta.join(" · ")).color(dim));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let tracks = || {
                        album
                            .tracks
                            .iter()
                            .map(|&i| snap.tracks[i as usize].id)
                            .collect::<Vec<_>>()
                    };
                    if ui.button(format!("▶  {}", s.play_album)).clicked() {
                        actions.push(Action::PlayTracks(tracks(), 0));
                    }
                    if ui.button(s.shuffle_album).clicked() {
                        let mut shuffled = tracks();
                        fastrand::shuffle(&mut shuffled);
                        actions.push(Action::PlayTracks(shuffled, 0));
                    }
                });
            });
        });
        ui.add_space(8.0);
        let options = TableOptions {
            track_no: true,
            hide_album: true,
            natural_order: true,
            playlist: None,
        };
        self.track_list(ui, nav, options, actions);
    }

    fn artist_list(&mut self, ui: &mut Ui, nav: &Nav, actions: &mut Vec<Action>) {
        let state = self.views.remove(nav).unwrap_or_default();
        if state.rows.is_empty() {
            self.empty_list_message(ui, nav, !state.query.is_empty());
            self.views.insert(nav.clone(), state);
            return;
        }
        let s = self.s();
        let snap = self.snapshot.clone();
        let dim = theme::dim(ui);
        let lang = self.lang;
        TableBuilder::new(ui)
            .id_salt("artists")
            .striped(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .min_scrolled_height(0.0)
            .column(Column::remainder().at_least(160.0).clip(true))
            .column(Column::exact(220.0))
            .header(22.0, |mut header| {
                header.col(|ui| {
                    ui.label(RichText::new(s.col_artist).small().strong().color(dim));
                });
                header.col(|_| {});
            })
            .body(|body| {
                body.rows(theme::ROW_HEIGHT + 4.0, state.rows.len(), |mut row| {
                    let Some(artist) = snap.artists().get(state.rows[row.index()].idx as usize)
                    else {
                        return;
                    };
                    row.col(|ui| {
                        ui.add_space(6.0);
                        let name = if artist.name.is_empty() {
                            s.unknown_artist
                        } else {
                            artist.name.as_str()
                        };
                        widgets::truncated(ui, RichText::new(name).strong());
                    });
                    row.col(|ui| {
                        ui.label(
                            RichText::new(
                                lang.artist_summary(artist.albums.len(), artist.track_count),
                            )
                            .color(dim),
                        );
                    });
                    if row.response().clicked() {
                        actions.push(Action::Go(Nav::Artist(artist.name.clone())));
                    }
                });
            });
        self.views.insert(nav.clone(), state);
    }
}
