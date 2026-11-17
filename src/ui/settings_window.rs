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

/// The playback speeds offered, in percent of a song's own.
const SPEEDS: [u16; 6] = [50, 75, 100, 125, 150, 200];
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

/// Size of the window where there is room for it. In a small window, or one zoomed in
/// (⌘+), it shrinks to what there is and its body scrolls.
const WIDTH: f32 = 620.0;
const BODY_HEIGHT: f32 = 404.0;
const MIN_WIDTH: f32 = 360.0;
const MIN_BODY_HEIGHT: f32 = 120.0;
/// Space kept free around the window when it has to shrink.
const EDGE: f32 = 12.0;
/// Where there is room the window sits this much above the centre.
const LIFT: f32 = 20.0;
/// Height of the header (title, tabs, divider) until it has been measured.
const HEADER_GUESS: f32 = 92.0;
const SIDE_MARGIN: f32 = 20.0;

/// The settings window's width, body height and lift above the centre for a window
/// content area of `screen` points, with a header `header` points tall.
fn fit(screen: egui::Vec2, header: f32) -> (f32, f32, f32) {
    let width = WIDTH.min(screen.x - 2.0 * EDGE).max(MIN_WIDTH);
    let room = screen.y - 2.0 * EDGE - header;
    let body = BODY_HEIGHT.min(room).max(MIN_BODY_HEIGHT);
    let lift = ((room - body) / 2.0).clamp(0.0, LIFT);
    (width, body, lift)
}

/// A table of keys and what they do, in groups under their titles.
fn shortcut_table(ui: &mut egui::Ui, groups: &[(&str, &[(&str, &str)])]) {
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
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(ui.available_width(), 34.0), Sense::hover());
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
}

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
        // The header's height depends on the font size; it is measured as it is laid out
        // and used from the next frame on.
        let header_id = egui::Id::unique("settings-header-height");
        let header = ctx
            .data(|d| d.get_temp::<f32>(header_id))
            .unwrap_or(HEADER_GUESS);
        let (width, body_height, lift) = fit(screen.size(), header);
        egui::Window::new(s.settings)
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .frame(frame)
            .fixed_size([width, 0.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -lift])
            .show(ctx, |ui| {
                ui.set_width(width);
                ui.spacing_mut().item_spacing.y = 0.0;
                let top = ui.cursor().top();
                close |= self.settings_header(ui);
                widgets::row_divider(ui);
                let measured = ui.cursor().top() - top;
                if (measured - header).abs() > 0.5 {
                    ctx.data_mut(|d| d.insert_temp(header_id, measured));
                    ctx.request_repaint();
                }
                egui::ScrollArea::vertical()
                    .id_salt(("settings-body", self.settings_tab))
                    .max_height(body_height)
                    .min_scrolled_height(body_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            ui.add_space(SIDE_MARGIN);
                            ui.vertical(|ui| {
                                ui.set_width(width - SIDE_MARGIN * 2.0);
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

    pub(super) fn settings_general(&mut self, ui: &mut egui::Ui) {
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

        // The song table and its columns are the desk's.
        if !self.phone_mode {
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

        // Lyrics from the internet: the one thing here that asks something of a
        // service out there, and only for who switches it on.
        ui.add_space(14.0);
        widgets::section_title(ui, s.info_lyrics);
        let mut online = self.settings.online_lyrics;
        widgets::card(ui, |ui| {
            widgets::setting_row(
                ui,
                s.online_lyrics_row,
                Some(s.online_lyrics_desc),
                true,
                |ui| {
                    widgets::toggle(ui, &mut online, s.online_lyrics_row);
                },
            );
        });
        widgets::section_note(ui, s.online_lyrics_hint);
        if online != self.settings.online_lyrics {
            self.set_online_lyrics(online);
        }
    }

    pub(super) fn settings_library(&mut self, ui: &mut egui::Ui) {
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

        // Songs that were removed stay out of every scan: this is the way back. Always
        // there, also with nothing to bring back: a row that comes and goes is not found.
        let removed = self.core.library.removed_count();
        ui.add_space(12.0);
        widgets::card(ui, |ui| {
            widgets::setting_row(
                ui,
                s.restore_removed,
                Some(s.restore_removed_desc),
                true,
                |ui| {
                    let button =
                        egui::Button::new(s.restore_removed_button).min_size(egui::vec2(0.0, 26.0));
                    if ui
                        .add_enabled(removed > 0 && !scan.running, button)
                        .clicked()
                    {
                        self.announce_scan = true;
                        self.core.library.restore_removed();
                    }
                    ui.label(
                        RichText::new(self.lang.song_count(removed))
                            .small()
                            .color(dim),
                    );
                },
            );
        });

        ui.add_space(18.0);
        if self.check_card(ui) {
            self.settings_open = false;
            self.show_search(super::Nav::Songs, "check:damaged");
        }

        ui.add_space(14.0);
        widgets::section_title(ui, s.import_title);
        let mut import = false;
        widgets::card(ui, |ui| {
            widgets::setting_row(
                ui,
                s.apple_import_row,
                Some(s.apple_import_desc),
                true,
                |ui| {
                    let button =
                        egui::Button::new(s.apple_import_button).min_size(egui::vec2(0.0, 26.0));
                    import = ui.add(button).clicked();
                },
            );
        });
        widgets::section_note(ui, s.apple_import_hint);
        if import {
            self.pick_apple_library();
        }

        // The library's own data, to keep or to take to another device.
        ui.add_space(14.0);
        widgets::section_title(ui, s.data_title);
        let (mut export_data, mut import_data) = (false, false);
        widgets::card(ui, |ui| {
            widgets::setting_row(
                ui,
                s.data_export_row,
                Some(s.data_export_desc),
                true,
                |ui| {
                    let button =
                        egui::Button::new(s.data_export_button).min_size(egui::vec2(0.0, 26.0));
                    export_data = ui.add(button).clicked();
                },
            );
            widgets::row_divider(ui);
            widgets::setting_row(
                ui,
                s.data_import_row,
                Some(s.data_import_desc),
                true,
                |ui| {
                    let button =
                        egui::Button::new(s.apple_import_button).min_size(egui::vec2(0.0, 26.0));
                    import_data = ui.add(button).clicked();
                },
            );
        });
        widgets::section_note(ui, s.data_hint);
        if export_data {
            self.pick_data_export();
        }
        if import_data {
            self.pick_data_import();
        }
    }

    /// The file check: start or stop it, what it found, and the way to the damaged files.
    /// True when the damaged files were asked for: where they are shown is the caller's.
    pub(super) fn check_card(&mut self, ui: &mut egui::Ui) -> bool {
        let s = self.s();
        let library = self.core.library.clone();
        let status = library.check_status();
        let (mut waiting, mut checked, mut damaged) = (0usize, 0usize, 0usize);
        for track in &self.snapshot.tracks {
            match track.check {
                Some(check) => {
                    checked += 1;
                    damaged += usize::from(check.verdict == crate::model::CheckVerdict::Damaged);
                }
                None => waiting += usize::from(track.available),
            }
        }
        let text = if status.running {
            self.lang.check_progress(
                true,
                status.done,
                status.total,
                status.damaged,
                status.failed,
            )
        } else if waiting > 0 {
            self.lang.check_waiting(waiting)
        } else if status.done > 0 {
            self.lang.check_progress(
                false,
                status.done,
                status.total,
                status.damaged,
                status.failed,
            )
        } else {
            s.check_none.to_string()
        };
        widgets::section_title(ui, s.check_title);
        let mut show_damaged = false;
        widgets::card(ui, |ui| {
            widgets::setting_row(ui, s.check_row, Some(&text), true, |ui| {
                let label = if status.running {
                    s.check_stop
                } else {
                    s.check_start
                };
                let button = egui::Button::new(label).min_size(egui::vec2(0.0, 26.0));
                if ui
                    .add_enabled(status.running || waiting > 0, button)
                    .clicked()
                {
                    if status.running {
                        library.cancel_check();
                    } else {
                        library.check_files();
                    }
                }
            });
            if status.running {
                let fraction = status.done as f32 / status.total.max(1) as f32;
                ui.add(egui::ProgressBar::new(fraction).desired_height(4.0));
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(250));
            }
            if checked > 0 {
                widgets::row_divider(ui);
                let summary = format!(
                    "{} · {}",
                    self.lang.song_count(checked),
                    self.lang
                        .check_progress(false, checked as u64, checked as u64, damaged as u64, 0)
                        .split(" · ")
                        .nth(1)
                        .unwrap_or_default()
                        .trim_end_matches('.')
                );
                widgets::setting_row(ui, s.check_results, Some(&summary), !status.running, |ui| {
                    let forget = egui::Button::new(s.check_forget).min_size(egui::vec2(0.0, 26.0));
                    if ui.add(forget).clicked() {
                        // Waits for the database: on the edit thread, not in a frame.
                        self.edit(|library| {
                            library.forget_checks();
                            None
                        });
                    }
                    if damaged > 0 {
                        let show =
                            egui::Button::new(s.check_show_damaged).min_size(egui::vec2(0.0, 26.0));
                        if ui.add(show).clicked() {
                            show_damaged = true;
                        }
                    }
                });
            }
        });
        widgets::section_note(ui, s.check_hint);
        show_damaged
    }

    /// Which output device to play on.
    fn settings_output_device(&mut self, ui: &mut egui::Ui) {
        let s = self.s();
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
    }

    pub(super) fn settings_playback(&mut self, ui: &mut egui::Ui) {
        let s = self.s();
        let player = self.core.player.clone();
        // A phone sends its sound where the system says (speaker, headphones, AirPlay):
        // there is no device to choose, and none whose rate or access could be set.
        let phone = crate::platform::PHONE;
        if !phone {
            self.refresh_devices();
            self.settings_output_device(ui);
            ui.add_space(14.0);
        }
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
            if phone && !crate::platform::RATE_CONTROL {
                // The system cannot be asked for a rate (Android): nothing to switch.
            } else if phone {
                // The system is asked for the song's rate and decides: the built-in
                // output follows between 44.1 and 48 kHz, a USB DAC as far as it can.
                widgets::setting_row(
                    ui,
                    s.out_match_rate,
                    Some(s.phone_match_rate_desc),
                    true,
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
                    s.out_upsample,
                    Some(s.phone_upsample_desc),
                    true,
                    |ui| {
                        widgets::toggle(ui, &mut self.settings.output.upsample, s.out_upsample);
                    },
                );
                widgets::row_divider(ui);
            } else {
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
                widgets::setting_row(
                    ui,
                    s.out_upsample,
                    describe(s.out_upsample_desc),
                    device_control,
                    |ui| {
                        widgets::toggle(ui, &mut self.settings.output.upsample, s.out_upsample);
                    },
                );
                widgets::row_divider(ui);
            }
            // (Where the system cannot be asked for a rate the output is never at a
            // whole multiple of a song's: the filter would have nothing to do.)
            if !phone || crate::platform::RATE_CONTROL {
                widgets::setting_row(ui, s.out_filter, Some(s.out_filter_desc), true, |ui| {
                    use crate::audio::upsample::Filter;
                    widgets::segmented(
                        ui,
                        "upsampling-filter",
                        &mut self.settings.output.filter,
                        &[
                            (Filter::Sharp, s.filter_sharp),
                            (Filter::Slow, s.filter_slow),
                            (Filter::Minimum, s.filter_minimum),
                        ],
                    );
                });
                widgets::row_divider(ui);
            }
            widgets::setting_row(ui, s.out_memory, Some(s.out_memory_desc), true, |ui| {
                widgets::toggle(ui, &mut self.settings.output.memory_playback, s.out_memory);
            });
        });
        if !phone || crate::platform::RATE_CONTROL {
            widgets::section_note(ui, s.out_filter_hint);
        }
        if phone && !crate::platform::RATE_CONTROL {
            widgets::section_note(ui, s.phone_quality_hint_fixed);
        } else if phone {
            widgets::section_note(ui, s.phone_quality_hint);
        } else {
            widgets::section_note(ui, s.out_quality_hint);
        }
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
        if self.settings.replaygain != before {
            player.send(Command::SetReplayGain(self.settings.replaygain));
        }
        self.loudness_card(ui);
        widgets::section_note(ui, s.rg_hint);
        self.room_card(ui);

        ui.add_space(14.0);
        widgets::section_title(ui, s.play_order);
        let order_before = (
            self.settings.shuffle_by_album,
            self.settings.resume_long_tracks,
        );
        let by_rating_before = self.settings.shuffle_by_rating;
        widgets::card(ui, |ui| {
            widgets::setting_row(
                ui,
                s.shuffle_by_album_row,
                Some(s.shuffle_by_album_desc),
                true,
                |ui| {
                    widgets::toggle(
                        ui,
                        &mut self.settings.shuffle_by_album,
                        s.shuffle_by_album_row,
                    );
                },
            );
            widgets::row_divider(ui);
            widgets::setting_row(
                ui,
                s.resume_long_row,
                Some(s.resume_long_desc),
                true,
                |ui| {
                    widgets::toggle(ui, &mut self.settings.resume_long_tracks, s.resume_long_row);
                },
            );
            widgets::row_divider(ui);
            widgets::setting_row(
                ui,
                s.shuffle_by_rating_row,
                Some(s.shuffle_by_rating_desc),
                true,
                |ui| {
                    widgets::toggle(
                        ui,
                        &mut self.settings.shuffle_by_rating,
                        s.shuffle_by_rating_row,
                    );
                },
            );
        });
        widgets::section_note(ui, s.play_order_hint);
        if self.settings.shuffle_by_rating != by_rating_before {
            player.send(Command::SetShuffleByRating(self.settings.shuffle_by_rating));
        }
        if self.settings.shuffle_by_album != order_before.0 {
            player.send(Command::SetShuffleByAlbum(self.settings.shuffle_by_album));
        }
        if self.settings.resume_long_tracks != order_before.1 {
            player.send(Command::SetResumeLongTracks(
                self.settings.resume_long_tracks,
            ));
        }

        ui.add_space(14.0);
        // Crossfade between songs that follow each other by themselves.
        widgets::section_title(ui, s.crossfade);
        let crossfade_before = self.settings.crossfade_secs;
        widgets::card(ui, |ui| {
            widgets::setting_row(ui, s.crossfade_row, Some(s.crossfade_desc), true, |ui| {
                widgets::segmented(
                    ui,
                    "crossfade",
                    &mut self.settings.crossfade_secs,
                    &[
                        (0, s.rg_off),
                        (2, s.crossfade_2),
                        (4, s.crossfade_4),
                        (6, s.crossfade_6),
                        (10, s.crossfade_10),
                    ],
                );
            });
        });
        widgets::section_note(ui, s.crossfade_hint);
        if self.settings.crossfade_secs != crossfade_before {
            player.send(Command::SetCrossfade(self.settings.crossfade_secs));
        }

        ui.add_space(14.0);
        // How fast songs go, at their own pitch.
        widgets::section_title(ui, s.speed);
        let speed_before = self.settings.speed_percent;
        let speeds = SPEEDS.map(|percent| (percent, crate::i18n::Lang::speed_label(percent)));
        let choices: Vec<(u16, &str)> = speeds
            .iter()
            .map(|(percent, label)| (*percent, label.as_str()))
            .collect();
        widgets::card(ui, |ui| {
            widgets::setting_row(ui, s.speed_row, Some(s.speed_desc), true, |ui| {
                widgets::segmented(ui, "speed", &mut self.settings.speed_percent, &choices);
            });
        });
        widgets::section_note(ui, s.speed_hint);
        if self.settings.speed_percent != speed_before {
            player.send(Command::SetSpeed(self.settings.speed_percent));
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
        self.correction_card(ui);

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

    /// Room correction: the impulse response that was imported, whether it is
    /// applied, importing another and taking it away.
    fn room_card(&mut self, ui: &mut egui::Ui) {
        let s = self.s();
        ui.add_space(14.0);
        widgets::section_title(ui, s.room);
        let kept = self.settings.room.clone();
        let (mut import, mut remove) = (false, false);
        let mut on = kept.as_ref().is_some_and(|room| room.enabled);
        widgets::card(ui, |ui| {
            let desc = kept
                .as_ref()
                .map_or(s.room_none_desc, |room| room.name.as_str());
            widgets::setting_row(ui, s.room_row, Some(desc), kept.is_some(), |ui| {
                ui.add_enabled_ui(kept.is_some(), |ui| {
                    widgets::toggle(ui, &mut on, s.room_row);
                });
            });
            widgets::row_divider(ui);
            widgets::setting_row(ui, s.room_file_row, Some(s.room_file_desc), true, |ui| {
                let button = |text: &str| egui::Button::new(text).min_size(egui::vec2(0.0, 26.0));
                import = ui.add(button(s.correction_import)).clicked();
                remove = ui.add_enabled(kept.is_some(), button(s.delete)).clicked();
            });
        });
        widgets::section_note(ui, s.room_hint);
        if import {
            self.pick_room_file();
        } else if remove {
            self.remove_room();
        } else if kept.is_some_and(|room| room.enabled != on) {
            self.set_room_enabled(on);
        }
    }

    /// Headphone correction: which imported profile is applied, importing a file, and
    /// removing the selected profile.
    fn correction_card(&mut self, ui: &mut egui::Ui) {
        let s = self.s();
        let player = self.core.player.clone();
        widgets::section_title(ui, s.correction);
        let before = self.settings.correction.clone();
        let rate = self.player.output_sample_rate.unwrap_or(48_000);
        let summary = self.settings.active_correction().map(|p| {
            self.lang
                .correction_summary(p.filters.len(), p.preamp_db, p.peak_db(rate))
        });
        let mut import = false;
        let mut remove = false;
        // (begin anew, begin from the selected profile)
        let mut edit = (false, false);
        let editing = self.peq_edit.is_some();
        widgets::card(ui, |ui| {
            let desc = summary.as_deref().unwrap_or(s.correction_none_desc);
            widgets::setting_row(ui, s.correction_row, Some(desc), true, |ui| {
                let selected = before.clone().unwrap_or_else(|| s.rg_off.to_string());
                // (While a profile is being made on screen, that one is heard:
                // another cannot be chosen under it.)
                ui.add_enabled_ui(!editing, |ui| {
                    egui::ComboBox::from_id_salt("correction-profile")
                        .selected_text(selected)
                        .width(200.0)
                        .height(320.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.settings.correction, None, s.rg_off);
                            let group = |ui: &mut egui::Ui, title: &str| {
                                ui.separator();
                                ui.label(RichText::new(title).small().weak());
                            };
                            // The user's own first, then the ones that come with the app.
                            let imported: Vec<String> = self
                                .settings
                                .corrections
                                .iter()
                                .map(|p| p.name.clone())
                                .collect();
                            if !imported.is_empty() {
                                group(ui, s.correction_group_imported);
                            }
                            for name in &imported {
                                ui.selectable_value(
                                    &mut self.settings.correction,
                                    Some(name.clone()),
                                    name,
                                );
                            }
                            group(ui, s.correction_group_builtin);
                            for profile in crate::audio::peq::builtin_profiles() {
                                // One of the same name that the user imported stands for it.
                                if imported.contains(&profile.name) {
                                    continue;
                                }
                                ui.selectable_value(
                                    &mut self.settings.correction,
                                    Some(profile.name.clone()),
                                    &profile.name,
                                );
                            }
                        });
                });
            });
            widgets::row_divider(ui);
            widgets::setting_row(
                ui,
                s.correction_file_row,
                Some(s.correction_file_desc),
                true,
                |ui| {
                    let button =
                        |text: &str| egui::Button::new(text).min_size(egui::vec2(0.0, 26.0));
                    import = ui
                        .add_enabled(!editing, button(s.correction_import))
                        .clicked();
                    // Removes the selected profile, if it is one that was imported.
                    remove = ui
                        .add_enabled(
                            self.settings.correction_is_imported() && !editing,
                            button(s.delete),
                        )
                        .clicked();
                },
            );
            widgets::row_divider(ui);
            widgets::setting_row(
                ui,
                s.correction_edit_row,
                Some(s.correction_edit_desc),
                true,
                |ui| {
                    let button =
                        |text: &str| egui::Button::new(text).min_size(egui::vec2(0.0, 26.0));
                    edit.0 = ui.add_enabled(!editing, button(s.correction_new)).clicked();
                    edit.1 = ui
                        .add_enabled(
                            !editing && self.settings.correction.is_some(),
                            button(s.correction_edit),
                        )
                        .clicked();
                },
            );
        });
        if edit.0 || edit.1 {
            self.peq_edit_begin(edit.1);
        }
        self.correction_editor(ui, rate);
        widgets::section_note(ui, s.correction_hint);
        if remove && let Some(name) = &before {
            self.settings.corrections.retain(|p| &p.name != name);
            // A built-in profile of the same name stays available; nothing is selected.
            self.settings.correction = None;
        }
        // (While a profile is edited, what is edited is what is heard.)
        if self.settings.correction != before && self.peq_edit.is_none() {
            player.send(Command::SetCorrection(
                self.settings.active_correction().cloned(),
            ));
        }
        if import {
            self.pick_correction_file();
        }
    }

    /// The correction profile that is being made or changed on screen: its name, its
    /// response as a curve, the preamp, and a line for each band (kind, frequency,
    /// gain, Q). Nothing here while no profile is being edited.
    fn correction_editor(&mut self, ui: &mut egui::Ui, rate: u32) {
        use crate::audio::{PeqFilter, PeqKind};
        let s = self.s();
        let Some(mut edit) = self.peq_edit.take() else {
            return;
        };
        edit.shown = true;
        let touch = super::theme::touch(ui.ctx());
        let (mut save, mut cancel) = (false, false);
        ui.add_space(10.0);
        widgets::section_title(ui, s.correction_editor);
        widgets::card(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.horizontal(|ui| {
                ui.label(RichText::new(s.correction_name).color(super::theme::dim(ui)));
                ui.add(
                    egui::TextEdit::singleline(&mut edit.profile.name)
                        .desired_width(ui.available_width().min(260.0))
                        .char_limit(crate::audio::peq::MAX_NAME_CHARS),
                );
            });
            // What the bands and the preamp do together, as the player applies them
            // (a preamp that had to give way shows here).
            let applied = edit.profile.clone().sanitized();
            let (curve, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), if touch { 130.0 } else { 150.0 }),
                egui::Sense::hover(),
            );
            super::equalizer::paint_response(ui, curve, true, |hz| applied.response_db(rate, hz));
            ui.horizontal(|ui| {
                ui.label(RichText::new(s.correction_preamp).color(super::theme::dim(ui)));
                ui.add(
                    egui::DragValue::new(&mut edit.profile.preamp_db)
                        .range(-24.0..=12.0)
                        .speed(0.1)
                        .fixed_decimals(1)
                        .suffix(" dB"),
                );
                if applied.preamp_db < edit.profile.preamp_db - 0.05 {
                    ui.label(
                        RichText::new(format!("→ {:.1} dB", applied.preamp_db))
                            .small()
                            .color(super::theme::dim(ui)),
                    );
                }
            });
            let mut removed = None;
            for (at, filter) in edit.profile.filters.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let kind = match filter.kind {
                        PeqKind::Peak => s.correction_kind_peak,
                        PeqKind::LowShelf => s.correction_kind_low,
                        PeqKind::HighShelf => s.correction_kind_high,
                    };
                    egui::ComboBox::from_id_salt(("correction-band-kind", at))
                        .selected_text(kind)
                        .width(if touch { 86.0 } else { 104.0 })
                        .show_ui(ui, |ui| {
                            for (value, label) in [
                                (PeqKind::Peak, s.correction_kind_peak),
                                (PeqKind::LowShelf, s.correction_kind_low),
                                (PeqKind::HighShelf, s.correction_kind_high),
                            ] {
                                ui.selectable_value(&mut filter.kind, value, label);
                            }
                        });
                    // (The higher the frequency, the wider a step of the drag.)
                    let step = f64::from(filter.hz) * 0.01;
                    ui.add(
                        egui::DragValue::new(&mut filter.hz)
                            .range(20.0..=20_000.0)
                            .speed(step)
                            .fixed_decimals(0)
                            .suffix(" Hz"),
                    );
                    ui.add(
                        egui::DragValue::new(&mut filter.gain_db)
                            .range(-24.0..=24.0)
                            .speed(0.1)
                            .fixed_decimals(1)
                            .suffix(" dB"),
                    );
                    ui.add(
                        egui::DragValue::new(&mut filter.q)
                            .range(0.1..=10.0)
                            .speed(0.01)
                            .fixed_decimals(2)
                            .prefix("Q "),
                    );
                    if widgets::icon_button(
                        ui,
                        super::icons::Icon::Close,
                        s.correction_remove_band,
                        if touch { 30.0 } else { 20.0 },
                        false,
                    )
                    .clicked()
                    {
                        removed = Some(at);
                    }
                });
            }
            if let Some(at) = removed {
                edit.profile.filters.remove(at);
            }
            ui.horizontal(|ui| {
                let button = |text: &str| egui::Button::new(text).min_size(egui::vec2(0.0, 26.0));
                let room = edit.profile.filters.len() < crate::audio::peq::MAX_FILTERS;
                if ui
                    .add_enabled(room, button(s.correction_add_band))
                    .clicked()
                {
                    edit.profile.filters.push(PeqFilter {
                        kind: PeqKind::Peak,
                        hz: 1_000.0,
                        gain_db: 0.0,
                        q: 1.0,
                    });
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    save = ui.add(button(s.correction_save)).clicked();
                    cancel = ui.add(button(s.cancel)).clicked();
                });
            });
        });
        widgets::section_note(ui, s.correction_edit_hint);
        self.peq_edit = Some(edit);
        if save {
            self.peq_edit_save();
        } else if cancel {
            self.peq_edit_cancel();
        } else {
            self.peq_edit_listen();
        }
    }

    /// Measuring the loudness of songs without leveling tags: how many are waiting,
    /// a button to start or stop, the progress, and a way to forget what was measured.
    fn loudness_card(&mut self, ui: &mut egui::Ui) {
        let s = self.s();
        let library = self.core.library.clone();
        let status = library.loudness_status();
        let (mut waiting, mut measured) = (0usize, 0usize);
        for track in &self.snapshot.tracks {
            measured += usize::from(track.measured.is_some());
            waiting += usize::from(track.available && track.wants_measuring());
        }
        let text = if status.running {
            self.lang
                .loudness_progress(true, status.done, status.total, status.failed)
        } else if waiting > 0 {
            self.lang.loudness_waiting(waiting)
        } else if status.done > 0 {
            self.lang
                .loudness_progress(false, status.done, status.total, status.failed)
        } else {
            s.rg_measure_none.to_string()
        };
        ui.add_space(8.0);
        widgets::card(ui, |ui| {
            widgets::setting_row(ui, s.rg_measure, Some(&text), true, |ui| {
                let label = if status.running {
                    s.rg_measure_stop
                } else {
                    s.rg_measure_start
                };
                let button = egui::Button::new(label).min_size(egui::vec2(0.0, 26.0));
                if ui
                    .add_enabled(status.running || waiting > 0, button)
                    .clicked()
                {
                    if status.running {
                        library.cancel_loudness();
                    } else {
                        library.measure_loudness();
                    }
                }
            });
            if status.running {
                let fraction = status.done as f32 / status.total.max(1) as f32;
                ui.add(egui::ProgressBar::new(fraction).desired_height(4.0));
                // The numbers move without any input.
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(250));
            }
            if measured > 0 {
                widgets::row_divider(ui);
                let count = self.lang.song_count(measured);
                widgets::setting_row(ui, s.rg_measured, Some(&count), !status.running, |ui| {
                    let button = egui::Button::new(s.rg_forget).min_size(egui::vec2(0.0, 26.0));
                    if ui.add(button).clicked() {
                        library.forget_loudness();
                    }
                });
            }
        });
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
                    ("⌥ ⌘ →", s.menu_seek_forward),
                    ("⌥ ⌘ ←", s.menu_seek_back),
                    ("⇧ ⌘ .", s.menu_stop_after_current),
                    ("⌘ B", s.shortcut_repeat_section),
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
                    ("⇧ ⌘ P", s.menu_mini_player),
                    ("⌘ E", s.equalizer),
                    ("⌘ ,", s.menu_settings),
                    ("⌘ W", s.menu_close_window),
                    ("⌘ Q", s.menu_quit),
                ],
            ),
        ];
        shortcut_table(ui, &groups);
        widgets::section_note(ui, s.shortcuts_note);
    }

    /// The keys of a keyboard that work on the phone screens (where the phone passes
    /// keys on): those of the menu commands that have a meaning there.
    pub(super) fn phone_settings_shortcuts(&mut self, ui: &mut egui::Ui) {
        let s = self.s();
        let groups: [(&str, &[(&str, &str)]); 4] = [
            (
                s.settings_playback,
                &[
                    ("Space", s.menu_play_pause),
                    ("⌘ →", s.menu_next),
                    ("⌘ ←", s.menu_previous),
                    ("⌥ ⌘ →", s.menu_seek_forward),
                    ("⌥ ⌘ ←", s.menu_seek_back),
                    ("⇧ ⌘ .", s.menu_stop_after_current),
                    ("⌘ B", s.shortcut_repeat_section),
                    ("⌘ ↑", s.menu_volume_up),
                    ("⌘ ↓", s.menu_volume_down),
                    ("⇧ ⌘ M", s.menu_mute),
                    ("⌘ S", s.menu_toggle_shuffle),
                    ("⇧ ⌘ R", s.menu_cycle_repeat),
                ],
            ),
            (
                s.phone_shortcuts_song,
                &[
                    ("⌘ D", s.menu_song_favorite),
                    ("⌘ I", s.song_info),
                    ("⌘ L", s.phone_show_in_songs),
                    ("⇧ ⌘ F", s.show_in_finder),
                ],
            ),
            (
                s.phone_shortcuts_screen,
                &[
                    ("⌘ F", s.menu_find),
                    ("⌘ U", s.menu_show_queue),
                    ("⇧ ⌘ U", s.menu_show_meter),
                    ("⌘ E", s.equalizer),
                    ("⌘ ,", s.menu_settings),
                    ("Esc", s.phone_shortcut_back),
                ],
            ),
            (
                s.library,
                &[
                    ("⌘ O", s.menu_open_files),
                    ("⇧ ⌘ O", s.menu_add_folder),
                    ("⌘ N", s.menu_new_playlist),
                    ("⌘ R", s.menu_rescan),
                ],
            ),
        ];
        shortcut_table(ui, &groups);
        widgets::section_note(ui, s.phone_shortcuts_note);
    }

    pub(super) fn settings_about(&mut self, ui: &mut egui::Ui) {
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
                    RichText::new(format!(
                        "{} {} · {} {}",
                        s.version,
                        crate::app::VERSION,
                        s.built,
                        crate::app::BUILT
                    ))
                    .color(dim),
                );
                ui.label(RichText::new(s.about_body).small().color(dim));
            });
        });

        ui.add_space(14.0);
        let mut open_guide = false;
        let mut save_log = false;
        let log_dir = self.core.paths.log_dir.clone();
        let data_dir = self.core.paths.data_dir.clone();
        widgets::card(ui, |ui| {
            let button = |ui: &mut egui::Ui, text: &str| {
                ui.add(egui::Button::new(text).min_size(egui::vec2(0.0, 26.0)))
                    .clicked()
            };
            // On a phone the guide is a page of its own in Settings, and there is no
            // file manager to show the app's folders in.
            if !self.phone_mode {
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
            }
            if !crate::platform::PHONE {
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
            } else {
                // A phone's app folders cannot be looked into: a copy of the log is
                // handed out instead.
                widgets::setting_row(ui, s.log_save, Some(s.log_folder_desc), true, |ui| {
                    save_log = button(ui, s.log_save_button);
                });
                widgets::row_divider(ui);
            }
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
        if save_log {
            self.save_log();
        }

        // A phone: what earlier runs left of their errors, to be read here (and passed
        // on) without getting a file off the phone first.
        let mut copy_record = false;
        if crate::platform::PHONE {
            let errors = self.past_errors().to_vec();
            if !errors.is_empty() {
                ui.add_space(14.0);
                widgets::card(ui, |ui| {
                    ui.label(egui::RichText::new(s.past_errors).strong());
                    ui.label(
                        egui::RichText::new(s.past_errors_desc)
                            .small()
                            .color(super::theme::dim(ui)),
                    );
                    ui.add_space(6.0);
                    // All of it at once, and more than fits here: to be pasted where
                    // the problem is reported, instead of a photo of this page.
                    copy_record |= ui
                        .add(egui::Button::new(s.copy_record).min_size(egui::vec2(0.0, 30.0)))
                        .clicked();
                    ui.add_space(6.0);
                    for line in &errors {
                        ui.add(
                            egui::Label::new(egui::RichText::new(line).monospace().small()).wrap(),
                        );
                        ui.add_space(4.0);
                    }
                });
            }
        }

        // Android has two interfaces to its graphics hardware (and the UI framework a
        // second renderer for one of them), and drivers that fail on one of them: what
        // the screen is drawn through can be seen and chosen here, and what the
        // app holds of memory is shown (a phone ran out of it, and nothing told why).
        if cfg!(target_os = "android") {
            use crate::app::settings::GraphicsChoice;
            ui.add_space(14.0);
            widgets::section_title(ui, s.graphics_section);
            let before = self.settings.graphics;
            widgets::card(ui, |ui| {
                widgets::setting_row(
                    ui,
                    s.graphics_choice,
                    Some(s.graphics_choice_desc),
                    true,
                    |ui| {
                        widgets::segmented(
                            ui,
                            "graphics",
                            &mut self.settings.graphics,
                            &[
                                (GraphicsChoice::Auto, s.graphics_auto),
                                (GraphicsChoice::Vulkan, "Vulkan"),
                                (GraphicsChoice::OpenGl, "OpenGL ES"),
                                (GraphicsChoice::Glow, "glow"),
                            ],
                        );
                    },
                );
                if let Some(adapter) = super::renderer::adapter() {
                    ui.add_space(4.0);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(format!("{}: {adapter}", s.graphics_now))
                                .small()
                                .color(super::theme::dim(ui)),
                        )
                        .wrap(),
                    );
                }
            });
            if self.settings.graphics != before {
                self.restart_for_graphics();
            }
        }
        if let Some(reading) = super::memory::latest() {
            ui.add_space(14.0);
            widgets::section_title(ui, s.memory_section);
            widgets::card(ui, |ui| {
                ui.label(
                    egui::RichText::new(s.memory_desc)
                        .small()
                        .color(super::theme::dim(ui)),
                );
                ui.add_space(6.0);
                widgets::setting_row(ui, s.copy_record, Some(s.copy_record_desc), true, |ui| {
                    copy_record |= ui
                        .add(egui::Button::new(s.copy_short).min_size(egui::vec2(0.0, 26.0)))
                        .clicked();
                });
                ui.add_space(6.0);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!(
                            "{reading} (of {} mappings)",
                            super::memory::MAPPINGS_ALLOWED
                        ))
                        .monospace()
                        .small(),
                    )
                    .wrap(),
                );
            });
            // The numbers are read every two seconds.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs(1));
        }

        if copy_record {
            let report = self.error_report();
            self.copy_text(ui.ctx(), report);
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

    #[test]
    fn the_window_fits_small_and_zoomed_windows() {
        let header = 92.0;
        // Room enough: full size, a little above the centre.
        assert_eq!(
            fit(egui::vec2(1180.0, 760.0), header),
            (WIDTH, BODY_HEIGHT, LIFT)
        );
        // The smallest main window (760 × 480): narrower body, whole window inside.
        let (width, body, lift) = fit(egui::vec2(760.0, 480.0), header);
        assert_eq!(width, WIDTH);
        assert!(header + body + 2.0 * EDGE <= 480.0, "{body}");
        assert_eq!(lift, 0.0);
        // Zoomed to 150 %: 760 × 480 pixels are about 507 × 320 points.
        let (width, body, _) = fit(egui::vec2(507.0, 320.0), header);
        assert!(width <= 507.0 - 2.0 * EDGE);
        assert!(header + body + 2.0 * EDGE <= 320.0, "{body}");
        assert!(body >= MIN_BODY_HEIGHT);
    }

    /// The notices that ship: all of the document is understood, the tables are whole.
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
        assert_eq!(headings, ["License summary", "Crates", "Data"]);
        let tables: Vec<_> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Table { head, rows } => Some((head, rows)),
                _ => None,
            })
            .collect();
        // The crates, and the data that is compiled in without being a crate.
        let [(head, rows), (data_head, data_rows)] = tables[..] else {
            panic!("two tables expected, found {}", tables.len());
        };
        let names: Vec<String> = data_head.iter().map(|c| plain(c)).collect();
        assert_eq!(names, ["Data", "License", "Source"]);
        assert_eq!(data_rows.len(), 1);
        for row in data_rows {
            assert_eq!(row.len(), 3, "{row:?}");
            assert!(plain(&row[0]).contains("AutoEq"), "{row:?}");
            assert!(plain(&row[1]).contains("Jaakko Pasanen"), "{row:?}");
            assert!(web_address(&row[2]).is_some(), "{row:?}");
        }
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
