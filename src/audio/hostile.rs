//! Damaged and hostile input to the audio code: music files, MP4 boxes, correction
//! files. Nothing may panic or run away, and nothing that is not a number may come out
//! (what comes out here goes to someone's headphones).
use super::decoder::TrackDecoder;
use super::loudness::{gain_db, integrated, measure_file};
use super::mp4_edit;
use super::peq::{Corrector, PeqFilter, PeqKind, PeqProfile};
use crate::testing::Noise;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures")).join(name);
    std::fs::read(path).expect("fixture")
}

/// One file of every format that is played, damaged 150 different ways each: opened,
/// decoded, sought in and measured. The good files hold 44,100 frames (the one at
/// 96 kHz 96,000, the Monkey's Audio file 183,456).
#[test]
fn damaged_music_files_are_decoded_or_refused_and_give_numbers_only() {
    let dir = tempfile::tempdir().unwrap();
    let mut noise = Noise::new(7);
    let mut played = 0;
    for name in [
        "gap_a.flac",
        "gap_a.mp3",
        "gap_a.aac.m4a",
        "gap_a.alac.m4a",
        "gap_a.ogg",
        "gap_a.wav",
        "gap_a.aiff",
        "gap_a.mp2",
        "gap_a.opus",
        "gap_a.wv",
        "tone_96k_24bit.wv",
        "silence.ape",
    ] {
        let good = fixture(name);
        for round in 0..150 {
            let path = dir.path().join(format!("{round}-{name}"));
            std::fs::write(&path, noise.damaged(&good)).unwrap();
            let what = format!("{name}, round {round}");

            if let Ok(mut decoder) = TrackDecoder::open(&path) {
                let channels = decoder.info().channels.max(1);
                let mut out = Vec::new();
                let mut frames = 0;
                while let Ok(true) = decoder.next(&mut out) {
                    frames += out.len() / channels;
                    assert!(out.iter().all(|s| s.is_finite()), "{what}");
                    // A length field cannot make more music than the file holds.
                    assert!(frames < 1_000_000, "{what}: {frames} frames");
                }
                played += usize::from(frames > 0);
            }
            // Playback seeks on a decoder that was just opened.
            if let Ok(mut decoder) = TrackDecoder::open(&path) {
                let target = noise.next() % 100_000;
                if decoder.seek(target).is_ok() {
                    let mut out = Vec::new();
                    for _ in 0..8 {
                        if !matches!(decoder.next(&mut out), Ok(true)) {
                            break;
                        }
                        assert!(out.iter().all(|s| s.is_finite()), "{what}, after a seek");
                    }
                }
            }
            if let Ok(Some(measured)) = measure_file(&path, &AtomicBool::new(false)) {
                assert!(measured.peak.is_finite(), "{what}");
                assert!(measured.blocks.iter().all(|b| b.is_finite()), "{what}");
                assert!(gain_db(integrated(&[measured])).is_finite(), "{what}");
            }
            std::fs::remove_file(&path).unwrap();
        }
    }
    // The damage is light enough that much of it still plays: the loop above ran.
    assert!(played > 300, "{played}");
}

/// The box walker reads lengths and offsets from the file. Whatever they say, it
/// answers, and an answer is one that can be used.
#[test]
fn the_mp4_reader_answers_whatever_the_boxes_say() {
    let mut noise = Noise::new(11);
    let usable = |trim: Option<mp4_edit::Trim>, rate: u32| {
        if let Some(trim) = trim {
            assert!(trim.valid > 0);
            assert!(trim.delay <= u64::from(rate) * 60, "{trim:?}");
        }
    };
    for _ in 0..2_000 {
        let len = noise.below(4_096);
        let bytes = noise.bytes(len);
        usable(mp4_edit::read(Cursor::new(bytes), 44_100), 44_100);
    }
    let mut kept = 0;
    for name in ["gap_a.aac.m4a", "gap_b.aac.m4a"] {
        let good = fixture(name);
        assert!(mp4_edit::read(Cursor::new(&good), 44_100).is_some());
        for _ in 0..3_000 {
            let rate = [0, 1, 8_000, 44_100, 192_000, u32::MAX][noise.below(6)];
            let trim = mp4_edit::read(Cursor::new(noise.damaged(&good)), rate);
            kept += usize::from(trim.is_some());
            usable(trim, rate);
        }
    }
    // Most damage lands in the audio data and leaves the boxes readable.
    assert!(kept > 1_000, "{kept}");
}

/// Runs a profile over a loud signal at the sample rates a device can have.
fn sounds_like_numbers(profile: &PeqProfile, what: &str) {
    for rate in [8_000, 44_100, 48_000, 192_000, 384_000] {
        assert!(profile.peak_db(rate).is_finite(), "{what} at {rate}");
        let mut corrector = Corrector::new(rate, Some(profile));
        // Full-scale steps: the hardest thing for a filter with a narrow band.
        let mut samples: Vec<f32> = (0..4_000)
            .map(|i| if (i / 200) % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        corrector.process(&mut samples);
        assert!(samples.iter().all(|s| s.is_finite()), "{what} at {rate}");
        // Changing the profile cross-fades two chains.
        corrector.set(None);
        corrector.process(&mut samples);
        assert!(samples.iter().all(|s| s.is_finite()), "{what} at {rate}");
    }
}

/// Correction files made of the format's own words in no order, and of plain noise:
/// refused with a reason or read into a profile whose every value is in range.
#[test]
fn correction_files_of_any_content_give_an_error_or_a_profile_within_limits() {
    let pieces = [
        "Preamp",
        "Filter",
        " 1",
        " 33",
        ":",
        ": ",
        " ON ",
        " OFF ",
        "PK",
        "LSC",
        "HSC",
        "LP",
        " Fc ",
        " Gain ",
        " Q ",
        " Hz",
        " dB",
        "\n",
        "\r\n",
        "\u{feff}",
        "#",
        "105",
        "-6.2",
        "0",
        "-0",
        "1e30",
        "-1e30",
        "1e-30",
        "NaN",
        "inf",
        "-inf",
        "0x10",
        "٣",
        " ",
        "\t",
        "99999999999999999999999999999999999999999",
    ];
    // Numbers as a file can spell them, the sensible and the absurd.
    let numbers = [
        "105",
        "-6.2",
        "0",
        "-0",
        "0.70",
        "1e30",
        "-1e30",
        "1e-30",
        "NaN",
        "inf",
        "-inf",
        "+3",
        "20000",
        "0.0001",
        "99999999999999999999999999999999999999999",
        "٣",
        "",
    ];
    let mut noise = Noise::new(13);
    let mut read = 0;
    for round in 0..4_000 {
        let text = match round % 4 {
            0 => {
                let len = noise.below(600);
                String::from_utf8_lossy(&noise.bytes(len)).into_owned()
            }
            1 => {
                let count = noise.below(60);
                noise.text(&pieces, count)
            }
            // Lines of the right shape with numbers of any kind, up to 40 of them.
            _ => {
                let number = |noise: &mut Noise| numbers[noise.below(numbers.len())];
                let mut text = format!("Preamp: {} dB\r\n", number(&mut noise));
                for i in 0..noise.below(40) {
                    let kind = ["PK", "LSC", "HSC", "pk", "PEQ"][noise.below(5)];
                    let state = ["ON", "ON", "ON", "on", "OFF"][noise.below(5)];
                    text += &format!(
                        "Filter {i}: {state} {kind} Fc {} Hz Gain {} dB Q {}\n",
                        number(&mut noise),
                        number(&mut noise),
                        number(&mut noise)
                    );
                }
                text
            }
        };
        let name = noise.text(&["Sennheiser", " ", "\n", "\u{0}", "한", "🎧", "x"], 80);
        let Ok(profile) = PeqProfile::parse(&name, &text) else {
            continue;
        };
        read += 1;
        assert!(profile.name.chars().count() <= 60, "{profile:?}");
        assert!(!profile.name.chars().any(char::is_control), "{profile:?}");
        assert!(profile.filters.len() <= 32, "{profile:?}");
        assert!(profile.preamp_db <= 12.0, "{profile:?}");
        // Whatever the filters add up to, nothing is more than 12 dB louder.
        assert!(profile.peak_db(48_000) <= 12.01, "{profile:?}");
        for f in &profile.filters {
            assert!((10.0..=24_000.0).contains(&f.hz), "{profile:?}");
            assert!((-30.0..=30.0).contains(&f.gain_db), "{profile:?}");
            assert!((0.05..=30.0).contains(&f.q), "{profile:?}");
        }
        if round % 16 == 0 {
            sounds_like_numbers(&profile, &format!("{profile:?}"));
        }
    }
    // Some of the well-formed files hold only numbers that can be used.
    assert!(read > 50, "{read}");
}

/// A settings file edited by hand can hold any number. After tidying, the worst
/// combinations still filter without blowing up.
#[test]
fn the_most_extreme_profiles_a_settings_file_can_hold_still_give_numbers() {
    let extremes = [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::MAX,
        f32::MIN,
        f32::MIN_POSITIVE,
        0.0,
        -0.0,
        -1.0,
        1e-30,
        10.0,
        24_000.0,
        1e9,
    ];
    let mut noise = Noise::new(17);
    for round in 0..600 {
        let mut pick = || extremes[noise.below(extremes.len())];
        let filters = (0..40)
            .map(|i| PeqFilter {
                kind: [PeqKind::Peak, PeqKind::LowShelf, PeqKind::HighShelf][i % 3],
                hz: pick(),
                gain_db: pick(),
                q: pick(),
            })
            .collect();
        let profile = PeqProfile {
            name: "x".repeat(500),
            preamp_db: pick(),
            filters,
        }
        .sanitized();
        assert!(profile.filters.len() <= 32);
        if round % 6 == 0 {
            sounds_like_numbers(&profile, &format!("round {round}"));
        }
    }
}

/// DSD files damaged every way the generator knows, and Matroska: opened, decoded to
/// the end, sought in, finished into a follower. Numbers only, never a panic, never
/// more frames than the header can hold.
#[test]
fn damaged_dsd_files_are_played_or_refused() {
    use super::dsd::encode::{Modulator, dff, dsf};
    let mut m = Modulator::new();
    for n in 0..60_000 {
        m.push(0.4 * (n as f64 * 0.003).sin());
    }
    let bits = m.finish();
    let goods = [
        dsf(
            2_822_400,
            &[bits.clone(), bits.clone()],
            60_000,
            Some(b"ID3\x03\0\0\0\0\0\0"),
        ),
        dff(
            2_822_400,
            &[bits.clone(), bits.clone()],
            Some(b"ID3\x03\0\0\0\0\0\0"),
        ),
        dsf(5_644_800, &vec![bits.clone(); 6], 60_000, None),
        dff(2_822_400, &vec![bits.clone(); 6], None),
    ];
    let dir = tempfile::tempdir().unwrap();
    let mut noise = Noise::new(43);
    let mut played = 0;
    for (g, good) in goods.iter().enumerate() {
        for round in 0..200 {
            let path = dir.path().join(format!("{g}-{round}.dsd"));
            std::fs::write(&path, noise.damaged(good)).unwrap();
            let what = format!("file {g}, round {round}");
            let Ok(mut decoder) = TrackDecoder::open(&path) else {
                continue;
            };
            let info = decoder.info().clone();
            let limit = info.total_frames.unwrap_or(0);
            let mut out = Vec::new();
            let mut frames = 0u64;
            decoder.defer_tail();
            while let Ok(true) = decoder.next(&mut out) {
                assert!(out.iter().all(|s| s.is_finite()), "{what}");
                frames += (out.len() / info.channels) as u64;
            }
            if decoder.holds_tail() {
                let mut next = TrackDecoder::open(&path).ok();
                decoder.finish(next.as_mut(), &mut out).unwrap_or(());
                assert!(out.iter().all(|s| s.is_finite()), "{what}");
                frames += (out.len() / info.channels) as u64;
            }
            assert!(frames <= limit, "{what}: {frames} of {limit}");
            played += usize::from(frames > 0);
            if let Ok(mut again) = TrackDecoder::open(&path) {
                let _ = again.seek(noise.next() % (limit + 10_000));
                let _ = again.next(&mut out);
                assert!(out.iter().all(|s| s.is_finite()), "{what}, after a seek");
            }
            std::fs::remove_file(&path).unwrap();
        }
    }
    assert!(played > 200, "{played}");
}
