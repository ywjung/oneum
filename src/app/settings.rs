//! User preferences (`settings.json`). Playback session state (queue, position, volume)
//! is stored separately in the library database.
use crate::audio::{
    Crossfeed, EqPreset, EqSettings, OutputSettings, PeqProfile, ReplayGainSettings,
};
use crate::i18n::LanguageChoice;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RightPanel {
    #[default]
    // "Meter" was a right-panel choice for one build, before the meter got its own deck.
    #[serde(alias = "Meter")]
    Hidden,
    Queue,
    Info,
}

/// A search that is kept under a name and shown like a playlist: its songs are whatever
/// the search finds at the moment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmartPlaylist {
    pub name: String,
    pub query: String,
}

/// The optional columns of song lists (title, artist, album and length are always there;
/// the format column has its own, older switch).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ListColumns {
    pub year: bool,
    pub genre: bool,
    pub plays: bool,
    pub added: bool,
    pub last_played: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub language: LanguageChoice,
    pub theme: ThemeChoice,
    pub replaygain: ReplayGainSettings,
    /// `None` = follow the system default output device.
    pub output_device: Option<String>,
    /// Rate matching, exclusive access, memory playback (all off by default).
    pub output: OutputSettings,
    /// The equalizer as it is set (off by default).
    pub eq: EqSettings,
    /// Headphone crossfeed (off by default).
    pub crossfeed: Crossfeed,
    /// Parametric correction profiles that were imported (headphone corrections), in
    /// the order they were imported.
    pub corrections: Vec<PeqProfile>,
    /// Name of the profile that is applied; `None`: none.
    pub correction: Option<String>,
    /// Radio: when the queue runs out, go on with songs like the last one (off by default).
    pub radio: bool,
    /// The equalizer presets the user has saved, in the order they were saved.
    pub eq_presets: Vec<EqPreset>,
    pub show_format_column: bool,
    pub list_columns: ListColumns,
    /// Searches kept as smart playlists, in the order they were made.
    pub smart_playlists: Vec<SmartPlaylist>,
    pub right_panel: RightPanel,
    /// Show the VU meter deck above the player bar.
    pub show_meter: bool,
    /// Inner size of the main window in points, restored at the next launch.
    pub window_size: Option<[f32; 2]>,
    /// Volume to return to when un-muting.
    pub volume_before_mute: Option<f32>,
    /// Show the time left instead of the total length next to the seek bar.
    pub show_remaining_time: bool,
}

/// How many equalizer presets of the user's own are kept (the equalizer sheet offers
/// no way to save more) and how long a preset's name may be.
pub const MAX_EQ_PRESETS: usize = 40;
pub const MAX_EQ_PRESET_NAME_CHARS: usize = 32;
/// How many correction profiles are kept.
pub const MAX_CORRECTIONS: usize = 30;
/// How many smart playlists are kept, and how long their names and searches may be.
pub const MAX_SMART_PLAYLISTS: usize = 100;
pub const MAX_SMART_NAME_CHARS: usize = 60;
pub const MAX_SMART_QUERY_CHARS: usize = 300;

/// A name or a search as it is kept: one line, without control characters, cut at `max`.
pub fn tidy_text(text: &str, max: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

impl Settings {
    /// The settings as they are used: values that a hand-edited or damaged file can
    /// hold but the app never writes are brought back into range.
    pub fn sanitized(mut self) -> Self {
        self.eq = self.eq.sanitized();
        self.eq_presets.truncate(MAX_EQ_PRESETS);
        for preset in &mut self.eq_presets {
            let values = EqSettings {
                enabled: true,
                preamp_db: preset.preamp_db,
                gains_db: preset.gains_db,
            }
            .sanitized();
            preset.preamp_db = values.preamp_db;
            preset.gains_db = values.gains_db;
            preset.name = preset
                .name
                .chars()
                .filter(|c| !c.is_control())
                .take(MAX_EQ_PRESET_NAME_CHARS)
                .collect::<String>()
                .trim()
                .to_string();
        }
        self.eq_presets.retain(|preset| !preset.name.is_empty());
        self.corrections.truncate(MAX_CORRECTIONS);
        let mut profile_names = std::collections::HashSet::new();
        self.corrections = std::mem::take(&mut self.corrections)
            .into_iter()
            .map(PeqProfile::sanitized)
            .filter(|p| !p.name.is_empty() && profile_names.insert(p.name.clone()))
            .collect();
        // A name that no profile has (imported or built in) selects nothing.
        if self.correction.as_ref().is_some_and(|name| {
            !profile_names.contains(name)
                && !crate::audio::peq::builtin_profiles()
                    .iter()
                    .any(|p| &p.name == name)
        }) {
            self.correction = None;
        }
        self.smart_playlists.truncate(MAX_SMART_PLAYLISTS);
        let mut names = std::collections::HashSet::new();
        for smart in &mut self.smart_playlists {
            smart.name = tidy_text(&smart.name, MAX_SMART_NAME_CHARS);
            smart.query = tidy_text(&smart.query, MAX_SMART_QUERY_CHARS);
        }
        // Names identify them: empty and repeated ones go.
        self.smart_playlists.retain(|smart| {
            !smart.name.is_empty() && !smart.query.is_empty() && names.insert(smart.name.clone())
        });
        if !self.replaygain.preamp_db.is_finite() {
            self.replaygain.preamp_db = 0.0;
        }
        self.replaygain.preamp_db = self.replaygain.preamp_db.clamp(-12.0, 12.0);
        self
    }
}

impl Settings {
    /// The correction profile that is applied, if one is selected: one the user
    /// imported, or else one of those that come with the app.
    pub fn active_correction(&self) -> Option<&PeqProfile> {
        let name = self.correction.as_ref()?;
        self.corrections
            .iter()
            .find(|p| &p.name == name)
            .or_else(|| {
                crate::audio::peq::builtin_profiles()
                    .iter()
                    .find(|p| &p.name == name)
            })
    }

    /// True if the selected profile is one the user imported (and can remove).
    pub fn correction_is_imported(&self) -> bool {
        self.correction
            .as_ref()
            .is_some_and(|name| self.corrections.iter().any(|p| &p.name == name))
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: LanguageChoice::System,
            theme: ThemeChoice::System,
            replaygain: ReplayGainSettings::default(),
            output_device: None,
            output: OutputSettings::default(),
            eq: EqSettings::default(),
            crossfeed: Crossfeed::Off,
            corrections: Vec::new(),
            correction: None,
            radio: false,
            eq_presets: Vec::new(),
            show_format_column: false,
            list_columns: ListColumns::default(),
            smart_playlists: Vec::new(),
            right_panel: RightPanel::Hidden,
            show_meter: false,
            window_size: None,
            volume_before_mute: None,
            show_remaining_time: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_and_missing_fields_fall_back_to_defaults() {
        let s: Settings =
            serde_json::from_str(r#"{"theme":"Dark","some_future_field":1}"#).expect("parses");
        assert_eq!(s.theme, ThemeChoice::Dark);
        assert_eq!(s.language, LanguageChoice::System);
        assert!(!s.show_format_column);
        // The output device is left alone unless the user asks otherwise.
        assert_eq!(s.output, OutputSettings::default());
        assert!(!s.output.touches_device() && !s.output.memory_playback);
        assert!(!s.show_meter);
        // A settings file written while the meter lived in the right panel still loads.
        let old: Settings =
            serde_json::from_str(r#"{"theme":"Light","right_panel":"Meter"}"#).expect("parses");
        assert_eq!(old.right_panel, RightPanel::Hidden);
        assert_eq!(old.theme, ThemeChoice::Light);
        // The equalizer is off and flat until the user sets it; a half-written entry
        // and saved presets load.
        assert!(!s.eq.enabled && !s.eq.is_active() && s.eq_presets.is_empty());
        assert!(
            !s.radio,
            "the queue ends where it ends unless the user asks for radio"
        );
        assert_eq!(s.crossfeed, Crossfeed::Off);
        let cf: Settings = serde_json::from_str(r#"{"crossfeed":"Medium"}"#).expect("parses");
        assert_eq!(cf.crossfeed, Crossfeed::Medium);
        let eq: Settings = serde_json::from_str(
            r#"{"eq":{"enabled":true,"gains_db":[3,0,0,0,0,0,0,0,0,-2]},
                "eq_presets":[{"name":"내 헤드폰","preamp_db":-3,"gains_db":[3,2,1,0,0,0,0,0,1,2]}]}"#,
        )
        .expect("parses");
        assert!(eq.eq.is_active() && eq.eq.preamp_db == 0.0);
        assert_eq!((eq.eq.gains_db[0], eq.eq.gains_db[9]), (3.0, -2.0));
        assert_eq!(eq.eq_presets[0].name, "내 헤드폰");
        let again: Settings =
            serde_json::from_str(&serde_json::to_string(&eq).expect("serializes")).expect("parses");
        assert_eq!(again, eq);
        // A file edited by hand (or damaged) is brought back into range when loaded.
        let wild: Settings = serde_json::from_str(
            r#"{"eq":{"enabled":true,"preamp_db":500,"gains_db":[99,-99,0,0,0,0,0,0,0,1e30]},
                "replaygain":{"mode":"Track","preamp_db":900,"prevent_clipping":true},
                "eq_presets":[{"name":"  ","preamp_db":0,"gains_db":[0,0,0,0,0,0,0,0,0,0]},
                              {"name":"ok\nname","preamp_db":-80,"gains_db":[50,0,0,0,0,0,0,0,0,0]}]}"#,
        )
        .expect("parses");
        let wild = wild.sanitized();
        assert_eq!(wild.eq.preamp_db, 12.0);
        assert_eq!(
            (
                wild.eq.gains_db[0],
                wild.eq.gains_db[1],
                wild.eq.gains_db[9]
            ),
            (12.0, -12.0, 12.0)
        );
        assert_eq!(wild.replaygain.preamp_db, 12.0);
        assert_eq!(
            wild.eq_presets.len(),
            1,
            "a preset without a name is dropped"
        );
        assert_eq!(wild.eq_presets[0].name, "okname");
        assert_eq!(
            (wild.eq_presets[0].preamp_db, wild.eq_presets[0].gains_db[0]),
            (-12.0, 12.0)
        );
        let many = Settings {
            eq_presets: (0..100)
                .map(|i| EqPreset {
                    name: format!("p{i}"),
                    preamp_db: 0.0,
                    gains_db: [0.0; 10],
                })
                .collect(),
            ..Settings::default()
        }
        .sanitized();
        assert_eq!(many.eq_presets.len(), MAX_EQ_PRESETS);
        assert_eq!(Settings::default().sanitized(), Settings::default());
        let partial: Settings =
            serde_json::from_str(r#"{"output":{"exclusive":true}}"#).expect("parses");
        assert!(partial.output.exclusive && !partial.output.match_sample_rate);
        let round: Settings =
            serde_json::from_str(&serde_json::to_string(&s).expect("serializes")).expect("parses");
        assert_eq!(round, s);
    }

    #[test]
    fn smart_playlists_from_a_damaged_file_are_tidied() {
        let long = "x".repeat(500);
        let json = format!(
            r#"{{"smart_playlists":[
                {{"name":"  Jazz\n ","query":"genre:jazz"}},
                {{"name":"Jazz","query":"genre:bebop"}},
                {{"name":"","query":"year:1990"}},
                {{"name":"Empty","query":"  "}},
                {{"name":"{long}","query":"{long}"}}
            ],"list_columns":{{"year":true,"unknown":1}}}}"#
        );
        let s: Settings = serde_json::from_str::<Settings>(&json)
            .expect("parses")
            .sanitized();
        assert_eq!(s.smart_playlists.len(), 2);
        assert_eq!(s.smart_playlists[0].name, "Jazz");
        assert_eq!(s.smart_playlists[0].query, "genre:jazz");
        assert_eq!(
            s.smart_playlists[1].name.chars().count(),
            MAX_SMART_NAME_CHARS
        );
        assert_eq!(
            s.smart_playlists[1].query.chars().count(),
            MAX_SMART_QUERY_CHARS
        );
        assert!(s.list_columns.year && !s.list_columns.genre);
    }

    #[test]
    fn correction_profiles_from_a_damaged_file_are_tidied() {
        let json = r#"{"correction":"Gone","corrections":[
            {"name":"HD 600","preamp_db":-900,"filters":[{"kind":"Peak","hz":1e12,"gain_db":99,"q":-1}]},
            {"name":"HD 600","preamp_db":0,"filters":[]},
            {"name":"","preamp_db":-3,"filters":[]}
        ]}"#;
        let s: Settings = serde_json::from_str::<Settings>(json)
            .expect("parses")
            .sanitized();
        assert_eq!(s.corrections.len(), 1);
        assert_eq!(s.corrections[0].preamp_db, -30.0);
        let f = s.corrections[0].filters[0];
        assert_eq!((f.hz, f.gain_db, f.q), (24_000.0, 30.0, 0.05));
        // The selected name has to exist.
        assert_eq!(s.correction, None);
        assert!(s.active_correction().is_none());
        let mut s = s;
        s.correction = Some("HD 600".into());
        assert_eq!(
            s.clone()
                .sanitized()
                .active_correction()
                .map(|p| p.name.as_str()),
            Some("HD 600")
        );
        assert!(s.correction_is_imported());
        // A profile that comes with the app can be selected by its name; an imported
        // one of the same name comes first.
        s.correction = Some("Sennheiser HD 800 S".into());
        let s = s.sanitized();
        let builtin = s.active_correction().expect("built in");
        assert_eq!(builtin.filters.len(), 10);
        assert!(!s.correction_is_imported());
        let mut shadowed = s.clone();
        shadowed.corrections.push(PeqProfile {
            name: "Sennheiser HD 800 S".into(),
            preamp_db: -1.0,
            filters: Vec::new(),
        });
        assert_eq!(
            shadowed.active_correction().map(|p| p.preamp_db),
            Some(-1.0)
        );
    }
}
