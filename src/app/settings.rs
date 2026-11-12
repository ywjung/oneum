//! User preferences (`settings.json`). Playback session state (queue, position, volume)
//! is stored separately in the library database.
use crate::audio::ReplayGainSettings;
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
    pub show_format_column: bool,
    pub right_panel: RightPanel,
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
            show_format_column: false,
            right_panel: RightPanel::Hidden,
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
        let round: Settings =
            serde_json::from_str(&serde_json::to_string(&s).expect("serializes")).expect("parses");
        assert_eq!(round, s);
    }
}
