//! Top bar, sidebar, right panel (queue / song info), the VU meter deck and the notice strip.
use super::dialogs::Dialog;
use super::icons::Icon;
use super::{
    Action, DragTracks, KeyPane, Nav, OneumApp, ToastAction, meter, spectrum, theme, widgets,
};
use crate::app::settings::{Pin, RightPanel};
use crate::audio::dsp::replaygain_factor;
use crate::i18n::{format_time, group_digits};
use crate::playback::{Command, PlayState};
use eframe::egui::{self, Align, Layout, RichText, Sense, Ui};

/// The path of the playing track through the app, in words (see `OneumApp::signal_path`).
pub(super) struct SignalPath {
    pub(super) track_id: crate::model::TrackId,
    /// Codec of the file ("FLAC", "MP3", …).
    pub(super) codec: String,
    /// Sample rate and bit depth of the file.
    pub(super) source_format: String,
    /// Name of the output device.
    pub(super) device: String,
    /// Rate, hardware sample format and exclusive access of the output.
    pub(super) format: String,
    /// What the app does to the samples on their way, in the order it does it (empty
    /// when it does nothing).
    pub(super) stages: Vec<Stage>,
    /// The file is played from memory.
    pub(super) in_memory: bool,
}

/// Why a stage of the signal path is there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StageKind {
    /// Needed to get the file to this device: rate conversion, channel mapping, dither.
    Conversion,
    /// Asked for by the user: volume leveling, the equalizer, the app volume.
    Adjustment,
    /// Peak protection stepped in.
    Protection,
}

/// One thing the app does to the samples.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Stage {
    kind: StageKind,
    pub(super) name: String,
    /// What exactly ("44,100 → 48,000 Hz", "−6.0 dB"); may be empty.
    pub(super) detail: String,
}

/// The signal path in a word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathSummary {
    /// The app passes the decoded samples on as they are.
    Untouched,
    /// Only what the user asked for (and what follows from it).
    Adjusted,
    /// Only what the device needs.
    Converted,
    ConvertedAndAdjusted,
}

fn path_summary(stages: &[Stage]) -> PathSummary {
    let has = |kind| stages.iter().any(|stage| stage.kind == kind);
    match (has(StageKind::Conversion), has(StageKind::Adjustment)) {
        (false, false) if stages.is_empty() => PathSummary::Untouched,
        // Peak protection alone only happens to audio that came out of the decoder
        // above full scale: nobody asked for it, the device needs it.
        (false, false) => PathSummary::Converted,
        (true, false) => PathSummary::Converted,
        (false, true) => PathSummary::Adjusted,
        (true, true) => PathSummary::ConvertedAndAdjusted,
    }
}

/// What decides the stages of the signal path (see [`path_stages`]).
struct PathInputs<'a> {
    s: &'a crate::i18n::Strings,
    /// For DSD: the one-bit rate, converted to `source_rate` before anything else.
    dsd_rate: Option<u32>,
    source_rate: u32,
    out_rate: u32,
    channels: usize,
    /// Linear gain of volume leveling (1.0 = none).
    leveling: f32,
    /// Name of the equalizer setting while the equalizer is changing the sound.
    equalizer: Option<String>,
    /// Name of the parametric correction profile while one is applied.
    correction: Option<&'a str>,
    /// Strength of the headphone crossfeed while it is on.
    crossfeed: Option<&'a str>,
    /// The crossfade between songs in words, while it is on.
    crossfade: Option<String>,
    /// The playback speed in words, while it is not the song's own.
    speed: Option<String>,
    /// The name of the upsampling filter, while the song's rate is raised with one
    /// the listener chose.
    filter: Option<&'a str>,
    /// The room correction in words (its name, and how far it is turned down),
    /// while one is applied.
    room: Option<String>,
    peak_protection: bool,
    /// The volume slider, 0..=1.
    volume: f32,
    dither_bits: Option<u32>,
}

fn decibels(gain: f32) -> String {
    let db = 20.0 * gain.max(1e-6).log10();
    if db < 0.0 {
        format!("−{:.1} dB", -db)
    } else {
        format!("+{db:.1} dB")
    }
}

/// The stages in the order the audio passes them: channel mapping and volume leveling
/// at the file's rate, then the rate conversion, the equalizer and peak protection in
/// the decode thread, the app volume and dither in the output callback.
fn path_stages(input: &PathInputs) -> Vec<Stage> {
    let s = input.s;
    let mut stages = Vec::new();
    let mut add = |kind, name: &str, detail: String| {
        stages.push(Stage {
            kind,
            name: name.to_string(),
            detail,
        });
    };
    if let Some(dsd_rate) = input.dsd_rate {
        add(
            StageKind::Conversion,
            s.info_proc_dsd,
            format!(
                "{} → {} Hz",
                group_digits(dsd_rate as usize),
                group_digits(input.source_rate as usize)
            ),
        );
    }
    if input.channels != 2 {
        add(
            StageKind::Conversion,
            s.info_proc_channels,
            format!("{} → 2 {}", input.channels, s.path_channels_unit),
        );
    }
    if input.leveling != 1.0 {
        add(
            StageKind::Adjustment,
            s.info_proc_replaygain,
            decibels(input.leveling),
        );
    }
    if input.source_rate != input.out_rate {
        let rates = format!(
            "{} → {} Hz",
            group_digits(input.source_rate as usize),
            group_digits(input.out_rate as usize)
        );
        add(
            StageKind::Conversion,
            s.info_proc_resample,
            match input.filter {
                Some(filter) => format!("{rates} · {filter}"),
                None => rates,
            },
        );
    }
    if let Some(setting) = &input.equalizer {
        add(StageKind::Adjustment, s.info_proc_eq, setting.clone());
    }
    if let Some(profile) = input.correction {
        add(
            StageKind::Adjustment,
            s.info_proc_correction,
            profile.to_string(),
        );
    }
    if let Some(strength) = input.crossfeed {
        add(
            StageKind::Adjustment,
            s.info_proc_crossfeed,
            strength.to_string(),
        );
    }
    if let Some(room) = &input.room {
        add(StageKind::Adjustment, s.info_proc_room, room.clone());
    }
    if let Some(between) = &input.crossfade {
        add(
            StageKind::Adjustment,
            s.info_proc_crossfade,
            between.clone(),
        );
    }
    if let Some(speed) = &input.speed {
        add(StageKind::Adjustment, s.info_proc_speed, speed.clone());
    }
    if input.peak_protection {
        add(StageKind::Protection, s.info_proc_limiter, String::new());
    }
    if input.volume < 1.0 {
        let detail = if input.volume <= 0.0 {
            s.path_muted.to_string()
        } else {
            decibels(crate::audio::dsp::volume_to_gain(input.volume))
        };
        add(StageKind::Adjustment, s.info_proc_volume, detail);
    }
    if let Some(bits) = input.dither_bits {
        add(
            StageKind::Conversion,
            s.info_proc_dither,
            format!("{bits} bit"),
        );
    }
    stages
}

/// The most lines the meter deck's "processing" readout takes when the deck is tall
/// enough for them.
const PROCESSING_LINES: usize = 3;

/// The lines of the meter deck's "processing" readout: as many steps to a line as fit
/// into `width` (measured with `measure`, in the font they are drawn in), the first
/// line led by `label`. No more than `max_lines` lines: what does not fit goes on the
/// last one, which is cut short when drawn.
fn processing_lines(
    label: &str,
    none: &str,
    steps: &[String],
    width: f32,
    max_lines: usize,
    measure: impl Fn(&str) -> f32,
) -> Vec<String> {
    let Some((first, rest)) = steps.split_first() else {
        return vec![format!("{label}: {none}")];
    };
    let mut lines = vec![format!("{label}: {first}")];
    for step in rest {
        let last = lines.len() - 1;
        let joined = format!("{} · {step}", lines[last]);
        if measure(&joined) <= width || lines.len() >= max_lines.max(1) {
            lines[last] = joined;
        } else {
            lines.push(step.clone());
        }
    }
    lines
}

/// What a role in a song's credits is called.
pub(super) fn role_label(
    s: &crate::i18n::Strings,
    role: crate::library::details::Role,
) -> &'static str {
    use crate::library::details::Role;
    match role {
        Role::Composer => s.role_composer,
        Role::Lyricist => s.role_lyricist,
        Role::Conductor => s.role_conductor,
        Role::Arranger => s.role_arranger,
        Role::Performer => s.role_performer,
        Role::Producer => s.role_producer,
        Role::Engineer => s.role_engineer,
        Role::Mixer => s.role_mixer,
        Role::Remixer => s.role_remixer,
    }
}

/// The facts of a song's file that are worth a line each (work, label, dates), with
/// what each is called. A date that is just the song's year again says nothing new.
pub(super) fn facts_to_show(
    s: &crate::i18n::Strings,
    details: &crate::library::details::Details,
    year: Option<i32>,
) -> Vec<(&'static str, String)> {
    use crate::library::details::Fact;
    let year = year.map(|y| y.to_string());
    details
        .facts
        .iter()
        .filter(|(fact, text)| {
            let date = matches!(fact, Fact::Recorded | Fact::Released);
            !(date && Some(text) == year.as_ref())
        })
        .map(|(fact, text)| {
            let label = match fact {
                Fact::Work => s.info_work,
                Fact::Movement => s.info_movement,
                Fact::Label => s.info_label,
                Fact::CatalogNumber => s.info_catalog,
                Fact::Recorded => s.info_recorded,
                Fact::Released => s.info_released,
            };
            (label, text.clone())
        })
        .collect()
}

/// What the file of one song says beyond what the library keeps (credits, facts,
/// lyrics), read on a thread of its own: a file on a drive that has gone to sleep must
/// not hold up the window.
pub(super) struct Extras {
    track: crate::model::TrackId,
    /// The file's modification time when it was read: an edited file is read again.
    mtime_ns: i64,
    /// Whether the file was there: one that has come back is read then.
    available: bool,
    /// Still being read while this is there.
    pending: Option<std::sync::mpsc::Receiver<crate::library::details::Details>>,
    pub(super) details: Option<std::sync::Arc<crate::library::details::Details>>,
    /// The song's lyrics were looked for on the internet (or are being looked for:
    /// `lookup`). Only ever true for a listener who switched online lyrics on.
    asked_online: bool,
    /// The lookup of the lyrics on the internet, while it is out.
    lookup: Option<std::sync::mpsc::Receiver<crate::library::details::Details>>,
    /// Other tracks in the library that are the same song (see `other_versions`).
    versions: Vec<crate::model::TrackId>,
    /// The synchronised line that was current when the panel was last drawn, so that
    /// the lyrics are scrolled only when the line changes.
    shown_line: Option<usize>,
    /// Where that line was, from the top of the lyrics: the lyrics scroll there.
    line_top: Option<f32>,
    /// Scroll the lyrics to `line_top` in the next frame.
    scroll_due: bool,
}

impl Extras {
    /// How many other versions of the song the library has.
    pub(super) fn version_count(&self) -> usize {
        self.versions.len()
    }
}

/// A sample rate as it is spoken of: "44.1 kHz", "96 kHz".
pub(super) fn kilohertz(rate: u32) -> String {
    if rate.is_multiple_of(1000) {
        format!("{} kHz", rate / 1000)
    } else {
        format!("{:.1} kHz", f64::from(rate) / 1000.0)
    }
}

fn extras_scroll_pending(extras: Option<&Extras>) -> bool {
    extras.is_some_and(|extras| extras.scroll_due)
}

/// The most "other versions" of a song that are listed.
const MAX_VERSIONS: usize = 6;

/// How a title or an artist is compared when looking for the same song again:
/// without regard to case and to space around it.
/// The library's own spelling of `name`, if it has an artist of that name.
pub(super) fn known_artist(
    artists: &std::collections::HashMap<String, String>,
    name: &str,
) -> Option<String> {
    let wanted = folded(name);
    (!wanted.is_empty())
        .then(|| artists.get(&wanted).cloned())
        .flatten()
}

fn folded(text: &str) -> String {
    text.trim().to_lowercase()
}

impl OneumApp {
    /// The library's artists by their folded name, for looking names up in: collected
    /// once per change of the library, not once per frame. Of names that fold alike,
    /// the first in the library's order is the one kept.
    pub(super) fn artist_names(
        &mut self,
    ) -> std::sync::Arc<std::collections::HashMap<String, String>> {
        match &self.artist_names {
            Some((generation, names)) if *generation == self.lib_generation => names.clone(),
            _ => {
                let mut names = std::collections::HashMap::new();
                for artist in self.snapshot.artists().iter() {
                    names
                        .entry(folded(&artist.name))
                        .or_insert_with(|| artist.name.clone());
                }
                let names = std::sync::Arc::new(names);
                self.artist_names = Some((self.lib_generation, names.clone()));
                names
            }
        }
    }
}

/// Other tracks in the library that are the same song as `track`: the same title by
/// the same artist (another album, another mastering, another format). The best
/// sounding first: lossless before lossy, then the higher sample rate and bit depth.
fn other_versions(
    tracks: &[std::sync::Arc<crate::model::Track>],
    track: &crate::model::Track,
) -> Vec<crate::model::TrackId> {
    let (title, artist) = (folded(&track.title), folded(&track.artist));
    if title.is_empty() || artist.is_empty() {
        return Vec::new();
    }
    let lossless = |t: &crate::model::Track| t.is_lossless();
    let mut same: Vec<&crate::model::Track> = tracks
        .iter()
        .map(|t| &**t)
        .filter(|t| t.id != track.id)
        .filter(|t| folded(&t.title) == title && folded(&t.artist) == artist)
        .collect();
    same.sort_by_key(|t| {
        (
            std::cmp::Reverse(lossless(t)),
            std::cmp::Reverse(t.sample_rate),
            std::cmp::Reverse(t.bit_depth),
            t.id,
        )
    });
    same.into_iter().take(MAX_VERSIONS).map(|t| t.id).collect()
}

/// Colour of a stage's dot in the signal path.
fn stage_color(ui: &Ui, kind: StageKind) -> egui::Color32 {
    let dark = ui.visuals().dark_mode;
    match (kind, dark) {
        (StageKind::Adjustment, _) => theme::accent(dark),
        (StageKind::Conversion, true) => egui::Color32::from_rgb(0x5C, 0xC8, 0xB4),
        (StageKind::Conversion, false) => egui::Color32::from_rgb(0x1F, 0x8F, 0x7C),
        (StageKind::Protection, true) => egui::Color32::from_rgb(0xE6, 0xB4, 0x5A),
        (StageKind::Protection, false) => egui::Color32::from_rgb(0xB0, 0x78, 0x10),
    }
}

/// The signal path of the track that is playing, drawn as a chain from the file down
/// to the device: every stage the audio passes, in order, with what it does.
pub(super) fn signal_path_block(ui: &mut Ui, s: &crate::i18n::Strings, path: &SignalPath) {
    let dim = theme::dim(ui);
    let strong = ui.visuals().strong_text_color();
    let summary = path_summary(&path.stages);
    let (word, hint, color) = match summary {
        PathSummary::Untouched => (s.path_untouched, s.path_untouched_hint, strong),
        PathSummary::Adjusted => (
            s.path_adjusted,
            s.path_adjusted_hint,
            stage_color(ui, StageKind::Adjustment),
        ),
        PathSummary::Converted => (
            s.path_converted,
            s.path_converted_hint,
            stage_color(ui, StageKind::Conversion),
        ),
        PathSummary::ConvertedAndAdjusted => (
            s.path_converted_adjusted,
            s.path_converted_hint,
            stage_color(ui, StageKind::Conversion),
        ),
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(s.info_signal_path).small().color(dim));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(word).small().color(color))
                .on_hover_text(hint);
        });
    });
    ui.add_space(4.0);

    // The file at the top, the device at the bottom, the stages in between.
    let mut source = format!("{} · {}", path.codec, path.source_format);
    if path.in_memory {
        source = format!("{source} · {}", s.info_memory);
    }
    let mut nodes: Vec<(Option<StageKind>, &str, String)> = vec![(None, s.info_source, source)];
    for stage in &path.stages {
        nodes.push((Some(stage.kind), &stage.name, stage.detail.clone()));
    }
    nodes.push((
        None,
        s.info_output,
        format!("{} · {}", path.device, path.format),
    ));

    const GUTTER: f32 = 20.0;
    let mut dots: Vec<(egui::Pos2, Option<StageKind>)> = Vec::new();
    for (kind, name, detail) in &nodes {
        let top = ui.cursor().top();
        ui.horizontal_top(|ui| {
            ui.add_space(GUTTER);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                let title = RichText::new(*name)
                    .font(theme::semibold(12.5))
                    .color(strong);
                ui.add(egui::Label::new(title).truncate());
                if !detail.is_empty() {
                    ui.add(egui::Label::new(RichText::new(detail).small().color(dim)).wrap());
                }
            });
        });
        dots.push((egui::pos2(ui.min_rect().left() + 7.0, top + 9.0), *kind));
        ui.add_space(7.0);
    }
    let painter = ui.painter();
    let line = egui::Stroke::new(1.5, widgets::divider_color(ui));
    for pair in dots.windows(2) {
        painter.line_segment(
            [
                pair[0].0 + egui::vec2(0.0, 6.0),
                pair[1].0 - egui::vec2(0.0, 6.0),
            ],
            line,
        );
    }
    for (center, kind) in dots {
        match kind {
            // The two ends are rings, what happens in between is a filled dot.
            None => {
                painter.circle_stroke(center, 4.0, egui::Stroke::new(1.5, strong));
            }
            Some(kind) => {
                painter.circle_filled(center, 4.0, stage_color(ui, kind));
            }
        }
    }
}

const SIDEBAR_ROW: f32 = 28.0;
const QUEUE_ROW: f32 = 44.0;

/// A full-width selectable row in the sidebar.
/// How far to scroll per frame while something is dragged near the top or bottom edge of
/// a list occupying `rect`; `None` away from the edges.
fn edge_scroll_step(pointer_y: f32, rect: egui::Rect) -> Option<f32> {
    const ZONE: f32 = 28.0;
    const STEP: f32 = 9.0;
    if rect.height() < ZONE * 3.0 {
        None
    } else if (rect.top() - ZONE..rect.top() + ZONE).contains(&pointer_y) {
        Some(-STEP)
    } else if (rect.bottom() - ZONE..rect.bottom() + ZONE).contains(&pointer_y) {
        Some(STEP)
    } else {
        None
    }
}

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
                    // A search of the whole library can be kept as a smart playlist.
                    let can_save = nav == Nav::Songs;
                    let mut save_search: Option<String> = None;
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
                                let savable = can_save && !state.query.trim().is_empty();
                                let edit = egui::TextEdit::singleline(&mut state.query)
                                    .hint_text(s.search_hint)
                                    .frame(egui::Frame::NONE)
                                    .align(egui::Align2::LEFT_CENTER)
                                    .min_size(egui::vec2(0.0, 20.0))
                                    .desired_width(width - if savable { 96.0 } else { 72.0 });
                                let response = ui.add(edit).on_hover_text(s.search_syntax_hint);
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
                                if can_save
                                    && !state.query.trim().is_empty()
                                    && widgets::icon_button(
                                        ui,
                                        Icon::Plus,
                                        s.smart_save,
                                        20.0,
                                        false,
                                    )
                                    .clicked()
                                {
                                    save_search = Some(state.query.clone());
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
                    if let Some(query) = save_search {
                        let query = crate::app::settings::tidy_text(
                            &query,
                            crate::app::settings::MAX_SMART_QUERY_CHARS,
                        );
                        self.dialog = Some(Dialog::smart_playlist(&query, query.clone(), None));
                    }

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
                        let queue_button = widgets::icon_button(
                            ui,
                            Icon::Queue,
                            s.show_queue,
                            28.0,
                            panel == RightPanel::Queue,
                        );
                        if queue_button.clicked() {
                            self.toggle_right_panel(RightPanel::Queue);
                        }
                        // The button is a drop target too, so songs can be queued by
                        // dragging while the panel is closed.
                        if queue_button.dnd_hover_payload::<DragTracks>().is_some() {
                            ui.painter().rect_stroke(
                                queue_button.rect,
                                6.0,
                                egui::Stroke::new(1.5, theme::accent(ui.visuals().dark_mode)),
                                egui::StrokeKind::Inside,
                            );
                        }
                        if let Some(payload) = queue_button.dnd_release_payload::<DragTracks>() {
                            self.core
                                .player
                                .send(Command::Enqueue(payload.tracks.clone()));
                            self.info_toast(self.lang.added_to_queue(payload.tracks.len()));
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
                            if self.settings_open {
                                self.settings_open = false;
                            } else {
                                self.open_settings();
                            }
                        }
                        if widgets::icon_button(
                            ui,
                            Icon::Meter,
                            s.show_meter,
                            28.0,
                            self.settings.show_meter,
                        )
                        .clicked()
                        {
                            self.settings.show_meter = !self.settings.show_meter;
                        }
                        // Lit while the equalizer is changing the sound.
                        if widgets::icon_button(
                            ui,
                            Icon::Equalizer,
                            s.equalizer,
                            28.0,
                            self.settings.eq.is_active(),
                        )
                        .clicked()
                        {
                            self.handle_request(super::UiRequest::Equalizer);
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

    /// Where a pin leads and what it is called; `None` once what it pins is gone (a
    /// deleted playlist, an album that is no longer in the library).
    pub(super) fn pin_target(&self, pin: &Pin) -> Option<(Icon, String)> {
        let s = self.s();
        Some(match pin {
            Pin::Album(key) => {
                let album = self
                    .snapshot
                    .album_index(*key)
                    .and_then(|i| self.snapshot.albums().get(i as usize))?;
                let title = if album.title.is_empty() {
                    s.unknown_album.to_string()
                } else {
                    album.title.clone()
                };
                (Icon::Album, title)
            }
            Pin::Artist(name) => (
                Icon::Person,
                if name.is_empty() {
                    s.unknown_artist.to_string()
                } else {
                    name.clone()
                },
            ),
            Pin::Genre(name) => (Icon::Tag, name.clone()),
            Pin::Playlist(id) => (
                Icon::Playlist,
                self.playlists.iter().find(|p| p.id == *id)?.name.clone(),
            ),
            Pin::Smart(name) => self
                .settings
                .smart_playlists
                .iter()
                .any(|p| &p.name == name)
                .then_some((Icon::Search, name.clone()))?,
        })
    }

    /// The pins that still lead somewhere (those the sidebar shows). Only these count
    /// toward the limit: one whose playlist was deleted makes room.
    /// Albums are given by the key they are shown under now (see `shown_pin`), each
    /// pin once.
    pub(super) fn live_pins(&self) -> Vec<Pin> {
        let mut live: Vec<Pin> = self
            .settings
            .pinned
            .iter()
            .filter(|pin| self.pin_target(pin).is_some())
            .cloned()
            .collect();
        super::restate_pins(&mut live, |pin| super::shown_pin(&self.snapshot, pin));
        live
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
                // Pinned things first, those that still exist.
                let pins: Vec<(Pin, Icon, String)> = self
                    .settings
                    .pinned
                    .iter()
                    .filter_map(|pin| {
                        let (icon, name) = self.pin_target(pin)?;
                        Some((pin.clone(), icon, name))
                    })
                    .collect();
                let live_pins: Vec<Pin> = pins.iter().map(|(pin, _, _)| pin.clone()).collect();
                if !pins.is_empty() {
                    header(ui, s.pinned);
                    for (pin, icon, name) in pins {
                        let nav = super::pin_nav(&pin);
                        let response = sidebar_row(ui, self.nav == nav, icon, &name, None);
                        if response.clicked() {
                            actions.push(Action::Go(nav));
                        }
                        response.context_menu(|ui| {
                            if ui.button(s.unpin).clicked() {
                                actions.push(Action::TogglePin(pin.clone()));
                                ui.close();
                            }
                        });
                    }
                    ui.add_space(4.0);
                }
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
                    (Nav::Genres, Icon::Tag, s.genres, None),
                    (Nav::Favorites, Icon::Heart, s.favorites, None),
                    (Nav::Recent, Icon::Clock, s.recently_played, None),
                    (
                        Nav::Folder(std::path::PathBuf::new()),
                        Icon::Folder,
                        s.folders,
                        None,
                    ),
                    (Nav::Stats, Icon::Meter, s.stats, None),
                    (Nav::Network, Icon::Network, s.network, None),
                ];
                for (nav, icon, label, trailing) in entries {
                    let selected = self.nav == nav
                        || matches!(
                            (&self.nav, &nav),
                            (Nav::Album(_), Nav::Albums)
                                | (Nav::Artist(_), Nav::Artists)
                                | (Nav::Genre(_), Nav::Genres)
                        ) && self.history.first() == Some(&nav)
                        || matches!(&self.nav, Nav::Folder(_)) && matches!(&nav, Nav::Folder(_));
                    if sidebar_row(ui, selected, icon, label, trailing.filter(|_| total > 0))
                        .clicked()
                    {
                        actions.push(Action::Go(nav));
                    }
                }

                ui.add_space(6.0);
                let header = ui
                    .horizontal(|ui| {
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
                    })
                    .response;
                // Songs dropped on the heading become a new playlist.
                if header.dnd_hover_payload::<DragTracks>().is_some() {
                    ui.painter().rect_stroke(
                        header.rect.expand(2.0),
                        6.0,
                        egui::Stroke::new(1.5, theme::accent(ui.visuals().dark_mode)),
                        egui::StrokeKind::Inside,
                    );
                    ui.painter().text(
                        header.rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "＋",
                        egui::FontId::proportional(13.0),
                        theme::accent(ui.visuals().dark_mode),
                    );
                }
                if let Some(payload) = header.dnd_release_payload::<DragTracks>() {
                    self.dialog = Some(Dialog::new_playlist(
                        s.untitled_playlist,
                        payload.tracks.clone(),
                    ));
                }
                egui::ScrollArea::vertical()
                    .id_salt("playlists")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // Smart playlists first: searches kept under a name.
                        let smart: Vec<String> = self
                            .settings
                            .smart_playlists
                            .iter()
                            .map(|p| p.name.clone())
                            .collect();
                        for name in smart {
                            let response = sidebar_row(
                                ui,
                                self.nav == Nav::Smart(name.clone()),
                                Icon::Search,
                                &name,
                                None,
                            )
                            .on_hover_text(s.smart_hint);
                            if response.clicked() {
                                actions.push(Action::Go(Nav::Smart(name.clone())));
                            }
                            response.context_menu(|ui| {
                                if ui.button(s.play_now).clicked() {
                                    let query = self
                                        .settings
                                        .smart_playlists
                                        .iter()
                                        .find(|p| p.name == name)
                                        .map(|p| p.query.clone())
                                        .unwrap_or_default();
                                    let tracks: Vec<crate::model::TrackId> = self
                                        .snapshot
                                        .search(&query)
                                        .into_iter()
                                        .filter_map(|i| self.snapshot.tracks.get(i as usize))
                                        .map(|t| t.id)
                                        .collect();
                                    if !tracks.is_empty() {
                                        actions.push(Action::PlayTracks(tracks, 0));
                                    }
                                    ui.close();
                                }
                                super::pin_menu_item(
                                    ui,
                                    s,
                                    &live_pins,
                                    Pin::Smart(name.clone()),
                                    actions,
                                );
                                ui.separator();
                                if ui.button(s.smart_edit).clicked() {
                                    actions.push(Action::EditSmart(name.clone()));
                                    ui.close();
                                }
                                if ui.button(s.rename).clicked() {
                                    actions.push(Action::RenameSmart(name.clone()));
                                    ui.close();
                                }
                                ui.separator();
                                if ui.button(s.delete).clicked() {
                                    actions.push(Action::DeleteSmart(name.clone()));
                                    ui.close();
                                }
                            });
                        }
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
                                    if enter && !new_name.is_empty() && new_name != name {
                                        self.edit(move |library| {
                                            library
                                                .rename_playlist(id, &new_name)
                                                .err()
                                                .map(super::AppEvent::EditFailed)
                                        });
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
                            // Songs dragged here are added to this playlist (dragging a
                            // playlist's own rows onto itself does nothing).
                            let from_other_list = |payload: &DragTracks| {
                                payload
                                    .playlist_items
                                    .as_ref()
                                    .is_none_or(|(from, _)| *from != id)
                            };
                            if response
                                .dnd_hover_payload::<DragTracks>()
                                .is_some_and(|p| from_other_list(&p))
                            {
                                ui.painter().rect_stroke(
                                    response.rect,
                                    6.0,
                                    egui::Stroke::new(1.5, theme::accent(ui.visuals().dark_mode)),
                                    egui::StrokeKind::Inside,
                                );
                            }
                            if let Some(payload) = response.dnd_release_payload::<DragTracks>()
                                && from_other_list(&payload)
                            {
                                actions.push(Action::AddToPlaylist(id, payload.tracks.clone()));
                            }
                            response.context_menu(|ui| {
                                if ui
                                    .add_enabled(count > 0, egui::Button::new(s.play_now))
                                    .clicked()
                                {
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
                                super::pin_menu_item(ui, s, &live_pins, Pin::Playlist(id), actions);
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
        // A transient message is shown in front of a standing one for its few seconds.
        let showing_transient = self.toast.is_some();
        let Some(toast) = self.toast.as_ref().or(self.alert.as_ref()) else {
            return;
        };
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
                            run = Some(action.clone());
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
                    self.open_settings();
                    self.settings_tab = super::settings_window::Tab::Playback;
                }
                ToastAction::RestorePlaylist(id, tracks) => self.restore_playlist(id, tracks),
            }
            dismiss = true;
        }
        if dismiss {
            if showing_transient {
                self.toast = None;
            } else {
                self.alert = None;
            }
        }
    }

    /// What the file of `track` says beyond the library (see [`Extras`]), once it has
    /// been read. `None` while it is being read.
    ///
    /// `words`: the caller shows the song's lyrics. Then, for a listener who has
    /// switched online lyrics on, a song whose file has no lyrics that follow it is
    /// looked for on the internet (once the file has been read: what the file says is
    /// shown without waiting for that).
    pub(super) fn extras_of(
        &mut self,
        track: &crate::model::Track,
        words: bool,
    ) -> Option<std::sync::Arc<crate::library::details::Details>> {
        // Two places show what a file says, and on a phone both can be up at once
        // with different songs (the lyrics of the song that plays, and the sheet of
        // another song over them): each has its own, or they would take it from
        // each other frame after frame.
        let mut slot = match words {
            true => self.extras.take(),
            false => self.info_extras.take(),
        };
        let details = self.extras_in(&mut slot, track, words);
        match words {
            true => self.extras = slot,
            false => self.info_extras = slot,
        }
        details
    }

    fn extras_in(
        &mut self,
        slot: &mut Option<Extras>,
        track: &crate::model::Track,
        words: bool,
    ) -> Option<std::sync::Arc<crate::library::details::Details>> {
        use std::sync::mpsc::TryRecvError;
        // Collect what has finished: the read of the file, the lookup of the lyrics.
        if let Some(extras) = slot.as_mut() {
            if let Some(pending) = &extras.pending {
                match pending.try_recv() {
                    Ok(details) => {
                        extras.details = Some(std::sync::Arc::new(details));
                        extras.pending = None;
                    }
                    Err(TryRecvError::Disconnected) => extras.pending = None,
                    Err(TryRecvError::Empty) => {}
                }
            }
            if let Some(lookup) = &extras.lookup {
                match lookup.try_recv() {
                    Ok(details) => {
                        extras.details = Some(std::sync::Arc::new(details));
                        extras.lookup = None;
                    }
                    Err(TryRecvError::Disconnected) => extras.lookup = None,
                    Err(TryRecvError::Empty) => {}
                }
            }
        }
        let known = slot.as_ref().is_some_and(|e| {
            e.track == track.id && e.mtime_ns == track.mtime_ns && e.available == track.available
        });
        if known {
            if words && self.settings.online_lyrics {
                self.seek_lyrics_online(slot, track);
            }
            return slot.as_ref().and_then(|e| e.details.clone());
        }
        // Another song. One read at a time: stepping through a list on a drive that is
        // slow to answer must not start a thread for every row passed. While a read
        // for an earlier song is still out, this one waits (the thread wakes the
        // window when it is done, and the question is asked again). (A lookup of
        // lyrics on the internet does not make a file wait: it may take seconds.)
        if slot.as_ref().is_some_and(|e| e.pending.is_some()) {
            return None;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let path = track.path.clone();
        let core = self.core.clone();
        let started = track.available
            && std::thread::Builder::new()
                .name("oneum-details".into())
                .spawn(move || {
                    let _ = tx.send(crate::library::details::read(&path));
                    core.request_repaint();
                })
                .is_ok();
        *slot = Some(Extras {
            track: track.id,
            mtime_ns: track.mtime_ns,
            available: track.available,
            pending: started.then_some(rx),
            details: None,
            asked_online: false,
            lookup: None,
            versions: other_versions(&self.snapshot.tracks, track),
            shown_line: None,
            line_top: None,
            scroll_due: false,
        });
        None
    }

    /// Looks for the lyrics of `track` on the internet, once: when its file has been
    /// read and has none that follow the song. Only called while the listener has
    /// online lyrics switched on and the lyrics are shown. One song at a time: while
    /// a lookup for another song is still out, this one waits its turn.
    fn seek_lyrics_online(&mut self, slot: &mut Option<Extras>, track: &crate::model::Track) {
        use std::sync::atomic::Ordering;
        let Some(extras) = slot.as_mut() else {
            return;
        };
        let Some(details) = extras.details.clone() else {
            return;
        };
        if extras.asked_online {
            return;
        }
        let song = crate::net::lyrics::Song {
            title: &track.title,
            artist: &track.artist,
            album: &track.album,
            duration_ms: track.duration_ms,
        };
        if !details.wants_online_lyrics() || !song.can_be_asked_for() {
            extras.asked_online = true;
            return;
        }
        // (The thread that is out wakes the window when it is done: asked again then.)
        if self.lyrics_busy.swap(true, Ordering::AcqRel) {
            return;
        }
        extras.asked_online = true;
        let (title, artist, album, duration_ms) = (
            track.title.clone(),
            track.artist.clone(),
            track.album.clone(),
            track.duration_ms,
        );
        let source = self.lyrics_source.clone();
        let core = self.core.clone();
        // Given back when the lookup is over, however it ends: a lookup that broke
        // off must not be the last one of the run.
        struct Turn(std::sync::Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Turn {
            fn drop(&mut self) {
                self.0.store(false, std::sync::atomic::Ordering::Release);
            }
        }
        let turn = Turn(self.lyrics_busy.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        let started = std::thread::Builder::new()
            .name("oneum-lyrics".into())
            .spawn(move || {
                let song = crate::net::lyrics::Song {
                    title: &title,
                    artist: &artist,
                    album: &album,
                    duration_ms,
                };
                let details = crate::library::details::with_online_lyrics(
                    std::sync::Arc::unwrap_or_clone(details),
                    &source,
                    &song,
                );
                drop(turn);
                let _ = tx.send(details);
                core.request_repaint();
            })
            .is_ok();
        // (A thread that could not be started never took the closure: the turn was
        // dropped with it, and is free again.)
        extras.lookup = started.then_some(rx);
    }

    /// Switches lyrics from the internet on or off. What is shown of the song at hand
    /// is read again: lyrics that were found go when the switch does, and a song
    /// without any is looked for once it is on.
    pub(super) fn set_online_lyrics(&mut self, on: bool) {
        self.settings.online_lyrics = on;
        self.extras = None;
    }

    /// The lyrics of the song whose details are shown are being looked for on the
    /// internet right now.
    pub(super) fn seeking_lyrics(&self) -> bool {
        self.extras.as_ref().is_some_and(|e| e.lookup.is_some())
    }

    /// Synchronised lyrics of the song that is playing, in a box of their own that
    /// scrolls with the song (the info panel around it stays where the user left it).
    fn synced_lyrics(
        &mut self,
        ui: &mut Ui,
        lyrics: &crate::library::lyrics::Lyrics,
        lines: &[crate::library::lyrics::TimedLine],
    ) {
        const HEIGHT: f32 = 230.0;
        let s = self.s();
        let position = self.player.position_now_ms();
        let current = lyrics.line_at(position);
        let Some(extras) = self.extras.as_mut() else {
            return;
        };
        let line_changed = extras.shown_line != current;
        extras.shown_line = current;
        let mut area = egui::ScrollArea::vertical()
            .id_salt("lyrics")
            .max_height(HEIGHT)
            // Inside the panel's own scroll area "what is left" is next to nothing:
            // the box gets its height whenever the lyrics are longer than it.
            .min_scrolled_height(HEIGHT)
            .auto_shrink([false, true]);
        // One frame after the line changed, when its place is known: bring it to the
        // middle of the box.
        if std::mem::take(&mut extras.scroll_due)
            && let Some(top) = extras.line_top
        {
            area = area.vertical_scroll_offset((top - HEIGHT * 0.4).max(0.0));
        }
        let dim = theme::dim(ui);
        let strong = ui.visuals().strong_text_color();
        let accent = theme::accent(ui.visuals().dark_mode);
        let mut seek = None;
        let mut current_top = None;
        area.show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 5.0;
            let origin = ui.min_rect().top();
            for (index, line) in lines.iter().enumerate() {
                // A stamp without words is a pause in the singing.
                let words = if line.text.is_empty() {
                    "♪"
                } else {
                    &line.text
                };
                let text = if current == Some(index) {
                    RichText::new(words)
                        .font(theme::semibold(13.5))
                        .color(accent)
                } else if current.is_some_and(|now| index < now) {
                    RichText::new(words).size(12.5).color(dim)
                } else {
                    RichText::new(words)
                        .size(12.5)
                        .color(strong.gamma_multiply(0.8))
                };
                let response = ui
                    .add(egui::Label::new(text).wrap().sense(Sense::click()))
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text(s.lyrics_seek_hint);
                if current == Some(index) {
                    current_top = Some(response.rect.top() - origin);
                }
                let mut clicked = response.clicked();
                // A translation or pronunciation of the same moment, right under it
                // and a little quieter.
                for also in &line.also {
                    let color = if current == Some(index) {
                        accent.gamma_multiply(0.75)
                    } else if current.is_some_and(|now| index < now) {
                        dim
                    } else {
                        strong.gamma_multiply(0.6)
                    };
                    let response = ui
                        .add(
                            egui::Label::new(RichText::new(also).size(11.5).color(color))
                                .wrap()
                                .sense(Sense::click()),
                        )
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text(s.lyrics_seek_hint);
                    clicked |= response.clicked();
                }
                if clicked {
                    seek = Some(u64::from(line.at_ms));
                }
            }
        });
        if let Some(extras) = self.extras.as_mut() {
            if line_changed && current.is_some() {
                extras.scroll_due = true;
            }
            if current_top.is_some() {
                extras.line_top = current_top;
            }
        }
        if let Some(position) = seek {
            self.core.player.send(Command::SeekTo(position));
        }
        // Wake up for the next line rather than for the next quarter of a second.
        if self.player.state == PlayState::Playing {
            let next = current.map_or(0, |now| now + 1);
            if let Some(line) = lines.get(next) {
                let wait = u64::from(line.at_ms)
                    .saturating_sub(position)
                    .clamp(20, 1000);
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(wait));
            } else if extras_scroll_pending(self.extras.as_ref()) {
                ui.ctx().request_repaint();
            }
        } else if extras_scroll_pending(self.extras.as_ref()) {
            ui.ctx().request_repaint();
        }
    }

    /// What is happening to the track that is playing: where it comes from, where it
    /// goes and what the app does to it in between. `None` while nothing is playing.
    pub(super) fn signal_path(&self) -> Option<SignalPath> {
        let s = self.s();
        let current = self.player.current?;
        let source = self.player.source.as_ref()?;
        let out_rate = self.player.output_sample_rate?;
        let track = self.track(current.track_id)?;
        let hz = |rate: u32| format!("{} Hz", group_digits(rate as usize));

        // DSD: the file's own rate (what plays is the PCM it is converted to).
        let mut source_format = hz(source.dsd_rate.unwrap_or(source.sample_rate));
        if let Some(bits) = track.bit_depth {
            source_format = format!("{source_format} · {bits} bit");
        }
        let status = self.player.output.as_ref();
        let mut format = hz(out_rate);
        if let Some((bits, float)) = status.and_then(|o| o.hardware_bits) {
            let kind = if float { " float" } else { "" };
            format = format!("{format} · {bits} bit{kind}");
        }
        if status.is_some_and(|o| o.exclusive) {
            format = format!("{format} · {}", s.info_exclusive);
        }
        // Everything the app does to the samples on their way out.
        let equalizer = self.player.eq.is_active().then(|| {
            use super::equalizer::{PresetRef, builtin_name, matching_preset};
            match matching_preset(&self.player.eq, &self.settings.eq_presets) {
                Some(PresetRef::Builtin(i)) => {
                    builtin_name(s, crate::audio::eq::BUILTIN_PRESETS[i].id).to_string()
                }
                Some(PresetRef::User(i)) => self.settings.eq_presets[i].name.clone(),
                None => s.eq_custom.to_string(),
            }
        });
        let stages = path_stages(&PathInputs {
            s,
            dsd_rate: source.dsd_rate,
            source_rate: source.sample_rate,
            out_rate,
            channels: source.channels,
            leveling: replaygain_factor(&track.leveling(), &self.player.replaygain),
            equalizer,
            correction: self.player.correction.as_deref(),
            crossfeed: match self.player.crossfeed {
                crate::audio::Crossfeed::Off => None,
                crate::audio::Crossfeed::Light => Some(s.crossfeed_light),
                crate::audio::Crossfeed::Medium => Some(s.crossfeed_medium),
                crate::audio::Crossfeed::Strong => Some(s.crossfeed_strong),
            },
            crossfade: (self.player.crossfade_secs > 0)
                .then(|| self.lang.crossfade_detail(self.player.crossfade_secs)),
            speed: (self.player.speed_percent != 100)
                .then(|| self.lang.speed_detail(self.player.speed_percent)),
            filter: {
                use crate::audio::upsample::Filter;
                let filter = self.player.output_settings.filter;
                // (At another speed the song goes through a rate of its own on the
                // way: converted the usual way.)
                (self.player.speed_percent == 100 && filter.applies(source.sample_rate, out_rate))
                    .then_some(match filter {
                        Filter::Sharp => s.filter_sharp,
                        Filter::Slow => s.filter_slow,
                        Filter::Minimum => s.filter_minimum,
                    })
            },
            room: self.player.room.as_ref().map(|(name, trim)| {
                // (How far it is turned down so that nothing gets louder than it
                // was, where that is more than nothing.)
                match *trim < 0.999 {
                    true => format!("{name} · {}", decibels(*trim)),
                    false => name.clone(),
                }
            }),
            peak_protection: self.player.peak_limited_frames > 0,
            volume: self.player.volume,
            dither_bits: self.player.dither_bits,
        });
        Some(SignalPath {
            track_id: current.track_id,
            codec: track.codec.clone(),
            source_format,
            device: self.player.output_device.clone().unwrap_or_default(),
            format,
            stages,
            in_memory: source.in_memory,
        })
    }

    /// One side of the meter deck: a small heading, a main line and a detail line,
    /// aligned towards the meters.
    fn deck_readout(
        ui: &mut Ui,
        rect: egui::Rect,
        towards_right: bool,
        heading: &str,
        main: &str,
        details: &[String],
    ) {
        let align = if towards_right {
            Align::Max
        } else {
            Align::Min
        };
        let dim = theme::dim(ui);
        // Lines of about 18 points each, centred on the deck.
        let height = 26.0 + 18.0 * (details.len() + 1) as f32;
        let rect = egui::Rect::from_center_size(rect.center(), egui::vec2(rect.width(), height));
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::top_down(align)),
            |ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.add(egui::Label::new(RichText::new(heading).small().color(dim)).truncate());
                ui.add(
                    egui::Label::new(RichText::new(main).font(theme::semibold(14.0))).truncate(),
                );
                for detail in details {
                    ui.add(egui::Label::new(RichText::new(detail).small().color(dim)).truncate());
                }
            },
        );
    }

    /// The VU meter deck: the left and right channel side by side in a strip above the
    /// player bar, like the meters on the front of an amplifier. Drag its top edge to
    /// make the meters larger or smaller.
    pub(super) fn meter_deck(&mut self, ui: &mut Ui) {
        if !self.settings.show_meter {
            self.meter.suspend();
            self.spectrum.suspend();
            return;
        }
        let s = self.s();
        let frame = egui::Frame::new()
            .fill(meter::deck_fill(ui.visuals().dark_mode))
            .inner_margin(egui::Margin::symmetric(16, 12));
        egui::Panel::bottom("meter-deck")
            .frame(frame)
            .resizable(true)
            .default_size(meter::DECK_DEFAULT_HEIGHT)
            .size_range(meter::DECK_HEIGHT_RANGE)
            .show(ui, |ui| {
                // The needles follow the music only while it is actually sounding; in
                // every other state they fall back to rest.
                let levels =
                    (self.player.state == PlayState::Playing).then(|| self.core.player.levels());
                let busy = self.meter.update(levels, std::time::Instant::now());
                let (area, _) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
                let channels = [s.meter_left_name, s.meter_right_name];
                let labels = [s.meter_left, s.meter_right];
                let layout = meter::deck_layout(area);
                let dials = layout.dials;
                // The spectrum display follows the same rule as the needles.
                let mut busy = busy;
                match layout.spectrum {
                    Some(rect) => {
                        let levels = (self.player.state == PlayState::Playing).then(|| {
                            let mut samples = [0f32; spectrum::MAX_SAMPLES];
                            self.core.player.scope_samples(&mut samples);
                            let rate = self.player.output_sample_rate.unwrap_or(0);
                            self.spectrum.analyse(&samples, rate)
                        });
                        busy |= self.spectrum.update(levels, std::time::Instant::now());
                        let response =
                            ui.interact(rect, ui.scope_id().with("spectrum"), Sense::hover());
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::Role::Meter, true, s.spectrum_name)
                        });
                        if ui.is_rect_visible(rect) {
                            spectrum::draw(
                                ui.painter(),
                                rect,
                                &self.spectrum.bars,
                                &self.spectrum.caps,
                            );
                        }
                    }
                    None => self.spectrum.suspend(),
                }
                // Beside the meters, when there is room: what goes in on the left, what
                // comes out on the right.
                let side = dials[0].left() - area.left() - meter::DECK_READOUT_GAP;
                if side >= meter::DECK_READOUT_MIN_WIDTH
                    && let Some(path) = self.signal_path()
                {
                    let left = egui::Rect::from_min_max(
                        area.left_top(),
                        egui::pos2(dials[0].left() - meter::DECK_READOUT_GAP, area.bottom()),
                    );
                    let right = egui::Rect::from_min_max(
                        egui::pos2(dials[1].right() + meter::DECK_READOUT_GAP, area.top()),
                        area.right_bottom(),
                    );
                    let mut from = vec![path.source_format.clone()];
                    if path.in_memory {
                        from.push(s.info_memory.to_string());
                    }
                    Self::deck_readout(ui, left, true, s.info_source, &path.codec, &from);
                    let mut details = vec![path.format.clone()];
                    let steps: Vec<String> =
                        path.stages.iter().map(|stage| stage.name.clone()).collect();
                    let small = egui::TextStyle::Small.resolve(ui.style());
                    let measure = |text: &str| {
                        ui.painter()
                            .layout_no_wrap(text.to_string(), small.clone(), egui::Color32::WHITE)
                            .size()
                            .x
                    };
                    details.extend(processing_lines(
                        s.info_processing,
                        s.info_proc_none,
                        &steps,
                        right.width(),
                        // What the deck's height leaves under the heading, the device
                        // and the format line (18 points a line, see `deck_readout`).
                        (((right.height() - 26.0) / 18.0) as usize)
                            .saturating_sub(2)
                            .clamp(1, PROCESSING_LINES),
                        measure,
                    ));
                    Self::deck_readout(ui, right, false, s.info_output, &path.device, &details);
                }
                for (ch, rect) in dials.into_iter().enumerate() {
                    let response =
                        ui.interact(rect, ui.scope_id().with(("vu-dial", ch)), Sense::hover());
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::Role::Meter, true, channels[ch])
                    });
                    response.on_hover_text(s.meter_note);
                    if ui.is_rect_visible(rect) {
                        meter::dial(
                            ui.painter(),
                            rect,
                            self.meter.needles[ch].position,
                            self.meter.peak_lit(ch),
                            labels[ch],
                            // Channel letters sit in the outer corners.
                            ch == 1,
                        );
                    }
                }
                if busy {
                    // Only while something moves: an idle window is not redrawn.
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(16));
                }
            });
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
        // Songs dragged from a list and dropped anywhere on the panel join the queue.
        let panel_rect = ui.max_rect();
        if let Some(payload) = egui::DragAndDrop::payload::<DragTracks>(ui.ctx())
            && ui.rect_contains_pointer(panel_rect)
        {
            ui.painter().rect_stroke(
                panel_rect.shrink(2.0),
                6.0,
                egui::Stroke::new(2.0, theme::accent(ui.visuals().dark_mode)),
                egui::StrokeKind::Inside,
            );
            if ui.input(|i| i.pointer.any_released()) {
                egui::DragAndDrop::clear_payload(ui.ctx());
                self.core
                    .player
                    .send(Command::Enqueue(payload.tracks.clone()));
                self.info_toast(self.lang.added_to_queue(payload.tracks.len()));
            }
        }
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
                if !queue.is_empty()
                    && ui
                        .small_button(s.queue_save)
                        .on_hover_text(s.queue_save_hint)
                        .clicked()
                {
                    let tracks = queue.iter().map(|item| item.track).collect();
                    self.dialog = Some(Dialog::new_playlist(s.untitled_playlist, tracks));
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
        let drag_id = egui::Id::unique("queue-drag");
        let dragging_now = ui
            .ctx()
            .data(|d| d.get_temp::<bool>(drag_id).unwrap_or(false));
        // Follow the playing track, but never pull the list away from under the pointer
        // while the user is reading, dragging or using a menu in it.
        let busy = dragging_now
            || ui.rect_contains_pointer(ui.available_rect_before_wrap())
            || egui::Popup::is_any_open(ui.ctx());
        let forced = std::mem::take(&mut self.queue_scroll_forced);
        let scroll_to_current =
            std::mem::take(&mut self.queue_scroll_to_current) && (forced || !busy);
        let mut commands: Vec<Command> = Vec::new();
        let key_row = self.queue_keys(ui, &queue, &mut commands);

        let mut area = egui::ScrollArea::vertical()
            .id_salt("queue")
            .auto_shrink([false, false]);
        if let Some(row) = key_row {
            // Keep the keyboard cursor in view, moving the list as little as possible.
            let (offset, height) = self.queue_view;
            let top = row as f32 * QUEUE_ROW;
            if top < offset {
                area = area.vertical_scroll_offset(top);
            } else if top + QUEUE_ROW > offset + height {
                area = area.vertical_scroll_offset(top + QUEUE_ROW - height);
            }
        } else if dragging_now
            && let Some(pointer) = ui.ctx().pointer_interact_pos()
            && let Some(step) = edge_scroll_step(pointer.y, self.queue_rect)
        {
            // Dragging towards the top or bottom edge moves the list along, so an item
            // can be dropped further away than what is visible.
            let max = (queue.len() as f32 * QUEUE_ROW - self.queue_view.1).max(0.0);
            area = area.vertical_scroll_offset((self.queue_view.0 + step).clamp(0.0, max));
            ui.ctx().request_repaint();
        } else if scroll_to_current && let Some(index) = current_index {
            // Leave one earlier row visible for context.
            area = area.vertical_scroll_offset((index.saturating_sub(1)) as f32 * QUEUE_ROW);
        }
        ui.spacing_mut().item_spacing.y = 0.0;
        let mut row_clicked = false;
        let output = area.show_rows(ui, QUEUE_ROW, queue.len(), |ui, range| {
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
                if response.clicked() || response.secondary_clicked() || response.drag_started() {
                    row_clicked = true;
                }
                if response.double_clicked() {
                    commands.push(Command::JumpTo(item.id));
                } else if response.clicked() {
                    self.queue_cursor = Some(item.id);
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
                    self.queue_cursor = Some(item.id);
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
        self.queue_view = (output.state.clamped_offset().y, output.inner_rect.height());
        self.queue_rect = output.inner_rect;
        if row_clicked {
            self.key_pane = KeyPane::Queue;
        }
        if ui.input(|i| i.pointer.any_released() || !i.pointer.any_down()) {
            ui.ctx().data_mut(|d| d.remove_temp::<bool>(drag_id));
        }
        for command in commands {
            if matches!(command, Command::RemoveItems(_)) {
                self.queue_selection.clear();
                if let Some(next) = self.queue_pending_selection.take() {
                    self.queue_selection.insert(next);
                    self.queue_anchor = Some(next);
                    self.queue_cursor = Some(next);
                }
            }
            self.core.player.send(command);
        }
    }

    /// Keyboard control of the queue while it has the keys: ↑/↓ (with ⇧ to extend),
    /// Return plays, Delete removes, ⌥↑/⌥↓ moves the selection, ⌘A selects all.
    /// Returns the row the cursor moved to, if it moved.
    fn queue_keys(
        &mut self,
        ui: &Ui,
        queue: &[crate::playback::QueueItem],
        commands: &mut Vec<Command>,
    ) -> Option<usize> {
        if self.key_pane != KeyPane::Queue
            || self.typing
            || self.widget_focused
            || ui.ctx().text_edit_focused()
            || self.dialog.is_some()
            || self.sheet_open()
            || queue.is_empty()
        {
            return None;
        }
        let select_all = ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::A));
        let (down, up, enter, delete, shift, alt) = ui.input(|i| {
            let free = !i.modifiers.command;
            (
                free && i.key_pressed(egui::Key::ArrowDown),
                free && i.key_pressed(egui::Key::ArrowUp),
                free && i.key_pressed(egui::Key::Enter),
                free && (i.key_pressed(egui::Key::Backspace) || i.key_pressed(egui::Key::Delete)),
                i.modifiers.shift,
                i.modifiers.alt,
            )
        });
        if select_all {
            self.queue_selection = queue.iter().map(|q| q.id).collect();
        }
        let position = |id: Option<u64>| id.and_then(|id| queue.iter().position(|q| q.id == id));
        let selected: Vec<u64> = queue
            .iter()
            .filter(|q| self.queue_selection.contains(&q.id))
            .map(|q| q.id)
            .collect();
        let mut moved_to = None;
        if (down || up) && alt {
            // Move the selected items one place, keeping them together.
            if let Some(first) = queue
                .iter()
                .position(|q| self.queue_selection.contains(&q.id))
            {
                let target = if up {
                    first.checked_sub(1)
                } else if first + selected.len() < queue.len() {
                    Some(first + 1)
                } else {
                    None
                };
                if let Some(to_index) = target {
                    commands.push(Command::MoveItems {
                        ids: selected.clone(),
                        to_index,
                    });
                    moved_to = Some(if up {
                        to_index
                    } else {
                        to_index + selected.len() - 1
                    });
                }
            }
        } else if down || up {
            let last = queue.len() - 1;
            let cursor = position(self.queue_cursor.or(self.queue_anchor));
            let next = match (cursor, down) {
                (Some(i), true) => (i + 1).min(last),
                (Some(i), false) => i.saturating_sub(1),
                (None, true) => 0,
                (None, false) => last,
            };
            let id = queue[next].id;
            if shift {
                let from = position(self.queue_anchor).unwrap_or(next);
                self.queue_selection = queue[from.min(next)..=from.max(next)]
                    .iter()
                    .map(|q| q.id)
                    .collect();
                self.queue_anchor.get_or_insert(id);
            } else {
                self.queue_selection.clear();
                self.queue_selection.insert(id);
                self.queue_anchor = Some(id);
            }
            self.queue_cursor = Some(id);
            moved_to = Some(next);
        }
        if enter && let Some(index) = position(self.queue_cursor.or(self.queue_anchor)) {
            commands.push(Command::JumpTo(queue[index].id));
        }
        if delete && !selected.is_empty() {
            // The row after the removed block becomes the selection, so that Delete can
            // be pressed repeatedly.
            let after = queue
                .iter()
                .skip_while(|q| !self.queue_selection.contains(&q.id))
                .find(|q| !self.queue_selection.contains(&q.id))
                .or_else(|| {
                    queue
                        .iter()
                        .rev()
                        .find(|q| !self.queue_selection.contains(&q.id))
                })
                .map(|q| q.id);
            commands.push(Command::RemoveItems(selected));
            self.queue_pending_selection = after;
        }
        moved_to
    }

    /// The other tracks of the library that are the same song as `track` (another
    /// album, another format), once its extras have been read (see `extras_of`).
    pub(super) fn other_versions_of(
        &self,
        track: crate::model::TrackId,
    ) -> Vec<std::sync::Arc<crate::model::Track>> {
        [&self.extras, &self.info_extras]
            .into_iter()
            .filter_map(Option::as_ref)
            .find(|extras| extras.track == track)
            .map(|extras| {
                extras
                    .versions
                    .iter()
                    .filter_map(|id| self.track(*id).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// What is known about a song, a line each: what the library keeps (place on the
    /// album, format, leveling, what the file check found, plays) and the facts of its
    /// file (work, label, dates). The same lines on a desk and on a phone.
    pub(super) fn info_rows(
        &self,
        track: &crate::model::Track,
        details: Option<&crate::library::details::Details>,
    ) -> Vec<(&'static str, String)> {
        let s = self.s();
        let mut rows: Vec<(&'static str, String)> = Vec::new();
        if !track.album_artist.is_empty() && track.album_artist != track.artist {
            rows.push((s.info_album_artist, track.album_artist.clone()));
        }
        if let Some(n) = track.track_no {
            let text = match track.disc_no.filter(|d| *d > 1) {
                Some(disc) => self.lang.track_on_disc(n, disc),
                None => n.to_string(),
            };
            rows.push((s.info_track, text));
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
        let rg = track.leveling();
        let gain = match (rg.track_gain_db, rg.album_gain_db) {
            (None, None) => s.info_none.to_string(),
            (t, a) => {
                let fmt = |v: Option<f32>| v.map_or("—".to_string(), |v| format!("{v:+.2} dB"));
                let values = format!("{} {} / {} {}", s.rg_track, fmt(t), s.rg_album, fmt(a));
                // Not from the file's tags: say whose numbers these are.
                if track.replaygain.track_gain_db.is_none() && track.measured.is_some() {
                    format!("{values} ({})", s.info_measured)
                } else {
                    values
                }
            }
        };
        rows.push((s.info_replaygain, gain));
        // What the file check found, for the file as it is.
        if let Some(check) = track.check {
            use crate::model::CheckVerdict;
            let verdict = match check.verdict {
                CheckVerdict::Verified => s.info_check_verified,
                CheckVerdict::Decoded => s.info_check_decoded,
                CheckVerdict::Damaged => s.info_check_damaged,
            };
            rows.push((s.info_check, verdict.to_string()));
            if let Some(dr) = check.dr {
                // The album's value: the mean of its checked tracks.
                let album: Vec<f32> = self
                    .snapshot
                    .album_index(track.album_key)
                    .and_then(|i| self.snapshot.albums().get(i as usize))
                    .map(|album| {
                        album
                            .tracks
                            .iter()
                            .filter_map(|&i| self.snapshot.tracks.get(i as usize))
                            .filter_map(|t| t.check.and_then(|c| c.dr))
                            .collect()
                    })
                    .unwrap_or_default();
                let text = if album.len() > 1 {
                    let mean = album.iter().sum::<f32>() / album.len() as f32;
                    format!(
                        "DR{} ({} DR{})",
                        dr.round() as i32,
                        s.rg_album,
                        mean.round() as i32
                    )
                } else {
                    format!("DR{}", dr.round() as i32)
                };
                rows.push((s.info_dr, text));
            }
            if let Some(peak) = check.true_peak.filter(|p| *p > 0.0) {
                let db = 20.0 * peak.log10();
                let text = if db < 0.0 {
                    format!("−{:.1} dBTP", -db)
                } else {
                    format!("+{db:.1} dBTP")
                };
                rows.push((s.info_true_peak, text));
            }
        }
        rows.push((s.info_plays, track.play_count.to_string()));
        // What the file says beyond the library: work, label, dates.
        if let Some(details) = details {
            rows.extend(facts_to_show(s, details, track.year));
        }
        rows
    }

    fn info_panel(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let s = self.s();
        // Explicitly requested track, else the single selected row, else what is playing.
        let selected = self.views.get(&self.nav).and_then(|state| {
            let key = state.anchor.filter(|a| state.selection.contains(a))?;
            let row = state.rows.iter().find(|r| r.key == key)?;
            match self.nav {
                Nav::Albums | Nav::Artists | Nav::Artist(_) | Nav::Genres => None,
                _ => self.snapshot.tracks.get(row.idx as usize).map(|t| t.id),
            }
        });
        let id = self
            .info_track
            .or(selected)
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
        let details = self.extras_of(&track, true);
        let versions = self.other_versions_of(track.id);
        // The library's own spelling of a name, if it has an artist of that name:
        // only such names are links, so that a link always leads somewhere.
        let artists = self.artist_names();
        let in_library = move |name: &str| known_artist(&artists, name);
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
                match in_library(&track.artist).or_else(|| in_library(&track.album_artist)) {
                    Some(known) => {
                        if ui.link(RichText::new(artist).size(14.0)).clicked() {
                            actions.push(Action::Go(Nav::Artist(known)));
                        }
                    }
                    None => {
                        ui.add(egui::Label::new(RichText::new(artist).size(14.0)).wrap());
                    }
                }
                if !track.album.is_empty() && ui.link(&track.album).clicked() {
                    actions.push(Action::Go(Nav::Album(track.album_key)));
                }
                if !track.available {
                    ui.add_space(4.0);
                    ui.colored_label(ui.visuals().warn_fg_color, s.info_missing);
                }
                ui.add_space(8.0);

                // The listener's stars, right below what the song is: shown, and set by
                // a click on one (a click on the last one that is filled takes them away).
                if !crate::library::Library::is_remote(track.id) {
                    if let Some(stars) = widgets::stars(ui, track.rating, 16.0, Some(s.col_rating))
                    {
                        actions.push(Action::SetRating(vec![track.id], stars));
                    }
                    ui.add_space(10.0);
                }
                let rows = self.info_rows(&track, details.as_deref());
                egui::Grid::new("info-grid")
                    .num_columns(2)
                    .spacing([10.0, 5.0])
                    .show(ui, |ui| {
                        for (label, value) in rows {
                            ui.label(RichText::new(label).small().color(dim));
                            if label == s.info_genre {
                                // Every genre is a way to the other songs of its kind.
                                ui.horizontal_wrapped(|ui| {
                                    ui.spacing_mut().item_spacing.x = 8.0;
                                    for genre in super::lists::genres(&value) {
                                        if ui.link(genre).clicked() {
                                            actions.push(Action::Go(Nav::Genre(genre.to_string())));
                                        }
                                    }
                                });
                            } else {
                                ui.add(egui::Label::new(value).wrap());
                            }
                            ui.end_row();
                        }
                    });
                // Who wrote, played and produced it. A name the library has as an
                // artist is a link to that artist.
                if let Some(details) = details.as_ref().filter(|d| !d.credits.is_empty()) {
                    ui.add_space(12.0);
                    ui.label(RichText::new(s.info_credits).small().color(dim));
                    ui.add_space(3.0);
                    egui::Grid::new("credits-grid")
                        .num_columns(2)
                        .spacing([10.0, 5.0])
                        .show(ui, |ui| {
                            for credit in &details.credits {
                                let role = role_label(s, credit.role);
                                ui.label(RichText::new(role).small().color(dim));
                                ui.horizontal_wrapped(|ui| {
                                    ui.spacing_mut().item_spacing.x = 8.0;
                                    for name in &credit.names {
                                        match in_library(name) {
                                            Some(known) => {
                                                if ui.link(name).clicked() {
                                                    actions.push(Action::Go(Nav::Artist(known)));
                                                }
                                            }
                                            None => {
                                                ui.label(name);
                                            }
                                        }
                                    }
                                });
                                ui.end_row();
                            }
                        });
                }
                // The same song elsewhere in the library: another album, another format.
                if !versions.is_empty() {
                    ui.add_space(12.0);
                    ui.label(RichText::new(s.info_versions).small().color(dim));
                    ui.add_space(2.0);
                    for other in &versions {
                        let mut what =
                            format!("{} · {}", other.codec, kilohertz(other.sample_rate));
                        if let Some(bits) = other.bit_depth {
                            what = format!("{what} · {bits} bit");
                        }
                        let album = if other.album.is_empty() {
                            s.unknown_album
                        } else {
                            other.album.as_str()
                        };
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            if ui.link(album).on_hover_text(s.info_versions_hint).clicked() {
                                actions.push(Action::ShowInfo(other.id));
                            }
                            ui.label(RichText::new(what).small().color(dim));
                        });
                    }
                }
                // What the engine is doing with the track that is playing right now.
                if let Some(path) = self.signal_path().filter(|p| p.track_id == track.id) {
                    ui.add_space(12.0);
                    signal_path_block(ui, s, &path);
                }
                // The words, when the file (or a text file next to it) has them, or
                // when they were found on the internet for a listener who asked for that.
                let found_online = details.as_ref().is_some_and(|d| d.lyrics_online);
                if details.as_ref().is_some_and(|d| d.lyrics.is_none()) && self.seeking_lyrics() {
                    ui.add_space(12.0);
                    ui.label(RichText::new(s.info_lyrics).small().color(dim));
                    ui.add_space(2.0);
                    ui.label(RichText::new(s.lyrics_seeking).size(12.5).color(dim));
                }
                if let Some(lyrics) = details.as_ref().and_then(|d| d.lyrics.as_ref()) {
                    ui.add_space(12.0);
                    ui.label(RichText::new(s.info_lyrics).small().color(dim));
                    ui.add_space(2.0);
                    let playing_this = self.player.current.map(|c| c.track_id) == Some(track.id);
                    match &lyrics.timed {
                        // With times, and this is the song that is on: the line being
                        // sung is lit and kept in view; a click on a line goes there.
                        Some(lines) if playing_this => self.synced_lyrics(ui, lyrics, lines),
                        _ => {
                            ui.add(
                                egui::Label::new(RichText::new(&lyrics.plain).size(12.5))
                                    .wrap()
                                    .selectable(true),
                            );
                        }
                    }
                    if found_online {
                        ui.add_space(4.0);
                        ui.label(RichText::new(s.lyrics_found_online).small().color(dim));
                    }
                }
                ui.add_space(10.0);
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

#[cfg(test)]
mod tests {
    use super::{
        PathInputs, PathSummary, StageKind, edge_scroll_step, kilohertz, other_versions,
        path_stages, path_summary, processing_lines,
    };
    use crate::model::{Track, test_track};
    use std::sync::Arc;

    #[test]
    fn the_same_song_elsewhere_in_the_library_is_found() {
        let song = |id, title: &str, artist: &str, codec: &str, rate, bits| {
            Arc::new(Track {
                title: title.into(),
                artist: artist.into(),
                codec: codec.into(),
                sample_rate: rate,
                bit_depth: bits,
                ..test_track(id)
            })
        };
        let library = vec![
            song(1, "Blackbird", "The Beatles", "FLAC", 44_100, Some(16)),
            song(2, "Blackbird", "The Beatles", "MP3", 44_100, None),
            song(3, " blackbird ", "the beatles", "FLAC", 96_000, Some(24)),
            song(4, "Blackbird", "Sarah McLachlan", "FLAC", 44_100, Some(16)),
            song(
                5,
                "Blackbird (Take 4)",
                "The Beatles",
                "FLAC",
                44_100,
                Some(16),
            ),
            song(6, "", "The Beatles", "FLAC", 44_100, Some(16)),
        ];
        // The same title by the same artist, however it is capitalised; the best
        // sounding first. A cover and another take are other songs.
        assert_eq!(other_versions(&library, &library[0]), [3, 2]);
        assert_eq!(other_versions(&library, &library[1]), [3, 1]);
        assert!(other_versions(&library, &library[3]).is_empty());
        // A song without a title is not "the same" as every other untitled one.
        assert!(other_versions(&library, &library[5]).is_empty());
        let many: Vec<Arc<Track>> = (1..=20)
            .map(|id| song(id, "Same", "Artist", "FLAC", 44_100, Some(16)))
            .collect();
        assert_eq!(other_versions(&many, &many[0]).len(), super::MAX_VERSIONS);
        assert_eq!(
            (kilohertz(44_100), kilohertz(96_000), kilohertz(88_200)),
            ("44.1 kHz".into(), "96 kHz".into(), "88.2 kHz".into())
        );
    }
    use crate::i18n::KO;
    use eframe::egui;

    fn plain_path() -> PathInputs<'static> {
        PathInputs {
            s: &KO,
            dsd_rate: None,
            source_rate: 44_100,
            out_rate: 44_100,
            channels: 2,
            leveling: 1.0,
            equalizer: None,
            correction: None,
            crossfeed: None,
            crossfade: None,
            speed: None,
            filter: None,
            room: None,
            peak_protection: false,
            volume: 1.0,
            dither_bits: None,
        }
    }

    #[test]
    fn the_signal_path_lists_what_is_done_in_the_order_it_is_done() {
        // Nothing done: no stages, "unchanged".
        let untouched = path_stages(&plain_path());
        assert!(untouched.is_empty());
        assert_eq!(path_summary(&untouched), PathSummary::Untouched);

        // Everything at once, in the order the audio passes it.
        let all = path_stages(&PathInputs {
            source_rate: 96_000,
            channels: 1,
            leveling: 0.5,
            equalizer: Some("록".into()),
            correction: Some("HD 600"),
            crossfeed: Some("보통"),
            peak_protection: true,
            volume: 0.5,
            dither_bits: Some(16),
            ..plain_path()
        });
        let names: Vec<&str> = all.iter().map(|stage| stage.name.as_str()).collect();
        assert_eq!(
            names,
            [
                KO.info_proc_channels,
                KO.info_proc_replaygain,
                KO.info_proc_resample,
                KO.info_proc_eq,
                KO.info_proc_correction,
                KO.info_proc_crossfeed,
                KO.info_proc_limiter,
                KO.info_proc_volume,
                KO.info_proc_dither
            ]
        );
        let details: Vec<&str> = all.iter().map(|stage| stage.detail.as_str()).collect();
        assert_eq!(
            details,
            [
                "1 → 2 채널",
                "−6.0 dB",
                "96,000 → 44,100 Hz",
                "록",
                "HD 600",
                "보통",
                "",
                "−18.1 dB",
                "16 bit"
            ]
        );
        assert_eq!(path_summary(&all), PathSummary::ConvertedAndAdjusted);
    }

    #[test]
    fn the_signal_path_is_summed_up_by_why_it_is_changed() {
        // What the device needs.
        let converted = path_stages(&PathInputs {
            out_rate: 48_000,
            ..plain_path()
        });
        assert_eq!(converted[0].kind, StageKind::Conversion);
        assert_eq!(path_summary(&converted), PathSummary::Converted);
        // What the user asked for; dither that follows from it does not make it
        // "unchanged", and the summary names both reasons.
        let adjusted = path_stages(&PathInputs {
            equalizer: Some("사용자 설정".into()),
            ..plain_path()
        });
        assert_eq!(path_summary(&adjusted), PathSummary::Adjusted);
        let muted = path_stages(&PathInputs {
            volume: 0.0,
            dither_bits: Some(24),
            ..plain_path()
        });
        assert_eq!(muted[0].detail, KO.path_muted);
        assert_eq!(path_summary(&muted), PathSummary::ConvertedAndAdjusted);
        // Peak protection alone (a lossy file that decodes above full scale).
        let protected = path_stages(&PathInputs {
            peak_protection: true,
            ..plain_path()
        });
        assert_eq!(protected[0].kind, StageKind::Protection);
        assert_eq!(path_summary(&protected), PathSummary::Converted);
    }

    #[test]
    fn the_decks_processing_readout_fits_its_steps_into_the_width_it_has() {
        let steps = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        // Every character ten units wide.
        let measure = |text: &str| text.chars().count() as f32 * 10.0;
        let lines = |names: &[&str], width: f32| {
            processing_lines("Processing", "None", &steps(names), width, 3, measure)
        };
        assert_eq!(lines(&[], 100.0), ["Processing: None"]);
        assert_eq!(lines(&["Resampling"], 100.0), ["Processing: Resampling"]);
        // Wide enough for two steps on the first line, then one each.
        assert_eq!(
            lines(
                &["Resampling", "Equalizer", "Crossfeed", "App volume"],
                340.0
            ),
            [
                "Processing: Resampling · Equalizer",
                "Crossfeed · App volume"
            ]
        );
        // Narrow: a step to a line, and what is left over on the third and last.
        assert_eq!(
            lines(
                &["Resampling", "Equalizer", "Crossfeed", "App volume"],
                230.0
            ),
            [
                "Processing: Resampling",
                "Equalizer · Crossfeed",
                "App volume"
            ]
        );
        assert_eq!(
            lines(
                &[
                    "Resampling",
                    "Equalizer",
                    "Crossfeed",
                    "App volume",
                    "Dither"
                ],
                120.0
            ),
            [
                "Processing: Resampling",
                "Equalizer",
                "Crossfeed · App volume · Dither"
            ]
        );
        // Wide: everything on one line.
        assert_eq!(lines(&["a", "b", "c"], 1000.0), ["Processing: a · b · c"]);
        // A low deck has room for one line only.
        assert_eq!(
            processing_lines("P", "None", &steps(&["a", "b", "c"]), 10.0, 1, measure),
            ["P: a · b · c"]
        );
    }

    #[test]
    fn dragging_near_an_edge_scrolls_towards_it() {
        let list = egui::Rect::from_min_max(egui::pos2(0.0, 100.0), egui::pos2(300.0, 500.0));
        assert!(edge_scroll_step(110.0, list).is_some_and(|step| step < 0.0));
        assert!(edge_scroll_step(95.0, list).is_some_and(|step| step < 0.0));
        assert!(edge_scroll_step(490.0, list).is_some_and(|step| step > 0.0));
        assert_eq!(edge_scroll_step(300.0, list), None);
        assert_eq!(edge_scroll_step(20.0, list), None);
        // A list too short to have a middle never scrolls by itself.
        let tiny = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(300.0, 60.0));
        assert_eq!(edge_scroll_step(5.0, tiny), None);
    }
}
