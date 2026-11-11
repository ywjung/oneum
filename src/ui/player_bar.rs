//! The bottom bar: current track, transport controls, seek bar and volume.
use super::icons::Icon;
use super::{Nav, OneumApp, next_repeat, theme, widgets};
use crate::i18n::format_time;
use crate::playback::{Command, PlayState, RepeatMode};
use eframe::egui::{self, Align, Layout, RichText, Sense, Ui};

impl OneumApp {
    pub(super) fn player_bar(&mut self, ui: &mut Ui) {
        let s = self.s();
        let frame = egui::Frame::new()
            .fill(theme::chrome_fill(ui))
            .inner_margin(egui::Margin::symmetric(12, 8));
        egui::Panel::bottom("player-bar")
            .frame(frame)
            .exact_size(theme::PLAYER_BAR_HEIGHT)
            .show(ui, |ui| {
                let player = self.core.player.clone();
                let current = self.player.current;
                let track = current.and_then(|c| self.track(c.track_id)).cloned();
                let dim = theme::dim(ui);
                let total_width = ui.available_width();
                let side_width = (total_width * 0.26).clamp(150.0, 320.0);

                ui.horizontal_centered(|ui| {
                    // Left: cover and titles.
                    ui.allocate_ui_with_layout(
                        egui::vec2(side_width, ui.available_height()),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            let (rect, art_response) =
                                ui.allocate_exact_size(egui::Vec2::splat(52.0), Sense::click());
                            let texture = self
                                .art
                                .get(track.as_ref().and_then(|t| t.art_id.as_deref()), false);
                            widgets::art(ui, rect, texture, 6.0);
                            ui.vertical(|ui| {
                                ui.add_space(8.0);
                                ui.spacing_mut().item_spacing.y = 2.0;
                                match &track {
                                    Some(track) => {
                                        widgets::truncated(
                                            ui,
                                            RichText::new(&track.title).strong(),
                                        );
                                        let artist = if track.artist.is_empty() {
                                            s.unknown_artist
                                        } else {
                                            &track.artist
                                        };
                                        let line = if track.album.is_empty() {
                                            artist.to_string()
                                        } else {
                                            format!("{artist} — {}", track.album)
                                        };
                                        widgets::truncated(
                                            ui,
                                            RichText::new(line).small().color(dim),
                                        );
                                    }
                                    None => {
                                        ui.add_space(8.0);
                                        ui.label(RichText::new(s.nothing_playing).color(dim));
                                    }
                                }
                            });
                            if art_response.clicked()
                                && let Some(track) = &track
                            {
                                self.go(Nav::Album(track.album_key));
                            }
                        },
                    );

                    // Right: volume (laid out first so the centre can take what is left).
                    let centre_width = (ui.available_width() - side_width).max(200.0);
                    ui.allocate_ui_with_layout(
                        egui::vec2(centre_width, ui.available_height()),
                        Layout::top_down(Align::Center),
                        |ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            // Transport buttons.
                            ui.allocate_ui_with_layout(
                                egui::vec2(230.0, 34.0),
                                Layout::left_to_right(Align::Center),
                                |ui| {
                                    ui.spacing_mut().item_spacing.x = 8.0;
                                    let shuffle_label = if self.player.shuffle {
                                        s.shuffle_on
                                    } else {
                                        s.shuffle
                                    };
                                    if widgets::icon_button(
                                        ui,
                                        Icon::Shuffle,
                                        shuffle_label,
                                        28.0,
                                        self.player.shuffle,
                                    )
                                    .clicked()
                                    {
                                        player.send(Command::SetShuffle(!self.player.shuffle));
                                    }
                                    if widgets::icon_button(
                                        ui,
                                        Icon::Previous,
                                        s.previous_track,
                                        30.0,
                                        false,
                                    )
                                    .clicked()
                                    {
                                        player.send(Command::Previous);
                                    }
                                    let playing = matches!(
                                        self.player.state,
                                        PlayState::Playing
                                            | PlayState::Loading
                                            | PlayState::Seeking
                                    );
                                    let (icon, label) = if playing {
                                        (Icon::Pause, s.pause)
                                    } else {
                                        (Icon::Play, s.play)
                                    };
                                    if widgets::primary_icon_button(ui, icon, label, 34.0).clicked()
                                    {
                                        player.send(Command::TogglePlayPause);
                                    }
                                    if widgets::icon_button(
                                        ui,
                                        Icon::Next,
                                        s.next_track,
                                        30.0,
                                        false,
                                    )
                                    .clicked()
                                    {
                                        player.send(Command::Next);
                                    }
                                    let (icon, label) = match self.player.repeat {
                                        RepeatMode::Off => (Icon::Repeat, s.repeat_off),
                                        RepeatMode::All => (Icon::Repeat, s.repeat_all),
                                        RepeatMode::One => (Icon::RepeatOne, s.repeat_one),
                                    };
                                    let active = self.player.repeat != RepeatMode::Off;
                                    if widgets::icon_button(ui, icon, label, 28.0, active).clicked()
                                    {
                                        player.send(Command::SetRepeat(next_repeat(
                                            self.player.repeat,
                                        )));
                                    }
                                },
                            );
                            // Seek bar with elapsed / total time.
                            ui.horizontal(|ui| {
                                let duration_s =
                                    current.map_or(0.0, |c| c.duration_ms as f64 / 1000.0);
                                let live_s = self.player.position_now_ms() as f64 / 1000.0;
                                let mut shown =
                                    self.seek_drag.unwrap_or(live_s).min(duration_s.max(0.0));
                                let time_width = 44.0;
                                ui.allocate_ui_with_layout(
                                    egui::vec2(time_width, 18.0),
                                    Layout::right_to_left(Align::Center),
                                    |ui| {
                                        ui.label(
                                            RichText::new(format_time((shown * 1000.0) as u64))
                                                .small()
                                                .color(dim),
                                        );
                                    },
                                );
                                let slider_width =
                                    (ui.available_width() - time_width - 12.0).max(60.0);
                                ui.spacing_mut().slider_width = slider_width;
                                let enabled = current.is_some() && duration_s > 0.0;
                                let slider =
                                    egui::Slider::new(&mut shown, 0.0..=duration_s.max(0.001))
                                        .show_value(false);
                                let response = ui.add_enabled(enabled, slider);
                                response.widget_info(|| {
                                    egui::WidgetInfo::slider(true, shown, s.seek_bar)
                                });
                                if response.dragged() {
                                    self.seek_drag = Some(shown);
                                } else if response.drag_stopped() {
                                    player.send(Command::SeekTo((shown * 1000.0) as u64));
                                    self.seek_drag = None;
                                } else if response.changed() {
                                    // A click on the rail or a keyboard step.
                                    player.send(Command::SeekTo((shown * 1000.0) as u64));
                                    self.seek_drag = None;
                                }
                                ui.label(
                                    RichText::new(format_time(
                                        current.map_or(0, |c| c.duration_ms),
                                    ))
                                    .small()
                                    .color(dim),
                                );
                            });
                        },
                    );

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let mut volume = self.player.volume;
                        ui.spacing_mut().slider_width =
                            (ui.available_width() - 40.0).clamp(60.0, 110.0);
                        let response =
                            ui.add(egui::Slider::new(&mut volume, 0.0..=1.0).show_value(false));
                        response.widget_info(|| {
                            egui::WidgetInfo::slider(true, f64::from(volume), s.volume)
                        });
                        if response.changed() {
                            player.send(Command::SetVolume(volume));
                            self.player.volume = volume;
                        }
                        let icon = if volume <= 0.001 {
                            Icon::VolumeMuted
                        } else {
                            Icon::Volume
                        };
                        if widgets::icon_button(ui, icon, s.volume, 24.0, false).clicked() {
                            // Toggle mute, remembering the previous level in egui's memory.
                            let id = egui::Id::unique("volume-before-mute");
                            if volume > 0.001 {
                                ui.ctx().data_mut(|d| d.insert_temp(id, volume));
                                player.send(Command::SetVolume(0.0));
                            } else {
                                let restore =
                                    ui.ctx().data(|d| d.get_temp::<f32>(id)).unwrap_or(0.8);
                                player.send(Command::SetVolume(restore));
                            }
                        }
                    });
                });
            });
    }
}
