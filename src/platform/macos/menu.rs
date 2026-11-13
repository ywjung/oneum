//! The native menu bar (muda). Menu items only produce [`MenuAction`]s; the app decides
//! what they do, so menu commands and in-window controls share one code path.
use crate::i18n::Strings;
use crate::platform::MenuAction;
use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{AboutMetadata, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};

const ACTIONS: &[(&str, MenuAction)] = &[
    ("settings", MenuAction::Settings),
    ("open_files", MenuAction::OpenFiles),
    ("add_folder", MenuAction::AddFolder),
    ("new_playlist", MenuAction::NewPlaylist),
    ("import_playlist", MenuAction::ImportPlaylist),
    ("find", MenuAction::Find),
    ("play_pause", MenuAction::PlayPause),
    ("next", MenuAction::Next),
    ("previous", MenuAction::Previous),
    ("volume_up", MenuAction::VolumeUp),
    ("volume_down", MenuAction::VolumeDown),
    ("toggle_shuffle", MenuAction::ToggleShuffle),
    ("cycle_repeat", MenuAction::CycleRepeat),
    ("go_to_current", MenuAction::GoToCurrent),
    ("toggle_queue", MenuAction::ToggleQueue),
    ("toggle_info", MenuAction::ToggleInfo),
    ("toggle_meter", MenuAction::ToggleMeter),
    ("equalizer", MenuAction::Equalizer),
    ("rescan", MenuAction::Rescan),
    ("main_window", MenuAction::ShowMainWindow),
    ("user_guide", MenuAction::UserGuide),
    ("toggle_mute", MenuAction::ToggleMute),
    ("playlist_rename", MenuAction::PlaylistRename),
    ("playlist_export", MenuAction::PlaylistExport),
    ("playlist_delete", MenuAction::PlaylistDelete),
    ("song_play", MenuAction::SongPlay),
    ("song_play_next", MenuAction::SongPlayNext),
    ("song_add_to_queue", MenuAction::SongAddToQueue),
    ("song_toggle_favorite", MenuAction::SongToggleFavorite),
    ("song_show_in_finder", MenuAction::SongShowInFinder),
    ("song_info", MenuAction::SongInfo),
];

fn id_of(action: MenuAction) -> &'static str {
    ACTIONS
        .iter()
        .find(|(_, a)| *a == action)
        .map_or("", |(id, _)| id)
}

fn item(action: MenuAction, text: &str, accelerator: Option<Accelerator>) -> MenuItem {
    MenuItem::with_id(id_of(action), text, true, accelerator)
}

fn cmd(code: Code) -> Option<Accelerator> {
    Some(Accelerator::new(Modifiers::META, code))
}

fn cmd_shift(code: Code) -> Option<Accelerator> {
    Some(Accelerator::new(Modifiers::META | Modifiers::SHIFT, code))
}

/// Owns the installed menu. Dropping it removes the menu from the application.
pub struct AppMenu {
    menu: Menu,
}

impl AppMenu {
    /// Builds and installs the menu bar. Call on the main thread after the app has launched.
    pub fn install(
        s: &Strings,
        version: &str,
        on_action: impl Fn(MenuAction) + Send + Sync + 'static,
    ) -> Option<Self> {
        let menu = Menu::new();
        let app_menu = Submenu::new(s.app_name, true);
        let file = Submenu::new(s.menu_file, true);
        let edit = Submenu::new(s.menu_edit, true);
        let song = Submenu::new(s.menu_song, true);
        let controls = Submenu::new(s.menu_controls, true);
        let view = Submenu::new(s.menu_view, true);
        let window = Submenu::new(s.menu_window, true);
        let help = Submenu::new(s.menu_help, true);
        let about = AboutMetadata {
            name: Some(s.app_name.to_string()),
            version: Some(version.to_string()),
            copyright: Some("MIT License".to_string()),
            ..Default::default()
        };
        let sep = PredefinedMenuItem::separator;
        let built = (|| -> muda::Result<()> {
            menu.append_items(&[
                &app_menu, &file, &edit, &song, &controls, &view, &window, &help,
            ])?;
            app_menu.append_items(&[
                &PredefinedMenuItem::about(Some(s.menu_about), Some(about)),
                &sep(),
                &item(MenuAction::Settings, s.menu_settings, cmd(Code::Comma)),
                &sep(),
                &PredefinedMenuItem::hide(Some(s.menu_hide)),
                &PredefinedMenuItem::hide_others(Some(s.menu_hide_others)),
                &PredefinedMenuItem::show_all(Some(s.menu_show_all)),
                &sep(),
                &PredefinedMenuItem::quit(Some(s.menu_quit)),
            ])?;
            file.append_items(&[
                &item(MenuAction::OpenFiles, s.menu_open_files, cmd(Code::KeyO)),
                &item(
                    MenuAction::AddFolder,
                    s.menu_add_folder,
                    cmd_shift(Code::KeyO),
                ),
                &sep(),
                &item(
                    MenuAction::NewPlaylist,
                    s.menu_new_playlist,
                    cmd(Code::KeyN),
                ),
                &item(MenuAction::ImportPlaylist, s.import_m3u8, None),
                &item(MenuAction::PlaylistExport, s.menu_playlist_export, None),
                &item(MenuAction::PlaylistRename, s.menu_playlist_rename, None),
                &item(MenuAction::PlaylistDelete, s.menu_playlist_delete, None),
                &sep(),
                &item(MenuAction::Rescan, s.menu_rescan, cmd(Code::KeyR)),
                &sep(),
                &PredefinedMenuItem::close_window(Some(s.menu_close_window)),
            ])?;
            edit.append_items(&[
                &PredefinedMenuItem::undo(Some(s.menu_undo)),
                &PredefinedMenuItem::redo(Some(s.menu_redo)),
                &sep(),
                &PredefinedMenuItem::cut(Some(s.menu_cut)),
                &PredefinedMenuItem::copy(Some(s.menu_copy)),
                &PredefinedMenuItem::paste(Some(s.menu_paste)),
                &PredefinedMenuItem::select_all(Some(s.menu_select_all)),
                &sep(),
                &item(MenuAction::Find, s.menu_find, cmd(Code::KeyF)),
            ])?;
            // Everything the right-click menu of a song offers, reachable from the keyboard
            // and by VoiceOver. Return itself stays a list key (it must work in text fields).
            song.append_items(&[
                &item(MenuAction::SongPlay, s.menu_song_play, None),
                &item(MenuAction::SongPlayNext, s.play_next, cmd(Code::Enter)),
                &item(
                    MenuAction::SongAddToQueue,
                    s.add_to_queue,
                    cmd_shift(Code::Enter),
                ),
                &sep(),
                &item(
                    MenuAction::SongToggleFavorite,
                    s.menu_song_favorite,
                    cmd(Code::KeyD),
                ),
                &sep(),
                &item(
                    MenuAction::SongShowInFinder,
                    s.show_in_finder,
                    cmd_shift(Code::KeyF),
                ),
                &item(
                    MenuAction::SongInfo,
                    s.song_info,
                    Some(Accelerator::new(
                        Modifiers::META | Modifiers::ALT,
                        Code::KeyI,
                    )),
                ),
            ])?;
            // Space is deliberately not a menu shortcut: it must stay typeable in text fields.
            controls.append_items(&[
                &item(MenuAction::PlayPause, s.menu_play_pause, None),
                &item(MenuAction::Next, s.menu_next, cmd(Code::ArrowRight)),
                &item(MenuAction::Previous, s.menu_previous, cmd(Code::ArrowLeft)),
                &sep(),
                &item(MenuAction::VolumeUp, s.menu_volume_up, cmd(Code::ArrowUp)),
                &item(
                    MenuAction::VolumeDown,
                    s.menu_volume_down,
                    cmd(Code::ArrowDown),
                ),
                &item(MenuAction::ToggleMute, s.menu_mute, cmd_shift(Code::KeyM)),
                &sep(),
                &item(
                    MenuAction::ToggleShuffle,
                    s.menu_toggle_shuffle,
                    cmd(Code::KeyS),
                ),
                &item(
                    MenuAction::CycleRepeat,
                    s.menu_cycle_repeat,
                    cmd_shift(Code::KeyR),
                ),
                &sep(),
                &item(
                    MenuAction::GoToCurrent,
                    s.menu_go_to_current,
                    cmd(Code::KeyL),
                ),
            ])?;
            view.append_items(&[
                &item(MenuAction::ToggleQueue, s.menu_show_queue, cmd(Code::KeyU)),
                &item(MenuAction::ToggleInfo, s.menu_show_info, cmd(Code::KeyI)),
                &item(
                    MenuAction::ToggleMeter,
                    s.menu_show_meter,
                    cmd_shift(Code::KeyU),
                ),
                &item(MenuAction::Equalizer, s.menu_equalizer, cmd(Code::KeyE)),
                &sep(),
                &PredefinedMenuItem::fullscreen(Some(s.menu_fullscreen)),
            ])?;
            window.append_items(&[
                &PredefinedMenuItem::minimize(Some(s.menu_minimize)),
                &PredefinedMenuItem::maximize(Some(s.menu_zoom)),
                &sep(),
                &item(
                    MenuAction::ShowMainWindow,
                    s.menu_main_window,
                    cmd(Code::Digit0),
                ),
                &sep(),
                &PredefinedMenuItem::bring_all_to_front(Some(s.menu_bring_all_to_front)),
            ])?;
            help.append_items(&[&item(MenuAction::UserGuide, s.menu_user_guide, None)])?;
            Ok(())
        })();
        if let Err(e) = built {
            tracing::error!("could not build the menu bar: {e}");
            return None;
        }
        menu.init_for_nsapp();
        window.set_as_windows_menu_for_nsapp();
        help.set_as_help_menu_for_nsapp();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some((_, action)) = ACTIONS.iter().find(|(id, _)| event.id.0 == *id) {
                on_action(*action);
            }
        }));
        Some(Self { menu })
    }
}

impl Drop for AppMenu {
    fn drop(&mut self) {
        self.menu.remove_for_nsapp();
    }
}
