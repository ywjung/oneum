//! The bottom bar: current track, transport controls, seek bar and volume.
//!
//! The bar is split into three zones with fixed proportions so the transport controls
//! stay where they are regardless of title length or window width.
use super::icons::Icon;
use super::{Nav, OneumApp, next_repeat, theme, widgets};
use crate::i18n::format_time;
use crate::playback::{Command, PlayState, RepeatMode};
use eframe::egui::{self, Align, Layout, RichText, Sense, Ui, UiBuilder};

const ART_SIZE: f32 = 52.0;

impl OneumApp {
    pub(super) fn player_bar(&mut self, ui: &mut Ui) {
        let frame = egui::Frame::new()
            .fill(theme::chrome_fill(ui))
            .inner_margin(egui::Margin::symmetric(12, 8));
        egui::Panel::bottom("player-bar")
            .frame(frame)
            .exact_size(theme::PLAYER_BAR_HEIGHT)
            .show(ui, |ui| {
                let full = ui.available_rect_before_wrap();
                let side = (full.width() * 0.27).clamp(170.0, 340.0);
                let left = egui::Rect::from_min_max(
                    full.min,
                    egui::pos2(full.left() + side, full.bottom()),
                );
                let right =
                    egui::Rect::from_min_max(egui::pos2(full.right() - side, full.top()), full.max);
                let centre = egui::Rect::from_min_max(
                    egui::pos2(left.right() + 12.0, full.top()),
                    egui::pos2(right.left() - 12.0, full.bottom()),
                );
                ui.allocate_rect(full, Sense::hover());
                let mut left_ui = ui.new_child(
                    UiBuilder::new()
                        .max_rect(left)
                        .layout(Layout::left_to_right(Align::Center)),
                );
                self.now_playing(&mut left_ui);
                let mut centre_ui = ui.new_child(
                    UiBuilder::new()
                        .max_rect(centre)
                        .layout(Layout::top_down(Align::Center)),
                );
                self.transport(&mut centre_ui);
                let mut right_ui = ui.new_child(
                    UiBuilder::new()
                        .max_rect(right)
                        .layout(Layout::right_to_left(Align::Center)),
                );
                self.volume_controls(&mut right_ui);
            });
    }

    /// Cover, title and artist of the current track. Clicking the cover opens the album,
    /// clicking the title shows the track in the song list.
    fn now_playing(&mut self, ui: &mut Ui) {
        let s = self.s();
        let track = self
            .player
            .current
            .and_then(|c| self.track(c.track_id))
            .cloned();
        let dim = theme::dim(ui);
        let (rect, art_response) =
            ui.allocate_exact_size(egui::Vec2::splat(ART_SIZE), Sense::click());
        let texture = self
            .art
            .get(track.as_ref().and_then(|t| t.art_id.as_deref()), false);
        widgets::art(ui, rect, texture, 6.0);
        let Some(track) = track else {
            ui.add_space(4.0);
            ui.label(RichText::new(s.nothing_playing).color(dim));
            return;
        };
        let album = if track.album.is_empty() {
            s.unknown_album
        } else {
            track.album.as_str()
        };
        if art_response.on_hover_text(album).clicked() {
            self.go(Nav::Album(track.album_key));
        }
        ui.add_space(2.0);
        let mut go_to_track = false;
        ui.vertical(|ui| {
            ui.add_space((ui.available_height() - 36.0).max(0.0) / 2.0);
            ui.spacing_mut().item_spacing.y = 3.0;
            let title = RichText::new(&track.title)
                .font(theme::semibold(13.5))
                .color(ui.visuals().strong_text_color());
            let response = ui.add(egui::Label::new(title).truncate().sense(Sense::click()));
            go_to_track = response.clicked();
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
            widgets::truncated(ui, RichText::new(line).small().color(dim));
        });
        if go_to_track {
            self.go_to_current();
        }
    }

    fn transport(&mut self, ui: &mut Ui) {
        let s = self.s();
        let player = self.core.player.clone();
        let dim = theme::dim(ui);
        let current = self.player.current;
        ui.spacing_mut().item_spacing.y = 2.0;
        // Buttons, centred as a group.
        let group_width = 28.0 + 30.0 + 36.0 + 30.0 + 28.0 + 4.0 * 10.0;
        ui.allocate_ui_with_layout(
            egui::vec2(group_width, 36.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                let shuffle_label = if self.player.shuffle {
                    s.shuffle_on
                } else {
                    s.shuffle
                };
                if widgets::icon_button(
                    ui,
                    Icon::Shuffle,
                    &format!("{shuffle_label} (⌘S)"),
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
                    &format!("{} (⌘←)", s.previous_track),
                    30.0,
                    false,
                )
                .clicked()
                {
                    player.send(Command::Previous);
                }
                let playing = matches!(
                    self.player.state,
                    PlayState::Playing | PlayState::Loading | PlayState::Seeking
                );
                let (icon, label) = if playing {
                    (Icon::Pause, s.pause)
                } else {
                    (Icon::Play, s.play)
                };
                if widgets::primary_icon_button(ui, icon, &format!("{label} (Space)"), 36.0)
                    .clicked()
                {
                    player.send(Command::TogglePlayPause);
                }
                if widgets::icon_button(
                    ui,
                    Icon::Next,
                    &format!("{} (⌘→)", s.next_track),
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
                if widgets::icon_button(ui, icon, &format!("{label} (⌘⇧R)"), 28.0, active).clicked()
                {
                    player.send(Command::SetRepeat(next_repeat(self.player.repeat)));
                }
            },
        );
        // Seek bar with elapsed / total time.
        ui.horizontal(|ui| {
            let duration_s = current.map_or(0.0, |c| c.duration_ms as f64 / 1000.0);
            let live_s = self.player.position_now_ms() as f64 / 1000.0;
            let mut shown = self.seek_drag.unwrap_or(live_s).min(duration_s.max(0.0));
            let time_width = 40.0;
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
            ui.spacing_mut().slider_width = (ui.available_width() - time_width - 12.0).max(60.0);
            let enabled = current.is_some() && duration_s > 0.0;
            let slider =
                egui::Slider::new(&mut shown, 0.0..=duration_s.max(0.001)).show_value(false);
            let response = ui.add_enabled(enabled, slider);
            response.widget_info(|| egui::WidgetInfo::slider(true, shown, s.seek_bar));
            if response.dragged() {
                self.seek_drag = Some(shown);
            } else if response.drag_stopped() || response.changed() {
                // Released after a drag, a click on the rail, or a keyboard step.
                player.send(Command::SeekTo((shown * 1000.0) as u64));
                self.seek_drag = None;
            }
            ui.allocate_ui_with_layout(
                egui::vec2(time_width, 18.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.label(
                        RichText::new(format_time(current.map_or(0, |c| c.duration_ms)))
                            .small()
                            .color(dim),
                    );
                },
            );
        });
    }

    fn volume_controls(&mut self, ui: &mut Ui) {
        let s = self.s();
        let player = self.core.player.clone();
        let mut volume = self.player.volume;
        ui.spacing_mut().slider_width = (ui.available_width() - 44.0).clamp(60.0, 120.0);
        let response = ui.add(egui::Slider::new(&mut volume, 0.0..=1.0).show_value(false));
        response.widget_info(|| egui::WidgetInfo::slider(true, f64::from(volume), s.volume));
        if response.changed() {
            player.send(Command::SetVolume(volume));
            self.player.volume = volume;
        }
        let icon = if volume <= 0.001 {
            Icon::VolumeMuted
        } else {
            Icon::Volume
        };
        if widgets::icon_button(ui, icon, s.volume, 26.0, false).clicked() {
            // Toggle mute, remembering the previous level for un-muting.
            if volume > 0.001 {
                self.volume_before_mute = volume;
                player.send(Command::SetVolume(0.0));
            } else {
                let restore = if self.volume_before_mute > 0.001 {
                    self.volume_before_mute
                } else {
                    0.8
                };
                player.send(Command::SetVolume(restore));
            }
        }
    }
}
