//! A settings file that was damaged or edited by hand: whatever still parses is brought
//! within the limits the rest of the app relies on.
use super::settings::{
    MAX_CORRECTIONS, MAX_EQ_PRESET_NAME_CHARS, MAX_EQ_PRESETS, MAX_SMART_NAME_CHARS,
    MAX_SMART_PLAYLISTS, MAX_SMART_QUERY_CHARS, Settings, SmartPlaylist,
};
use crate::audio::eq::EqPreset;
use crate::audio::peq::{PeqFilter, PeqKind, PeqProfile};
use crate::testing::Noise;

/// Settings with something in every list, as a full file has.
fn full() -> Settings {
    let mut settings = Settings::default();
    settings.eq.enabled = true;
    settings.eq.preamp_db = -3.0;
    settings.eq_presets = (0..3)
        .map(|i| EqPreset {
            name: format!("Preset {i}"),
            preamp_db: -2.0,
            gains_db: [1.5; crate::audio::eq::BANDS],
        })
        .collect();
    settings.corrections = (0..3)
        .map(|i| PeqProfile {
            name: format!("Phones {i}"),
            preamp_db: -6.2,
            filters: vec![
                PeqFilter {
                    kind: PeqKind::Peak,
                    hz: 105.0,
                    gain_db: 6.6,
                    q: 0.7,
                };
                4
            ],
        })
        .collect();
    settings.correction = Some("Phones 1".into());
    settings.smart_playlists = (0..3)
        .map(|i| SmartPlaylist {
            name: format!("Late {i}"),
            query: "year:2019-2020".into(),
        })
        .collect();
    settings.window_size = Some([1180.0, 760.0]);
    settings.volume_before_mute = Some(0.8);
    settings
}

fn within_limits(settings: &Settings) {
    assert!(settings.eq.preamp_db.is_finite());
    assert!(settings.eq.gains_db.iter().all(|g| g.is_finite()));
    assert!(settings.replaygain.preamp_db.is_finite());
    assert!((-12.0..=12.0).contains(&settings.replaygain.preamp_db));
    assert!(settings.eq_presets.len() <= MAX_EQ_PRESETS);
    for preset in &settings.eq_presets {
        let chars = preset.name.chars().count();
        assert!(
            (1..=MAX_EQ_PRESET_NAME_CHARS).contains(&chars),
            "{preset:?}"
        );
        assert!(preset.preamp_db.is_finite());
        assert!(preset.gains_db.iter().all(|g| g.is_finite()), "{preset:?}");
    }
    assert!(settings.corrections.len() <= MAX_CORRECTIONS);
    for profile in &settings.corrections {
        assert!(!profile.name.is_empty() && profile.name.chars().count() <= 60);
        assert!(profile.filters.len() <= 32);
        assert!(profile.preamp_db.is_finite());
        for f in &profile.filters {
            assert!(f.hz.is_finite() && f.gain_db.is_finite() && f.q.is_finite());
            assert!(f.hz >= 10.0 && f.q >= 0.05, "{f:?}");
        }
    }
    // A selected profile exists, or nothing is selected.
    assert_eq!(
        settings.correction.is_some(),
        settings.active_correction().is_some()
    );
    assert!(settings.smart_playlists.len() <= MAX_SMART_PLAYLISTS);
    for smart in &settings.smart_playlists {
        assert!((1..=MAX_SMART_NAME_CHARS).contains(&smart.name.chars().count()));
        assert!((1..=MAX_SMART_QUERY_CHARS).contains(&smart.query.chars().count()));
    }
}

#[test]
fn a_damaged_settings_file_is_refused_or_brought_within_limits() {
    let good = serde_json::to_string_pretty(&full()).expect("serializes");
    // Values a text editor can put in the place of a number or a name.
    let swaps = [
        ("-6.2", "-1e308"),
        ("-6.2", "1e308"),
        ("105.0", "0"),
        ("105.0", "-5"),
        ("0.7", "0"),
        ("0.7", "1e-300"),
        ("6.6", "99999999"),
        ("1.5", "-99999999"),
        ("-3.0", "1e39"),
        ("-2.0", "null"),
        ("\"Phones 1\"", "\"\""),
        ("\"Phones 2\"", "\"Phones 0\""),
        ("\"Late 1\"", "\"\\u0000\\n\\t\""),
        ("\"year:2019-2020\"", "\"\""),
        ("1180.0", "1e30"),
        ("0.8", "-1e30"),
    ];
    let mut noise = Noise::new(41);
    let (mut parsed, mut refused) = (0, 0);
    for round in 0..6_000 {
        let text = if round % 3 == 0 {
            String::from_utf8_lossy(&noise.damaged(good.as_bytes())).into_owned()
        } else {
            let mut text = good.clone();
            for _ in 0..1 + noise.below(5) {
                let (from, to) = swaps[noise.below(swaps.len())];
                // One occurrence at a time, somewhere in the file.
                let places: Vec<usize> = text.match_indices(from).map(|(at, _)| at).collect();
                if let Some(at) = places.get(noise.below(places.len())).copied() {
                    text.replace_range(at..at + from.len(), to);
                }
            }
            text
        };
        let Ok(settings) = serde_json::from_str::<Settings>(&text) else {
            refused += 1;
            continue;
        };
        parsed += 1;
        let settings = settings.sanitized();
        within_limits(&settings);
        // Tidying what is tidy changes nothing, and what is written is read back the same.
        let again: Settings =
            serde_json::from_str(&serde_json::to_string(&settings).expect("serializes"))
                .expect("what the app writes parses");
        assert_eq!(again.sanitized(), settings, "round {round}");
    }
    assert!(
        parsed > 1_000 && refused > 100,
        "{parsed} parsed, {refused} refused"
    );
}

/// Lists far longer than the app ever writes.
#[test]
fn lists_of_any_length_are_cut_to_what_the_app_keeps() {
    let mut settings = full();
    settings.corrections = (0..5_000)
        .map(|i| PeqProfile {
            name: format!("{i}"),
            preamp_db: f32::NAN,
            filters: vec![
                PeqFilter {
                    kind: PeqKind::HighShelf,
                    hz: f32::INFINITY,
                    gain_db: f32::NAN,
                    q: 0.0,
                };
                500
            ],
        })
        .collect();
    settings.smart_playlists = (0..5_000)
        .map(|i| SmartPlaylist {
            name: format!("{i}{}", "n".repeat(1_000)),
            query: "q".repeat(100_000),
        })
        .collect();
    settings.eq_presets = (0..5_000)
        .map(|i| EqPreset {
            name: format!("{i}{}", "p".repeat(1_000)),
            preamp_db: f32::NEG_INFINITY,
            gains_db: [f32::NAN; crate::audio::eq::BANDS],
        })
        .collect();
    settings.correction = Some("4999".into());
    within_limits(&settings.sanitized());
}
