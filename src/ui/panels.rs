//! Top bar, sidebar, right panel (queue / song info) and the notice strip.
use super::dialogs::Dialog;
use super::icons::Icon;
use super::{Action, Nav, OneumApp, ToastAction, theme, widgets};
use crate::app::settings::RightPanel;
use crate::i18n::{format_time, group_digits};
use crate::playback::{Command, PlayState};
use eframe::egui::{self, Align, Layout, RichText, Sense, Ui};

const SIDEBAR_ROW: f32 = 28.0;
const QUEUE_ROW: f32 = 44.0;

/// A full-width selectable row in the sidebar.
fn sidebar_row(
    ui: &mut Ui,
    selected: bool,
    icon: Icon,
    text: &str,
    trailing: Option<String>,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), SIDEBAR_ROW),
        Sense::click(),
    );
    let label = text.to_string();
    response.widget_info(move || {
        egui::WidgetInfo::selected(egui::Role::ListBoxOption, true, selected, &label)
    });
    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        if selected {
            ui.painter()
                .rect_filled(rect, 6.0, visuals.selection.bg_fill);
        } else if response.hovered() || response.has_focus() {
            ui.painter()
                .rect_filled(rect, 6.0, visuals.widgets.hovered.weak_bg_fill);
        }
        let color = if selected {
            visuals.strong_text_color()
        } else {
            visuals.text_color()
        };
        let icon_color = if selected {
            theme::accent(visuals.dark_mode)
        } else {
            visuals.weak_text_color()
        };
        let icon_rect = egui::Rect::from_center_size(
            egui::pos2(rect.left() + 18.0, rect.center().y),
            egui::Vec2::splat(15.0),
        );
        super::icons::paint(ui.painter(), icon_rect, icon, icon_color);
        let font = egui::TextStyle::Body.resolve(ui.style());
        let trailing_width = trailing.as_ref().map_or(0.0, |_| 44.0);
        let text_rect = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 34.0, rect.top()),
            egui::pos2(rect.right() - 8.0 - trailing_width, rect.bottom()),
        );
        let galley = ui.painter().layout_no_wrap(text.to_string(), font, color);
        let clipped = ui.painter().with_clip_rect(text_rect);
        clipped.galley(
            egui::pos2(text_rect.left(), rect.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
        if let Some(trailing) = trailing {
            ui.painter().text(
                egui::pos2(rect.right() - 10.0, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                trailing,
                egui::TextStyle::Small.resolve(ui.style()),
                visuals.weak_text_color(),
            );
        }
    }
    response
}

impl OneumApp {
    pub(super) fn top_bar(&mut self, ui: &mut Ui) {
        let s = self.s();
        egui::Panel::top("top-bar")
            .exact_size(theme::TOP_BAR_HEIGHT)
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.add_space(4.0);
                    if !self.history.is_empty()
                        && widgets::icon_button(ui, Icon::Back, s.back, 26.0, false).clicked()
                    {
                        self.go_back();
                    }
                    // Search field for the current view.
                    let nav = self.nav.clone();
                    let state = self.views.entry(nav).or_default();
                    let width = (ui.available_width() - 250.0).clamp(170.0, 400.0);
                    let outline = if self.search_had_focus {
                        egui::Stroke::new(1.5, theme::accent(ui.visuals().dark_mode))
                    } else {
                        ui.visuals().widgets.noninteractive.bg_stroke
                    };
                    let frame = egui::Frame::new()
                        .fill(ui.visuals().extreme_bg_color)
                        .stroke(outline)
                        .corner_radius(8.0)
                        .inner_margin(egui::Margin::symmetric(8, 3));
                    let mut has_focus = false;
                    let mut want_focus = std::mem::take(&mut self.focus_search);
                    let field_size = egui::vec2(width, 28.0);
                    ui.allocate_ui_with_layout(
                        field_size,
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            frame.show(ui, |ui| {
                                ui.set_height(20.0);
                                let (icon_rect, _) =
                                    ui.allocate_exact_size(egui::Vec2::splat(14.0), Sense::hover());
                                super::icons::paint(
                                    ui.painter(),
                                    icon_rect,
                                    Icon::Search,
                                    ui.visuals().weak_text_color(),
                                );
                                let edit = egui::TextEdit::singleline(&mut state.query)
                                    .hint_text(s.search_hint)
                                    .frame(egui::Frame::NONE)
                                    .align(egui::Align2::LEFT_CENTER)
                                    .min_size(egui::vec2(0.0, 20.0))
                                    .desired_width(width - 72.0);
                                let response = ui.add(edit);
                                response.widget_info(|| {
                                    egui::WidgetInfo::labeled(
                                        egui::Role::SearchInput,
                                        true,
                                        s.search_label,
                                    )
                                });
                                if std::mem::take(&mut want_focus) {
                                    response.request_focus();
                                }
                                has_focus = response.has_focus();
                                // Esc in the search field clears it (egui itself only drops focus).
                                if response.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Escape))
                                {
                                    state.query.clear();
                                }
                                if state.query.is_empty() {
                                    ui.add_space(20.0);
                                } else if widgets::icon_button(
                                    ui,
                                    Icon::Close,
                                    s.clear_search,
                                    20.0,
                                    false,
                                )
                                .clicked()
                                {
                                    state.query.clear();
                                    response.request_focus();
                                }
                            });
                        },
                    );
                    self.search_had_focus = has_focus;

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.add_space(4.0);
                        let panel = self.settings.right_panel;
                        if widgets::icon_button(
                            ui,
                            Icon::Info,
                            s.show_info,
                            28.0,
                            panel == RightPanel::Info,
                        )
                        .clicked()
                        {
                            self.toggle_right_panel(RightPanel::Info);
                        }
                        if widgets::icon_button(
                            ui,
                            Icon::Queue,
                            s.show_queue,
                            28.0,
                            panel == RightPanel::Queue,
                        )
                        .clicked()
                        {
                            self.toggle_right_panel(RightPanel::Queue);
                        }
                        if widgets::icon_button(
                            ui,
                            Icon::Gear,
                            s.settings,
                            28.0,
                            self.settings_open,
                        )
                        .clicked()
                        {
                            self.settings_open = !self.settings_open;
                        }
                        let scan = self.core.library.scan_status();
                        if scan.running {
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new(self.lang.scan_progress(scan.files_done))
                                    .small()
                                    .color(theme::dim(ui)),
                            );
                            ui.add(egui::Spinner::new().size(14.0));
                        }
                    });
                });
            });
    }

    pub(super) fn sidebar(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let s = self.s();
        let frame = egui::Frame::new()
            .fill(theme::chrome_fill(ui))
            .inner_margin(egui::Margin::symmetric(8, 8));
        egui::Panel::left("sidebar")
            .frame(frame)
            .default_size(204.0)
            .size_range(160.0..=320.0)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                let header = |ui: &mut Ui, text: &str| {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        ui.label(RichText::new(text).small().strong().color(theme::dim(ui)));
                    });
                    ui.add_space(2.0);
                };
                header(ui, s.library);
                let total = self.snapshot.len();
                let entries = [
                    (
                        Nav::Songs,
                        Icon::Note,
                        s.all_songs,
                        Some(group_digits(total)),
                    ),
                    (Nav::Albums, Icon::Album, s.albums, None),
                    (Nav::Artists, Icon::Person, s.artists, None),
                    (Nav::Favorites, Icon::Heart, s.favorites, None),
                    (Nav::Recent, Icon::Clock, s.recently_played, None),
                ];
                for (nav, icon, label, trailing) in entries {
                    let selected = self.nav == nav
                        || matches!(
                            (&self.nav, &nav),
                            (Nav::Album(_), Nav::Albums) | (Nav::Artist(_), Nav::Artists)
                        ) && self.history.first() == Some(&nav);
                    if sidebar_row(ui, selected, icon, label, trailing.filter(|_| total > 0))
                        .clicked()
                    {
                        actions.push(Action::Go(nav));
                    }
                }

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(s.playlists)
                            .small()
                            .strong()
                            .color(theme::dim(ui)),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::icon_button(ui, Icon::Plus, s.new_playlist, 22.0, false)
                            .clicked()
                        {
                            self.dialog =
                                Some(Dialog::new_playlist(s.untitled_playlist, Vec::new()));
                        }
                    });
                });
                egui::ScrollArea::vertical()
                    .id_salt("playlists")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let playlists: Vec<(i64, String, usize)> = self
                            .playlists
                            .iter()
                            .map(|p| (p.id, p.name.clone(), p.item_count))
                            .collect();
                        for (id, name, count) in playlists {
                            if let Some((rename_id, text, focus)) =
                                self.renaming.as_mut().filter(|(rid, _, _)| *rid == id)
                            {
                                let _ = rename_id;
                                let response = ui.add(
                                    egui::TextEdit::singleline(text).desired_width(f32::INFINITY),
                                );
                                if *focus {
                                    *focus = false;
                                    response.request_focus();
                                }
                                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                                if response.lost_focus() {
                                    let new_name = text.trim().to_string();
                                    self.renaming = None;
                                    if enter
                                        && !new_name.is_empty()
                                        && new_name != name
                                        && let Err(error) =
                                            self.core.library.rename_playlist(id, &new_name)
                                    {
                                        self.library_error_toast(&error);
                                    }
                                }
                                continue;
                            }
                            let response = sidebar_row(
                                ui,
                                self.nav == Nav::Playlist(id),
                                Icon::Playlist,
                                &name,
                                Some(count.to_string()),
                            );
                            if response.clicked() {
                                actions.push(Action::Go(Nav::Playlist(id)));
                            }
                            if response.double_clicked() {
                                self.renaming = Some((id, name.clone(), true));
                            }
                            response.context_menu(|ui| {
                                if ui.button(s.play_now).clicked() {
                                    let tracks = self
                                        .core
                                        .library
                                        .playlist_items(id)
                                        .into_iter()
                                        .map(|i| i.track_id)
                                        .collect();
                                    actions.push(Action::PlayTracks(tracks, 0));
                                    ui.close();
                                }
                                ui.separator();
                                if ui.button(s.rename).clicked() {
                                    self.renaming = Some((id, name.clone(), true));
                                    ui.close();
                                }
                                if ui.button(s.export_m3u8).clicked() {
                                    self.export_playlist(id, &name);
                                    ui.close();
                                }
                                ui.separator();
                                if ui.button(s.delete).clicked() {
                                    self.dialog = Some(Dialog::DeletePlaylist(id, name.clone()));
                                    ui.close();
                                }
                            });
                        }
                    });
            });
    }

    pub(super) fn toast_bar(&mut self, ui: &mut Ui) {
        let Some(toast) = &self.toast else { return };
        let s = self.s();
        let mut dismiss = false;
        let mut run: Option<ToastAction> = None;
        let dark = ui.visuals().dark_mode;
        let fill = match (toast.warning, dark) {
            (true, true) => egui::Color32::from_rgb(0x4A, 0x39, 0x1A),
            (true, false) => egui::Color32::from_rgb(0xFF, 0xF1, 0xD0),
            (false, _) => ui.visuals().faint_bg_color,
        };
        let frame = egui::Frame::new()
            .fill(fill)
            .inner_margin(egui::Margin::symmetric(12, 6));
        egui::Panel::bottom("toast")
            .frame(frame)
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if toast.warning {
                        let (rect, _) =
                            ui.allocate_exact_size(egui::Vec2::splat(16.0), Sense::hover());
                        super::icons::paint(
                            ui.painter(),
                            rect,
                            Icon::Warning,
                            ui.visuals().text_color(),
                        );
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::icon_button(ui, Icon::Close, s.dismiss, 22.0, false).clicked() {
                            dismiss = true;
                        }
                        if let Some((label, action)) = &toast.action
                            && ui.button(label).clicked()
                        {
                            run = Some(*action);
                        }
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.add(egui::Label::new(&toast.text).wrap());
                        });
                    });
                });
            });
        if let Some(action) = run {
            match action {
                ToastAction::RetryOutput => self.core.player.send(Command::RetryOutput),
                ToastAction::OpenPlaybackSettings => {
                    self.settings_open = true;
                    self.settings_tab = super::settings_window::Tab::Playback;
                }
            }
            dismiss = true;
        }
        if dismiss {
            self.toast = None;
        }
    }

    pub(super) fn right_panel(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let which = self.settings.right_panel;
        if which == RightPanel::Hidden {
            return;
        }
        egui::Panel::right("right-panel")
            .default_size(300.0)
            .size_range(240.0..=420.0)
            .show(ui, |ui| match which {
                RightPanel::Queue => self.queue_panel(ui),
                RightPanel::Info => self.info_panel(ui, actions),
                RightPanel::Hidden => {}
            });
    }

    fn queue_panel(&mut self, ui: &mut Ui) {
        let s = self.s();
        let queue = self.player.queue.clone();
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(s.show_queue)
                    .font(theme::semibold(15.0))
                    .strong(),
            );
            ui.label(
                RichText::new(self.lang.song_count(queue.len()))
                    .small()
                    .color(theme::dim(ui)),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if !queue.is_empty() && ui.small_button(s.clear_queue).clicked() {
                    self.core.player.send(Command::ClearQueue);
                    self.queue_selection.clear();
                }
            });
        });
        ui.separator();
        if queue.is_empty() {
            widgets::empty_state(ui, Icon::Queue, s.empty_queue, s.empty_queue_hint);
            return;
        }
        let snap = self.snapshot.clone();
        let current_index = self.player.queue_index;
        let is_playing = self.player.state == PlayState::Playing;
        let accent = theme::accent(ui.visuals().dark_mode);
        let dim = theme::dim(ui);
        let scroll_to_current = std::mem::take(&mut self.queue_scroll_to_current);
        let drag_id = egui::Id::unique("queue-drag");
        let mut commands: Vec<Command> = Vec::new();

        let mut area = egui::ScrollArea::vertical()
            .id_salt("queue")
            .auto_shrink([false, false]);
        if scroll_to_current && let Some(index) = current_index {
            // Leave one earlier row visible for context.
            area = area.vertical_scroll_offset((index.saturating_sub(1)) as f32 * QUEUE_ROW);
        }
        ui.spacing_mut().item_spacing.y = 0.0;
        area.show_rows(ui, QUEUE_ROW, queue.len(), |ui, range| {
            for index in range {
                let item = queue[index];
                let track = snap.get(item.track);
                let (rect, response) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), QUEUE_ROW),
                    Sense::click_and_drag(),
                );
                let title = track.map_or("—", |t| t.title.as_str());
                let artist = track.map_or("", |t| t.artist.as_str());
                let name = format!("{title}, {artist}");
                let selected = self.queue_selection.contains(&item.id);
                response.widget_info(|| {
                    egui::WidgetInfo::selected(egui::Role::ListBoxOption, true, selected, &name)
                });
                let is_current = current_index == Some(index);
                if ui.is_rect_visible(rect) {
                    if selected {
                        ui.painter()
                            .rect_filled(rect, 4.0, ui.visuals().selection.bg_fill);
                    } else if response.hovered() {
                        ui.painter().rect_filled(
                            rect,
                            4.0,
                            ui.visuals().widgets.hovered.weak_bg_fill,
                        );
                    }
                    let played = current_index.is_some_and(|c| index < c);
                    let art_rect = egui::Rect::from_min_size(
                        rect.min + egui::vec2(6.0, 5.0),
                        egui::Vec2::splat(QUEUE_ROW - 10.0),
                    );
                    let texture = self.art.get(track.and_then(|t| t.art_id.as_deref()), false);
                    widgets::art(ui, art_rect, texture, 4.0);
                    if is_current {
                        ui.painter().rect_filled(
                            art_rect,
                            4.0,
                            egui::Color32::from_black_alpha(120),
                        );
                        let icon = if is_playing { Icon::Play } else { Icon::Pause };
                        super::icons::paint(
                            ui.painter(),
                            art_rect.shrink(10.0),
                            icon,
                            egui::Color32::WHITE,
                        );
                    }
                    let text_rect = egui::Rect::from_min_max(
                        egui::pos2(art_rect.right() + 8.0, rect.top() + 5.0),
                        egui::pos2(rect.right() - 50.0, rect.bottom() - 4.0),
                    );
                    let mut text_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(text_rect)
                            .layout(Layout::top_down(Align::Min)),
                    );
                    text_ui.spacing_mut().item_spacing.y = 1.0;
                    let unavailable = track.is_none_or(|t| !t.available);
                    let color = if is_current {
                        accent
                    } else if played || unavailable {
                        dim
                    } else {
                        ui.visuals().text_color()
                    };
                    let mut title_text = RichText::new(title).color(color);
                    if is_current {
                        title_text = title_text.strong();
                    }
                    widgets::truncated(&mut text_ui, title_text);
                    widgets::truncated(&mut text_ui, RichText::new(artist).small().color(dim));
                    ui.painter().text(
                        egui::pos2(rect.right() - 8.0, rect.center().y),
                        egui::Align2::RIGHT_CENTER,
                        format_time(track.map_or(0, |t| t.duration_ms)),
                        egui::TextStyle::Small.resolve(ui.style()),
                        dim,
                    );
                }

                // Selection and activation.
                if response.double_clicked() {
                    commands.push(Command::JumpTo(item.id));
                } else if response.clicked() {
                    let modifiers = ui.input(|i| i.modifiers);
                    if modifiers.command {
                        if !self.queue_selection.remove(&item.id) {
                            self.queue_selection.insert(item.id);
                        }
                    } else if modifiers.shift {
                        let from = self
                            .queue_anchor
                            .and_then(|a| queue.iter().position(|q| q.id == a))
                            .unwrap_or(index);
                        self.queue_selection = queue[from.min(index)..=from.max(index)]
                            .iter()
                            .map(|q| q.id)
                            .collect();
                    } else {
                        self.queue_selection.clear();
                        self.queue_selection.insert(item.id);
                    }
                    if !modifiers.shift {
                        self.queue_anchor = Some(item.id);
                    }
                }
                if (response.secondary_clicked() || response.drag_started())
                    && !self.queue_selection.contains(&item.id)
                {
                    self.queue_selection.clear();
                    self.queue_selection.insert(item.id);
                    self.queue_anchor = Some(item.id);
                }

                // Drag to reorder: the dragged selection is dropped before or after the hovered row.
                if response.drag_started() {
                    ui.ctx().data_mut(|d| d.insert_temp(drag_id, true));
                }
                let dragging = ui
                    .ctx()
                    .data(|d| d.get_temp::<bool>(drag_id).unwrap_or(false));
                if dragging
                    && let Some(pointer) = ui
                        .ctx()
                        .pointer_interact_pos()
                        .filter(|p| rect.contains(*p))
                {
                    let after = pointer.y > rect.center().y;
                    let y = if after { rect.bottom() } else { rect.top() };
                    ui.painter()
                        .hline(rect.x_range(), y, egui::Stroke::new(2.0, accent));
                    if ui.input(|i| i.pointer.any_released()) {
                        let ids: Vec<u64> = queue
                            .iter()
                            .filter(|q| self.queue_selection.contains(&q.id))
                            .map(|q| q.id)
                            .collect();
                        // Index in the list with the moved items taken out.
                        let target = index + usize::from(after);
                        let removed_before = queue[..target.min(queue.len())]
                            .iter()
                            .filter(|q| ids.contains(&q.id))
                            .count();
                        commands.push(Command::MoveItems {
                            ids,
                            to_index: target - removed_before,
                        });
                    }
                }

                response.context_menu(|ui| {
                    let ids: Vec<u64> = queue
                        .iter()
                        .filter(|q| self.queue_selection.contains(&q.id))
                        .map(|q| q.id)
                        .collect();
                    let first = queue
                        .iter()
                        .position(|q| ids.contains(&q.id))
                        .unwrap_or(index);
                    ui.set_min_width(180.0);
                    if ui.button(s.play_now).clicked() {
                        commands.push(Command::JumpTo(item.id));
                        ui.close();
                    }
                    ui.separator();
                    if ui
                        .add_enabled(first > 0, egui::Button::new(s.move_to_top))
                        .clicked()
                    {
                        commands.push(Command::MoveItems {
                            ids: ids.clone(),
                            to_index: 0,
                        });
                        ui.close();
                    }
                    if ui
                        .add_enabled(first > 0, egui::Button::new(s.move_up))
                        .clicked()
                    {
                        commands.push(Command::MoveItems {
                            ids: ids.clone(),
                            to_index: first - 1,
                        });
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            first + ids.len() < queue.len(),
                            egui::Button::new(s.move_down),
                        )
                        .clicked()
                    {
                        commands.push(Command::MoveItems {
                            ids: ids.clone(),
                            to_index: first + 1,
                        });
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(s.remove_from_queue).clicked() {
                        commands.push(Command::RemoveItems(ids));
                        ui.close();
                    }
                });
            }
        });
        if ui.input(|i| i.pointer.any_released() || !i.pointer.any_down()) {
            ui.ctx().data_mut(|d| d.remove_temp::<bool>(drag_id));
        }
        for command in commands {
            if matches!(command, Command::RemoveItems(_)) {
                self.queue_selection.clear();
            }
            self.core.player.send(command);
        }
    }

    fn info_panel(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let s = self.s();
        // Explicitly requested track, else the single selected row, else what is playing.
        let selected = self.views.get(&self.nav).and_then(|state| {
            let key = state.anchor.filter(|a| state.selection.contains(a))?;
            let row = state.rows.iter().find(|r| r.key == key)?;
            match self.nav {
                Nav::Albums | Nav::Artists | Nav::Artist(_) => None,
                _ => self.snapshot.tracks.get(row.idx as usize).map(|t| t.id),
            }
        });
        let id = selected
            .or(self.info_track)
            .or(self.player.current.map(|c| c.track_id));
        ui.add_space(4.0);
        ui.label(
            RichText::new(s.show_info)
                .font(theme::semibold(15.0))
                .strong(),
        );
        ui.separator();
        let Some(track) = id.and_then(|id| self.track(id)).cloned() else {
            widgets::empty_state(ui, Icon::Info, s.no_selection, "");
            return;
        };
        let dim = theme::dim(ui);
        egui::ScrollArea::vertical()
            .id_salt("info")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let side = (ui.available_width() - 8.0).clamp(120.0, 300.0);
                ui.vertical_centered(|ui| {
                    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(side), Sense::hover());
                    let texture = self.art.get(track.art_id.as_deref(), true);
                    widgets::art(ui, rect, texture, 8.0);
                });
                ui.add_space(8.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(&track.title)
                            .font(theme::semibold(17.0))
                            .strong(),
                    )
                    .wrap(),
                );
                let artist = if track.artist.is_empty() {
                    s.unknown_artist
                } else {
                    track.artist.as_str()
                };
                ui.add(egui::Label::new(RichText::new(artist).size(14.0)).wrap());
                if !track.album.is_empty() && ui.link(&track.album).clicked() {
                    actions.push(Action::Go(Nav::Album(track.album_key)));
                }
                if !track.available {
                    ui.add_space(4.0);
                    ui.colored_label(ui.visuals().warn_fg_color, s.info_missing);
                }
                ui.add_space(8.0);

                let mut rows: Vec<(&str, String)> = Vec::new();
                if !track.album_artist.is_empty() && track.album_artist != track.artist {
                    rows.push((s.info_album_artist, track.album_artist.clone()));
                }
                if let Some(n) = track.track_no {
                    let disc = track
                        .disc_no
                        .filter(|d| *d > 1)
                        .map(|d| format!(" (CD {d})"))
                        .unwrap_or_default();
                    rows.push((s.info_track, format!("{n}{disc}")));
                }
                if let Some(year) = track.year {
                    rows.push((s.info_year, year.to_string()));
                }
                if !track.genre.is_empty() {
                    rows.push((s.info_genre, track.genre.clone()));
                }
                rows.push((s.col_duration, format_time(track.duration_ms)));
                let format = if track.codec == track.container || track.container.is_empty() {
                    track.codec.clone()
                } else {
                    format!("{} ({})", track.codec, track.container)
                };
                rows.push((s.info_format, format));
                rows.push((
                    s.info_sample_rate,
                    format!("{} Hz", group_digits(track.sample_rate as usize)),
                ));
                rows.push((s.info_channels, track.channels.to_string()));
                if let Some(bits) = track.bit_depth {
                    rows.push((s.info_bit_depth, format!("{bits} bit")));
                }
                if let Some(kbps) = track.bitrate_kbps {
                    rows.push((s.info_bitrate, format!("{kbps} kbps")));
                }
                rows.push((
                    s.info_size,
                    format!("{:.1} MB", track.file_size as f64 / 1_048_576.0),
                ));
                let rg = track.replaygain;
                let gain = match (rg.track_gain_db, rg.album_gain_db) {
                    (None, None) => s.info_none.to_string(),
                    (t, a) => {
                        let fmt =
                            |v: Option<f32>| v.map_or("—".to_string(), |v| format!("{v:+.2} dB"));
                        format!("{} {} / {} {}", s.rg_track, fmt(t), s.rg_album, fmt(a))
                    }
                };
                rows.push((s.info_replaygain, gain));
                rows.push((s.info_plays, track.play_count.to_string()));
                // What the engine is doing with the track that is playing right now.
                if self.player.current.is_some_and(|c| c.track_id == track.id)
                    && let (Some(source), Some(out_rate)) =
                        (&self.player.source, self.player.output_sample_rate)
                {
                    let mut text = format!("{} Hz", group_digits(out_rate as usize));
                    if source.sample_rate != out_rate {
                        text = format!("{text} ({})", s.info_resampled);
                    }
                    if let Some(device) = &self.player.output_device {
                        text = format!("{device} · {text}");
                    }
                    rows.push((s.info_output, text));
                }
                egui::Grid::new("info-grid")
                    .num_columns(2)
                    .spacing([10.0, 5.0])
                    .show(ui, |ui| {
                        for (label, value) in rows {
                            ui.label(RichText::new(label).small().color(dim));
                            ui.add(egui::Label::new(value).wrap());
                            ui.end_row();
                        }
                    });
                ui.add_space(8.0);
                ui.label(RichText::new(s.info_file).small().color(dim));
                ui.add(
                    egui::Label::new(RichText::new(track.path.display().to_string()).small())
                        .wrap()
                        .selectable(true),
                );
                ui.add_space(6.0);
                if ui.button(s.show_in_finder).clicked() {
                    actions.push(Action::Reveal(track.id));
                }
                ui.add_space(8.0);
            });
    }
}
