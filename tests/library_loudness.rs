//! Measuring the loudness of untagged files: real files, a real library, the real
//! decoder. The files are only read; what is measured lives in the app's database.
#[path = "library_support.rs"]
mod support;

use oneum::library::Library;
use oneum::model::Track;
use std::path::Path;
use std::sync::Arc;
use support::*;

/// A 16-bit stereo 44.1 kHz WAV holding a 1 kHz sine with its peaks at `peak_db` dBFS.
fn write_tone(path: &Path, seconds: f64, peak_db: f64, title: &str, album: &str) {
    const RATE: u32 = 44_100;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let amplitude = 10f64.powf(peak_db / 20.0) * 32_767.0;
    let frames = (f64::from(RATE) * seconds) as u32;
    let mut data = Vec::with_capacity(frames as usize * 4);
    for n in 0..frames {
        let phase = 2.0 * std::f64::consts::PI * 1000.0 * f64::from(n) / f64::from(RATE);
        let sample = (amplitude * phase.sin()).round() as i16;
        data.extend_from_slice(&sample.to_le_bytes());
        data.extend_from_slice(&sample.to_le_bytes());
    }
    let mut list = b"INFO".to_vec();
    for (id, text) in [("INAM", title), ("IART", "Tester"), ("IPRD", album)] {
        let mut value = text.as_bytes().to_vec();
        value.push(0);
        list.extend_from_slice(id.as_bytes());
        list.extend_from_slice(&(value.len() as u32).to_le_bytes());
        list.extend_from_slice(&value);
        if value.len() % 2 == 1 {
            list.push(0);
        }
    }
    let mut body = b"WAVEfmt ".to_vec();
    body.extend_from_slice(&16u32.to_le_bytes());
    body.extend_from_slice(&1u16.to_le_bytes()); // PCM
    body.extend_from_slice(&2u16.to_le_bytes()); // stereo
    body.extend_from_slice(&RATE.to_le_bytes());
    body.extend_from_slice(&(RATE * 4).to_le_bytes());
    body.extend_from_slice(&4u16.to_le_bytes());
    body.extend_from_slice(&16u16.to_le_bytes());
    body.extend_from_slice(b"LIST");
    body.extend_from_slice(&(list.len() as u32).to_le_bytes());
    body.extend_from_slice(&list);
    body.extend_from_slice(b"data");
    body.extend_from_slice(&(data.len() as u32).to_le_bytes());
    body.extend_from_slice(&data);
    let mut file = b"RIFF".to_vec();
    file.extend_from_slice(&(body.len() as u32).to_le_bytes());
    file.extend_from_slice(&body);
    std::fs::write(path, file).unwrap();
}

fn by_title(library: &Library, title: &str) -> Arc<Track> {
    let snapshot = library.snapshot();
    let found = snapshot
        .tracks
        .iter()
        .find(|t| t.title == title)
        .unwrap_or_else(|| panic!("no track titled {title:?}"));
    Arc::clone(found)
}

fn measure(library: &Library) {
    assert!(library.measure_loudness(), "a run starts");
    assert!(library.wait_for_loudness(TIMEOUT), "the run ends in time");
}

fn near(value: Option<f32>, expected: f32) -> bool {
    value.is_some_and(|v| (v - expected).abs() < 0.15)
}

#[test]
fn untagged_tracks_are_measured_and_the_files_left_alone() {
    let env = TestEnv::new();
    let music = env.music();
    write_tone(&music.join("album/loud.wav"), 3.0, -12.0, "Loud", "Album");
    write_tone(&music.join("album/soft.wav"), 3.0, -24.0, "Soft", "Album");
    write_tone(
        &music.join("single/other.wav"),
        2.0,
        -18.0,
        "Other",
        "Single",
    );
    let before: Vec<Vec<u8>> = ["album/loud.wav", "album/soft.wav", "single/other.wav"]
        .iter()
        .map(|name| std::fs::read(music.join(name)).unwrap())
        .collect();
    let (library, _) = env.open_scanned();
    for title in ["Loud", "Soft", "Other"] {
        let track = by_title(&library, title);
        assert!(track.wants_measuring());
        assert_eq!(track.leveling().track_gain_db, None);
    }

    measure(&library);
    let status = library.loudness_status();
    assert!(!status.running);
    assert_eq!((status.total, status.done, status.failed), (3, 3, 0));

    // A stereo sine with peaks at −12 dBFS reads −12 LUFS: 6 dB above the reference
    // of −18, so its gain is −6 dB. The soft one is 6 dB below: +6 dB.
    let loud = by_title(&library, "Loud");
    let soft = by_title(&library, "Soft");
    let other = by_title(&library, "Other");
    let (l, s, o) = (loud.leveling(), soft.leveling(), other.leveling());
    assert!(near(l.track_gain_db, -6.0), "{l:?}");
    assert!(near(s.track_gain_db, 6.0), "{s:?}");
    assert!(near(o.track_gain_db, 0.0), "{o:?}");
    assert!(near(l.track_peak, 0.251), "{l:?}");
    assert!(near(s.track_peak, 0.063), "{s:?}");
    // The album as one programme: half of it at −12 LUFS and half at −24 is, by energy,
    // 10·log10((10^-1.2 + 10^-2.4) / 2) = −14.74 LUFS (the soft half is within 10 LU of
    // that mean, so the gate keeps it). Both tracks get that one gain, which keeps the
    // soft track soft in album mode.
    assert_eq!(l.album_gain_db, s.album_gain_db);
    assert!(near(l.album_gain_db, -3.26), "{l:?}");
    assert_eq!(l.album_peak, s.album_peak);
    assert!(near(l.album_peak, 0.251));
    // An album of one track reads like its track.
    assert!(near(o.album_gain_db, 0.0), "{o:?}");
    // The tags themselves are still empty: these are the app's own values.
    assert_eq!(loud.replaygain.track_gain_db, None);
    assert!(!loud.wants_measuring());

    // Nothing was written to the files.
    let after: Vec<Vec<u8>> = ["album/loud.wav", "album/soft.wav", "single/other.wav"]
        .iter()
        .map(|name| std::fs::read(music.join(name)).unwrap())
        .collect();
    assert_eq!(before, after);

    // Nothing left to do: a second run finds no work.
    measure(&library);
    assert_eq!(library.loudness_status().total, 0);

    // The values survive a restart and a rescan of unchanged files.
    library.shutdown();
    drop(library);
    let library = env.open();
    assert_eq!(by_title(&library, "Loud").leveling(), l);
    rescan(&library);
    assert_eq!(by_title(&library, "Soft").leveling(), s);

    // A file that changed is no longer what was measured; the others keep theirs.
    write_tone(&music.join("album/soft.wav"), 3.5, -12.0, "Soft", "Album");
    rescan(&library);
    let soft = by_title(&library, "Soft");
    assert_eq!(soft.measured, None);
    assert!(soft.wants_measuring());
    assert_eq!(by_title(&library, "Loud").leveling(), l);
    // Its album is measured again as a whole (two files), with a new album value.
    measure(&library);
    assert_eq!(library.loudness_status().total, 2);
    let (l2, s2) = (
        by_title(&library, "Loud").leveling(),
        by_title(&library, "Soft").leveling(),
    );
    assert!(near(s2.track_gain_db, -6.0), "{s2:?}");
    assert!(near(l2.track_gain_db, -6.0), "{l2:?}");
    assert_eq!(l2.album_gain_db, s2.album_gain_db);

    // Forgetting brings everything back to "not measured".
    library.forget_loudness();
    for title in ["Loud", "Soft", "Other"] {
        assert!(by_title(&library, title).wants_measuring());
    }
    library.shutdown();
    drop(library);
    let library = env.open();
    assert_eq!(by_title(&library, "Loud").measured, None);
}

#[test]
fn a_file_that_cannot_be_read_does_not_stop_the_run() {
    let env = TestEnv::new();
    let music = env.music();
    write_tone(&music.join("a/one.wav"), 2.0, -12.0, "One", "Pair");
    write_tone(&music.join("a/two.wav"), 2.0, -12.0, "Two", "Pair");
    write_tone(&music.join("b/three.wav"), 2.0, -20.0, "Three", "Alone");
    let (library, _) = env.open_scanned();
    // Gone since the scan, and the library has not noticed yet.
    std::fs::remove_file(music.join("a/two.wav")).unwrap();
    measure(&library);
    let status = library.loudness_status();
    assert_eq!((status.total, status.done, status.failed), (3, 3, 1));
    // The readable track of the pair has its own values but no album values (the
    // album could not be measured as a whole); the other album is complete.
    let one = by_title(&library, "One").leveling();
    assert!(near(one.track_gain_db, -6.0), "{one:?}");
    assert_eq!(one.album_gain_db, None);
    assert_eq!(by_title(&library, "Two").measured, None);
    let three = by_title(&library, "Three").leveling();
    assert!(near(three.track_gain_db, 2.0), "{three:?}");
    assert!(near(three.album_gain_db, 2.0), "{three:?}");

    // What could not be read is remembered: it no longer counts as waiting, and the
    // next run reads nothing (before, it read the file again and its album with it).
    let two = by_title(&library, "Two");
    assert!(two.unmeasurable && !two.wants_measuring());
    measure(&library);
    let again = library.loudness_status();
    assert_eq!((again.total, again.done, again.failed), (0, 0, 0));
    // Forgetting the measurements is a clean slate: it is tried again.
    library.forget_loudness();
    let two = by_title(&library, "Two");
    assert!(!two.unmeasurable && two.wants_measuring());
    assert!(by_title(&library, "One").wants_measuring());
}

#[test]
fn a_run_can_be_cancelled_and_shutdown_ends_it() {
    let env = TestEnv::new();
    let music = env.music();
    for i in 0..12 {
        write_tone(
            &music.join(format!("loose/{i:02}.wav")),
            20.0,
            -15.0,
            &format!("Track {i:02}"),
            &format!("Album {i:02}"),
        );
    }
    let (library, _) = env.open_scanned();
    assert!(library.measure_loudness());
    assert!(!library.measure_loudness(), "one run at a time");
    library.cancel_loudness();
    assert!(library.wait_for_loudness(TIMEOUT));
    let status = library.loudness_status();
    assert!(!status.running);
    assert_eq!(status.total, 12);
    // What was stored before the cancel stays, and matches what was counted.
    let measured = library
        .snapshot()
        .tracks
        .iter()
        .filter(|t| t.measured.is_some())
        .count() as u64;
    assert!(measured <= status.done, "{measured} stored, {status:?}");
    // The rest is picked up by the next run.
    measure(&library);
    assert!(
        library
            .snapshot()
            .tracks
            .iter()
            .all(|t| t.measured.is_some())
    );
    // A run in progress does not keep the library from shutting down.
    library.forget_loudness();
    assert!(library.measure_loudness());
    library.shutdown();
    assert!(!library.loudness_status().running);
    assert!(!library.measure_loudness(), "not after shutdown");
}

/// Programme loudness of a file as ffmpeg's EBU R 128 scanner reads it (an
/// implementation that shares no code with ours). It prints one decimal.
fn ffmpeg_loudness(path: &Path) -> f64 {
    let output = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-i"])
        .arg(path)
        .args(["-filter:a", "ebur128=framelog=quiet", "-f", "null", "-"])
        .output()
        .expect("run ffmpeg");
    let log = String::from_utf8_lossy(&output.stderr);
    let summary = log
        .rsplit("Integrated loudness:")
        .next()
        .expect("a summary");
    let value = summary
        .split("I:")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("no integrated loudness in: {log}"));
    value.parse().expect("a number")
}

/// Music-like material (pink noise that swells and fades, with a quiet opening that
/// the gate has to deal with) in three formats and sample rates: our reading agrees
/// with ffmpeg's to the decimal it prints.
#[test]
fn readings_agree_with_an_independent_scanner() {
    if !ffmpeg_or_skip("readings_agree_with_an_independent_scanner") {
        return;
    }
    let env = TestEnv::new();
    let music = env.music();
    let mut cases: Vec<(&str, u32, Vec<&str>)> = vec![
        ("a/noise.flac", 44_100, vec!["-c:a", "flac"]),
        ("b/noise.wav", 96_000, vec!["-c:a", "pcm_s24le"]),
    ];
    if has_encoder("libmp3lame") {
        cases.push((
            "c/noise.mp3",
            48_000,
            vec!["-c:a", "libmp3lame", "-b:a", "192k"],
        ));
    }
    for (i, (name, rate, codec)) in cases.iter().enumerate() {
        let path = music.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let source = format!(
            "anoisesrc=color=pink:duration=24:sample_rate={rate}:seed={}:amplitude=0.6,\
             tremolo=f=0.4:d=0.8,volume='if(lt(t,6),0.05,1)':eval=frame",
            i + 1
        );
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-v", "error", "-y", "-f", "lavfi", "-i", &source, "-ac", "2",
            ])
            .args(codec)
            .arg(&path)
            .status()
            .expect("run ffmpeg");
        assert!(status.success(), "ffmpeg could not write {name}");
    }
    let (library, _) = env.open_scanned();
    measure(&library);
    assert_eq!(library.loudness_status().failed, 0);
    let snapshot = library.snapshot();
    assert_eq!(snapshot.tracks.len(), cases.len());
    for track in &snapshot.tracks {
        let gain = track.leveling().track_gain_db.expect("measured");
        let ours = -18.0 - f64::from(gain);
        let theirs = ffmpeg_loudness(&track.path);
        assert!(
            (ours - theirs).abs() <= 0.11,
            "{}: {ours:.2} LUFS here, {theirs:.1} LUFS by ffmpeg",
            track.path.display()
        );
        // The quiet opening (26 dB down for a quarter of the file) would pull a reading
        // without the gate about 1.2 LU lower, more than the tolerance above: the
        // agreement covers the gate too. And the file is not silent.
        assert!((-40.0..-5.0).contains(&ours), "{ours}");
    }
}

/// The file check over a real library: results per file, kept across a restart, and
/// dropped when a file changes.
#[test]
fn the_file_check_finds_damage_and_keeps_its_results() {
    use oneum::model::CheckVerdict;
    let env = TestEnv::new();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for name in ["gap_a.flac", "damaged_middle.flac", "gap_b.wav"] {
        std::fs::copy(fixtures.join(name), env.music().join(name)).unwrap();
    }
    write_tone(&env.music().join("tone.wav"), 12.0, -1.0, "Tone", "Checks");
    let (library, _) = env.open_scanned();
    assert!(library.check_files());
    assert!(library.wait_for_checks(std::time::Duration::from_secs(60)));
    let status = library.check_status();
    assert_eq!((status.total, status.done, status.damaged), (4, 4, 1));
    let verdict = |library: &Library, file: &str| {
        library
            .snapshot()
            .tracks
            .iter()
            .find(|t| t.path.ends_with(file))
            .and_then(|t| t.check)
    };
    assert_eq!(
        verdict(&library, "gap_a.flac").map(|c| c.verdict),
        Some(CheckVerdict::Verified)
    );
    assert_eq!(
        verdict(&library, "damaged_middle.flac").map(|c| c.verdict),
        Some(CheckVerdict::Damaged)
    );
    let tone = verdict(&library, "tone.wav").expect("checked");
    assert_eq!(tone.verdict, CheckVerdict::Decoded);
    // A sine: DR about 0, true peak at its −1 dB peak.
    assert!(tone.dr.is_some_and(|dr| dr.abs() < 0.2), "{:?}", tone.dr);
    let tp_db = 20.0 * tone.true_peak.expect("peak").log10();
    assert!((tp_db + 1.0).abs() < 0.1, "{tp_db}");
    // A second run has nothing left to do.
    assert!(library.check_files());
    assert!(library.wait_for_checks(std::time::Duration::from_secs(60)));
    assert_eq!(library.check_status().total, 0);
    library.shutdown();
    drop(library);

    // Kept across a restart; a changed file loses its result.
    let library = env.open();
    assert!(verdict(&library, "gap_b.wav").is_some());
    std::fs::copy(fixtures.join("gap_a.wav"), env.music().join("gap_b.wav")).unwrap();
    rescan(&library);
    assert!(verdict(&library, "gap_b.wav").is_none());
    assert!(verdict(&library, "gap_a.flac").is_some());
    // "Forget" clears everything.
    library.forget_checks();
    assert!(verdict(&library, "gap_a.flac").is_none());
    library.shutdown();
}
