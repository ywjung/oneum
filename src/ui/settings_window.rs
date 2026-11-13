//! The settings window. Nothing here is required to start listening.
//!
//! Layout: a header with the tabs, then groups of rows in rounded cards — what a setting
//! is on the left, its control on the right. The window keeps one size for all tabs.
use super::dialogs::Dialog;
use super::icons::Icon;
use super::{OneumApp, markdown, theme, widgets};
use crate::app::settings::ThemeChoice;
use crate::audio::ReplayGainMode;
use crate::i18n::LanguageChoice;
use crate::platform::finder;
use crate::playback::Command;
use eframe::egui::{self, Align, Layout, RichText, Sense};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Tab {
    General,
    Library,
    Playback,
    Shortcuts,
    About,
}

const LICENSE_NOTICES: &str = include_str!("../../THIRD_PARTY_NOTICES.md");
/// The notices are small print: a size below the rest of Settings.
const NOTICES_LOOK: markdown::Look = markdown::Look {
    body: 12.5,
    heading: 14.0,
    heading_gap: 14.0,
};

/// The open source notices, parsed once.
fn license_notices() -> &'static [markdown::Block] {
    static NOTICES: std::sync::OnceLock<Vec<markdown::Block>> = std::sync::OnceLock::new();
    NOTICES.get_or_init(|| markdown::parse(LICENSE_NOTICES))
}
const APP_ICON_PNG: &[u8] = include_bytes!("../../assets/icon-160.png");

const WIDTH: f32 = 620.0;
const BODY_HEIGHT: f32 = 404.0;
const SIDE_MARGIN: f32 = 20.0;

impl OneumApp {
    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let s = self.s();
        let style = ctx.global_style();
        let dark = style.visuals.dark_mode;
        let body_fill = if dark {
            egui::Color32::from_rgb(0x1E, 0x1E, 0x23)
        } else {
            egui::Color32::from_rgb(0xF4, 0xF4, 0xF7)
        };
        let frame = egui::Frame::window(&style)
            .inner_margin(0)
            .corner_radius(12.0)
            .fill(body_fill);
        let mut close = false;
        // Dim what is behind and let a click there close the window, like a sheet.
        let screen = ctx.content_rect();
        let backdrop_clicked = egui::Area::new(egui::Id::unique("settings-backdrop"))
            .order(egui::Order::Background)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let response = ui.allocate_rect(screen, Sense::click());
                let alpha = if dark { 120 } else { 70 };
                ui.painter()
                    .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(alpha));
                response.clicked()
            })
            .inner;
        if backdrop_clicked && self.dialog.is_none() {
            close = true;
        }
        egui::Window::new(s.settings)
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .frame(frame)
            .fixed_size([WIDTH, 0.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -20.0])
            .show(ctx, |ui| {
                ui.set_width(WIDTH);
                ui.spacing_mut().item_spacing.y = 0.0;
                close |= self.settings_header(ui);
                widgets::row_divider(ui);
                egui::ScrollArea::vertical()
                    .id_salt(("settings-body", self.settings_tab))
                    .max_height(BODY_HEIGHT)
                    .min_scrolled_height(BODY_HEIGHT)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            ui.add_space(SIDE_MARGIN);
                            ui.vertical(|ui| {
                                ui.set_width(WIDTH - SIDE_MARGIN * 2.0);
                                ui.spacing_mut().item_spacing.y = 4.0;
                                match self.settings_tab {
                                    Tab::General => self.settings_general(ui),
                                    Tab::Library => self.settings_library(ui),
                                    Tab::Playback => self.settings_playback(ui),
                                    Tab::Shortcuts => self.settings_shortcuts(ui),
                                    Tab::About => self.settings_about(ui),
                                }
                            });
                        });
                        ui.add_space(16.0);
                    });
            });
        if close {
            self.settings_open = false;
        }
    }

    /// Title, close button and the tab bar. Returns true when the window should close.
    fn settings_header(&mut self, ui: &mut egui::Ui) -> bool {
        let s = self.s();
        let mut close = false;
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.add_space(SIDE_MARGIN);
            ui.label(
                RichText::new(s.settings)
                    .font(theme::semibold(16.0))
                    .color(ui.visuals().strong_text_color()),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(12.0);
                if widgets::icon_button(ui, Icon::Close, s.close_settings, 26.0, false).clicked() {
                    close = true;
                }
            });
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(SIDE_MARGIN - 4.0);
            ui.spacing_mut().item_spacing.x = 4.0;
            for (tab, icon, label) in [
                (Tab::General, Icon::Gear, s.settings_general),
                (Tab::Library, Icon::Folder, s.settings_library),
                (Tab::Playback, Icon::Volume, s.settings_playback),
                (Tab::Shortcuts, Icon::Keyboard, s.settings_shortcuts),
                (Tab::About, Icon::Info, s.settings_about),
            ] {
                if widgets::tab_button(ui, self.settings_tab == tab, icon, label).clicked() {
                    self.settings_tab = tab;
                }
            }
        });
        ui.add_space(10.0);
        close
    }

    fn settings_general(&mut self, ui: &mut egui::Ui) {
        let s = self.s();
        widgets::section_title(ui, s.settings_section_display);
        widgets::card(ui, |ui| {
            let before = self.settings.language;
            widgets::setting_row(ui, s.language, Some(s.language_desc), true, |ui| {
                let text = match before {
                    LanguageChoice::System => s.language_system,
                    LanguageChoice::Korean => "한국어",
                    LanguageChoice::English => "English",
                };
                egui::ComboBox::from_id_salt("language")
                    .selected_text(text)
                    .width(190.0)
                    .show_ui(ui, |ui| {
                        for (choice, label) in [
                            (LanguageChoice::System, s.language_system),
                            (LanguageChoice::Korean, "한국어"),
                            (LanguageChoice::English, "English"),
                        ] {
                            ui.selectable_value(&mut self.settings.language, choice, label);
                        }
                    });
            });
            if self.settings.language != before {
                self.lang = self.settings.language.resolve(self.system_prefers_korean);
                let ctx = ui.ctx().clone();
                self.install_menu(&ctx);
            }
            widgets::row_divider(ui);
            let before = self.settings.theme;
            widgets::setting_row(ui, s.theme, Some(s.theme_desc), true, |ui| {
                widgets::segmented(
                    ui,
                    "theme",
                    &mut self.settings.theme,
                    &[
                        (ThemeChoice::System, s.theme_system_short),
                        (ThemeChoice::Light, s.theme_light),
                        (ThemeChoice::Dark, s.theme_dark),
                    ],
                );
            });
            if self.settings.theme != before {
                theme::apply_choice(ui.ctx(), self.settings.theme);
            }
        });

        ui.add_space(14.0);
        widgets::section_title(ui, s.settings_section_lists);
        widgets::card(ui, |ui| {
            widgets::setting_row(
                ui,
                s.show_format_title,
                Some(s.show_format_desc),
                true,
                |ui| {
                    widgets::toggle(
                        ui,
                        &mut self.settings.show_format_column,
                        s.show_format_title,
                    );
                },
            );
        });
    }

    fn settings_library(&mut self, ui: &mut egui::Ui) {
        let s = self.s();
        let dim = theme::dim(ui);
        let roots = self.core.library.roots();
        let scan = self.core.library.scan_status();

        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.label(
                RichText::new(s.music_folders)
                    .font(theme::semibold(12.0))
                    .color(dim),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(4.0);
                let summary = format!(
                    "{} · {} · {}",
                    self.lang.song_count(self.snapshot.len()),
                    self.lang.album_count(self.snapshot.albums().len()),
                    self.lang.artist_count(self.snapshot.artists().len()),
                );
                ui.label(RichText::new(summary).small().color(dim));
            });
        });
        ui.add_space(4.0);

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        widgets::card(ui, |ui| {
            if roots.is_empty() {
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), 64.0),
                    Layout::centered_and_justified(egui::Direction::LeftToRight),
                    |ui| {
                        ui.label(RichText::new(s.no_music_folders).color(dim));
                    },
                );
            }
            for (index, root) in roots.iter().enumerate() {
                if index > 0 {
                    widgets::row_divider(ui);
                }
                let name = root.path.file_name().map_or_else(
                    || root.path.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                // "~/Music/…" reads better than the full home path (and shows no user name).
                let shown_path = match home
                    .as_ref()
                    .and_then(|home| root.path.strip_prefix(home).ok())
                {
                    Some(rest) => format!("~/{}", rest.display()),
                    None => root.path.display().to_string(),
                };
                let scanned = match root.last_scan_at {
                    Some(at) => self.lang.scanned_ago(now - at),
                    None => s.folder_never_scanned.to_string(),
                };
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(ui.available_width(), 54.0), Sense::hover());
                let icon_rect = egui::Rect::from_center_size(
                    egui::pos2(rect.left() + 12.0, rect.center().y),
                    egui::Vec2::splat(20.0),
                );
                let icon_color = if root.online {
                    theme::accent(ui.visuals().dark_mode)
                } else {
                    dim
                };
                super::icons::paint(ui.painter(), icon_rect, Icon::Folder, icon_color);

                let mut controls = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(rect)
                        .layout(Layout::right_to_left(Align::Center)),
                );
                let remove_label = format!("{} — {name}", s.remove_folder);
                if widgets::icon_button(&mut controls, Icon::Trash, &remove_label, 28.0, false)
                    .clicked()
                {
                    self.dialog = Some(Dialog::RemoveFolder(
                        root.id,
                        root.path.display().to_string(),
                    ));
                }
                let reveal_label = format!("{} — {name}", s.show_in_finder);
                if widgets::icon_button(&mut controls, Icon::Search, &reveal_label, 28.0, false)
                    .clicked()
                {
                    finder::reveal(&root.path);
                }
                controls.add_space(6.0);
                if root.online {
                    controls.label(
                        RichText::new(format!(
                            "{} · {scanned}",
                            self.lang.song_count(root.track_count)
                        ))
                        .small()
                        .color(dim),
                    );
                } else {
                    controls.label(
                        RichText::new(s.folder_offline)
                            .small()
                            .color(controls.visuals().warn_fg_color),
                    );
                }
                let controls_left = controls.min_rect().left();

                let text_rect = egui::Rect::from_min_max(
                    egui::pos2(icon_rect.right() + 12.0, rect.center().y - 17.0),
                    egui::pos2(controls_left - 10.0, rect.bottom()),
                );
                let mut text_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(text_rect)
                        .layout(Layout::top_down(Align::Min)),
                );
                text_ui.spacing_mut().item_spacing.y = 0.0;
                text_ui.add(
                    egui::Label::new(RichText::new(&name).font(theme::semibold(13.5))).truncate(),
                );
                text_ui
                    .add(egui::Label::new(RichText::new(&shown_path).small().color(dim)).truncate())
                    .on_hover_text(root.path.display().to_string());
            }
        });
        widgets::section_note(ui, s.music_folders_hint);

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            let dark = ui.visuals().dark_mode;
            let ink = if dark {
                egui::Color32::from_rgb(0x14, 0x14, 0x18)
            } else {
                egui::Color32::WHITE
            };
            let add = egui::Button::new(
                RichText::new(format!("＋  {}…", s.add_music_folder))
                    .strong()
                    .color(ink),
            )
            .fill(theme::accent(dark))
            .min_size(egui::vec2(0.0, 30.0));
            if ui.add(add).clicked() {
                self.pick_folder();
            }
            let rescan = egui::Button::new(s.rescan).min_size(egui::vec2(0.0, 30.0));
            if ui
                .add_enabled(!scan.running && !roots.is_empty(), rescan)
                .clicked()
            {
                self.announce_scan = true;
                self.core.library.rescan();
            }
            if scan.running {
                ui.add_space(4.0);
                ui.add(egui::Spinner::new().size(14.0));
                ui.label(
                    RichText::new(self.lang.scan_progress(scan.files_done))
                        .small()
                        .color(dim),
                );
            }
        });
    }

    fn settings_playback(&mut self, ui: &mut egui::Ui) {
        let s = self.s();
        self.refresh_devices();
        let player = self.core.player.clone();

        widgets::section_title(ui, s.settings_section_output);
        let before = self.settings.output_device.clone();
        let selected_text = match &before {
            None => s.output_system_default.to_string(),
            Some(id) => match self.devices.iter().find(|d| &d.id == id) {
                Some(device) => device.name.clone(),
                None => format!("{id} {}", s.output_missing),
            },
        };
        let now_playing_on = match &self.player.output_device {
            Some(name) => self.lang.output_now(name, self.player.output_sample_rate),
            None => s.output_device_desc_idle.to_string(),
        };
        widgets::card(ui, |ui| {
            widgets::setting_row(ui, s.output_device, Some(&now_playing_on), true, |ui| {
                egui::ComboBox::from_id_salt("output-device")
                    .selected_text(selected_text)
                    .width(240.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.settings.output_device,
                            None,
                            s.output_system_default,
                        );
                        for device in &self.devices {
                            ui.selectable_value(
                                &mut self.settings.output_device,
                                Some(device.id.clone()),
                                &device.name,
                            );
                        }
                    });
            });
        });
        if self.settings.output_device != before {
            player.send(Command::SetOutputDevice(
                self.settings.output_device.clone(),
            ));
        }
        widgets::section_note(ui, s.output_hint);

        ui.add_space(14.0);
        widgets::section_title(ui, s.settings_section_quality);
        let before = self.settings.output;
        widgets::card(ui, |ui| {
            // Changing the device's rate and taking it exclusively is macOS-only.
            let device_control = crate::platform::DEVICE_CONTROL;
            let describe = |text: &'static str| {
                Some(if device_control {
                    text
                } else {
                    s.out_macos_only
                })
            };
            widgets::setting_row(
                ui,
                s.out_match_rate,
                describe(s.out_match_rate_desc),
                device_control,
                |ui| {
                    widgets::toggle(
                        ui,
                        &mut self.settings.output.match_sample_rate,
                        s.out_match_rate,
                    );
                },
            );
            widgets::row_divider(ui);
            widgets::setting_row(
                ui,
                s.out_exclusive,
                describe(s.out_exclusive_desc),
                device_control,
                |ui| {
                    widgets::toggle(ui, &mut self.settings.output.exclusive, s.out_exclusive);
                },
            );
            widgets::row_divider(ui);
            widgets::setting_row(ui, s.out_memory, Some(s.out_memory_desc), true, |ui| {
                widgets::toggle(ui, &mut self.settings.output.memory_playback, s.out_memory);
            });
        });
        widgets::section_note(ui, s.out_quality_hint);
        if self.settings.output != before {
            player.send(Command::SetOutput(self.settings.output));
        }

        ui.add_space(14.0);
        widgets::section_title(ui, s.replaygain);
        let before = self.settings.replaygain;
        widgets::card(ui, |ui| {
            widgets::setting_row(ui, s.rg_mode, Some(s.rg_mode_desc), true, |ui| {
                widgets::segmented(
                    ui,
                    "replaygain-mode",
                    &mut self.settings.replaygain.mode,
                    &[
                        (ReplayGainMode::Off, s.rg_off),
                        (ReplayGainMode::Track, s.rg_track),
                        (ReplayGainMode::Album, s.rg_album),
                    ],
                );
            });
            let enabled = self.settings.replaygain.mode != ReplayGainMode::Off;
            widgets::row_divider(ui);
            widgets::setting_row(ui, s.rg_preamp, Some(s.rg_preamp_desc), enabled, |ui| {
                let value = self.settings.replaygain.preamp_db;
                ui.allocate_ui_with_layout(
                    egui::vec2(58.0, 20.0),
                    Layout::right_to_left(Align::Center),
                    |ui| {
                        ui.label(RichText::new(format!("{value:+.1} dB")).small());
                    },
                );
                ui.spacing_mut().slider_width = 150.0;
                let slider =
                    egui::Slider::new(&mut self.settings.replaygain.preamp_db, -12.0..=12.0)
                        .step_by(0.5)
                        .show_value(false);
                let response = ui.add(slider);
                response.widget_info(|| {
                    egui::WidgetInfo::slider(enabled, f64::from(value), s.rg_preamp)
                });
                // Double-click returns to no extra gain.
                if response.double_clicked() {
                    self.settings.replaygain.preamp_db = 0.0;
                }
            });
            widgets::row_divider(ui);
            widgets::setting_row(
                ui,
                s.rg_prevent_clipping,
                Some(s.rg_clip_desc),
                enabled,
                |ui| {
                    widgets::toggle(
                        ui,
                        &mut self.settings.replaygain.prevent_clipping,
                        s.rg_prevent_clipping,
                    );
                },
            );
        });
        widgets::section_note(ui, s.rg_hint);
        if self.settings.replaygain != before {
            player.send(Command::SetReplayGain(self.settings.replaygain));
        }

        ui.add_space(14.0);
        widgets::section_title(ui, s.equalizer);
        let mut open_equalizer = false;
        widgets::card(ui, |ui| {
            let state = if self.settings.eq.is_active() {
                s.eq_row_on
            } else {
                s.eq_row_off
            };
            widgets::setting_row(ui, state, Some(s.eq_row_desc), true, |ui| {
                let button = egui::Button::new(s.open_guide).min_size(egui::vec2(0.0, 26.0));
                open_equalizer = ui.add(button).clicked();
            });
        });
        if open_equalizer {
            self.open_equalizer();
        }

        ui.add_space(14.0);
        widgets::section_title(ui, s.crossfeed);
        let crossfeed_before = self.settings.crossfeed;
        widgets::card(ui, |ui| {
            widgets::setting_row(ui, s.crossfeed_row, Some(s.crossfeed_desc), true, |ui| {
                use crate::audio::Crossfeed;
                widgets::segmented(
                    ui,
                    "crossfeed",
                    &mut self.settings.crossfeed,
                    &[
                        (Crossfeed::Off, s.rg_off),
                        (Crossfeed::Light, s.crossfeed_light),
                        (Crossfeed::Medium, s.crossfeed_medium),
                        (Crossfeed::Strong, s.crossfeed_strong),
                    ],
                );
            });
        });
        widgets::section_note(ui, s.crossfeed_hint);
        if self.settings.crossfeed != crossfeed_before {
            player.send(Command::SetCrossfeed(self.settings.crossfeed));
        }

        ui.add_space(14.0);
        widgets::section_title(ui, s.radio);
        let radio_before = self.settings.radio;
        widgets::card(ui, |ui| {
            widgets::setting_row(ui, s.radio_row, Some(s.radio_desc), true, |ui| {
                widgets::toggle(ui, &mut self.settings.radio, s.radio_row);
            });
        });
        widgets::section_note(ui, s.radio_hint);
        if self.settings.radio != radio_before {
            player.send(Command::SetRadio(self.settings.radio));
        }
    }

    /// A reference of the keyboard shortcuts, grouped the way people look for them.
    fn settings_shortcuts(&mut self, ui: &mut egui::Ui) {
        let s = self.s();
        let groups: [(&str, &[(&str, &str)]); 4] = [
            (
                s.settings_playback,
                &[
                    ("Space", s.menu_play_pause),
                    ("⌘ →", s.menu_next),
                    ("⌘ ←", s.menu_previous),
                    ("⌘ ↑", s.menu_volume_up),
                    ("⌘ ↓", s.menu_volume_down),
                    ("⇧ ⌘ M", s.menu_mute),
                    ("⌘ S", s.menu_toggle_shuffle),
                    ("⇧ ⌘ R", s.menu_cycle_repeat),
                ],
            ),
            (
                s.menu_song,
                &[
                    ("Return", s.shortcut_play_selected),
                    ("⌘ Return", s.play_next),
                    ("⇧ ⌘ Return", s.add_to_queue),
                    ("⌘ D", s.menu_song_favorite),
                    ("⇧ ⌘ F", s.show_in_finder),
                    ("⌥ ⌘ I", s.song_info),
                    ("↑ ↓   ⇧ ↑ ↓", s.shortcut_move_selection),
                    ("⌘ A", s.menu_select_all),
                    ("Delete", s.shortcut_remove),
                    ("⌥ ↑   ⌥ ↓", s.shortcut_reorder_queue),
                ],
            ),
            (
                s.library,
                &[
                    ("⌘ F", s.menu_find),
                    ("Esc", s.shortcut_clear_search),
                    ("⌘ L", s.menu_go_to_current),
                    ("⌘ O", s.menu_open_files),
                    ("⇧ ⌘ O", s.menu_add_folder),
                    ("⌘ N", s.menu_new_playlist),
                    ("⌘ R", s.menu_rescan),
                ],
            ),
            (
                s.shortcuts_group_window,
                &[
                    ("⌘ U", s.menu_show_queue),
                    ("⌘ I", s.menu_show_info),
                    ("⇧ ⌘ U", s.menu_show_meter),
                    ("⌘ ,", s.menu_settings),
                    ("⌘ W", s.menu_close_window),
                    ("⌘ Q", s.menu_quit),
                ],
            ),
        ];
        for (index, (title, rows)) in groups.iter().enumerate() {
            if index > 0 {
                ui.add_space(12.0);
            }
            widgets::section_title(ui, title);
            widgets::card(ui, |ui| {
                for (row, (keys, what)) in rows.iter().enumerate() {
                    if row > 0 {
                        widgets::row_divider(ui);
                    }
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 34.0),
                        Sense::hover(),
                    );
                    let mut keys_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(rect)
                            .layout(Layout::right_to_left(Align::Center)),
                    );
                    widgets::key_caps(&mut keys_ui, keys);
                    let left = keys_ui.min_rect().left();
                    let text_rect = egui::Rect::from_min_max(
                        egui::pos2(rect.left(), rect.center().y - 7.0),
                        egui::pos2(left - 10.0, rect.bottom()),
                    );
                    let mut text_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(text_rect)
                            .layout(Layout::top_down(Align::Min)),
                    );
                    // Menu titles end in "…" where they open a panel; a reference does not.
                    text_ui.add(egui::Label::new(what.trim_end_matches('…')).truncate());
                }
            });
        }
        widgets::section_note(ui, s.shortcuts_note);
    }

    fn settings_about(&mut self, ui: &mut egui::Ui) {
        let s = self.s();
        let dim = theme::dim(ui);
        if self.about_icon.is_none()
            && let Ok(image) = image::load_from_memory(APP_ICON_PNG)
        {
            let rgba = image.to_rgba8();
            let size = [rgba.width() as usize, rgba.height() as usize];
            let pixels = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
            self.about_icon = Some(ui.ctx().load_texture(
                "app-icon",
                pixels,
                egui::TextureOptions::LINEAR,
            ));
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(76.0), Sense::hover());
            if let Some(texture) = &self.about_icon {
                egui::Image::new(egui::load::SizedTexture::new(texture.id(), rect.size()))
                    .paint_at(ui, rect);
            }
            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.add_space(8.0);
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.label(
                    RichText::new(s.app_name)
                        .font(theme::semibold(22.0))
                        .color(ui.visuals().strong_text_color()),
                );
                ui.label(
                    RichText::new(format!("{} {}", s.version, crate::app::VERSION)).color(dim),
                );
                ui.label(RichText::new(s.about_body).small().color(dim));
            });
        });

        ui.add_space(14.0);
        let mut open_guide = false;
        let log_dir = self.core.paths.log_dir.clone();
        let data_dir = self.core.paths.data_dir.clone();
        widgets::card(ui, |ui| {
            let button = |ui: &mut egui::Ui, text: &str| {
                ui.add(egui::Button::new(text).min_size(egui::vec2(0.0, 26.0)))
                    .clicked()
            };
            widgets::setting_row(
                ui,
                s.menu_user_guide,
                Some(s.about_guide_desc),
                true,
                |ui| {
                    open_guide = button(ui, s.open_guide);
                },
            );
            widgets::row_divider(ui);
            widgets::setting_row(ui, s.log_folder, Some(s.log_folder_desc), true, |ui| {
                if button(ui, s.show_in_finder) {
                    finder::reveal(&log_dir);
                }
            });
            widgets::row_divider(ui);
            widgets::setting_row(ui, s.data_folder, Some(s.data_folder_desc), true, |ui| {
                if button(ui, s.show_in_finder) {
                    finder::reveal(&data_dir);
                }
            });
            widgets::row_divider(ui);
            let toggle = if self.licenses_shown {
                s.licenses_hide
            } else {
                s.licenses_show
            };
            widgets::setting_row(ui, s.licenses, Some(s.licenses_desc), true, |ui| {
                if button(ui, toggle) {
                    self.licenses_shown = !self.licenses_shown;
                }
            });
        });

        if open_guide {
            self.open_guide();
        }

        if self.licenses_shown {
            // The notices themselves (THIRD_PARTY_NOTICES.md), below the card.
            ui.add_space(14.0);
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                markdown::show(ui, license_notices(), NOTICES_LOOK, |_, _| {});
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use markdown::{Block, Span, web_address};

    fn plain(spans: &[Span]) -> String {
        spans.iter().map(|s| s.text.as_str()).collect()
    }

    /// The notices that ship: all of the document is understood, the table is whole.
    #[test]
    fn the_open_source_notices_are_fully_understood() {
        let blocks = license_notices();
        let headings: Vec<&str> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Heading(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(headings, ["License summary", "Crates"]);
        let tables: Vec<_> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Table { head, rows } => Some((head, rows)),
                _ => None,
            })
            .collect();
        let [(head, rows)] = tables[..] else {
            panic!("one table expected, found {}", tables.len());
        };
        let names: Vec<String> = head.iter().map(|c| plain(c)).collect();
        assert_eq!(names, ["Crate", "Version", "License", "Source"]);
        // As many rows as the document says there are crates.
        let stated = LICENSE_NOTICES
            .lines()
            .find_map(|l| l.strip_prefix("Crates: "))
            .and_then(|n| n.trim().parse::<usize>().ok())
            .expect("the crate count is stated");
        assert_eq!(rows.len(), stated);
        assert!(rows.len() > 100);
        for row in rows {
            assert_eq!(row.len(), 4, "{row:?}");
            assert!(row.iter().all(|cell| !plain(cell).is_empty()), "{row:?}");
            // Where the crate comes from is a link.
            assert!(web_address(&row[3]).is_some(), "{row:?}");
        }
        // One line per license in the summary, each with the license as code.
        let licenses = blocks
            .iter()
            .filter(
                |b| matches!(b, Block::Item { text, .. } if text.first().is_some_and(|s| s.code)),
            )
            .count();
        assert!(licenses >= 10);
        // Nothing is left as markup.
        for block in blocks {
            let texts: Vec<String> = match block {
                Block::Title(t) | Block::Heading(t) => vec![t.clone()],
                Block::Paragraph(s) | Block::Item { text: s, .. } => vec![plain(s)],
                Block::Table { head, rows } => head
                    .iter()
                    .chain(rows.iter().flatten())
                    .map(|c| plain(c))
                    .collect(),
            };
            for text in texts {
                assert!(
                    !text.contains('`') && !text.contains("**") && !text.starts_with(['#', '|']),
                    "markup left in: {text}"
                );
            }
        }
    }
}
