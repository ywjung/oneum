//! The statistics page: what was listened to in a period (from the app's own play
//! history), and what the library is made of. Worked out on a thread of its own, since
//! reading the history waits for the database.
use super::{Action, AppEvent, Nav, OneumApp, theme, widgets};
use crate::i18n::group_digits;
use crate::library::stats::{self, LibraryMakeup, ListeningStats};
use eframe::egui::{self, RichText, Sense, Ui};
use std::time::{Duration, Instant};

/// The span the statistics cover: back from now, or a year of the calendar (this one
/// and the one before it: the listener's year in review).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Period {
    Week,
    #[default]
    Month,
    ThisYear,
    LastYear,
    All,
}

impl Period {
    /// The span as Unix seconds, from its first moment to its end (not included),
    /// at `now` where local time is `offset_secs` ahead of UTC.
    fn span(self, now: i64, offset_secs: i32) -> (i64, i64) {
        let year = stats::year_of(now, offset_secs);
        match self {
            Period::Week => (now - 7 * 86_400, i64::MAX),
            Period::Month => (now - 30 * 86_400, i64::MAX),
            // (By the clock of the nights the years begin with: see `year_span_by`.)
            Period::ThisYear => {
                stats::year_span_by(year, crate::platform::clock::utc_offset_secs_at)
            }
            Period::LastYear => {
                stats::year_span_by(year - 1, crate::platform::clock::utc_offset_secs_at)
            }
            Period::All => (i64::MIN, i64::MAX),
        }
    }
}

/// Statistics as they were worked out.
pub(super) struct StatsData {
    period: Period,
    generation: u64,
    at: Instant,
    listening: ListeningStats,
    makeup: LibraryMakeup,
}

/// How often the figures are worked out again while the library keeps changing.
const REFRESH: Duration = Duration::from_secs(3);

impl OneumApp {
    /// Starts working the statistics out, unless what is shown is current or on its way.
    fn request_stats(&mut self) {
        let period = self.stats_period;
        let current = self.stats.as_ref().is_some_and(|d| {
            d.period == period && (d.generation == self.lib_generation || d.at.elapsed() < REFRESH)
        });
        if current || self.stats_pending == Some(period) {
            return;
        }
        let (library, core, events) = (
            self.core.library.clone(),
            self.core.clone(),
            self.events_tx.clone(),
        );
        let generation = self.lib_generation;
        let spawned = std::thread::Builder::new()
            .name("oneum-stats".into())
            .spawn(move || {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs() as i64);
                let (since, until) = period.span(now, crate::platform::clock::utc_offset_secs());
                let mut plays = library.plays_since(since);
                plays.retain(|&(_, at, _)| at < until);
                let snapshot = library.snapshot();
                let data = StatsData {
                    period,
                    generation,
                    at: Instant::now(),
                    listening: stats::listening(&snapshot, &plays),
                    makeup: stats::makeup(&snapshot),
                };
                let _ = events.send(AppEvent::StatsReady(Box::new(data)));
                core.request_repaint();
            });
        if spawned.is_ok() {
            self.stats_pending = Some(period);
        }
    }

    /// For the verification channel: plays and songs of the figures shown, if any
    /// (those of the period that is chosen: none while they are being worked out).
    pub(super) fn stats_summary_for_dump(&self) -> Option<(usize, usize, usize)> {
        self.stats
            .as_ref()
            .filter(|d| d.period == self.stats_period)
            .map(|d| (d.listening.plays, d.listening.songs, d.makeup.songs))
    }

    pub(super) fn stats_ready(&mut self, data: Box<StatsData>) {
        if self.stats_pending == Some(data.period) {
            self.stats_pending = None;
        }
        self.stats = Some(*data);
    }

    pub(super) fn stats_view(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let s = self.s();
        // On a phone the page's own bar says where one is, and the lists stand below
        // each other: two of them do not fit side by side.
        let touch = theme::touch(ui.ctx());
        if !touch {
            self.view_title(ui, s.stats, None);
        }
        // The two years of the calendar are named by their numbers.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let year = stats::year_of(now, crate::platform::clock::utc_offset_secs());
        let (this_year, last_year) = (year.to_string(), (year - 1).to_string());
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            widgets::segmented(
                ui,
                "stats-period",
                &mut self.stats_period,
                &[
                    (Period::Week, s.stats_7d),
                    (Period::Month, s.stats_30d),
                    (Period::ThisYear, this_year.as_str()),
                    (Period::LastYear, last_year.as_str()),
                    (Period::All, s.stats_all),
                ],
            );
        });
        ui.add_space(8.0);
        self.request_stats();
        let Some(data) = self
            .stats
            .as_ref()
            .filter(|d| d.period == self.stats_period)
        else {
            ui.add(egui::Spinner::new());
            return;
        };
        let lang = self.lang;
        let dim = theme::dim(ui);
        let listening = &data.listening;
        let makeup = &data.makeup;
        egui::ScrollArea::vertical()
            .id_salt("stats")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(4.0);
                    let summary = if listening.plays == 0 {
                        s.stats_none.to_string()
                    } else {
                        lang.stats_summary(listening.plays, listening.listened_ms, listening.songs)
                    };
                    ui.label(RichText::new(summary).font(theme::semibold(15.0)));
                });
                ui.add_space(10.0);
                if listening.plays > 0 {
                    let snapshot = self.snapshot.clone();
                    let artists = listening.top_artists.iter().map(|(name, n)| {
                        (
                            name.clone(),
                            *n,
                            Some(Action::Go(Nav::Artist(name.clone()))),
                        )
                    });
                    let albums = listening.top_albums.iter().map(|(key, title, artist, n)| {
                        let name = if artist.is_empty() {
                            title.clone()
                        } else {
                            format!("{title} — {artist}")
                        };
                        (name, *n, Some(Action::Go(Nav::Album(*key))))
                    });
                    let songs = listening.top_songs.iter().filter_map(|(id, n)| {
                        let track = snapshot.get(*id)?;
                        let name = if track.artist.is_empty() {
                            track.title.clone()
                        } else {
                            format!("{} — {}", track.title, track.artist)
                        };
                        Some((name, *n, Some(Action::ShowInfo(*id))))
                    });
                    let genres = listening.top_genres.iter().map(|(name, n)| {
                        (name.clone(), *n, Some(Action::Go(Nav::Genre(name.clone()))))
                    });
                    if touch {
                        // Below each other, and only those that have something to say.
                        if !listening.top_artists.is_empty() {
                            top_list(ui, s.top_artists, artists, lang, true, actions);
                            ui.add_space(14.0);
                        }
                        if !listening.top_albums.is_empty() {
                            top_list(ui, s.top_albums, albums, lang, true, actions);
                            ui.add_space(14.0);
                        }
                        if !listening.top_songs.is_empty() {
                            top_list(ui, s.top_songs, songs, lang, true, actions);
                            ui.add_space(14.0);
                        }
                        if !listening.top_genres.is_empty() {
                            top_list(ui, s.top_genres, genres, lang, true, actions);
                        }
                    } else {
                        ui.columns(2, |columns| {
                            top_list(
                                &mut columns[0],
                                s.top_artists,
                                artists,
                                lang,
                                false,
                                actions,
                            );
                            top_list(&mut columns[1], s.top_albums, albums, lang, false, actions);
                        });
                        ui.add_space(10.0);
                        ui.columns(2, |columns| {
                            top_list(&mut columns[0], s.top_songs, songs, lang, false, actions);
                            top_list(&mut columns[1], s.top_genres, genres, lang, false, actions);
                        });
                    }
                    ui.add_space(14.0);
                }
                widgets::section_title(ui, s.stats_library);
                widgets::card(ui, |ui| {
                    ui.add_space(8.0);
                    let line = |ui: &mut Ui, label: &str, text: String| {
                        ui.horizontal_wrapped(|ui| {
                            ui.add_space(8.0);
                            if !label.is_empty() {
                                ui.label(RichText::new(label).color(dim));
                            }
                            ui.label(text);
                        });
                    };
                    line(
                        ui,
                        "",
                        lang.library_makeup(
                            makeup.songs,
                            makeup.total_ms,
                            makeup.total_bytes,
                            makeup.lossless,
                        ),
                    );
                    let formats: Vec<String> = makeup
                        .formats
                        .iter()
                        .map(|(name, n)| format!("{name} {}", group_digits(*n)))
                        .collect();
                    line(ui, s.stats_formats, formats.join(" · "));
                    let rates: Vec<String> = makeup
                        .rates
                        .iter()
                        .map(|(rate, n)| {
                            let khz = f64::from(*rate) / 1000.0;
                            let khz = if khz.fract() == 0.0 {
                                format!("{khz:.0}")
                            } else {
                                format!("{khz:.1}")
                            };
                            format!("{khz} kHz {}", group_digits(*n))
                        })
                        .collect();
                    line(ui, s.stats_rates, rates.join(" · "));
                    if makeup.checked > 0 {
                        line(
                            ui,
                            s.check_title,
                            lang.check_progress(
                                false,
                                makeup.checked as u64,
                                makeup.checked as u64,
                                makeup.damaged as u64,
                                0,
                            ),
                        );
                    }
                    ui.add_space(8.0);
                });
                widgets::section_note(
                    ui,
                    if touch {
                        s.phone_stats_note
                    } else {
                        s.stats_note
                    },
                );
            });
    }
}

/// A "most played" list: rank, name (a way to it), plays.
fn top_list(
    ui: &mut Ui,
    title: &str,
    entries: impl Iterator<Item = (String, usize, Option<Action>)>,
    lang: crate::i18n::Lang,
    touch: bool,
    actions: &mut Vec<Action>,
) {
    let dim = theme::dim(ui);
    let title_size = if touch { 13.5 } else { 12.0 };
    ui.label(
        RichText::new(title)
            .font(theme::semibold(title_size))
            .color(dim),
    );
    ui.add_space(2.0);
    for (rank, (name, plays, action)) in entries.enumerate() {
        ui.horizontal(|ui| {
            if touch {
                // Tall enough to hit with a finger.
                ui.set_min_height(38.0);
            }
            ui.label(
                RichText::new(format!("{:>2}", rank + 1))
                    .monospace()
                    .color(dim),
            );
            // The count first (from the right), the name in what is left of the line.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(lang.plays_short(plays)).color(dim));
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let response = ui
                        .add(egui::Label::new(name).truncate().sense(Sense::click()))
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    if let Some(action) = action
                        && response.clicked()
                    {
                        actions.push(action);
                    }
                });
            });
        });
    }
}
