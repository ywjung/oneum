//! The settings window. Nothing here is required to start listening.
use super::dialogs::Dialog;
use super::{OneumApp, theme};
use crate::app::settings::ThemeChoice;
use crate::audio::ReplayGainMode;
use crate::i18n::LanguageChoice;
use crate::playback::Command;
use eframe::egui::{self, RichText};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tab {
    General,
    Library,
    Playback,
    About,
}

const LICENSE_NOTICES: &str = include_str!("../../THIRD_PARTY_NOTICES.md");

impl OneumApp {
    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let s = self.s();
        let mut open = true;
        egui::Window::new(s.settings)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_size([520.0, 420.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_width(520.0);
                ui.horizontal(|ui| {
                    for (tab, label) in [
                        (Tab::General, s.settings_general),
                        (Tab::Library, s.settings_library),
                        (Tab::Playback, s.settings_playback),
                        (Tab::About, s.settings_about),
                    ] {
                        ui.selectable_value(&mut self.settings_tab, tab, label);
                    }
                });
                ui.separator();
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .max_height(380.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| match self.settings_tab {
                        Tab::General => self.settings_general(ui),
                        Tab::Library => self.settings_library(ui),
                        Tab::Playback => self.settings_playback(ui),
                        Tab::About => self.settings_about(ui),
                    });
            });
        if !open {
            self.settings_open = false;
        }
    }

    fn settings_general(&mut self, ui: &mut egui::Ui) {
        let s = self.s();
        egui::Grid::new("general")
            .num_columns(2)
            .spacing([16.0, 12.0])
            .show(ui, |ui| {
                ui.label(s.language);
                let before = self.settings.language;
                let text = match before {
                    LanguageChoice::System => s.language_system,
                    LanguageChoice::Korean => "한국어",
                    LanguageChoice::English => "English",
                };
                egui::ComboBox::from_id_salt("language")
                    .selected_text(text)
                    .width(200.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.settings.language,
                            LanguageChoice::System,
                            s.language_system,
                        );
                        ui.selectable_value(
                            &mut self.settings.language,
                            LanguageChoice::Korean,
                            "한국어",
                        );
                        ui.selectable_value(
                            &mut self.settings.language,
                            LanguageChoice::English,
                            "English",
                        );
                    });
                if self.settings.language != before {
                    self.lang = self.settings.language.resolve(self.system_prefers_korean);
                    let ctx = ui.ctx().clone();
                    self.install_menu(&ctx);
                }
                ui.end_row();

                ui.label(s.theme);
                let before = self.settings.theme;
                ui.horizontal(|ui| {
                    ui.selectable_value(
                        &mut self.settings.theme,
                        ThemeChoice::System,
                        s.theme_system,
                    );
                    ui.selectable_value(
                        &mut self.settings.theme,
                        ThemeChoice::Light,
                        s.theme_light,
                    );
                    ui.selectable_value(&mut self.settings.theme, ThemeChoice::Dark, s.theme_dark);
                });
                if self.settings.theme != before {
                    theme::apply_choice(ui.ctx(), self.settings.theme);
                }
                ui.end_row();
            });
        ui.add_space(8.0);
        ui.checkbox(&mut self.settings.show_format_column, s.show_format_column);
    }

    fn settings_library(&mut self, ui: &mut egui::Ui) {
        let s = self.s();
        ui.label(
            RichText::new(s.music_folders)
                .font(theme::semibold(13.0))
                .strong(),
        );
        ui.add_space(4.0);
        let roots = self.core.library.roots();
        if roots.is_empty() {
            ui.label(RichText::new(s.no_music_folders).color(theme::dim(ui)));
        }
        for root in roots {
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(s.remove_folder).clicked() {
                        self.dialog = Some(Dialog::RemoveFolder(
                            root.id,
                            root.path.display().to_string(),
                        ));
                    }
                    ui.label(
                        RichText::new(self.lang.song_count(root.track_count))
                            .small()
                            .color(theme::dim(ui)),
                    );
                    if !root.online {
                        ui.colored_label(ui.visuals().warn_fg_color, s.folder_offline);
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(egui::Label::new(root.path.display().to_string()).truncate());
                    });
                });
            });
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(format!("{}…", s.add_music_folder)).clicked() {
                self.pick_folder();
            }
            let scanning = self.core.library.scan_status().running;
            if ui
                .add_enabled(!scanning, egui::Button::new(s.rescan))
                .clicked()
            {
                self.announce_scan = true;
                self.core.library.rescan();
            }
            if scanning {
                ui.add(egui::Spinner::new().size(14.0));
                ui.label(RichText::new(s.scanning).small().color(theme::dim(ui)));
            }
        });
    }

    fn settings_playback(&mut self, ui: &mut egui::Ui) {
        let s = self.s();
        self.refresh_devices();
        let player = self.core.player.clone();
        ui.label(
            RichText::new(s.output_device)
                .font(theme::semibold(13.0))
                .strong(),
        );
        ui.add_space(4.0);
        let before = self.settings.output_device.clone();
        let selected_text = match &before {
            None => s.output_system_default.to_string(),
            Some(id) => match self.devices.iter().find(|d| &d.id == id) {
                Some(device) => device.name.clone(),
                None => format!("{id} {}", s.output_missing),
            },
        };
        egui::ComboBox::from_id_salt("output-device")
            .selected_text(selected_text)
            .width(340.0)
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
        if self.settings.output_device != before {
            player.send(Command::SetOutputDevice(
                self.settings.output_device.clone(),
            ));
        }
        if let Some(name) = &self.player.output_device {
            ui.label(
                RichText::new(format!("▶ {name}"))
                    .small()
                    .color(theme::dim(ui)),
            );
        }

        ui.add_space(14.0);
        ui.label(
            RichText::new(s.replaygain)
                .font(theme::semibold(13.0))
                .strong(),
        );
        ui.add_space(4.0);
        let before = self.settings.replaygain;
        ui.horizontal(|ui| {
            ui.selectable_value(
                &mut self.settings.replaygain.mode,
                ReplayGainMode::Off,
                s.rg_off,
            );
            ui.selectable_value(
                &mut self.settings.replaygain.mode,
                ReplayGainMode::Track,
                s.rg_track,
            );
            ui.selectable_value(
                &mut self.settings.replaygain.mode,
                ReplayGainMode::Album,
                s.rg_album,
            );
        });
        let enabled = self.settings.replaygain.mode != ReplayGainMode::Off;
        ui.add_enabled_ui(enabled, |ui| {
            ui.horizontal(|ui| {
                ui.label(s.rg_preamp);
                let slider =
                    egui::Slider::new(&mut self.settings.replaygain.preamp_db, -12.0..=12.0)
                        .suffix(" dB")
                        .step_by(0.5);
                ui.add(slider);
            });
            ui.checkbox(
                &mut self.settings.replaygain.prevent_clipping,
                s.rg_prevent_clipping,
            );
        });
        ui.label(RichText::new(s.rg_hint).small().color(theme::dim(ui)));
        if self.settings.replaygain != before {
            player.send(Command::SetReplayGain(self.settings.replaygain));
        }
    }

    fn settings_about(&mut self, ui: &mut egui::Ui) {
        let s = self.s();
        ui.label(
            RichText::new(s.app_name)
                .font(theme::semibold(20.0))
                .strong(),
        );
        ui.label(format!("{} {}", s.version, crate::app::VERSION));
        ui.add_space(6.0);
        ui.label(s.about_body);
        ui.add_space(10.0);
        ui.collapsing(s.licenses, |ui| {
            ui.add(egui::Label::new(RichText::new(LICENSE_NOTICES).small().monospace()).wrap());
        });
    }
}
