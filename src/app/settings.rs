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

/// What the screen is drawn through, where a system has more than one interface to its
/// graphics hardware that works (Android: Vulkan and OpenGL ES).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum GraphicsChoice {
    /// The system's first interface, or the other one after a run lost its graphics
    /// device on that.
    #[default]
    Auto,
    Vulkan,
    OpenGl,
    /// OpenGL ES called directly by the UI framework's other renderer ("glow"), with
    /// no wgpu between (Android; a trial for a phone whose Vulkan driver runs out of
    /// memory). Elsewhere it is "automatic".
    Glow,
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

/// A media server of the network that the listener added by its address: its name, and
/// where its device description is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedServer {
    pub name: String,
    pub location: String,
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
    pub rating: bool,
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
    /// Crossfade between songs, in seconds (0, the default: none).
    pub crossfade_secs: u8,
    /// Network folders the listener added (a share of a NAS): where each is and who
    /// to be there. Never a password: those are in the system's store for passwords.
    pub network_folders: Vec<crate::net::folders::Folder>,
    /// How fast songs go, in percent of their own speed (100, the default: as they
    /// are; see `audio::pace`).
    pub speed_percent: u16,
    /// Parametric correction profiles that were imported (headphone corrections), in
    /// the order they were imported.
    #[serde(deserialize_with = "lenient_list")]
    pub corrections: Vec<PeqProfile>,
    /// Name of the profile that is applied; `None`: none.
    pub correction: Option<String>,
    /// Radio: when the queue runs out, go on with songs like the last one (off by default).
    pub radio: bool,
    /// Lyrics are looked for on the internet for a song whose file has none that
    /// follow it (off by default: nothing is asked of the lyrics service unless the
    /// listener switches this on).
    pub online_lyrics: bool,
    /// Shuffle whole albums instead of single tracks (off by default).
    pub shuffle_by_album: bool,
    /// Shuffle with the listener's stars in mind (off by default).
    pub shuffle_by_rating: bool,
    /// Tracks of 20 minutes or more start where they were left off (on by default).
    pub resume_long_tracks: bool,
    /// The equalizer presets the user has saved, in the order they were saved.
    #[serde(deserialize_with = "lenient_list")]
    pub eq_presets: Vec<EqPreset>,
    pub show_format_column: bool,
    pub list_columns: ListColumns,
    /// Searches kept as smart playlists, in the order they were made.
    #[serde(deserialize_with = "lenient_list")]
    pub smart_playlists: Vec<SmartPlaylist>,
    /// Media servers added by their address (those found by asking the network are
    /// not kept: they are found again).
    #[serde(deserialize_with = "lenient_list")]
    pub network_servers: Vec<SavedServer>,
    /// What the screen is drawn through (see [`GraphicsChoice`]).
    pub graphics: GraphicsChoice,
    pub right_panel: RightPanel,
    /// Show the VU meter deck above the player bar.
    pub show_meter: bool,
    /// Inner size of the main window in points, restored at the next launch.
    pub window_size: Option<[f32; 2]>,
    /// Volume to return to when un-muting.
    pub volume_before_mute: Option<f32>,
    /// Show the time left instead of the total length next to the seek bar.
    pub show_remaining_time: bool,
    /// Albums, artists, genres and playlists kept at the top of the sidebar, in the
    /// order they were pinned.
    #[serde(deserialize_with = "lenient_list")]
    pub pinned: Vec<Pin>,
}

/// Something kept at the top of the sidebar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pin {
    /// By its album key (see `library::album_key`).
    Album(u64),
    Artist(String),
    Genre(String),
    Playlist(i64),
    /// A smart playlist, by its name.
    Smart(String),
}

/// How many things can be pinned to the sidebar.
pub const MAX_PINS: usize = 12;
/// The longest crossfade between songs that can be set, in seconds.
pub const MAX_CROSSFADE_SECS: u8 = 12;

/// How many equalizer presets of the user's own are kept (the equalizer sheet offers
/// no way to save more) and how long a preset's name may be.
pub const MAX_EQ_PRESETS: usize = 40;
pub const MAX_EQ_PRESET_NAME_CHARS: usize = 32;
/// How many correction profiles are kept.
pub const MAX_CORRECTIONS: usize = 30;
/// How many smart playlists are kept, and how long their names and searches may be.
pub const MAX_SMART_PLAYLISTS: usize = 100;
/// The most media servers kept.
pub const MAX_NETWORK_SERVERS: usize = 16;
pub const MAX_SMART_NAME_CHARS: usize = 60;
pub const MAX_SMART_QUERY_CHARS: usize = 300;
/// How long an artist or genre name kept in a pin may be.
const MAX_PIN_NAME_CHARS: usize = 300;

/// A list read one entry at a time: an entry this build cannot read (damaged, or of a
/// kind a later version added) is left out instead of costing the whole file, and with
/// it every other setting. Something that is not a list at all reads as an empty one.
fn lenient_list<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    Ok(match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::Array(items) => items
            .into_iter()
            .filter_map(|item| serde_json::from_value(item).ok())
            .collect(),
        _ => Vec::new(),
    })
}

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
        self.speed_percent = crate::audio::pace::speed_of(self.speed_percent);
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
        // Network folders: each in shape, each id and each address once.
        let (mut ids, mut addresses) = (
            std::collections::HashSet::new(),
            std::collections::HashSet::new(),
        );
        self.network_folders = std::mem::take(&mut self.network_folders)
            .into_iter()
            .filter_map(crate::net::folders::Folder::sanitized)
            .filter(|folder| ids.insert(folder.id.clone()) && addresses.insert(folder.url.clone()))
            .take(crate::net::folders::MAX_FOLDERS)
            .collect();
        self.network_servers.truncate(MAX_NETWORK_SERVERS);
        let mut locations = std::collections::HashSet::new();
        for server in &mut self.network_servers {
            server.name = tidy_text(&server.name, MAX_SMART_NAME_CHARS);
        }
        // Only addresses the app would follow, each once.
        self.network_servers.retain(|server| {
            server.location.len() <= 2048
                && crate::model::http_address(std::path::Path::new(&server.location)).is_some()
                && locations.insert(server.location.clone())
        });
        let mut seen = Vec::new();
        self.pinned = std::mem::take(&mut self.pinned)
            .into_iter()
            .map(|pin| match pin {
                Pin::Artist(name) => Pin::Artist(tidy_text(&name, MAX_PIN_NAME_CHARS)),
                Pin::Genre(name) => Pin::Genre(tidy_text(&name, MAX_PIN_NAME_CHARS)),
                Pin::Smart(name) => Pin::Smart(tidy_text(&name, MAX_SMART_NAME_CHARS)),
                other => other,
            })
            .filter(|pin| {
                let empty =
                    matches!(pin, Pin::Artist(n) | Pin::Genre(n) | Pin::Smart(n) if n.is_empty());
                let new = !seen.contains(pin);
                if new {
                    seen.push(pin.clone());
                }
                !empty && new
            })
            .take(MAX_PINS)
            .collect();
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
            crossfade_secs: 0,
            network_folders: Vec::new(),
            speed_percent: 100,
            corrections: Vec::new(),
            correction: None,
            radio: false,
            online_lyrics: false,
            shuffle_by_album: false,
            shuffle_by_rating: false,
            resume_long_tracks: true,
            eq_presets: Vec::new(),
            show_format_column: false,
            list_columns: ListColumns::default(),
            smart_playlists: Vec::new(),
            network_servers: Vec::new(),
            graphics: GraphicsChoice::Auto,
            right_panel: RightPanel::Hidden,
            show_meter: false,
            window_size: None,
            volume_before_mute: None,
            show_remaining_time: false,
            pinned: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The settings file names the way of drawing by these words: a build that wrote
    /// one must read it again as the same choice (a word it does not know costs the
    /// whole file its place, see `storage::settings`).
    #[test]
    fn the_way_of_drawing_is_kept_under_its_word() {
        for (choice, word) in [
            (GraphicsChoice::Auto, "Auto"),
            (GraphicsChoice::Vulkan, "Vulkan"),
            (GraphicsChoice::OpenGl, "OpenGl"),
            (GraphicsChoice::Glow, "Glow"),
        ] {
            let settings = Settings {
                graphics: choice,
                ..Settings::default()
            };
            let json = serde_json::to_value(&settings).expect("serializes");
            assert_eq!(json["graphics"], word);
            let text = format!(r#"{{"graphics":"{word}"}}"#);
            let read: Settings = serde_json::from_str(&text).expect("parses");
            assert_eq!(read.graphics, choice);
        }
        // What the renderer makes of the choice that is on trial.
        assert_eq!(
            crate::ui::renderer::Prefer::from_choice(GraphicsChoice::Glow, None),
            crate::ui::renderer::Prefer::Glow
        );
    }

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
        assert!(
            !s.online_lyrics,
            "nothing is asked of the internet unless the listener switches it on"
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
    fn pins_from_a_damaged_file_are_tidied() {
        let json = r#"{"pinned":[
            {"Album":18446744073709551615},
            {"Artist":"  아이유\n"},
            {"Artist":"아이유"},
            {"Genre":""},
            {"Playlist":3},
            {"Smart":"Late"},
            {"Starred":1},
            "Album",
            {"Playlist":3}
        ]}"#;
        let s = serde_json::from_str::<Settings>(json)
            .expect("parses")
            .sanitized();
        assert_eq!(
            s.pinned,
            [
                Pin::Album(u64::MAX),
                Pin::Artist("아이유".into()),
                Pin::Playlist(3),
                Pin::Smart("Late".into()),
            ]
        );
        let many = format!(
            r#"{{"pinned":[{}]}}"#,
            (0..40)
                .map(|i| format!(r#"{{"Playlist":{i}}}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        let s = serde_json::from_str::<Settings>(&many)
            .expect("parses")
            .sanitized();
        assert_eq!(s.pinned.len(), MAX_PINS);
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

    /// One entry this build cannot read (damaged, or a filter kind of a later version)
    /// costs that entry, not the whole file.
    #[test]
    fn an_entry_that_cannot_be_read_is_left_out_and_the_rest_kept() {
        let json = r#"{
            "theme": "Light",
            "smart_playlists": [{"name": "Late"}, {"name": "Good", "query": "year:2020"}],
            "corrections": [
                {"name": "Future", "preamp_db": -3.0,
                 "filters": [{"kind": "Notch", "hz": 50.0, "gain_db": 0.0, "q": 1.0}]},
                {"name": "Phones", "preamp_db": -3.0, "filters": []}
            ],
            "eq_presets": 7
        }"#;
        let s: Settings = serde_json::from_str(json).expect("still parses");
        assert_eq!(s.theme, ThemeChoice::Light);
        assert_eq!(s.smart_playlists.len(), 1);
        assert_eq!(s.smart_playlists[0].name, "Good");
        assert_eq!(s.corrections.len(), 1);
        assert_eq!(s.corrections[0].name, "Phones");
        assert!(s.eq_presets.is_empty());
    }
}
