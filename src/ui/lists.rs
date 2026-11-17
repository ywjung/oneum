//! The central area: song tables, album grid, artist list and their detail views.
use super::icons::Icon;
use super::{
    Action, ColumnKind, DragTracks, GenreEntry, KeyPane, ListState, Nav, OneumApp, Row,
    TYPEAHEAD_TIMEOUT, theme, widgets,
};
use crate::app::settings::SmartPlaylist;
use crate::i18n::format_time;
use crate::library::{Album, Library, LibrarySnapshot, PreparedQuery, SortKey, normalize};
use crate::model::Track;
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
    /// Row positions are playlist positions (false while a search filters the rows).
    reorderable: bool,
}

/// What the "listen again" view shows: ways back into the library that do not start
/// from a name one already has in mind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub(super) enum Rediscover {
    /// What was played, most recent first.
    #[default]
    Played,
    /// What came into the library, newest first.
    Added,
    /// What has been played most.
    MostPlayed,
    /// What was played once and not for a long time since.
    Forgotten,
}

/// How many songs the "listen again" lists show at most.
const REDISCOVER_LIMIT: usize = 300;
/// A song counts as "not heard for a long time" after this many days.
const FORGOTTEN_AFTER_DAYS: i64 = 30;

/// The songs of a "listen again" list as indices into `tracks`, in the list's order.
/// `now` is the time in Unix seconds. ([`Rediscover::Played`] comes from the play
/// history instead, which also knows about repeats.)
pub(super) fn rediscover(
    tracks: &[std::sync::Arc<crate::model::Track>],
    mode: Rediscover,
    now: i64,
) -> Vec<u32> {
    let mut picked: Vec<u32> = (0..tracks.len() as u32)
        .filter(|&i| {
            let t = &tracks[i as usize];
            match mode {
                Rediscover::Played => false,
                Rediscover::Added => true,
                Rediscover::MostPlayed => t.play_count > 0,
                Rediscover::Forgotten => {
                    t.play_count > 0
                        && t.last_played_at
                            .is_some_and(|at| now - at >= FORGOTTEN_AFTER_DAYS * 86_400)
                }
            }
        })
        .collect();
    let track = |i: &u32| &tracks[*i as usize];
    match mode {
        Rediscover::Played => {}
        // Newest first; songs that came in together (one scan) by id, i.e. in the
        // order they were found.
        Rediscover::Added => {
            picked.sort_by_key(|i| (std::cmp::Reverse(track(i).added_at), track(i).id))
        }
        Rediscover::MostPlayed => picked.sort_by_key(|i| {
            (
                std::cmp::Reverse(track(i).play_count),
                std::cmp::Reverse(track(i).last_played_at),
                track(i).id,
            )
        }),
        // Longest ago first.
        Rediscover::Forgotten => picked.sort_by_key(|i| (track(i).last_played_at, track(i).id)),
    }
    picked.truncate(REDISCOVER_LIMIT);
    picked
}

/// The genres named in a genre tag ("Rock; Indie / Alternative"), as written.
pub(super) fn genres(tag: &str) -> impl Iterator<Item = &str> {
    tag.split([';', ',', '/', '|'])
        .map(str::trim)
        .filter(|genre| !genre.is_empty())
}

/// The genres of the library with their song counts, by name. Spellings that differ
/// only in case are one genre, shown as it was first met.
pub(super) fn genre_entries(tracks: &[std::sync::Arc<Track>]) -> Vec<GenreEntry> {
    let mut found: HashMap<String, GenreEntry> = HashMap::new();
    for track in tracks {
        // A song that names a genre twice still counts once.
        let mut seen: Vec<String> = Vec::new();
        for genre in genres(&track.genre) {
            let key = genre.to_lowercase();
            if seen.contains(&key) {
                continue;
            }
            found
                .entry(key.clone())
                .or_insert_with(|| GenreEntry {
                    name: genre.to_string(),
                    songs: 0,
                })
                .songs += 1;
            seen.push(key);
        }
    }
    let mut list: Vec<(String, GenreEntry)> = found
        .into_values()
        .map(|entry| (normalize(&entry.name), entry))
        .collect();
    list.sort_by(|a, b| a.0.cmp(&b.0));
    list.into_iter().map(|(_, entry)| entry).collect()
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Lists that are not part of the library itself but that some views are made from.
#[derive(Clone, Copy)]
struct Lists<'a> {
    smart: &'a [SmartPlaylist],
    genre_list: &'a [GenreEntry],
}

fn build_rows(
    snap: &LibrarySnapshot,
    library: &Library,
    nav: &Nav,
    query: &str,
    sort: Option<(SortKey, bool)>,
    mode: Rediscover,
    lists: Lists<'_>,
) -> Vec<Row> {
    let Lists { smart, genre_list } = lists;
    let q = LibrarySnapshot::prepare_query(query);
    let track_row = |idx: u32| Row {
        key: snap.tracks[idx as usize].id as u64,
        idx,
    };
    let matches = |idx: u32| q.is_empty() || snap.matches(idx, &q);
    // The saved search of a smart playlist: it may say its own order and number.
    let smart_query = match nav {
        Nav::Smart(name) => smart
            .iter()
            .find(|p| &p.name == name)
            .map(|p| PreparedQuery::new(&p.query)),
        _ => None,
    };
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
        Nav::Recent if mode == Rediscover::Played => library
            .recent_plays(REDISCOVER_LIMIT)
            .into_iter()
            .filter_map(|id| snap.index_of(id))
            .filter(|&i| matches(i))
            .map(track_row)
            .collect(),
        Nav::Recent => rediscover(&snap.tracks, mode, unix_now())
            .into_iter()
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
        Nav::Genre(name) => {
            let wanted = name.trim().to_lowercase();
            (0..snap.len() as u32)
                .filter(|&i| {
                    genres(&snap.tracks[i as usize].genre)
                        .any(|genre| genre.to_lowercase() == wanted)
                        && matches(i)
                })
                .map(track_row)
                .collect()
        }
        // Not a list of songs: nothing to build.
        Nav::Stats | Nav::Network => Vec::new(),
        Nav::Folder(folder) => {
            // In the order of the paths: the way the folders are laid out on disk.
            let mut indices: Vec<u32> = (0..snap.len() as u32)
                .filter(|&i| {
                    (folder.as_os_str().is_empty()
                        || snap.tracks[i as usize].path.starts_with(folder))
                        && matches(i)
                })
                .collect();
            indices.sort_by(|a, b| {
                snap.tracks[*a as usize]
                    .path
                    .cmp(&snap.tracks[*b as usize].path)
            });
            indices.into_iter().map(track_row).collect()
        }
        Nav::Smart(_) => smart_query
            .as_ref()
            .map(|saved| {
                snap.search_prepared(saved)
                    .into_iter()
                    .filter(|&i| matches(i))
                    .map(track_row)
                    .collect()
            })
            .unwrap_or_default(),
        Nav::Genres => {
            let wanted = normalize(query);
            genre_list
                .iter()
                .enumerate()
                .filter(|(_, genre)| {
                    let name = normalize(&genre.name);
                    wanted.split_whitespace().all(|term| name.contains(term))
                })
                .map(|(i, _)| Row {
                    key: i as u64,
                    idx: i as u32,
                })
                .collect()
        }
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
        Nav::Songs
            | Nav::Favorites
            | Nav::Recent
            | Nav::Album(_)
            | Nav::Playlist(_)
            | Nav::Genre(_)
            | Nav::Smart(_)
            | Nav::Folder(_)
    );
    let effective_sort = match (sort, nav) {
        // The order the user put the list in comes before any other.
        (Some(s), _) => Some(s),
        // Then the one the search that was typed asks for (`sort:plays`), then the
        // one a smart playlist's saved search gave its songs.
        (None, _) if q.order.is_some() => q.order,
        (None, Nav::Smart(_))
            if smart_query
                .as_ref()
                .is_some_and(|saved| saved.order.is_some()) =>
        {
            None
        }
        (None, Nav::Songs | Nav::Favorites | Nav::Genre(_) | Nav::Smart(_)) => {
            Some((SortKey::Default, true))
        }
        _ => None,
    };
    let put_in_order = |rows: &mut Vec<Row>, key: SortKey, ascending: bool| {
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
    };
    // No more songs than the search that was typed wants, counted in the order the
    // search itself names ("sort:plays limit:10": the ten most played), whatever
    // order the list is then shown in.
    let counted_first = is_track_list && q.limit.is_some() && q.order.is_some();
    if counted_first && let (Some((key, ascending)), Some(most)) = (q.order, q.limit) {
        put_in_order(&mut rows, key, ascending);
        rows.truncate(most);
    }
    // (Already in it, where the order shown is the search's own.)
    if is_track_list
        && let Some((key, ascending)) = effective_sort
        && !(counted_first && q.order == Some((key, ascending)))
    {
        put_in_order(&mut rows, key, ascending);
    }
    if is_track_list
        && !counted_first
        && let Some(most) = q.limit
    {
        rows.truncate(most);
    }
    rows
}

/// A folder inside the open folder view.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct FolderEntry {
    pub(super) name: String,
    pub(super) path: std::path::PathBuf,
    /// Songs in it, folders inside it included.
    pub(super) songs: usize,
}

/// The folders directly inside `folder` that hold songs, by name; for the empty path,
/// the music folders.
pub(super) fn folder_entries(
    tracks: &[std::sync::Arc<crate::model::Track>],
    folder: &std::path::Path,
    roots: &[std::path::PathBuf],
) -> Vec<FolderEntry> {
    if folder.as_os_str().is_empty() {
        return roots
            .iter()
            .map(|root| FolderEntry {
                name: root.file_name().map_or_else(
                    || root.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                ),
                path: root.clone(),
                songs: tracks.iter().filter(|t| t.path.starts_with(root)).count(),
            })
            .collect();
    }
    let mut counts: HashMap<std::ffi::OsString, usize> = HashMap::new();
    for track in tracks {
        // A sheet's tracks are in the sheet's folder (not in a folder named after it).
        let file = track.file();
        let Ok(rest) = file.strip_prefix(folder) else {
            continue;
        };
        let mut parts = rest.components();
        if let (Some(first), Some(_)) = (parts.next(), parts.next()) {
            *counts.entry(first.as_os_str().to_os_string()).or_default() += 1;
        }
    }
    let mut entries: Vec<FolderEntry> = counts
        .into_iter()
        .map(|(name, songs)| FolderEntry {
            name: name.to_string_lossy().into_owned(),
            path: folder.join(&name),
            songs,
        })
        .collect();
    entries.sort_by_key(|e| e.name.to_lowercase());
    entries
}

/// Title to show for an album. Untagged files have no album name; their folder name is
/// the best available description ("Unknown album" only if even that is missing).
pub(super) fn album_title<'a>(
    album: &'a Album,
    snap: &'a LibrarySnapshot,
    unknown: &'a str,
) -> &'a str {
    if !album.title.is_empty() {
        return &album.title;
    }
    album
        .tracks
        .first()
        .and_then(|&i| snap.tracks.get(i as usize))
        .and_then(|t| t.file().parent())
        .and_then(|folder| folder.file_name())
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(unknown)
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
    cursor: &mut Option<u64>,
    index: usize,
    modifiers: egui::Modifiers,
) {
    let Some(row) = state_rows.get(index) else {
        return;
    };
    *cursor = Some(row.key);
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

/// A cursor movement in a list.
#[derive(Clone, Copy)]
enum Step {
    By(isize),
    First,
    Last,
}

impl OneumApp {
    /// The rows `nav` would show for `query`, without keeping them (the phone's search).
    pub(super) fn rows_for(&mut self, nav: &Nav, query: &str) -> Vec<Row> {
        if *nav == Nav::Genres && self.genre_list_generation != Some(self.lib_generation) {
            self.genre_list = genre_entries(&self.snapshot.tracks);
            self.genre_list_generation = Some(self.lib_generation);
        }
        build_rows(
            &self.snapshot,
            &self.core.library,
            nav,
            query,
            None,
            self.rediscover,
            Lists {
                smart: &self.settings.smart_playlists,
                genre_list: &self.genre_list,
            },
        )
    }

    /// Rebuilds the row list of `nav` when the library, the query or the sort changed.
    pub(super) fn ensure_rows(&mut self, nav: &Nav) {
        let mode = self.rediscover;
        let extra = match nav {
            // The lists change with what is played, and with which of them is shown.
            Nav::Recent => self
                .player
                .current
                .map_or(0, |c| c.item_id)
                .wrapping_mul(4)
                .wrapping_add(mode as u64 + 1),
            // A smart playlist is its search: another search, other rows.
            Nav::Smart(name) => self
                .settings
                .smart_playlists
                .iter()
                .find(|p| &p.name == name)
                .map_or(1, |p| crate::library::fnv1a64(p.query.as_bytes()) | 1),
            _ => 0,
        };
        if *nav == Nav::Genres && self.genre_list_generation != Some(self.lib_generation) {
            self.genre_list = genre_entries(&self.snapshot.tracks);
            self.genre_list_generation = Some(self.lib_generation);
        }
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
        let build_started = std::time::Instant::now();
        let rows = build_rows(
            &self.snapshot,
            &self.core.library,
            nav,
            &state.query,
            state.sort,
            mode,
            Lists {
                smart: &self.settings.smart_playlists,
                genre_list: &self.genre_list,
            },
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
        state.total_ms = if matches!(
            nav,
            Nav::Albums | Nav::Artists | Nav::Artist(_) | Nav::Genres
        ) {
            0
        } else {
            rows.iter()
                .map(|r| self.snapshot.tracks[r.idx as usize].duration_ms)
                .sum()
        };
        state.rows = rows;
        let build_ms = build_started.elapsed().as_secs_f64() * 1000.0;
        self.perf.last_rows_build_ms = build_ms;
        self.perf.max_rows_build_ms = self.perf.max_rows_build_ms.max(build_ms);
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
        // (A playlist can be there without music, and a media server of the network
        // does not need any.)
        if library_empty
            && !matches!(nav, Nav::Playlist(_) | Nav::Network)
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
                let s = self.s();
                self.view_title(ui, s.recently_played, None);
                ui.horizontal(|ui| {
                    ui.add_space(4.0);
                    widgets::segmented(
                        ui,
                        "rediscover",
                        &mut self.rediscover,
                        &[
                            (Rediscover::Played, s.rediscover_played),
                            (Rediscover::Added, s.rediscover_added),
                            (Rediscover::MostPlayed, s.rediscover_most),
                            (Rediscover::Forgotten, s.rediscover_forgotten),
                        ],
                    );
                });
                ui.add_space(6.0);
                self.ensure_rows(&nav);
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
            Nav::Genre(name) => {
                let count = self.views.get(&nav).map_or(0, |state| state.rows.len());
                let summary = self.lang.song_count(count);
                self.view_title(ui, name, Some(&summary));
                self.track_list(ui, &nav, TableOptions::default(), actions);
            }
            Nav::Genres => {
                self.view_title(ui, self.s().genres, None);
                self.genre_list_view(ui, &nav, actions);
            }
            Nav::Stats => self.stats_view(ui, actions),
            Nav::Network => self.network_view(ui, actions),
            Nav::Folder(folder) => {
                self.folder_header(ui, folder, actions);
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
            Nav::Smart(name) => {
                let query = self
                    .settings
                    .smart_playlists
                    .iter()
                    .find(|p| &p.name == name)
                    .map(|p| p.query.clone())
                    .unwrap_or_default();
                self.view_title(ui, name, Some(&query));
                self.track_list(ui, &nav, TableOptions::default(), actions);
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

    pub(super) fn view_title(&self, ui: &mut Ui, title: &str, subtitle: Option<&str>) {
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
            ui.label(
                RichText::new(s.empty_library_title)
                    .font(theme::semibold(22.0))
                    .strong(),
            );
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

    pub(super) fn empty_list_message(&self, ui: &mut Ui, nav: &Nav, has_query: bool) {
        let s = self.s();
        let (icon, title, body) = if has_query {
            (Icon::Search, s.empty_search, s.empty_search_hint)
        } else {
            match nav {
                Nav::Favorites => (Icon::Heart, s.empty_favorites, s.empty_favorites_hint),
                Nav::Recent => match self.rediscover {
                    Rediscover::Played | Rediscover::MostPlayed => (Icon::Note, s.empty_recent, ""),
                    Rediscover::Added => (Icon::Note, s.empty_search, ""),
                    Rediscover::Forgotten => {
                        (Icon::Clock, s.empty_forgotten, s.empty_forgotten_hint)
                    }
                },
                Nav::Playlist(_) => (Icon::Queue, s.empty_playlist, s.empty_playlist_hint),
                Nav::Genres => (Icon::Tag, s.empty_genres, s.empty_genres_hint),
                Nav::Smart(_) => (Icon::Search, s.empty_search, s.smart_hint),
                Nav::Folder(_) => (Icon::Folder, s.empty_search, ""),
                _ => (Icon::Note, s.empty_search, ""),
            }
        };
        widgets::empty_state(ui, icon, title, body);
    }

    /// Arrow keys, Home/End, Page Up/Down, Return, ⌘A and Delete for the song list.
    ///
    /// The keys only belong to the list when nothing else has a claim on them: no text
    /// field (also not one that gives up focus with this very key press), no focused
    /// control, no dialog, and the list — not the queue — was clicked last.
    fn list_keys(
        &self,
        ui: &Ui,
        state: &mut ListState,
        options: TableOptions,
        actions: &mut Vec<Action>,
    ) {
        if self.typing
            || self.widget_focused
            || ui.ctx().text_edit_focused()
            || self.key_pane != KeyPane::List
            || self.dialog.is_some()
            || self.sheet_open()
            || state.rows.is_empty()
        {
            return;
        }
        // Edit ▸ Select All arrives as a key event carrying its own modifiers, so the
        // event is matched rather than the keyboard state.
        let select_all = ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::A));
        let (step, enter, delete, shift) = ui.input(|i| {
            let plain = !i.modifiers.command && !i.modifiers.alt;
            let page = 12isize;
            let step = if !plain {
                None
            } else if i.key_pressed(egui::Key::ArrowDown) {
                Some(Step::By(1))
            } else if i.key_pressed(egui::Key::ArrowUp) {
                Some(Step::By(-1))
            } else if i.key_pressed(egui::Key::PageDown) {
                Some(Step::By(page))
            } else if i.key_pressed(egui::Key::PageUp) {
                Some(Step::By(-page))
            } else if i.key_pressed(egui::Key::Home) {
                Some(Step::First)
            } else if i.key_pressed(egui::Key::End) {
                Some(Step::Last)
            } else {
                None
            };
            (
                step,
                plain && i.key_pressed(egui::Key::Enter),
                plain && (i.key_pressed(egui::Key::Backspace) || i.key_pressed(egui::Key::Delete)),
                i.modifiers.shift,
            )
        });
        if select_all {
            state.selection = state.rows.iter().map(|r| r.key).collect();
        }
        self.typeahead(ui, state);
        let position =
            |rows: &[Row], key: Option<u64>| key.and_then(|k| rows.iter().position(|r| r.key == k));
        if let Some(step) = step {
            let last = state.rows.len() - 1;
            let cursor = position(&state.rows, state.cursor.or(state.anchor));
            let next = match (step, cursor) {
                (Step::First, _) => 0,
                (Step::Last, _) => last,
                (Step::By(n), Some(i)) => i.saturating_add_signed(n).min(last),
                (Step::By(n), None) if n > 0 => 0,
                (Step::By(_), None) => last,
            };
            let key = state.rows[next].key;
            if shift {
                // The range always spans from where the selection started to the cursor,
                // so it shrinks again when the cursor moves back.
                let from = position(&state.rows, state.anchor).unwrap_or(next);
                state.selection = state.rows[from.min(next)..=from.max(next)]
                    .iter()
                    .map(|r| r.key)
                    .collect();
                state.anchor.get_or_insert(key);
            } else {
                state.selection.clear();
                state.selection.insert(key);
                state.anchor = Some(key);
            }
            state.cursor = Some(key);
            state.scroll_to = Some(next);
        }
        if enter && let Some(index) = position(&state.rows, state.cursor.or(state.anchor)) {
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

    /// Typing letters selects the first song whose title starts with them (or, failing
    /// that, contains them). The letters accumulate while typing continues.
    fn typeahead(&self, ui: &Ui, state: &mut ListState) {
        let typed: String = ui.input(|i| {
            if i.modifiers.command || i.modifiers.ctrl || i.modifiers.alt {
                return String::new();
            }
            i.events
                .iter()
                .filter_map(|event| match event {
                    egui::Event::Text(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect()
        });
        let now = std::time::Instant::now();
        let fresh = state
            .typeahead_at
            .is_some_and(|at| now.duration_since(at) < TYPEAHEAD_TIMEOUT);
        if !fresh {
            state.typeahead.clear();
        }
        // A leading space is play/pause, not the start of a title.
        let typed = if state.typeahead.is_empty() {
            typed.trim_start()
        } else {
            typed.as_str()
        };
        if typed.is_empty() {
            return;
        }
        state.typeahead.push_str(typed);
        state.typeahead_at = Some(now);
        ui.ctx().request_repaint_after(TYPEAHEAD_TIMEOUT);
        let needle = state.typeahead.to_lowercase();
        let title_of = |row: &Row| self.snapshot.tracks[row.idx as usize].title.to_lowercase();
        let found = state
            .rows
            .iter()
            .position(|row| title_of(row).starts_with(&needle))
            .or_else(|| {
                state
                    .rows
                    .iter()
                    .position(|row| title_of(row).contains(&needle))
            });
        if let Some(index) = found {
            let key = state.rows[index].key;
            state.selection.clear();
            state.selection.insert(key);
            state.anchor = Some(key);
            state.cursor = Some(key);
            state.scroll_to = Some(index);
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
        let now = unix_now();
        // The optional columns that are switched on, in the order they are shown, and
        // all of them with their switches for the header's menu.
        let columns = this.settings.list_columns;
        let all_columns = [
            (ColumnKind::Year, s.col_year, columns.year),
            (ColumnKind::Genre, s.col_genre, columns.genre),
            (ColumnKind::Plays, s.col_plays, columns.plays),
            (ColumnKind::Rating, s.col_rating, columns.rating),
            (ColumnKind::Added, s.col_added, columns.added),
            (
                ColumnKind::LastPlayed,
                s.col_last_played,
                columns.last_played,
            ),
            (
                ColumnKind::Format,
                s.col_format,
                this.settings.show_format_column,
            ),
        ];
        let column_width = |kind: ColumnKind| match kind {
            ColumnKind::Year | ColumnKind::Plays => 46.0,
            ColumnKind::Genre => 110.0,
            ColumnKind::Rating => 74.0,
            ColumnKind::Added => 72.0,
            ColumnKind::LastPlayed => 84.0,
            ColumnKind::Format => 64.0,
        };
        let mut extras: Vec<ColumnKind> = all_columns
            .iter()
            .filter(|(_, _, on)| *on)
            .map(|(kind, _, _)| *kind)
            .collect();
        let scroll_to = state.scroll_to.take();
        let sort = state.sort;
        // Positions in a filtered playlist are not positions in the playlist.
        let menu_options = TableOptions {
            reorderable: state.query.trim().is_empty(),
            ..options
        };
        let mut row_clicked = false;
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
            cursor,
            ..
        } = &mut state;
        let rows: &[Row] = rows;
        // Column widths are derived from the available width every frame, so the table
        // always fits, also in a small window or next to the side panel.
        let base = 26.0 + 58.0 + if options.track_no { 32.0 } else { 0.0 };
        let spacing = ui.spacing().item_spacing.x;
        let room_for = |extras: &[ColumnKind]| {
            let column_count =
                4 + usize::from(options.track_no) + usize::from(!options.hide_album) + extras.len();
            let fixed = base + extras.iter().map(|k| column_width(*k)).sum::<f32>();
            let gaps = spacing * column_count as f32 + ui.spacing().scroll.bar_width + 6.0;
            ui.available_width() - fixed - gaps
        };
        // In a narrow window the optional columns give way, last one first, before the
        // title, artist and album become too narrow to read.
        while !extras.is_empty() && room_for(&extras) < 320.0 {
            extras.pop();
        }
        let flexible = room_for(&extras).max(180.0);
        let (title_w, artist_w, album_w) = if options.hide_album {
            (flexible * 0.62, flexible * 0.38, 0.0)
        } else {
            (flexible * 0.44, flexible * 0.26, flexible * 0.30)
        };
        let mut table = TableBuilder::new(ui)
            .id_salt(("tracks", nav.clone()))
            .striped(true)
            // Clickable, but not a tab stop per cell: a list of a few thousand cells
            // would swallow the Tab key. Rows are reached with the arrow keys instead.
            // Rows can be dragged to a playlist or the queue, and within a playlist.
            .sense(Sense::CLICK | Sense::DRAG)
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
        for kind in &extras {
            table = table.column(Column::exact(column_width(*kind)).clip(true));
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
                let response = ui.add(egui::Label::new(label).sense(Sense::click()).truncate());
                if let Some(key) = key.filter(|_| sortable)
                    && response.clicked()
                {
                    actions.push(Action::Sort(key));
                }
                // Right-click on any column title: which optional columns to show.
                egui::Popup::context_menu(&response)
                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                    .show(|ui| {
                        ui.label(RichText::new(s.columns_menu).small().color(dim));
                        for (kind, name, on) in all_columns {
                            let mut shown = on;
                            // The menu stays open: several columns can be chosen in one go.
                            if ui.checkbox(&mut shown, name).changed() {
                                actions.push(Action::ToggleColumn(kind));
                            }
                        }
                    });
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
                for kind in &extras {
                    let (name, key) = match kind {
                        ColumnKind::Year => (s.col_year, Some(SortKey::Year)),
                        ColumnKind::Genre => (s.col_genre, None),
                        ColumnKind::Plays => (s.col_plays, Some(SortKey::PlayCount)),
                        ColumnKind::Rating => (s.col_rating, Some(SortKey::Rating)),
                        ColumnKind::Added => (s.col_added, Some(SortKey::DateAdded)),
                        ColumnKind::LastPlayed => (s.col_last_played, Some(SortKey::LastPlayed)),
                        ColumnKind::Format => (s.col_format, Some(SortKey::Format)),
                    };
                    header.col(|ui| header_cell(ui, name, key, actions));
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
                    for kind in &extras {
                        row.col(|ui| {
                            if *kind == ColumnKind::Rating {
                                // Shown, not set, in the list: a click here is for the row.
                                if track.rating > 0 {
                                    widgets::stars(ui, track.rating, 11.0, None);
                                }
                                return;
                            }
                            let text = match kind {
                                ColumnKind::Year => {
                                    track.year.map(|y| y.to_string()).unwrap_or_default()
                                }
                                ColumnKind::Genre => track.genre.clone(),
                                ColumnKind::Plays => match track.play_count {
                                    0 => String::new(),
                                    n => n.to_string(),
                                },
                                ColumnKind::Rating => String::new(),
                                ColumnKind::Added => this.lang.days_ago(now - track.added_at),
                                ColumnKind::LastPlayed => track.last_played_at.map_or_else(
                                    || s.never.to_string(),
                                    |at| this.lang.days_ago(now - at),
                                ),
                                ColumnKind::Format => track.codec.clone(),
                            };
                            widgets::truncated(ui, RichText::new(text).small().color(dim));
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
                        click_select(rows, selection, anchor, cursor, index, modifiers);
                    }
                    if response.clicked() || response.secondary_clicked() {
                        row_clicked = true;
                    }
                    if response.drag_started() {
                        // Dragging a row that is not selected drags just that row.
                        if !selection.contains(&key) {
                            selection.clear();
                            selection.insert(key);
                            *anchor = Some(key);
                            *cursor = Some(key);
                        }
                        let playlist_items = options.playlist.map(|id| {
                            let items = rows
                                .iter()
                                .filter(|r| selection.contains(&r.key))
                                .map(|r| r.key as i64)
                                .collect();
                            (id, items)
                        });
                        egui::DragAndDrop::set_payload(
                            &response.ctx,
                            DragTracks {
                                tracks: selected_tracks(&snap, rows, selection),
                                playlist_items,
                            },
                        );
                        row_clicked = true;
                    }
                    // Dropping rows of this playlist between two of its rows reorders it.
                    if let Some(playlist) = options.playlist.filter(|_| menu_options.reorderable)
                        && let Some(payload) = response.dnd_hover_payload::<DragTracks>()
                        && let Some((from, items)) = &payload.playlist_items
                        && *from == playlist
                        && let Some(pointer) = response.ctx.pointer_interact_pos()
                    {
                        let rect = response.rect;
                        let after = pointer.y > rect.center().y;
                        let y = if after { rect.bottom() } else { rect.top() };
                        response.ctx.layer_painter(response.layer_id).hline(
                            rect.x_range(),
                            y,
                            egui::Stroke::new(2.0, accent),
                        );
                        if response.ctx.input(|i| i.pointer.any_released()) {
                            // Index in the list with the moved items taken out.
                            let target = (index + usize::from(after)).min(rows.len());
                            let removed_before = rows[..target]
                                .iter()
                                .filter(|r| items.contains(&(r.key as i64)))
                                .count();
                            actions.push(Action::MovePlaylistItems(
                                playlist,
                                items.clone(),
                                target - removed_before,
                            ));
                            egui::DragAndDrop::clear_payload(&response.ctx);
                        }
                    }
                    if response.secondary_clicked() && !selection.contains(&key) {
                        selection.clear();
                        selection.insert(key);
                        *anchor = Some(key);
                        *cursor = Some(key);
                    }
                    response.context_menu(|ui| {
                        this.track_menu(ui, rows, selection, index, menu_options, actions);
                    });
                });
            });
        if row_clicked {
            // The list has the keys now, and the info panel follows the selection again.
            self.key_pane = KeyPane::List;
            self.info_track = None;
        }
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
        let reorderable = options.reorderable;
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
        // Stars, for the songs that are chosen; the one the menu was opened on says
        // what they have.
        ui.menu_button(s.col_rating, |ui| {
            for stars in [5, 4, 3, 2, 1, 0] {
                let has = clicked_track.rating == stars;
                if ui.radio(has, self.rating_words(stars)).clicked() {
                    actions.push(Action::SetRating(tracks.clone(), stars));
                    ui.close();
                }
            }
        });
        ui.separator();
        // Where this song belongs: its album, and its artist as the artist view groups it.
        if ui.button(s.go_to_album).clicked() {
            actions.push(Action::Go(Nav::Album(clicked_track.album_key)));
            ui.close();
        }
        let grouped_under = if clicked_track.album_artist.is_empty() {
            &clicked_track.artist
        } else {
            &clicked_track.album_artist
        };
        let artist = snap
            .artists()
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(grouped_under))
            .or_else(|| {
                snap.artists()
                    .iter()
                    .find(|a| a.name.eq_ignore_ascii_case(&clicked_track.artist))
            })
            .map(|a| a.name.clone());
        if ui
            .add_enabled(artist.is_some(), egui::Button::new(s.go_to_artist))
            .clicked()
        {
            if let Some(name) = artist {
                actions.push(Action::Go(Nav::Artist(name)));
            }
            ui.close();
        }
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
                .add_enabled(reorderable && first > 0, egui::Button::new(s.move_up))
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
                    reorderable && first + items.len() < rows.len(),
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
        // Songs of the library, wherever their files are: one inside a music folder
        // stays out when the folder is scanned again (`Library::remove_tracks`). (This
        // was offered for individually opened files only, while such a song came back
        // with the next scan; the phone's sheet offered it for all, and they did.)
        let removable: Vec<TrackId> = tracks
            .iter()
            .copied()
            .filter(|id| snap.get(*id).is_some())
            .collect();
        if !removable.is_empty() && removable.len() == tracks.len() {
            ui.separator();
            if ui.button(s.remove_from_library).clicked() {
                actions.push(Action::RemoveFromLibrary(removable));
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
        let pinned = self.live_pins();
        let pinned = pinned.as_slice();
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
                                Sense::click_and_drag(),
                            );
                            let title = album_title(album, &snap, s.unknown_album);
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
                            widgets::truncated(
                                &mut text_ui,
                                RichText::new(title).font(theme::semibold(13.0)).strong(),
                            );
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
                            // A play button on the cover while pointing at the album:
                            // one click plays it, a click anywhere else opens it.
                            let play_rect = egui::Rect::from_min_size(
                                art_rect.right_bottom() - egui::vec2(40.0, 40.0),
                                egui::Vec2::splat(32.0),
                            );
                            let pointing = response.hovered() || response.has_focus();
                            let over_play = pointing
                                && ui
                                    .ctx()
                                    .pointer_hover_pos()
                                    .is_some_and(|p| play_rect.contains(p));
                            if pointing {
                                let accent = theme::accent(ui.visuals().dark_mode);
                                let fill = if over_play {
                                    accent
                                } else {
                                    egui::Color32::from_black_alpha(150)
                                };
                                ui.painter().circle_filled(play_rect.center(), 16.0, fill);
                                super::icons::paint(
                                    ui.painter(),
                                    play_rect.shrink(9.0),
                                    Icon::Play,
                                    egui::Color32::WHITE,
                                );
                            }
                            // An album can be dragged to a playlist or the queue as well.
                            if response.drag_started() {
                                egui::DragAndDrop::set_payload(
                                    ui.ctx(),
                                    DragTracks {
                                        tracks: album_tracks(),
                                        playlist_items: None,
                                    },
                                );
                            }
                            if response.clicked() {
                                if over_play {
                                    actions.push(Action::PlayTracks(album_tracks(), 0));
                                } else {
                                    actions.push(Action::Go(Nav::Album(album.key)));
                                }
                            }
                            if over_play {
                                response.clone().on_hover_text(s.play_album_hint);
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
                                ui.separator();
                                super::pin_menu_item(
                                    ui,
                                    s,
                                    pinned,
                                    crate::app::settings::Pin::Album(album.key),
                                    actions,
                                );
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
                let title = album_title(album, &snap, s.unknown_album);
                ui.add(egui::Label::new(RichText::new(title).heading().strong()).truncate());
                let artist = if album.artist.is_empty() {
                    s.unknown_artist
                } else {
                    album.artist.as_str()
                };
                // "Various Artists" and the like have no artist page to go to.
                let has_page = snap.artists().iter().any(|a| a.name == album.artist);
                if !has_page {
                    ui.label(RichText::new(artist).size(15.0));
                } else if ui.link(RichText::new(artist).size(15.0)).clicked() {
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
            ..Default::default()
        };
        self.track_list(ui, nav, options, actions);
    }

    /// The path of the open folder (each part leads there) and the folders inside it.
    fn folder_header(&mut self, ui: &mut Ui, folder: &std::path::Path, actions: &mut Vec<Action>) {
        let s = self.s();
        let generation = self.lib_generation;
        let fresh = self
            .folder_children
            .as_ref()
            .is_some_and(|(f, g, _)| f == folder && *g == generation);
        if !fresh {
            let roots: Vec<std::path::PathBuf> = self
                .core
                .library
                .roots()
                .into_iter()
                .map(|r| r.path)
                .collect();
            let entries = folder_entries(&self.snapshot.tracks, folder, &roots);
            self.folder_children = Some((folder.to_path_buf(), generation, entries));
        }
        let entries = self
            .folder_children
            .as_ref()
            .map(|(_, _, e)| e.clone())
            .unwrap_or_default();
        let count = self
            .views
            .get(&Nav::Folder(folder.to_path_buf()))
            .map_or(0, |v| v.rows.len());
        // The path: "Folders › Music › Artist › Album", every part a way back.
        let roots: Vec<std::path::PathBuf> = self
            .core
            .library
            .roots()
            .into_iter()
            .map(|r| r.path)
            .collect();
        let root = roots.iter().find(|r| folder.starts_with(r)).cloned();
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.add_space(4.0);
            let top = ui.add(
                egui::Label::new(RichText::new(s.folders).heading().strong()).sense(Sense::click()),
            );
            if top.clicked() {
                actions.push(Action::Go(Nav::Folder(std::path::PathBuf::new())));
            }
            if let Some(root) = &root {
                let mut at = root.clone();
                let mut parts = vec![(
                    root.file_name().map_or_else(
                        || root.display().to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    ),
                    root.clone(),
                )];
                if let Ok(rest) = folder.strip_prefix(root) {
                    for part in rest.components() {
                        at = at.join(part);
                        parts.push((part.as_os_str().to_string_lossy().into_owned(), at.clone()));
                    }
                }
                for (name, path) in parts {
                    ui.label(RichText::new("›").heading().color(theme::dim(ui)));
                    let label = ui.add(
                        egui::Label::new(RichText::new(name).heading().strong())
                            .sense(Sense::click()),
                    );
                    if label.clicked() && path != folder {
                        actions.push(Action::Go(Nav::Folder(path)));
                    }
                }
            }
            ui.label(RichText::new(self.lang.song_count(count)).color(theme::dim(ui)));
        });
        if !entries.is_empty() {
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .id_salt(("subfolders", folder.to_path_buf()))
                .max_height(96.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.add_space(4.0);
                        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                        for entry in &entries {
                            let text = format!(
                                "{}  {}",
                                entry.name,
                                crate::i18n::group_digits(entry.songs)
                            );
                            let chip = ui
                                .add(
                                    egui::Button::new(RichText::new(text).size(12.5))
                                        .corner_radius(12.0)
                                        .min_size(egui::vec2(0.0, 24.0)),
                                )
                                .on_hover_text(s.subfolders);
                            if chip.clicked() {
                                actions.push(Action::Go(Nav::Folder(entry.path.clone())));
                            }
                        }
                    });
                });
        }
        ui.add_space(6.0);
    }

    fn genre_list_view(&mut self, ui: &mut Ui, nav: &Nav, actions: &mut Vec<Action>) {
        let state = self.views.remove(nav).unwrap_or_default();
        if state.rows.is_empty() {
            self.empty_list_message(ui, nav, !state.query.is_empty());
            self.views.insert(nav.clone(), state);
            return;
        }
        let s = self.s();
        let dim = theme::dim(ui);
        let lang = self.lang;
        let list = &self.genre_list;
        let pinned = &self.live_pins();
        TableBuilder::new(ui)
            .id_salt("genres")
            .striped(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .min_scrolled_height(0.0)
            .column(Column::remainder().at_least(160.0).clip(true))
            .column(Column::exact(120.0))
            .header(22.0, |mut header| {
                header.col(|ui| {
                    ui.label(RichText::new(s.col_genre).small().strong().color(dim));
                });
                header.col(|_| {});
            })
            .body(|body| {
                body.rows(theme::ROW_HEIGHT + 6.0, state.rows.len(), |mut row| {
                    let Some(genre) = list.get(state.rows[row.index()].idx as usize) else {
                        return;
                    };
                    row.col(|ui| {
                        ui.add_space(6.0);
                        let (mark, _) =
                            ui.allocate_exact_size(egui::Vec2::splat(18.0), Sense::hover());
                        super::icons::paint(
                            ui.painter(),
                            mark,
                            Icon::Tag,
                            theme::accent(ui.visuals().dark_mode),
                        );
                        widgets::truncated(
                            ui,
                            RichText::new(&genre.name)
                                .font(theme::semibold(13.0))
                                .strong(),
                        );
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(lang.song_count(genre.songs)).color(dim));
                    });
                    if row.response().clicked() {
                        actions.push(Action::Go(Nav::Genre(genre.name.clone())));
                    }
                    row.response().context_menu(|ui| {
                        super::pin_menu_item(
                            ui,
                            s,
                            pinned,
                            crate::app::settings::Pin::Genre(genre.name.clone()),
                            actions,
                        );
                    });
                });
            });
        self.views.insert(nav.clone(), state);
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
        let pinned = &self.live_pins();
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
                body.rows(theme::ROW_HEIGHT + 6.0, state.rows.len(), |mut row| {
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
                        // A small monogram disc in place of an artist picture.
                        let (disc, _) =
                            ui.allocate_exact_size(egui::Vec2::splat(22.0), Sense::hover());
                        if ui.is_rect_visible(disc) {
                            let accent = theme::accent(ui.visuals().dark_mode);
                            ui.painter().circle_filled(
                                disc.center(),
                                11.0,
                                accent.gamma_multiply(0.22),
                            );
                            if let Some(initial) = name.chars().find(|c| c.is_alphanumeric()) {
                                ui.painter().text(
                                    disc.center(),
                                    egui::Align2::CENTER_CENTER,
                                    initial.to_uppercase().to_string(),
                                    theme::semibold(11.5),
                                    accent,
                                );
                            }
                        }
                        widgets::truncated(
                            ui,
                            RichText::new(name).font(theme::semibold(13.0)).strong(),
                        );
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
                    row.response().context_menu(|ui| {
                        super::pin_menu_item(
                            ui,
                            s,
                            pinned,
                            crate::app::settings::Pin::Artist(artist.name.clone()),
                            actions,
                        );
                    });
                });
            });
        self.views.insert(nav.clone(), state);
    }
}

#[cfg(test)]
mod tests {
    use super::{Rediscover, folder_entries, genre_entries, rediscover};
    use crate::model::{Track, test_track};
    use std::sync::Arc;

    const DAY: i64 = 86_400;
    const NOW: i64 = 2_000_000_000;

    fn library() -> Vec<Arc<Track>> {
        // (id, added days ago, plays, last played days ago)
        [
            (1, 400, 12, Some(200)),
            (2, 400, 3, Some(2)),
            (3, 10, 0, None),
            (4, 1, 1, Some(45)),
            (5, 1, 30, Some(0)),
            (6, 90, 5, Some(31)),
        ]
        .into_iter()
        .map(|(id, added, plays, last)| {
            Arc::new(Track {
                added_at: NOW - added * DAY,
                play_count: plays,
                last_played_at: last.map(|days: i64| NOW - days * DAY),
                ..test_track(id)
            })
        })
        .collect()
    }

    fn ids(tracks: &[Arc<Track>], mode: Rediscover) -> Vec<i64> {
        rediscover(tracks, mode, NOW)
            .into_iter()
            .map(|i| tracks[i as usize].id)
            .collect()
    }

    #[test]
    fn the_listen_again_lists() {
        let tracks = library();
        // Newest first; added together: in the order they were found.
        assert_eq!(ids(&tracks, Rediscover::Added), [4, 5, 3, 6, 1, 2]);
        // Most plays first; never played songs are not in it.
        assert_eq!(ids(&tracks, Rediscover::MostPlayed), [5, 1, 6, 2, 4]);
        // Played before, but not for 30 days: longest ago first.
        assert_eq!(ids(&tracks, Rediscover::Forgotten), [1, 4, 6]);
        // "Recently played" comes from the play history, not from here.
        assert!(ids(&tracks, Rediscover::Played).is_empty());
        assert!(ids(&[], Rediscover::Added).is_empty());
    }

    #[test]
    fn the_genres_of_a_library() {
        let with = |id, genre: &str| {
            Arc::new(Track {
                genre: genre.into(),
                ..test_track(id)
            })
        };
        let tracks = vec![
            with(1, "Rock"),
            with(2, "rock; Indie"),
            with(3, "Jazz / jazz"),
            with(4, ""),
            with(5, "가요"),
            with(6, "ROCK"),
        ];
        let list = genre_entries(&tracks);
        let shown: Vec<(&str, usize)> = list.iter().map(|g| (g.name.as_str(), g.songs)).collect();
        // By name; spellings that differ in case are one genre, as first written; a song
        // that names a genre twice counts once; songs without a genre are not listed.
        assert_eq!(shown, [("Indie", 1), ("Jazz", 1), ("Rock", 3), ("가요", 1)]);
        assert!(genre_entries(&[]).is_empty());
    }

    #[test]
    fn the_genres_of_a_tag() {
        let of = |tag: &'static str| super::genres(tag).collect::<Vec<_>>();
        assert_eq!(
            of("Rock; Indie / Alternative"),
            ["Rock", "Indie", "Alternative"]
        );
        assert_eq!(of("K-Pop"), ["K-Pop"]);
        assert_eq!(of(" Jazz ,, Fusion| "), ["Jazz", "Fusion"]);
        assert!(of("").is_empty() && of(" ; / ").is_empty());
    }

    #[test]
    fn a_list_is_never_longer_than_its_limit() {
        let many: Vec<Arc<Track>> = (1..=1000).map(|id| Arc::new(test_track(id))).collect();
        assert_eq!(
            rediscover(&many, Rediscover::Added, NOW).len(),
            super::REDISCOVER_LIMIT
        );
    }

    #[test]
    fn the_folders_inside_a_folder() {
        use std::path::{Path, PathBuf};
        let track = |path: &str| {
            let mut t = test_track(1);
            t.path = PathBuf::from(path);
            std::sync::Arc::new(t)
        };
        let tracks = vec![
            track("/m/Beatles/Abbey Road/1.flac"),
            track("/m/Beatles/Abbey Road/2.flac"),
            track("/m/Beatles/loose.flac"),
            track("/m/아이유/Palette/1.flac"),
            track("/m/top.flac"),
            track("/elsewhere/a.flac"),
            // The tracks of a CUE sheet are in the sheet's folder.
            track("/m/Live/concert.cue/01"),
            track("/m/Live/concert.cue/02"),
        ];
        let names = |folder: &str| -> Vec<(String, usize)> {
            folder_entries(&tracks, Path::new(folder), &[])
                .into_iter()
                .map(|e| (e.name, e.songs))
                .collect()
        };
        assert_eq!(
            names("/m"),
            [
                ("Beatles".into(), 3),
                ("Live".into(), 2),
                ("아이유".into(), 1)
            ]
        );
        assert!(names("/m/Live").is_empty());
        assert_eq!(names("/m/Beatles"), [("Abbey Road".into(), 2)]);
        assert!(names("/m/Beatles/Abbey Road").is_empty());
        // The top: the music folders, with every song under them.
        let top = folder_entries(&tracks, Path::new(""), &[PathBuf::from("/m")]);
        assert_eq!(top.len(), 1);
        assert_eq!((top[0].name.as_str(), top[0].songs), ("m", 7));
    }
}
