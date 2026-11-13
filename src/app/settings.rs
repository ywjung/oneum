//! User preferences (`settings.json`). Playback session state (queue, position, volume)
//! is stored separately in the library database.
use crate::audio::{Crossfeed, EqPreset, EqSettings, OutputSettings, ReplayGainSettings};
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
    /// Radio: when the queue runs out, go on with songs like the last one (off by default).
    pub radio: bool,
    /// The equalizer presets the user has saved, in the order they were saved.
    pub eq_presets: Vec<EqPreset>,
    pub show_format_column: bool,
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
            radio: false,
            eq_presets: Vec::new(),
            show_format_column: false,
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
        let partial: Settings =
            serde_json::from_str(r#"{"output":{"exclusive":true}}"#).expect("parses");
        assert!(partial.output.exclusive && !partial.output.match_sample_rate);
        let round: Settings =
            serde_json::from_str(&serde_json::to_string(&s).expect("serializes")).expect("parses");
        assert_eq!(round, s);
    }
}
