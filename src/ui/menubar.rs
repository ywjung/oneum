//! The menu bar drawn inside the window, for systems without a system menu bar
//! (Windows). It offers the same commands as the macOS menu bar and sends the same
//! [`MenuAction`]s, so both go through one code path. The keyboard shortcuts that a
//! system menu would handle are handled here too, with Ctrl in the place of ⌘.
use super::OneumApp;
use crate::i18n::Strings;
use crate::platform::MenuAction;
use eframe::egui::{self, Key, KeyboardShortcut, Modifiers};

const CMD: Modifiers = Modifiers::COMMAND;
const CMD_SHIFT: Modifiers = Modifiers::COMMAND.plus(Modifiers::SHIFT);
const CMD_ALT: Modifiers = Modifiers::COMMAND.plus(Modifiers::ALT);

pub(super) enum Entry {
    Item(MenuAction, &'static str, Option<KeyboardShortcut>),
    /// A menu inside the menu.
    Sub(&'static str, Vec<Entry>),
    Separator,
    /// Closes the window, which quits where this menu bar is used.
    Quit(&'static str, KeyboardShortcut),
}

fn item(action: MenuAction, label: &'static str) -> Entry {
    Entry::Item(action, label, None)
}

fn key(action: MenuAction, label: &'static str, modifiers: Modifiers, key: Key) -> Entry {
    Entry::Item(action, label, Some(KeyboardShortcut::new(modifiers, key)))
}

/// The menus, left to right. Space is deliberately not a shortcut of "Play/Pause": it
/// has to stay typeable in text fields (the window handles it itself).
pub(super) fn menus(s: &'static Strings) -> Vec<(&'static str, Vec<Entry>)> {
    use Entry::Separator;
    use MenuAction as A;
    vec![
        (
            s.menu_file,
            vec![
                key(A::OpenFiles, s.menu_open_files, CMD, Key::O),
                key(A::AddFolder, s.menu_add_folder, CMD_SHIFT, Key::O),
                Separator,
                key(A::NewPlaylist, s.menu_new_playlist, CMD, Key::N),
                item(A::ImportPlaylist, s.import_m3u8),
                item(A::PlaylistExport, s.menu_playlist_export),
                item(A::PlaylistRename, s.menu_playlist_rename),
                item(A::PlaylistDelete, s.menu_playlist_delete),
                Separator,
                key(A::Rescan, s.menu_rescan, CMD, Key::R),
                Separator,
                key(A::Settings, s.menu_settings, CMD, Key::Comma),
                Separator,
                Entry::Quit(s.menu_quit, KeyboardShortcut::new(CMD, Key::Q)),
            ],
        ),
        (
            s.menu_song,
            vec![
                item(A::SongPlay, s.menu_song_play),
                key(A::SongPlayNext, s.play_next, CMD, Key::Enter),
                key(A::SongAddToQueue, s.add_to_queue, CMD_SHIFT, Key::Enter),
                Separator,
                key(A::SongToggleFavorite, s.menu_song_favorite, CMD, Key::D),
                Separator,
                key(A::SongShowInFinder, s.show_in_finder, CMD_SHIFT, Key::F),
                key(A::SongInfo, s.song_info, CMD_ALT, Key::I),
            ],
        ),
        (
            s.menu_controls,
            vec![
                item(A::PlayPause, s.menu_play_pause),
                key(A::Next, s.menu_next, CMD, Key::ArrowRight),
                key(A::Previous, s.menu_previous, CMD, Key::ArrowLeft),
                key(
                    A::SeekForward,
                    s.menu_seek_forward,
                    CMD_ALT,
                    Key::ArrowRight,
                ),
                key(A::SeekBack, s.menu_seek_back, CMD_ALT, Key::ArrowLeft),
                key(
                    A::StopAfterCurrent,
                    s.menu_stop_after_current,
                    CMD_SHIFT,
                    Key::Period,
                ),
                Entry::Sub(
                    s.menu_sleep_timer,
                    vec![
                        item(A::SleepTimer(15), s.sleep_15),
                        item(A::SleepTimer(30), s.sleep_30),
                        item(A::SleepTimer(45), s.sleep_45),
                        item(A::SleepTimer(60), s.sleep_60),
                        item(A::SleepTimer(90), s.sleep_90),
                        Separator,
                        item(A::SleepTimer(0), s.sleep_off),
                    ],
                ),
                Separator,
                key(A::VolumeUp, s.menu_volume_up, CMD, Key::ArrowUp),
                key(A::VolumeDown, s.menu_volume_down, CMD, Key::ArrowDown),
                key(A::ToggleMute, s.menu_mute, CMD_SHIFT, Key::M),
                Separator,
                key(A::ToggleShuffle, s.menu_toggle_shuffle, CMD, Key::S),
                key(A::CycleRepeat, s.menu_cycle_repeat, CMD_SHIFT, Key::R),
                Separator,
                key(A::GoToCurrent, s.menu_go_to_current, CMD, Key::L),
            ],
        ),
        (
            s.menu_view,
            vec![
                key(A::Find, s.menu_find, CMD, Key::F),
                Separator,
                key(A::ToggleQueue, s.menu_show_queue, CMD, Key::U),
                key(A::ToggleInfo, s.menu_show_info, CMD, Key::I),
                key(A::ToggleMeter, s.menu_show_meter, CMD_SHIFT, Key::U),
                key(A::Equalizer, s.menu_equalizer, CMD, Key::E),
            ],
        ),
        (s.menu_help, vec![item(A::UserGuide, s.menu_user_guide)]),
    ]
}

/// The entries with those of the menus inside them, in order.
fn flat(entries: Vec<Entry>) -> Vec<Entry> {
    let mut out = Vec::new();
    for entry in entries {
        match entry {
            Entry::Sub(_, inner) => out.extend(flat(inner)),
            other => out.push(other),
        }
    }
    out
}

/// Every shortcut of the menus with its action, the ones with more modifiers first:
/// Ctrl+Shift+U must be tried before Ctrl+U, which it would also satisfy.
pub(super) fn shortcuts(s: &'static Strings) -> Vec<(KeyboardShortcut, MenuAction)> {
    let mut list: Vec<(KeyboardShortcut, MenuAction)> = menus(s)
        .into_iter()
        .flat_map(|(_, entries)| flat(entries))
        .filter_map(|entry| match entry {
            Entry::Item(action, _, Some(shortcut)) => Some((shortcut, action)),
            _ => None,
        })
        .collect();
    let weight = |m: Modifiers| u8::from(m.shift) + u8::from(m.alt) + u8::from(m.ctrl);
    list.sort_by_key(|(shortcut, _)| std::cmp::Reverse(weight(shortcut.modifiers)));
    list
}

/// Whether the shortcut of a menu command works on the phone screens: all but those
/// that act on the songs chosen in a list, which no key chooses there.
fn on_phone(action: MenuAction) -> bool {
    !matches!(
        action,
        MenuAction::SongPlay | MenuAction::SongPlayNext | MenuAction::SongAddToQueue
    )
}

/// Keys that move the cursor or end a line while text is being typed: their shortcuts
/// stand back then, like on a Mac.
fn is_text_editing_key(key: Key) -> bool {
    matches!(
        key,
        Key::ArrowLeft | Key::ArrowRight | Key::ArrowUp | Key::ArrowDown | Key::Enter
    )
}

impl OneumApp {
    /// The menu bar itself, as the topmost strip of the window.
    pub(super) fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let s = self.s();
        let mut chosen = None;
        let mut quit = false;
        egui::Panel::top("menu-bar").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                for (title, entries) in menus(s) {
                    ui.menu_button(title, |ui| {
                        for entry in entries {
                            match entry {
                                Entry::Separator => {
                                    ui.separator();
                                }
                                Entry::Sub(label, inner) => {
                                    ui.menu_button(label, |ui| {
                                        for entry in inner {
                                            match entry {
                                                Entry::Item(action, label, _) => {
                                                    if ui.button(label).clicked() {
                                                        chosen = Some(action);
                                                        ui.close();
                                                    }
                                                }
                                                _ => {
                                                    ui.separator();
                                                }
                                            }
                                        }
                                    });
                                }
                                Entry::Item(action, label, shortcut) => {
                                    let mut button = egui::Button::new(label);
                                    if let Some(shortcut) = shortcut {
                                        button = button
                                            .shortcut_text(ui.ctx().format_shortcut(&shortcut));
                                    }
                                    if ui.add(button).clicked() {
                                        chosen = Some(action);
                                        ui.close();
                                    }
                                }
                                Entry::Quit(label, shortcut) => {
                                    let button = egui::Button::new(label)
                                        .shortcut_text(ui.ctx().format_shortcut(&shortcut));
                                    if ui.add(button).clicked() {
                                        quit = true;
                                        ui.close();
                                    }
                                }
                            }
                        }
                    });
                }
            });
        });
        if let (Some(action), Some(dispatch)) = (chosen, self.menu_dispatch.clone()) {
            dispatch(action);
        }
        if quit {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// The shortcuts of the menu commands (what the system menu bar does on a Mac).
    pub(super) fn menu_shortcuts(&mut self, ctx: &egui::Context) {
        let Some(dispatch) = self.menu_dispatch.clone() else {
            return;
        };
        let typing = ctx.text_edit_focused();
        for (shortcut, action) in shortcuts(self.s()) {
            if typing && is_text_editing_key(shortcut.logical_key) {
                continue;
            }
            // On the phone screens (with a keyboard) only what has a meaning there.
            if self.phone_mode && !on_phone(action) {
                continue;
            }
            if ctx.input_mut(|i| i.consume_shortcut(&shortcut)) {
                dispatch(action);
            }
        }
        // A phone's app is not quit with a key.
        if !self.phone_mode
            && ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(CMD, Key::Q)))
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{EN, KO};
    use std::collections::HashSet;

    #[test]
    fn every_command_of_the_system_menu_is_in_the_window_menu() {
        let offered: HashSet<MenuAction> = menus(&KO)
            .into_iter()
            .flat_map(|(_, entries)| flat(entries))
            .filter_map(|entry| match entry {
                Entry::Item(action, ..) => Some(action),
                _ => None,
            })
            .collect();
        use MenuAction as A;
        // Bringing a hidden window back only exists where windows are hidden.
        let all = [
            A::Settings,
            A::OpenFiles,
            A::AddFolder,
            A::NewPlaylist,
            A::ImportPlaylist,
            A::Find,
            A::PlayPause,
            A::Next,
            A::Previous,
            A::VolumeUp,
            A::VolumeDown,
            A::ToggleShuffle,
            A::CycleRepeat,
            A::GoToCurrent,
            A::ToggleQueue,
            A::ToggleInfo,
            A::ToggleMeter,
            A::Equalizer,
            A::Rescan,
            A::UserGuide,
            A::ToggleMute,
            A::PlaylistRename,
            A::PlaylistExport,
            A::PlaylistDelete,
            A::SongPlay,
            A::SongPlayNext,
            A::SongAddToQueue,
            A::SongToggleFavorite,
            A::SongShowInFinder,
            A::SongInfo,
            A::StopAfterCurrent,
            A::SeekForward,
            A::SeekBack,
            A::SleepTimer(0),
            A::SleepTimer(15),
            A::SleepTimer(30),
            A::SleepTimer(45),
            A::SleepTimer(60),
            A::SleepTimer(90),
        ];
        for action in all {
            assert!(offered.contains(&action), "{action:?} is missing");
        }
        assert!(!offered.contains(&A::ShowMainWindow));
        // Both languages have the same menus, and no label is empty.
        assert_eq!(menus(&KO).len(), menus(&EN).len());
        for (title, entries) in menus(&EN) {
            assert!(!title.is_empty());
            for entry in flat(entries) {
                if let Entry::Item(_, label, _) | Entry::Quit(label, _) = entry {
                    assert!(!label.is_empty());
                }
            }
        }
    }

    #[test]
    fn shortcuts_are_unique_and_the_more_specific_one_wins() {
        let list = shortcuts(&KO);
        let distinct: HashSet<(Modifiers, Key)> = list
            .iter()
            .map(|(s, _)| (s.modifiers, s.logical_key))
            .collect();
        assert_eq!(distinct.len(), list.len(), "two commands share a shortcut");
        assert!(!distinct.contains(&(CMD, Key::Q)), "Ctrl+Q is Quit");
        // Pressing a shortcut must reach its own command: the first entry in trying
        // order that the pressed keys satisfy is that very entry.
        for (pressed, action) in &list {
            let first = list
                .iter()
                .find(|(candidate, _)| {
                    candidate.logical_key == pressed.logical_key
                        && pressed.modifiers.matches_logically(candidate.modifiers)
                })
                .map(|(_, a)| *a);
            assert_eq!(first, Some(*action), "{pressed:?}");
        }
        // The pairs that differ only by Shift, spelled out.
        let action_of = |modifiers, key| {
            list.iter()
                .find(|(s, _)| {
                    s.logical_key == key
                        && KeyboardShortcut::new(modifiers, key)
                            .modifiers
                            .matches_logically(s.modifiers)
                })
                .map(|(_, a)| *a)
        };
        assert_eq!(action_of(CMD_SHIFT, Key::U), Some(MenuAction::ToggleMeter));
        assert_eq!(action_of(CMD, Key::U), Some(MenuAction::ToggleQueue));
        assert_eq!(action_of(CMD_SHIFT, Key::O), Some(MenuAction::AddFolder));
        assert_eq!(action_of(CMD, Key::O), Some(MenuAction::OpenFiles));
        assert_eq!(action_of(CMD_ALT, Key::I), Some(MenuAction::SongInfo));
        assert_eq!(action_of(CMD, Key::I), Some(MenuAction::ToggleInfo));
    }

    #[test]
    fn cursor_keys_are_left_to_text_fields() {
        assert!(is_text_editing_key(Key::ArrowLeft) && is_text_editing_key(Key::Enter));
        assert!(!is_text_editing_key(Key::U) && !is_text_editing_key(Key::Comma));
    }
}
