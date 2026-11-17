//! Small, allocation-free signal helpers: channel mapping, ReplayGain, volume curve.
use crate::model::ReplayGainTags;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ReplayGainMode {
    #[default]
    Off,
    Track,
    Album,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ReplayGainSettings {
    pub mode: ReplayGainMode,
    /// Extra gain in dB added to tagged tracks only.
    pub preamp_db: f32,
    pub prevent_clipping: bool,
}

impl Default for ReplayGainSettings {
    fn default() -> Self {
        Self {
            mode: ReplayGainMode::Off,
            preamp_db: 0.0,
            prevent_clipping: true,
        }
    }
}

pub fn db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Linear gain to apply to a track. Policy (docs/ARCHITECTURE.md, "ReplayGain 정책"):
/// - Off, or no usable tag → 1.0 (pre-amp is not applied to untagged tracks).
/// - Album mode falls back to track tags and vice versa.
/// - With clipping prevention: a known peak caps the gain so `gain * peak <= 1`;
///   an unknown peak caps the gain at unity (never amplify blindly).
pub fn replaygain_factor(tags: &ReplayGainTags, settings: &ReplayGainSettings) -> f32 {
    let (gain_db, peak) = match settings.mode {
        ReplayGainMode::Off => return 1.0,
        ReplayGainMode::Track => match tags.track_gain_db {
            Some(g) => (g, tags.track_peak),
            None => match tags.album_gain_db {
                Some(g) => (g, tags.album_peak),
                None => return 1.0,
            },
        },
        ReplayGainMode::Album => match tags.album_gain_db {
            Some(g) => (g, tags.album_peak),
            None => match tags.track_gain_db {
                Some(g) => (g, tags.track_peak),
                None => return 1.0,
            },
        },
    };
    if !gain_db.is_finite() {
        return 1.0;
    }
    // A damaged settings file must never turn into NaN samples.
    let preamp_db = if settings.preamp_db.is_finite() {
        settings.preamp_db
    } else {
        0.0
    };
    let mut gain = db_to_linear((gain_db + preamp_db).clamp(-60.0, 24.0));
    if settings.prevent_clipping {
        match peak.filter(|p| p.is_finite() && *p > 0.0) {
            Some(p) => gain = gain.min(1.0 / p),
            None => gain = gain.min(1.0),
        }
    }
    gain
}

/// Maps the 0..=1 volume slider to a linear gain with a perceptual (cubic) curve.
pub fn volume_to_gain(volume: f32) -> f32 {
    let v = if volume.is_finite() {
        volume.clamp(0.0, 1.0)
    } else {
        0.0
    };
    v * v * v
}

/// Converts interleaved `channels`-channel audio to interleaved stereo, appending to `dst`.
/// Mono is duplicated; 3.0, 5.0, 5.1, 6.1 and 7.1 (standard order FL FR FC LFE …) are
/// down-mixed with −3 dB centre/surrounds and normalised so the result cannot exceed the
/// input peak; other layouts average even channels into left and odd channels into right
/// (which is right for four channels: FL FR BL BR).
pub fn to_stereo(src: &[f32], channels: usize, dst: &mut Vec<f32>) {
    match channels {
        0 => {}
        1 => {
            dst.reserve(src.len() * 2);
            for &s in src {
                dst.push(s);
                dst.push(s);
            }
        }
        2 => dst.extend_from_slice(src),
        6 | 8 => {
            const C: f32 = std::f32::consts::FRAC_1_SQRT_2;
            let surrounds = (channels - 4) / 2;
            let norm = 1.0 / (1.0 + C + C * surrounds as f32);
            dst.reserve(src.len() / channels * 2);
            for f in src.chunks_exact(channels) {
                let mut l = f[0] + C * f[2];
                let mut r = f[1] + C * f[2];
                for pair in f[4..].as_chunks::<2>().0 {
                    l += C * pair[0];
                    r += C * pair[1];
                }
                dst.push(l * norm);
                dst.push(r * norm);
            }
        }
        // FL FR FC, and FL FR FC BL BR: the centre belongs to both sides.
        3 | 5 => {
            const C: f32 = std::f32::consts::FRAC_1_SQRT_2;
            let norm = 1.0 / (1.0 + C + C * ((channels - 3) / 2) as f32);
            dst.reserve(src.len() / channels * 2);
            for f in src.chunks_exact(channels) {
                let mut l = f[0] + C * f[2];
                let mut r = f[1] + C * f[2];
                if channels == 5 {
                    l += C * f[3];
                    r += C * f[4];
                }
                dst.push(l * norm);
                dst.push(r * norm);
            }
        }
        // FL FR FC LFE BC SL SR: the centres (front and back) belong to both sides.
        7 => {
            const C: f32 = std::f32::consts::FRAC_1_SQRT_2;
            let norm = 1.0 / (1.0 + C + C + 0.5);
            dst.reserve(src.len() / channels * 2);
            for f in src.chunks_exact(channels) {
                let middle = C * f[2] + 0.5 * f[4];
                dst.push((f[0] + middle + C * f[5]) * norm);
                dst.push((f[1] + middle + C * f[6]) * norm);
            }
        }
        n => {
            let left_n = n.div_ceil(2) as f32;
            let right_n = (n / 2) as f32;
            dst.reserve(src.len() / n * 2);
            for f in src.chunks_exact(n) {
                let (mut l, mut r) = (0.0, 0.0);
                for (i, &s) in f.iter().enumerate() {
                    if i % 2 == 0 {
                        l += s;
                    } else {
                        r += s;
                    }
                }
                dst.push(l / left_n);
                dst.push(r / right_n);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(tg: Option<f32>, tp: Option<f32>, ag: Option<f32>, ap: Option<f32>) -> ReplayGainTags {
        ReplayGainTags {
            track_gain_db: tg,
            track_peak: tp,
            album_gain_db: ag,
            album_peak: ap,
        }
    }
    fn settings(
        mode: ReplayGainMode,
        preamp_db: f32,
        prevent_clipping: bool,
    ) -> ReplayGainSettings {
        ReplayGainSettings {
            mode,
            preamp_db,
            prevent_clipping,
        }
    }
    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn replaygain_off_and_untagged_are_unity() {
        let t = tags(Some(-6.0), Some(0.9), Some(-3.0), Some(0.95));
        assert_eq!(
            replaygain_factor(&t, &settings(ReplayGainMode::Off, 6.0, true)),
            1.0
        );
        let none = tags(None, None, None, None);
        // Pre-amp must not be applied to untagged tracks.
        assert_eq!(
            replaygain_factor(&none, &settings(ReplayGainMode::Track, 6.0, false)),
            1.0
        );
        assert_eq!(
            replaygain_factor(&none, &settings(ReplayGainMode::Album, 6.0, false)),
            1.0
        );
    }

    #[test]
    fn replaygain_mode_selection_and_fallback() {
        let t = tags(Some(-6.0), Some(0.5), Some(-3.0), Some(0.5));
        assert!(close(
            replaygain_factor(&t, &settings(ReplayGainMode::Track, 0.0, true)),
            db_to_linear(-6.0)
        ));
        assert!(close(
            replaygain_factor(&t, &settings(ReplayGainMode::Album, 0.0, true)),
            db_to_linear(-3.0)
        ));
        let only_track = tags(Some(-4.0), None, None, None);
        assert!(close(
            replaygain_factor(&only_track, &settings(ReplayGainMode::Album, 0.0, true)),
            db_to_linear(-4.0)
        ));
        let only_album = tags(None, None, Some(-2.0), None);
        assert!(close(
            replaygain_factor(&only_album, &settings(ReplayGainMode::Track, 0.0, true)),
            db_to_linear(-2.0)
        ));
    }

    #[test]
    fn replaygain_clipping_prevention() {
        // +6 dB on a track peaking at 0.9 would clip: capped to 1/0.9.
        let loud = tags(Some(6.0), Some(0.9), None, None);
        assert!(close(
            replaygain_factor(&loud, &settings(ReplayGainMode::Track, 0.0, true)),
            1.0 / 0.9
        ));
        // Without prevention the tag gain is used as is.
        assert!(close(
            replaygain_factor(&loud, &settings(ReplayGainMode::Track, 0.0, false)),
            db_to_linear(6.0)
        ));
        // Unknown peak: never amplify.
        let no_peak = tags(Some(4.0), None, None, None);
        assert_eq!(
            replaygain_factor(&no_peak, &settings(ReplayGainMode::Track, 0.0, true)),
            1.0
        );
        // Pre-amp is added before the clip check.
        let quiet = tags(Some(-10.0), Some(1.0), None, None);
        assert!(close(
            replaygain_factor(&quiet, &settings(ReplayGainMode::Track, 4.0, true)),
            db_to_linear(-6.0)
        ));
        assert!(close(
            replaygain_factor(&quiet, &settings(ReplayGainMode::Track, 20.0, true)),
            1.0
        ));
    }

    #[test]
    fn replaygain_ignores_garbage_values() {
        let bad = tags(Some(f32::NAN), Some(0.0), None, None);
        assert_eq!(
            replaygain_factor(&bad, &settings(ReplayGainMode::Track, 0.0, true)),
            1.0
        );
    }

    #[test]
    fn volume_curve_endpoints() {
        assert_eq!(volume_to_gain(0.0), 0.0);
        assert_eq!(volume_to_gain(1.0), 1.0);
        assert_eq!(volume_to_gain(2.0), 1.0);
        assert_eq!(volume_to_gain(f32::NAN), 0.0);
        assert!(volume_to_gain(0.5) < 0.5);
    }

    #[test]
    fn stereo_mapping() {
        let mut out = Vec::new();
        to_stereo(&[0.5, -0.5], 1, &mut out);
        assert_eq!(out, vec![0.5, 0.5, -0.5, -0.5]);
        out.clear();
        to_stereo(&[0.1, 0.2, 0.3, 0.4], 2, &mut out);
        assert_eq!(out, vec![0.1, 0.2, 0.3, 0.4]);
        out.clear();
        // Full-scale on every 5.1 channel must not exceed full scale after down-mix.
        to_stereo(&[1.0; 6], 6, &mut out);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|s| *s <= 1.0 + 1e-6 && *s > 0.9));
    }

    /// Three, five and seven channels have a centre, which is heard from both
    /// sides; four have none. Nothing gets louder than it was.
    #[test]
    fn a_centre_channel_goes_to_both_sides() {
        let centre_only = |channels: usize| {
            let mut frame = vec![0.0; channels];
            frame[2] = 1.0;
            let mut out = Vec::new();
            to_stereo(&frame, channels, &mut out);
            out
        };
        for channels in [3, 5, 6, 7, 8] {
            let out = centre_only(channels);
            assert_eq!(out.len(), 2);
            assert!(out[0] > 0.2 && out[0] == out[1], "{channels}: {out:?}");
            let mut out = Vec::new();
            to_stereo(&vec![1.0; channels], channels, &mut out);
            assert!(
                out.iter().all(|s| *s <= 1.0 + 1e-6 && *s > 0.9),
                "{channels}: {out:?}"
            );
        }
        // FL FR BL BR: the third channel is a left one.
        assert_eq!(centre_only(4), vec![0.5, 0.0]);
        // FL FR FC BL BR: each surround on its own side; FL FR FC LFE BC SL SR: the
        // back centre on both, the LFE on neither.
        let side = |channels: usize, at: usize| {
            let mut frame = vec![0.0; channels];
            frame[at] = 1.0;
            let mut out = Vec::new();
            to_stereo(&frame, channels, &mut out);
            (out[0], out[1])
        };
        assert!(matches!(side(5, 3), (l, r) if l > 0.2 && r == 0.0));
        assert!(matches!(side(5, 4), (l, r) if l == 0.0 && r > 0.2));
        assert!(matches!(side(7, 4), (l, r) if l > 0.1 && l == r));
        assert_eq!(side(7, 3), (0.0, 0.0));
        assert!(matches!(side(7, 5), (l, r) if l > 0.2 && r == 0.0));
        assert!(matches!(side(7, 6), (l, r) if l == 0.0 && r > 0.2));
    }
}
