//! The editor of a smart playlist: its search set with buttons and a few fields
//! instead of typed (`library::smart` reads a search into the conditions shown here
//! and writes them back). The same window on a desk and on a phone; on a phone it
//! stands in the upper part of the screen and scrolls above the keyboard.
use super::{Nav, OneumApp, theme};
use crate::app::settings::{
    MAX_SMART_NAME_CHARS, MAX_SMART_PLAYLISTS, MAX_SMART_QUERY_CHARS, Pin, SmartPlaylist, tidy_text,
};
use crate::i18n::Lang;
use crate::library::SortKey;
use crate::library::smart::{ORDERS, Plays, Rules, Stars};
use eframe::egui::{self, RichText};

/// How many of the library's genres are offered to be chosen with a tap.
const GENRES_OFFERED: usize = 8;

/// The editor while it is open.
pub(super) struct SmartEditor {
    name: String,
    /// The smart playlist that is being changed, by its name; `None`: a new one.
    original: Option<String>,
    rules: Rules,
    /// The years as they stand in their fields.
    year_from: String,
    year_to: String,
    /// The name's field takes the keyboard when the editor opens (a new playlist on a
    /// desk; on a phone the keyboard would cover half of what there is to choose).
    focus: bool,
    /// The genres of the library with the most songs.
    genres: Vec<String>,
    /// How many songs a search finds: the search and the count.
    counted: Option<(String, usize)>,
    /// Where what can be pressed was drawn last, by a name (for verification).
    shown: Vec<(String, egui::Rect)>,
}

impl SmartEditor {
    /// The search the editor's conditions make.
    fn query(&self) -> String {
        tidy_text(&self.rules.query(), MAX_SMART_QUERY_CHARS)
    }

    /// What a dump of the app's state tells of the open editor.
    pub(super) fn state(&self) -> serde_json::Value {
        let shown: serde_json::Map<String, serde_json::Value> = self
            .shown
            .iter()
            .map(|(name, rect)| {
                (
                    name.clone(),
                    serde_json::json!([rect.left(), rect.top(), rect.width(), rect.height()]),
                )
            })
            .collect();
        serde_json::json!({
            "name": self.name,
            "original": self.original,
            "query": self.query(),
            "songs": self.counted.as_ref().map(|(_, songs)| *songs),
            "shown": shown,
        })
    }
}

/// What a row of choices says for a number of days.
fn days_label(lang: Lang, days: u32) -> String {
    match (lang, days) {
        (Lang::Ko, 365) => "1년".into(),
        (Lang::Ko, days) => format!("{days}일"),
        (Lang::En, 365) => "1 year".into(),
        (Lang::En, 1) => "1 day".into(),
        (Lang::En, days) => format!("{days} days"),
    }
}

/// A row of choices of which one holds: a button each, the one that holds filled.
/// Says whether another was chosen.
fn choices<T: PartialEq + Clone>(
    ui: &mut egui::Ui,
    touch: bool,
    key: &str,
    shown: &mut Vec<(String, egui::Rect)>,
    current: &mut T,
    options: &[(String, T)],
) -> bool {
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        for (nth, (label, value)) in options.iter().enumerate() {
            let selected = current == value;
            let button = egui::Button::selectable(selected, label.as_str())
                .min_size(egui::vec2(0.0, if touch { 36.0 } else { 24.0 }));
            let response = ui.add(button);
            shown.push((format!("{key}:{nth}"), response.rect));
            if response.clicked() && !selected {
                *current = value.clone();
                changed = true;
            }
        }
    });
    changed
}

impl OneumApp {
    /// Opens the editor: for the smart playlist of that name, or for a new one.
    pub(super) fn open_smart_editor(&mut self, original: Option<&str>) {
        let existing = original.and_then(|name| {
            self.settings
                .smart_playlists
                .iter()
                .find(|smart| smart.name == name)
                .cloned()
        });
        let rules = Rules::of(existing.as_ref().map_or("", |smart| smart.query.as_str()));
        let year = |year: Option<u16>| year.map(|year| year.to_string()).unwrap_or_default();
        let mut genres = super::lists::genre_entries(&self.snapshot.tracks);
        // (Stable: those with as many songs stay in the order of their names.)
        genres.sort_by_key(|genre| std::cmp::Reverse(genre.songs));
        self.smart_editor = Some(SmartEditor {
            name: existing
                .as_ref()
                .map(|smart| smart.name.clone())
                .unwrap_or_default(),
            original: existing.map(|smart| smart.name),
            year_from: year(rules.year_from),
            year_to: year(rules.year_to),
            rules,
            focus: original.is_none() && !self.phone_mode,
            genres: genres
                .into_iter()
                .take(GENRES_OFFERED)
                .map(|genre| genre.name)
                .collect(),
            counted: None,
            shown: Vec::new(),
        });
    }

    /// Whether the editor is open.
    pub(super) fn smart_editor_open(&self) -> bool {
        self.smart_editor.is_some()
    }

    /// What a dump of the app's state tells of the editor (`null`: not open).
    pub(super) fn smart_editor_state(&self) -> serde_json::Value {
        self.smart_editor
            .as_ref()
            .map_or(serde_json::Value::Null, SmartEditor::state)
    }

    /// Keeps a smart playlist as the editor leaves it. One that was changed keeps its
    /// place in the list (and its pin, under a new name too); another one of the name
    /// it is given gives way, as when a smart playlist is renamed.
    fn save_smart(&mut self, name: &str, query: &str, original: Option<String>) {
        let name = tidy_text(name, MAX_SMART_NAME_CHARS);
        let query = tidy_text(query, MAX_SMART_QUERY_CHARS);
        if name.is_empty() || query.is_empty() {
            return;
        }
        let list = &mut self.settings.smart_playlists;
        match original.filter(|old| list.iter().any(|smart| &smart.name == old)) {
            Some(old) => {
                list.retain(|smart| smart.name != name || smart.name == old);
                if let Some(smart) = list.iter_mut().find(|smart| smart.name == old) {
                    smart.name = name.clone();
                    smart.query = query;
                }
                if old != name {
                    super::repoint_pin(
                        &mut self.settings.pinned,
                        &Pin::Smart(old.clone()),
                        Pin::Smart(name.clone()),
                    );
                    self.views.remove(&Nav::Smart(old));
                }
            }
            None => {
                list.retain(|smart| smart.name != name);
                if list.len() < MAX_SMART_PLAYLISTS {
                    list.push(SmartPlaylist {
                        name: name.clone(),
                        query,
                    });
                }
            }
        }
        if self.phone_mode {
            self.phone_open(Nav::Smart(name));
        } else {
            self.go(Nav::Smart(name));
        }
    }

    /// Draws the editor, if it is open.
    pub(super) fn smart_editor_window(&mut self, ctx: &egui::Context) {
        let Some(mut editor) = self.smart_editor.take() else {
            return;
        };
        let s = self.s();
        let lang = self.lang;
        let touch = self.phone_mode;
        let modal = self.modal(ctx, egui::Id::unique("oneum-smart-editor"));
        let width = 460.0_f32.min(ctx.content_rect().width() - 56.0);
        // What is above and below the conditions (the title, the search and the
        // buttons) always shows; the conditions scroll where they do not fit: in a
        // low window, and on a phone above its keyboard.
        let around = if touch { 260.0 } else { 150.0 };
        let room = if touch {
            let gap = super::dialogs::phone_dialog_gap(ctx);
            super::dialogs::phone_dialog_room(ctx, gap) - around
        } else {
            ctx.content_rect().height() - around - 100.0
        }
        .max(96.0);
        let (mut cancel, mut save) = (false, false);
        editor.shown.clear();
        let dim = |ui: &egui::Ui| theme::dim(ui);
        let shown = modal.show(ctx, |ui| {
            ui.set_width(width);
            ui.label(
                RichText::new(if editor.original.is_some() {
                    s.smart_playlist
                } else {
                    s.smart_new
                })
                .font(theme::semibold(if touch { 18.0 } else { 16.0 }))
                .strong(),
            );
            ui.add_space(8.0);
            // A field that is typed into: its rectangle is noted, and it is brought
            // into what shows when it takes the keyboard.
            let field = |ui: &mut egui::Ui,
                         shown: &mut Vec<(String, egui::Rect)>,
                         text: &mut String,
                         hint: &str,
                         key: &str,
                         width: f32| {
                let mut edit = egui::TextEdit::singleline(text)
                    .hint_text(hint)
                    .desired_width(width)
                    .id(egui::Id::unique(("oneum-smart", key.to_string())));
                if touch {
                    edit = edit
                        .font(egui::FontId::proportional(17.0))
                        .margin(egui::Margin::symmetric(10, 9));
                }
                let response = ui.add(edit);
                response
                    .widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, hint));
                if response.gained_focus() {
                    response.scroll_to_me(Some(egui::Align::Center));
                }
                shown.push((key.to_string(), response.rect));
                response
            };
            let name = field(
                ui,
                &mut editor.shown,
                &mut editor.name,
                s.smart_name,
                "name",
                f32::INFINITY,
            );
            if editor.focus {
                editor.focus = false;
                name.request_focus();
            }
            ui.add_space(8.0);
            let heading = |ui: &mut egui::Ui, text: &str| {
                ui.add_space(8.0);
                ui.label(RichText::new(text).small().strong().color(dim(ui)));
                ui.add_space(2.0);
            };
            egui::ScrollArea::vertical()
                .id_salt("oneum-smart-conditions")
                .max_height(room)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    let rules = &mut editor.rules;
                    let shown = &mut editor.shown;
                    ui.label(RichText::new(s.smart_words).small().strong().color(dim(ui)));
                    ui.add_space(2.0);
                    field(
                        ui,
                        shown,
                        &mut rules.words,
                        s.smart_words_hint,
                        "words",
                        f32::INFINITY,
                    );
                    heading(ui, s.col_artist);
                    field(
                        ui,
                        shown,
                        &mut rules.artist,
                        s.smart_text_hint,
                        "artist",
                        f32::INFINITY,
                    );
                    heading(ui, s.col_genre);
                    field(
                        ui,
                        shown,
                        &mut rules.genre,
                        s.smart_text_hint,
                        "genre",
                        f32::INFINITY,
                    );
                    if !editor.genres.is_empty() {
                        // The library's own genres, to be taken with a tap; the one
                        // that stands in the field is the one that holds.
                        ui.add_space(4.0);
                        let options: Vec<(String, String)> = editor
                            .genres
                            .iter()
                            .map(|genre| (genre.clone(), genre.clone()))
                            .collect();
                        let mut current = rules.genre.trim().to_string();
                        if choices(ui, touch, "genre_pick", shown, &mut current, &options) {
                            rules.genre = current;
                        }
                    }
                    heading(ui, s.col_year);
                    ui.horizontal(|ui| {
                        let width = if touch { 92.0 } else { 64.0 };
                        for (text, hint, key) in [
                            (&mut editor.year_from, s.smart_year_from, "year_from"),
                            (&mut editor.year_to, s.smart_year_to, "year_to"),
                        ] {
                            field(ui, shown, text, hint, key, width);
                            // (Digits, four at most: a year.)
                            text.retain(|c| c.is_ascii_digit());
                            text.truncate(4);
                            if key == "year_from" {
                                ui.label("–");
                            }
                        }
                    });
                    rules.year_from = editor.year_from.parse().ok();
                    rules.year_to = editor.year_to.parse().ok();

                    heading(ui, s.col_rating);
                    let mut stars = vec![(s.smart_any.to_string(), Stars::Any)];
                    stars.extend((1..=5u8).map(|least| {
                        let label = if least == 5 {
                            "★5".to_string()
                        } else {
                            lang.at_least(&format!("★{least}"))
                        };
                        (label, Stars::AtLeast(least))
                    }));
                    stars.push((s.rating_none.to_string(), Stars::Unrated));
                    choices(ui, touch, "stars", shown, &mut rules.stars, &stars);

                    heading(ui, s.favorites);
                    let favorite = [
                        (s.smart_any.to_string(), None),
                        (s.smart_fav_yes.to_string(), Some(true)),
                        (s.smart_fav_no.to_string(), Some(false)),
                    ];
                    choices(ui, touch, "favorite", shown, &mut rules.favorite, &favorite);

                    heading(ui, s.smart_plays);
                    let mut plays = vec![
                        (s.smart_any.to_string(), Plays::Any),
                        (s.smart_never_played.to_string(), Plays::Never),
                    ];
                    let mut counts = vec![1u32, 5, 10];
                    if let Plays::AtLeast(least) = rules.plays
                        && !counts.contains(&least)
                    {
                        counts.push(least);
                        counts.sort_unstable();
                    }
                    plays.extend(
                        counts
                            .into_iter()
                            .map(|least| (lang.plays_at_least(least), Plays::AtLeast(least))),
                    );
                    choices(ui, touch, "plays", shown, &mut rules.plays, &plays);

                    for (title, key, days) in [
                        (s.smart_added, "added", &mut rules.added_days),
                        (s.smart_played, "played", &mut rules.played_days),
                    ] {
                        heading(ui, title);
                        let mut spans = vec![7u32, 30, 90, 365];
                        if let Some(own) = days.filter(|own| !spans.contains(own)) {
                            spans.push(own);
                            spans.sort_unstable();
                        }
                        let mut options = vec![(s.smart_any.to_string(), None)];
                        options.extend(
                            spans
                                .into_iter()
                                .map(|span| (days_label(lang, span), Some(span))),
                        );
                        choices(ui, touch, key, shown, days, &options);
                    }

                    heading(ui, s.smart_order);
                    let mut key: Option<SortKey> = rules.order.map(|(key, _)| key);
                    let mut options = vec![(s.smart_order_default.to_string(), None)];
                    options.extend(ORDERS.iter().map(|(order, _)| {
                        let label = match order {
                            SortKey::Title => s.col_title,
                            SortKey::Artist => s.col_artist,
                            SortKey::Album => s.col_album,
                            SortKey::Year => s.col_year,
                            SortKey::Duration => s.col_duration,
                            SortKey::PlayCount => s.col_plays,
                            SortKey::DateAdded => s.col_added,
                            SortKey::LastPlayed => s.col_last_played,
                            SortKey::Rating => s.col_rating,
                            _ => s.col_format,
                        };
                        (label.to_string(), Some(*order))
                    }));
                    if choices(ui, touch, "order", shown, &mut key, &options) {
                        rules.order = key.map(|key| (key, false));
                    }
                    if let Some((_, turned)) = rules.order.as_mut() {
                        ui.add_space(4.0);
                        // (A button like the choices above it: as easy to hit.)
                        let button = egui::Button::selectable(*turned, s.smart_reversed)
                            .min_size(egui::vec2(0.0, if touch { 36.0 } else { 24.0 }));
                        let response = ui.add(button);
                        shown.push(("reversed".into(), response.rect));
                        if response.clicked() {
                            *turned = !*turned;
                        }
                    }

                    heading(ui, s.smart_limit);
                    let mut most = vec![25usize, 50, 100, 200];
                    if let Some(own) = rules.limit.filter(|own| !most.contains(own)) {
                        most.push(own);
                        most.sort_unstable();
                    }
                    let mut options = vec![(s.smart_any.to_string(), None)];
                    options.extend(most.into_iter().map(|n| (lang.song_count(n), Some(n))));
                    choices(ui, touch, "limit", shown, &mut rules.limit, &options);
                    ui.add_space(4.0);
                });

            // The search these make, as it is kept (and as it can be typed into the
            // search field), and how many songs it finds now.
            let query = editor.query();
            if editor.counted.as_ref().map(|(counted, _)| counted) != Some(&query) {
                let songs = if query.is_empty() {
                    0
                } else {
                    self.snapshot.search(&query).len()
                };
                editor.counted = Some((query.clone(), songs));
            }
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(4.0);
            if query.is_empty() {
                ui.label(RichText::new(s.smart_empty).small().color(dim(ui)));
            } else {
                ui.label(RichText::new(&query).monospace().small().weak());
                let songs = editor.counted.as_ref().map_or(0, |(_, songs)| *songs);
                ui.label(RichText::new(lang.song_count(songs)).small().color(dim(ui)));
            }
            let can_save = !editor.name.trim().is_empty() && !query.is_empty();
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let dark = ui.visuals().dark_mode;
                let ink = if dark {
                    egui::Color32::from_rgb(0x14, 0x14, 0x18)
                } else {
                    egui::Color32::WHITE
                };
                let size = if touch {
                    egui::vec2((ui.available_width() - 8.0) / 2.0, 46.0)
                } else {
                    egui::vec2(76.0, 28.0)
                };
                let keep = egui::Button::new(RichText::new(s.queue_save).strong().color(ink))
                    .fill(theme::accent(dark))
                    .min_size(size);
                let response = ui.add_enabled(can_save, keep);
                editor.shown.push(("save".into(), response.rect));
                if response.clicked() {
                    save = true;
                }
                let response = ui.add(egui::Button::new(s.cancel).min_size(size));
                editor.shown.push(("cancel".into(), response.rect));
                if response.clicked() {
                    cancel = true;
                }
            });
        });
        if save {
            let query = editor.query();
            self.save_smart(&editor.name, &query, editor.original);
            return;
        }
        if cancel || shown.should_close() {
            return;
        }
        self.smart_editor = Some(editor);
    }
}
