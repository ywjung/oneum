//! The bottom bar: current track, transport controls, seek bar and volume.
//!
//! The bar is split into three zones with fixed proportions so the transport controls
//! stay where they are regardless of title length or window width.
use super::icons::Icon;
use super::{Nav, OneumApp, next_repeat, theme, widgets};
use crate::i18n::format_time;
use crate::playback::{Command, PlayState, RepeatMode};
use eframe::egui::{self, Align, Layout, RichText, Sense, Ui, UiBuilder};
use std::time::{Duration, Instant};

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
            // (From the mini player: the full window comes back to show it.)
            self.mini_toggle |= self.mini.is_some();
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
            self.mini_toggle |= self.mini.is_some();
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
                    &with_keys(shuffle_label, "⌘S"),
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
                    &with_keys(s.previous_track, "⌘←"),
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
                let stop_after = self.player.stop_after_current;
                let hint = if stop_after {
                    format!("{label} (Space)\n{}", s.stop_after_current_on)
                } else {
                    format!("{label} (Space)")
                };
                let play = widgets::primary_icon_button(ui, icon, &hint, 36.0);
                if play.clicked() {
                    player.send(Command::TogglePlayPause);
                }
                play.context_menu(|ui| {
                    let mut on = stop_after;
                    if ui
                        .add_enabled(
                            self.player.current.is_some(),
                            egui::Checkbox::new(&mut on, s.menu_stop_after_current),
                        )
                        .changed()
                    {
                        player.send(Command::SetStopAfterCurrent(on));
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            self.player.current.is_some(),
                            egui::Button::new(self.section_step()),
                        )
                        .clicked()
                    {
                        player.send(Command::MarkSection);
                        ui.close();
                    }
                    ui.menu_button(s.menu_sleep_timer, |ui| {
                        for (minutes, label) in [
                            (15, s.sleep_15),
                            (30, s.sleep_30),
                            (45, s.sleep_45),
                            (60, s.sleep_60),
                            (90, s.sleep_90),
                        ] {
                            if ui.button(label).clicked() {
                                player.send(Command::SetSleepTimer(Some(Duration::from_secs(
                                    minutes * 60,
                                ))));
                                ui.close();
                            }
                        }
                        ui.separator();
                        if ui
                            .add_enabled(
                                self.player.sleep_at.is_some(),
                                egui::Button::new(s.sleep_off),
                            )
                            .clicked()
                        {
                            player.send(Command::SetSleepTimer(None));
                            ui.close();
                        }
                    });
                });
                if widgets::icon_button(ui, Icon::Next, &with_keys(s.next_track, "⌘→"), 30.0, false)
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
                if widgets::icon_button(ui, icon, &with_keys(label, "⌘⇧R"), 28.0, active).clicked()
                {
                    player.send(Command::SetRepeat(next_repeat(self.player.repeat)));
                }
            },
        );
        // Seek bar with elapsed / total time.
        ui.horizontal(|ui| {
            let duration_s = current.map_or(0.0, |c| c.duration_ms as f64 / 1000.0);
            let live_s = self.player.position_now_ms() as f64 / 1000.0;
            // After a seek the snapshot still shows the old position for a moment; keep
            // the thumb where the user put it until the player has caught up.
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
            // "1:02:03" needs more room than "4:05".
            let remaining = self.settings.show_remaining_time;
            let time_width =
                if duration_s >= 3600.0 { 56.0 } else { 40.0 } + if remaining { 6.0 } else { 0.0 };
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
                self.seek_pending = Some((shown, Instant::now()));
            }
            // The section that is marked for repeating: its ends, on the rail.
            if let Some(section) = self.player.section
                && duration_s > 0.0
            {
                section_marks(ui, response.rect, section, duration_s);
            }
            // Where a click would land, while pointing at the rail.
            if enabled
                && !response.dragged()
                && let Some(pointer) = response.hover_pos()
            {
                let rail = response.rect;
                let fraction = ((pointer.x - rail.left()) / rail.width().max(1.0)).clamp(0.0, 1.0);
                let at = f64::from(fraction) * duration_s;
                response
                    .clone()
                    .on_hover_text_at_pointer(format_time((at * 1000.0) as u64));
            }
            ui.allocate_ui_with_layout(
                egui::vec2(time_width, 18.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    // Click to switch between the total length and the time left.
                    let total_ms = current.map_or(0, |c| c.duration_ms);
                    let text = if remaining && total_ms > 0 {
                        let left = total_ms.saturating_sub((shown * 1000.0) as u64);
                        format!("-{}", format_time(left))
                    } else {
                        format_time(total_ms)
                    };
                    let hint = if remaining {
                        s.time_total_hint
                    } else {
                        s.time_remaining_hint
                    };
                    let label = egui::Label::new(RichText::new(text).small().color(dim))
                        .sense(Sense::click());
                    if ui.add(label).on_hover_text(hint).clicked() {
                        self.settings.show_remaining_time = !remaining;
                    }
                },
            );
        });
    }

    fn volume_controls(&mut self, ui: &mut Ui) {
        let s = self.s();
        let player = self.core.player.clone();
        // At the very right: the way to the mini player, and from it back.
        let mini = self.mini.is_some();
        let label = if mini { s.mini_leave } else { s.mini_enter };
        if widgets::icon_button(ui, Icon::Window, label, 18.0, mini).clicked() {
            self.mini_toggle = true;
        }
        let mut volume = self.player.volume;
        ui.spacing_mut().slider_width = (ui.available_width() - 44.0).clamp(60.0, 120.0);
        let response = ui.add(egui::Slider::new(&mut volume, 0.0..=1.0).show_value(false));
        response.widget_info(|| egui::WidgetInfo::slider(true, f64::from(volume), s.volume));
        if response.changed() {
            player.send(Command::SetVolume(volume));
            self.player.volume = volume;
        }
        let muted = volume <= 0.001;
        let (icon, label) = if muted {
            (Icon::VolumeMuted, s.unmute)
        } else {
            (Icon::Volume, s.mute)
        };
        if widgets::icon_button(ui, icon, &with_keys(label, "⌘⇧M"), 26.0, false).clicked() {
            self.toggle_mute();
        }
        // The sleep timer, while one is running: the time left; a click switches it off.
        if let Some(at) = self.player.sleep_at {
            let left = at.saturating_duration_since(Instant::now()).as_secs();
            let text = RichText::new(format!("{}:{:02}", left / 60, left % 60))
                .small()
                .color(theme::accent(ui.visuals().dark_mode));
            // Right to left: the time first, then the clock in front of it.
            let label = ui
                .add(egui::Label::new(text).sense(Sense::click()))
                .on_hover_text(s.sleep_cancel_hint);
            let (clock, _) = ui.allocate_exact_size(egui::Vec2::splat(13.0), Sense::hover());
            super::icons::paint(
                ui.painter(),
                clock,
                Icon::Clock,
                theme::accent(ui.visuals().dark_mode),
            );
            if label.clicked() {
                player.send(Command::SetSleepTimer(None));
            }
            // The seconds count down without any input.
            ui.ctx().request_repaint_after(Duration::from_millis(500));
        }
        // The section of the song that is marked for repeating: said in words; a click
        // gives it up.
        if let Some(words) = self.section_words() {
            let text = RichText::new(words)
                .small()
                .color(theme::accent(ui.visuals().dark_mode));
            let label = ui
                .add(egui::Label::new(text).sense(Sense::click()))
                .on_hover_text(s.section_cancel_hint);
            if label.clicked() {
                player.send(Command::ClearSection);
            }
        }
        // Another speed than the songs' own, while it is set: a click sets it back.
        if self.player.speed_percent != 100 {
            let text = RichText::new(crate::i18n::Lang::speed_label(self.player.speed_percent))
                .small()
                .color(theme::accent(ui.visuals().dark_mode));
            let label = ui
                .add(egui::Label::new(text).sense(Sense::click()))
                .on_hover_text(s.speed_cancel_hint);
            if label.clicked() {
                self.settings.speed_percent = 100;
                player.send(Command::SetSpeed(100));
            }
        }
        // "Stop after this song", while it is set: said in words (a mark on the play
        // button read as a drawing fault); a click takes it back.
        if self.player.stop_after_current {
            let text = RichText::new(s.stop_after_short)
                .small()
                .color(theme::accent(ui.visuals().dark_mode));
            let label = ui
                .add(egui::Label::new(text).sense(Sense::click()))
                .on_hover_text(s.stop_after_cancel_hint);
            if label.clicked() {
                player.send(Command::SetStopAfterCurrent(false));
            }
        }
    }
}

/// The ends of the section that is marked for repeating, on the rail of a seek bar
/// (`rail`: the slider's rectangle; the thumb's travel is a little shorter than it,
/// which a mark of two points' width does not show).
pub(super) fn section_marks(
    ui: &Ui,
    rail: egui::Rect,
    section: crate::playback::SectionRepeat,
    duration_s: f64,
) {
    let accent = theme::accent(ui.visuals().dark_mode);
    let x_of = |ms: u64| {
        let fraction = (ms as f64 / 1000.0 / duration_s).clamp(0.0, 1.0) as f32;
        rail.left() + fraction * rail.width()
    };
    let from = x_of(section.from_ms);
    let mark = |x: f32| {
        ui.painter().rect_filled(
            egui::Rect::from_center_size(
                egui::pos2(x, rail.center().y),
                egui::vec2(2.0, rail.height().min(14.0)),
            ),
            1.0,
            accent,
        );
    };
    mark(from);
    if let Some(to) = section.to_ms {
        let to = x_of(to);
        mark(to);
        // What lies between them, as a faint band behind the thumb's line.
        ui.painter().rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(from, rail.center().y - 1.5),
                egui::pos2(to, rail.center().y + 1.5),
            ),
            0.0,
            accent.gamma_multiply(0.45),
        );
    }
}

/// A control's name with the keys that do the same, named as this system's keyboard
/// has them ("Next (⌘→)" on a Mac, "Next (Ctrl+→)" elsewhere).
fn with_keys(label: &str, keys: &str) -> String {
    format!("{label} ({})", crate::platform::key_names(keys))
}
