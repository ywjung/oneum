//! Scanning real files in real temporary directories: formats, incremental rescans,
//! offline folders, moved/missing files, symlinks, broken files, artwork, watching.
// Unix only: many of these tests build their folders with symbolic links and
// permission bits, and compare paths in their Unix form.
#![cfg(unix)]
#[path = "library_support.rs"]
mod support;

use oneum::library::{Library, LibraryError};
use oneum::model::Track;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use support::*;

fn by_title(library: &Library, title: &str) -> Arc<Track> {
    let snapshot = library.snapshot();
    let found: Vec<_> = snapshot
        .tracks
        .iter()
        .filter(|t| t.title == title)
        .collect();
    assert_eq!(
        found.len(),
        1,
        "expected exactly one track titled {title:?}"
    );
    Arc::clone(found[0])
}

fn titles(library: &Library) -> Vec<String> {
    let mut titles: Vec<String> = library
        .snapshot()
        .tracks
        .iter()
        .map(|t| t.title.clone())
        .collect();
    titles.sort();
    titles
}

#[test]
fn scan_then_incremental_rescan() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("a/one.wav"), 300, "One", "Artist", "Album");
    write_tagged_wav(&music.join("a/two.wav"), 400, "Two", "Artist", "Album");
    write_wav(&music.join("b/deep/untagged song.wav"), 500, &[]);
    // Things the scanner must ignore.
    write_wav(&music.join("a/.hidden.wav"), 100, &[]);
    write_wav(&music.join("a/._one.wav"), 100, &[]);
    write_wav(&music.join(".hidden-dir/x.wav"), 100, &[]);
    fs::write(music.join("notes.txt"), b"not audio").unwrap();
    fs::write(music.join("a/track.opus"), b"not scanned by extension").unwrap();

    let notified = Arc::new(AtomicUsize::new(0));
    let library = env.open();
    {
        let notified = Arc::clone(&notified);
        library.set_notifier(Box::new(move || {
            notified.fetch_add(1, Ordering::Relaxed);
        }));
    }
    assert!(library.snapshot().is_empty());
    let generation_before = library.generation();
    let root = library.add_folder(&music).unwrap();
    assert!(
        library.scan_status().running,
        "scan must be reported as running at once"
    );
    wait_scan(&library);

    assert_eq!(titles(&library), ["One", "Two", "untagged song"]);
    assert!(library.generation() > generation_before);
    assert!(notified.load(Ordering::Relaxed) > 0);
    let status = library.scan_status();
    assert!(!status.running);
    assert_eq!(
        (status.files_found, status.files_done, status.tracks_added),
        (3, 3, 3)
    );
    assert_eq!(
        (status.tracks_updated, status.unsupported, status.errors),
        (0, 0, 0)
    );
    assert_eq!(library.debug_tag_reads(), 3);

    let untagged = by_title(&library, "untagged song");
    assert_eq!(untagged.path, music.join("b/deep/untagged song.wav"));
    assert_eq!(untagged.root_id, Some(root));
    assert!(untagged.available);
    assert_eq!(
        (untagged.codec.as_str(), untagged.container.as_str()),
        ("PCM", "WAV")
    );
    assert_eq!((untagged.sample_rate, untagged.channels), (8000, 1));
    assert_eq!(untagged.bit_depth, Some(16));
    assert_eq!(untagged.duration_ms, 500);
    assert_eq!(
        untagged.file_size,
        fs::metadata(&untagged.path).unwrap().len()
    );
    assert!(untagged.added_at > 1_600_000_000);
    assert_eq!(untagged.artist, "");

    let roots = library.roots();
    assert_eq!(roots.len(), 1);
    assert_eq!(
        (roots[0].id, roots[0].track_count, roots[0].online),
        (root, 3, true)
    );
    assert_eq!(roots[0].path, music);
    assert!(roots[0].last_scan_at.is_some());

    // Nothing changed: nothing is read again and the snapshot is not replaced.
    let one_before = by_title(&library, "One");
    let generation = library.generation();
    rescan(&library);
    assert_eq!(
        library.debug_tag_reads(),
        3,
        "unchanged files must not be re-read"
    );
    let status = library.scan_status();
    assert_eq!((status.files_found, status.files_done), (3, 3));
    assert_eq!((status.tracks_added, status.tracks_updated), (0, 0));
    assert_eq!(library.generation(), generation);

    // One file changes: only that file is read, and its row keeps its identity.
    write_tagged_wav(
        &music.join("a/one.wav"),
        700,
        "One (remaster)",
        "Artist",
        "Album",
    );
    rescan(&library);
    assert_eq!(library.debug_tag_reads(), 4);
    let status = library.scan_status();
    assert_eq!((status.tracks_added, status.tracks_updated), (0, 1));
    let one_after = by_title(&library, "One (remaster)");
    assert_eq!(one_after.id, one_before.id);
    assert_eq!(one_after.added_at, one_before.added_at);
    assert_eq!(one_after.duration_ms, 700);
    assert_eq!(library.snapshot().search("remaster").len(), 1);
    assert_eq!(library.snapshot().len(), 3);

    // A new file appears, another disappears (unreferenced: removed from the library).
    write_tagged_wav(&music.join("c/three.wav"), 200, "Three", "Artist", "Album");
    fs::remove_file(music.join("a/two.wav")).unwrap();
    rescan(&library);
    assert_eq!(
        titles(&library),
        ["One (remaster)", "Three", "untagged song"]
    );
    assert_eq!(library.debug_tag_reads(), 5);
}

#[test]
fn state_survives_reopen() {
    let env = TestEnv::new();
    write_tagged_wav(&env.music().join("one.wav"), 300, "One", "Artist", "Album");
    write_tagged_wav(&env.music().join("two.wav"), 400, "Two", "Artist", "Album");
    let before: Vec<Track> = {
        let (library, _) = env.open_scanned();
        let id = by_title(&library, "Two").id;
        library.set_favorite(&[id], true);
        library.record_play(id);
        let tracks = library
            .snapshot()
            .tracks
            .iter()
            .map(|t| (**t).clone())
            .collect();
        library.shutdown();
        library.shutdown(); // idempotent
        tracks
    };
    assert_eq!(before.len(), 2);

    let (library, report) = Library::open(env.paths()).unwrap();
    assert!(report.recovered_corrupt_db.is_none());
    assert!(report.migrated_from.is_none());
    let after: Vec<Track> = library
        .snapshot()
        .tracks
        .iter()
        .map(|t| (**t).clone())
        .collect();
    assert_eq!(
        after, before,
        "tracks must come back from the database unchanged"
    );
    assert_eq!(library.debug_tag_reads(), 0);
    let two = by_title(&library, "Two");
    assert!(two.favorite);
    assert_eq!(two.play_count, 1);
    assert_eq!(library.roots().len(), 1);
    // A rescan after reopening still recognises every file as unchanged.
    rescan(&library);
    assert_eq!(library.debug_tag_reads(), 0);
}

#[test]
fn corrupt_database_is_replaced_and_reported() {
    let env = TestEnv::new();
    let paths = env.paths();
    fs::create_dir_all(&paths.data_dir).unwrap();
    fs::write(paths.data_dir.join("library.db"), noise(4096, 7)).unwrap();
    let (library, report) = Library::open(paths.clone()).unwrap();
    let aside = report
        .recovered_corrupt_db
        .expect("must report the recovery");
    assert!(aside.exists());
    assert!(library.snapshot().is_empty());
    // The fresh database is fully usable.
    write_wav(&env.music().join("a.wav"), 200, &[]);
    library.add_folder(&env.music()).unwrap();
    wait_scan(&library);
    assert_eq!(library.snapshot().len(), 1);
}

#[test]
fn formats_and_korean_metadata_round_trip() {
    if !ffmpeg_or_skip("formats_and_korean_metadata_round_trip") {
        return;
    }
    let env = TestEnv::new();
    let music = env.music();
    let meta = [
        ("title", "밤편지 🌙"),
        ("artist", "아이유"),
        ("album", "Palette 🎨"),
        ("album_artist", "아이유 (IU)"),
        ("track", "3/12"),
        ("disc", "1/2"),
        ("date", "2017-04-21"),
        ("genre", "K-Pop"),
    ];
    /// One fixture: file name, ffmpeg codec arguments and what the scan must report.
    struct Case {
        file: &'static str,
        args: Vec<&'static str>,
        codec: &'static str,
        container: &'static str,
        /// Bit depth for lossless formats, `None` for lossy ones.
        bit_depth: Option<u8>,
        /// False for formats whose tags cannot carry album artist and disc number.
        full_tags: bool,
    }
    let case = |file, args: &[&'static str], codec, container, bit_depth, full_tags| Case {
        file,
        args: args.to_vec(),
        codec,
        container,
        bit_depth,
        full_tags,
    };
    let mut cases = vec![
        case(
            "k.mp3",
            &["-c:a", "libmp3lame", "-b:a", "128k"],
            "MP3",
            "MP3",
            None,
            true,
        ),
        case("k.flac", &["-c:a", "flac"], "FLAC", "FLAC", Some(16), true),
        case(
            "k-aac.m4a",
            &["-c:a", "aac", "-b:a", "96k"],
            "AAC",
            "MP4",
            None,
            true,
        ),
        case(
            "k-alac.m4a",
            &["-c:a", "alac"],
            "ALAC",
            "MP4",
            Some(16),
            true,
        ),
        case(
            "k.aiff",
            &["-c:a", "pcm_s16be", "-write_id3v2", "1"],
            "PCM",
            "AIFF",
            Some(16),
            true,
        ),
        // RIFF INFO has no album artist or disc number.
        case(
            "k.wav",
            &["-c:a", "pcm_s16le"],
            "PCM",
            "WAV",
            Some(16),
            false,
        ),
    ];
    match vorbis_args() {
        Some(args) => cases.push(case("k.ogg", &args, "Vorbis", "Ogg", None, true)),
        None => eprintln!("note: Ogg Vorbis case skipped: ffmpeg has no Vorbis encoder"),
    }
    for case in &cases {
        ffmpeg_audio(&music.join(case.file), &case.args, &meta, None);
    }

    let (library, _) = env.open_scanned();
    let status = library.scan_status();
    assert_eq!(status.errors, 0);
    assert_eq!(status.unsupported, 0);
    let snapshot = library.snapshot();
    assert_eq!(snapshot.len(), cases.len());

    for Case {
        file,
        codec,
        container,
        bit_depth,
        full_tags,
        ..
    } in &cases
    {
        let track = snapshot
            .tracks
            .iter()
            .find(|t| t.path == music.join(file))
            .unwrap_or_else(|| panic!("{file} not in the library"));
        assert_eq!(track.title, "밤편지 🌙", "{file}");
        assert_eq!(track.artist, "아이유", "{file}");
        assert_eq!(track.album, "Palette 🎨", "{file}");
        assert_eq!(track.track_no, Some(3), "{file}");
        assert_eq!(track.year, Some(2017), "{file}");
        assert_eq!(track.genre, "K-Pop", "{file}");
        if *full_tags {
            assert_eq!(track.album_artist, "아이유 (IU)", "{file}");
            assert_eq!(track.disc_no, Some(1), "{file}");
        }
        assert_eq!(track.codec, *codec, "{file}");
        assert_eq!(track.container, *container, "{file}");
        assert_eq!(track.bit_depth, *bit_depth, "{file}");
        assert_eq!(track.sample_rate, 44_100, "{file}");
        assert!(
            matches!(track.channels, 1 | 2),
            "{file}: {}",
            track.channels
        );
        assert!(
            (900..=1200).contains(&track.duration_ms),
            "{file}: duration {} ms",
            track.duration_ms
        );
        if bit_depth.is_none() {
            assert!(track.bitrate_kbps.is_some_and(|kbps| kbps > 0), "{file}");
        }
        assert!(track.available);
    }

    // Search over the scanned metadata: partial Hangul, choseong, mixed terms, emoji.
    let n = cases.len();
    assert_eq!(snapshot.search("이유").len(), n);
    assert_eq!(snapshot.search("ㅂㅍㅈ").len(), n);
    assert_eq!(snapshot.search("ㅇㅇㅇ palette").len(), n);
    assert_eq!(snapshot.search("PALETTE 밤편").len(), n);
    assert_eq!(snapshot.search("🌙").len(), n);
    assert!(snapshot.search("아이유 nothing").is_empty());

    // Every file with the full tag set belongs to one album (same title + album artist).
    let full: Vec<_> = cases.iter().filter(|c| c.full_tags).collect();
    let album_keys: std::collections::HashSet<u64> = snapshot
        .tracks
        .iter()
        .filter(|t| !t.album_artist.is_empty())
        .map(|t| t.album_key)
        .collect();
    assert_eq!(album_keys.len(), 1);
    let album = &snapshot.albums()[snapshot
        .album_index(*album_keys.iter().next().unwrap())
        .unwrap() as usize];
    // Files of the same folder and album title that lack the album-artist tag (formats
    // that cannot carry it) are shown as part of that album too.
    let same_title = snapshot
        .tracks
        .iter()
        .filter(|t| t.album == album.title)
        .count();
    assert!(album.tracks.len() >= full.len());
    assert_eq!(album.tracks.len(), same_title);
    assert_eq!(album.artist, "아이유 (IU)");
    assert_eq!(album.year, Some(2017));

    // The same data comes back from the database after a restart.
    let before: Vec<Track> = snapshot.tracks.iter().map(|t| (**t).clone()).collect();
    drop(snapshot);
    drop(library);
    let library = env.open();
    let after: Vec<Track> = library
        .snapshot()
        .tracks
        .iter()
        .map(|t| (**t).clone())
        .collect();
    assert_eq!(after, before);
}

#[test]
fn m4a_codec_is_read_from_the_content() {
    if !ffmpeg_or_skip("m4a_codec_is_read_from_the_content") {
        return;
    }
    let env = TestEnv::new();
    let music = env.music();
    // Same extension, different codecs.
    ffmpeg_audio(
        &music.join("lossy.m4a"),
        &["-c:a", "aac"],
        &[("title", "Lossy")],
        None,
    );
    ffmpeg_audio(
        &music.join("lossless.m4a"),
        &["-c:a", "alac"],
        &[("title", "Lossless")],
        None,
    );
    // The extension lies: FLAC data in a file named .mp3 is still described correctly.
    ffmpeg_audio(
        &music.join("really-flac.mp3"),
        &["-c:a", "flac", "-f", "flac"],
        &[("title", "Mislabelled")],
        None,
    );
    let (library, _) = env.open_scanned();
    let lossy = by_title(&library, "Lossy");
    assert_eq!(
        (lossy.codec.as_str(), lossy.container.as_str()),
        ("AAC", "MP4")
    );
    assert_eq!(lossy.bit_depth, None);
    let lossless = by_title(&library, "Lossless");
    assert_eq!(
        (lossless.codec.as_str(), lossless.container.as_str()),
        ("ALAC", "MP4")
    );
    assert_eq!(lossless.bit_depth, Some(16));
    let mislabelled = by_title(&library, "Mislabelled");
    assert_eq!(
        (mislabelled.codec.as_str(), mislabelled.container.as_str()),
        ("FLAC", "FLAC")
    );
}

/// MPEG Layer II is told apart from MP3 by what is in the file, whatever it is called.
#[test]
fn mpeg_layer_2_is_recognised_under_either_name() {
    if !has_encoder("mp2") {
        eprintln!("skipped: this ffmpeg has no mp2 encoder");
        return;
    }
    let env = TestEnv::new();
    let music = env.music();
    ffmpeg_audio(
        &music.join("broadcast.mp2"),
        &["-c:a", "mp2", "-b:a", "192k"],
        &[],
        None,
    );
    ffmpeg_audio(
        &music.join("misnamed.mp3"),
        &["-c:a", "mp2", "-f", "mp2"],
        &[],
        None,
    );
    ffmpeg_audio(
        &music.join("real.mp3"),
        &["-c:a", "libmp3lame", "-b:a", "128k"],
        &[],
        None,
    );
    let (library, _) = env.open_scanned();
    let status = library.scan_status();
    assert_eq!((status.unsupported, status.errors), (0, 0));
    for (title, codec) in [("broadcast", "MP2"), ("misnamed", "MP2"), ("real", "MP3")] {
        let track = by_title(&library, title);
        assert_eq!(track.codec, codec, "{title}");
        assert_eq!((track.sample_rate, track.channels), (44_100, 1), "{title}");
        assert!(
            (900..=1200).contains(&track.duration_ms),
            "{title}: {}",
            track.duration_ms
        );
    }
}

#[test]
fn unsupported_codecs_are_counted_not_added() {
    if !ffmpeg_or_skip("unsupported_codecs_are_counted_not_added") {
        return;
    }
    let env = TestEnv::new();
    let music = env.music();
    write_wav(&music.join("ok.wav"), 200, &[]);
    let mut expected_unsupported = 0;
    if has_encoder("libopus") {
        // Opus in an Ogg container named .ogg.
        ffmpeg_audio(
            &music.join("opus.ogg"),
            &["-c:a", "libopus", "-ar", "48000", "-f", "ogg"],
            &[("title", "Opus")],
            None,
        );
        expected_unsupported += 1;
    }
    if has_encoder("adpcm_ms") {
        ffmpeg_audio(&music.join("adpcm.wav"), &["-c:a", "adpcm_ms"], &[], None);
        expected_unsupported += 1;
    }
    // AAC that goes by the same name as what plays, but that the decoder refuses:
    // 5.1, and HE-AAC (v1 and v2; only Apple's encoder writes those here).
    ffmpeg_audio(
        &music.join("aac51.m4a"),
        &["-ac", "6", "-ar", "48000", "-c:a", "aac", "-b:a", "256k"],
        &[("title", "Surround")],
        None,
    );
    expected_unsupported += 1;
    if has_encoder("aac_at") {
        for (name, profile) in [("he.m4a", "4"), ("hev2.m4a", "28")] {
            ffmpeg_audio(
                &music.join(name),
                &[
                    "-ac",
                    "2",
                    "-c:a",
                    "aac_at",
                    "-profile:a",
                    profile,
                    "-b:a",
                    "48k",
                ],
                &[("title", "High Efficiency")],
                None,
            );
            expected_unsupported += 1;
        }
    }
    let (library, _) = env.open_scanned();
    let status = library.scan_status();
    assert_eq!(titles(&library), ["ok"]);
    assert_eq!(status.unsupported, expected_unsupported);
    assert_eq!(status.errors, 0);
    assert_eq!(status.files_found, expected_unsupported + 1);
    assert_eq!(status.files_done, status.files_found);
}

#[test]
fn replaygain_tags_are_parsed() {
    if !ffmpeg_or_skip("replaygain_tags_are_parsed") {
        return;
    }
    use lofty::config::WriteOptions;
    use lofty::file::{AudioFile, TaggedFileExt};
    use lofty::tag::ItemKey;

    let env = TestEnv::new();
    let music = env.music();
    // Vorbis comments, upper case, with unit and explicit sign.
    ffmpeg_audio(
        &music.join("rg.flac"),
        &["-c:a", "flac"],
        &[
            ("title", "RG FLAC"),
            ("REPLAYGAIN_TRACK_GAIN", "-6.54 dB"),
            ("REPLAYGAIN_TRACK_PEAK", "0.987654"),
            ("REPLAYGAIN_ALBUM_GAIN", "+1.20 dB"),
            ("REPLAYGAIN_ALBUM_PEAK", "1.000000"),
        ],
        None,
    );
    // ID3v2 TXXX frames, lower case, no space before the unit.
    ffmpeg_audio(
        &music.join("rg.mp3"),
        &["-c:a", "libmp3lame"],
        &[
            ("title", "RG MP3"),
            ("replaygain_track_gain", "-3.10dB"),
            ("replaygain_track_peak", "0.5"),
        ],
        None,
    );
    // MP4 freeform atoms (----:com.apple.iTunes:replaygain_*), written with lofty since
    // ffmpeg cannot produce them.
    let m4a = music.join("rg.m4a");
    ffmpeg_audio(&m4a, &["-c:a", "aac"], &[("title", "RG M4A")], None);
    {
        let mut tagged = lofty::read_from_path(&m4a).unwrap();
        let tag = tagged.primary_tag_mut().expect("ilst tag");
        assert!(tag.insert_text(ItemKey::ReplayGainTrackGain, "-9.25 dB".into()));
        assert!(tag.insert_text(ItemKey::ReplayGainTrackPeak, "1.050000".into()));
        assert!(tag.insert_text(ItemKey::ReplayGainAlbumGain, "-8.00 dB".into()));
        tagged.save_to_path(&m4a, WriteOptions::default()).unwrap();
    }
    ffmpeg_audio(
        &music.join("none.flac"),
        &["-c:a", "flac"],
        &[("title", "No RG")],
        None,
    );

    let (library, _) = env.open_scanned();
    let close =
        |value: Option<f32>, expected: f32| value.is_some_and(|v| (v - expected).abs() < 1e-4);

    let flac = by_title(&library, "RG FLAC").replaygain;
    assert!(close(flac.track_gain_db, -6.54), "{flac:?}");
    assert!(close(flac.track_peak, 0.987654), "{flac:?}");
    assert!(close(flac.album_gain_db, 1.2), "{flac:?}");
    assert!(close(flac.album_peak, 1.0), "{flac:?}");

    let mp3 = by_title(&library, "RG MP3").replaygain;
    assert!(close(mp3.track_gain_db, -3.1), "{mp3:?}");
    assert!(close(mp3.track_peak, 0.5), "{mp3:?}");
    assert_eq!((mp3.album_gain_db, mp3.album_peak), (None, None));

    let m4a = by_title(&library, "RG M4A").replaygain;
    assert!(close(m4a.track_gain_db, -9.25), "{m4a:?}");
    assert!(close(m4a.track_peak, 1.05), "{m4a:?}");
    assert!(close(m4a.album_gain_db, -8.0), "{m4a:?}");
    assert_eq!(m4a.album_peak, None);

    let none = by_title(&library, "No RG").replaygain;
    assert_eq!(none, Default::default());
}

#[test]
fn offline_folder_keeps_its_tracks() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("a/one.wav"), 300, "One", "Artist", "Album");
    write_tagged_wav(&music.join("a/two.wav"), 400, "Two", "Artist", "Album");
    let (library, root) = env.open_scanned();
    let ids: Vec<i64> = library.snapshot().tracks.iter().map(|t| t.id).collect();
    assert_eq!(ids.len(), 2);

    // "Unplug the drive": the folder is gone. Nothing is deleted from the library.
    let away = env.dir.path().join("music-unplugged");
    fs::rename(&music, &away).unwrap();
    rescan(&library);
    let snapshot = library.snapshot();
    assert_eq!(snapshot.len(), 2);
    assert!(snapshot.tracks.iter().all(|t| !t.available));
    assert_eq!(library.roots()[0].track_count, 2);
    assert!(!library.roots()[0].online);
    for id in &ids {
        assert!(library.track(*id).is_some());
    }

    // Still offline when the app is restarted.
    drop(snapshot);
    drop(library);
    let library = env.open();
    assert!(!library.roots()[0].online);
    assert_eq!(library.snapshot().len(), 2);
    assert!(library.snapshot().tracks.iter().all(|t| !t.available));

    // "Plug it back in": same rows, available again, and no file is read again.
    fs::rename(&away, &music).unwrap();
    rescan(&library);
    let snapshot = library.snapshot();
    assert!(library.roots()[0].online);
    assert_eq!(library.roots()[0].id, root);
    assert_eq!(snapshot.len(), 2);
    assert!(snapshot.tracks.iter().all(|t| t.available));
    let mut back: Vec<i64> = snapshot.tracks.iter().map(|t| t.id).collect();
    back.sort();
    let mut ids = ids;
    ids.sort();
    assert_eq!(back, ids);
    assert_eq!(library.debug_tag_reads(), 0);
}

#[test]
fn emptied_mount_point_does_not_wipe_the_library() {
    // A volume that unmounts but leaves its (now empty) mount point behind must not be
    // mistaken for "the user deleted everything".
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("a/one.wav"), 300, "One", "Artist", "Album");
    let (library, _) = env.open_scanned();
    let away = env.dir.path().join("elsewhere");
    fs::rename(music.join("a"), &away).unwrap();
    assert_eq!(fs::read_dir(&music).unwrap().count(), 0);
    rescan(&library);
    let track = by_title(&library, "One");
    assert!(!track.available);
    fs::rename(&away, music.join("a")).unwrap();
    rescan(&library);
    assert!(by_title(&library, "One").available);
}

#[test]
fn moved_file_is_relinked() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(
        &music.join("x/song a.wav"),
        300,
        "Song A",
        "Artist",
        "Album",
    );
    write_tagged_wav(
        &music.join("x/song b.wav"),
        400,
        "Song B",
        "Artist",
        "Album",
    );
    write_wav(&music.join("x/untagged take.wav"), 550, &[]);
    let (library, root) = env.open_scanned();
    let a = by_title(&library, "Song A");
    let b = by_title(&library, "Song B");
    let untagged = by_title(&library, "untagged take");
    library.set_favorite(&[a.id], true);
    library.record_play(a.id);
    let playlist = library.create_playlist("mix").unwrap();
    library
        .add_to_playlist(playlist, &[a.id, b.id, a.id])
        .unwrap();
    assert_eq!(library.debug_tag_reads(), 3);

    // Rename and move one tagged file, rename the untagged one.
    fs::create_dir_all(music.join("y")).unwrap();
    fs::rename(music.join("x/song a.wav"), music.join("y/renamed a.wav")).unwrap();
    fs::rename(
        music.join("x/untagged take.wav"),
        music.join("x/final take.wav"),
    )
    .unwrap();
    rescan(&library);

    assert_eq!(library.snapshot().len(), 3);
    let moved = library.track(a.id).expect("the row survives the move");
    assert_eq!(moved.path, music.join("y/renamed a.wav"));
    assert!(moved.available);
    assert!(moved.favorite);
    assert_eq!(moved.play_count, 1);
    assert_eq!(moved.added_at, a.added_at);
    assert_eq!(moved.root_id, Some(root));
    assert_eq!(moved.title, "Song A");
    // Albums without an album artist are identified by folder, so the key follows.
    assert_ne!(moved.album_key, a.album_key);
    let items: Vec<i64> = library
        .playlist_items(playlist)
        .iter()
        .map(|item| item.track_id)
        .collect();
    assert_eq!(items, [a.id, b.id, a.id]);
    assert_eq!(library.recent_plays(10), [a.id]);

    // The untagged file keeps its row too; its title follows the new file name.
    let renamed = library.track(untagged.id).expect("untagged row survives");
    assert_eq!(renamed.path, music.join("x/final take.wav"));
    assert_eq!(renamed.title, "final take");
    // Only the two files at new paths were read.
    assert_eq!(library.debug_tag_reads(), 5);

    // The relink is persistent.
    drop(library);
    let library = env.open();
    assert_eq!(library.snapshot().len(), 3);
    assert_eq!(
        library.track(a.id).unwrap().path,
        music.join("y/renamed a.wav")
    );
    assert!(library.track(a.id).unwrap().favorite);
}

#[test]
fn missing_files_follow_the_keep_or_remove_policy() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("in playlist.wav"), 300, "In Playlist", "A", "B");
    write_tagged_wav(&music.join("favourite.wav"), 350, "Favourite", "A", "B");
    write_tagged_wav(&music.join("plain.wav"), 400, "Plain", "A", "B");
    write_tagged_wav(&music.join("stays.wav"), 450, "Stays", "A", "B");
    let (library, _) = env.open_scanned();
    let in_playlist = by_title(&library, "In Playlist");
    let favourite = by_title(&library, "Favourite");
    let plain = by_title(&library, "Plain");
    let playlist = library.create_playlist("keep").unwrap();
    library
        .add_to_playlist(playlist, &[in_playlist.id])
        .unwrap();
    library.set_favorite(&[favourite.id], true);

    let saved = fs::read(music.join("in playlist.wav")).unwrap();
    for name in ["in playlist.wav", "favourite.wav", "plain.wav"] {
        fs::remove_file(music.join(name)).unwrap();
    }
    rescan(&library);

    // Referenced tracks stay, marked unavailable; the unreferenced one is removed.
    assert_eq!(titles(&library), ["Favourite", "In Playlist", "Stays"]);
    assert!(!library.track(in_playlist.id).unwrap().available);
    assert!(!library.track(favourite.id).unwrap().available);
    assert!(library.track(plain.id).is_none());
    assert!(by_title(&library, "Stays").available);
    assert_eq!(library.playlist_items(playlist).len(), 1);
    // The files that remain on disk were never touched.
    assert!(music.join("stays.wav").exists());

    // When the file comes back, the same row becomes available again.
    fs::write(music.join("in playlist.wav"), saved).unwrap();
    rescan(&library);
    let back = library.track(in_playlist.id).unwrap();
    assert!(back.available);
    assert_eq!(library.snapshot().len(), 3);
}

#[test]
fn symlinks_are_followed_and_loops_terminate() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("a/inside.wav"), 300, "Inside", "A", "B");
    let external = env.dir.path().join("external-drive");
    write_tagged_wav(
        &external.join("album/outside.wav"),
        300,
        "Outside",
        "A",
        "B",
    );
    // A link to a directory outside the folder, a link back to an ancestor (a loop), a
    // link to itself, and a dangling link.
    symlink(&external, music.join("linked")).unwrap();
    symlink(&music, music.join("a/loop")).unwrap();
    symlink(music.join("self"), music.join("self")).unwrap();
    symlink(env.dir.path().join("nowhere"), music.join("dangling")).unwrap();

    let (library, _) = env.open_scanned();
    assert_eq!(titles(&library), ["Inside", "Outside"]);
    let outside = by_title(&library, "Outside");
    assert_eq!(outside.path, music.join("linked/album/outside.wav"));

    // The link's target disappears (drive unplugged): although nothing refers to the
    // track, it is kept as unavailable rather than removed.
    let unplugged = env.dir.path().join("external-unplugged");
    fs::rename(&external, &unplugged).unwrap();
    rescan(&library);
    assert_eq!(titles(&library), ["Inside", "Outside"]);
    assert!(!library.track(outside.id).unwrap().available);
    assert!(by_title(&library, "Inside").available);

    fs::rename(&unplugged, &external).unwrap();
    rescan(&library);
    assert!(library.track(outside.id).unwrap().available);
}

#[test]
fn broken_files_do_not_abort_the_scan() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("good.wav"), 300, "Good", "A", "B");
    let mut bad = 0;
    for seed in 1..=6u64 {
        fs::write(music.join(format!("noise-{seed}.mp3")), noise(20_000, seed)).unwrap();
        bad += 1;
    }
    fs::write(music.join("noise.flac"), noise(5_000, 99)).unwrap();
    fs::write(music.join("noise.m4a"), noise(5_000, 98)).unwrap();
    fs::write(music.join("noise.ogg"), noise(5_000, 97)).unwrap();
    fs::write(music.join("noise.wav"), noise(5_000, 96)).unwrap();
    fs::write(music.join("noise.aiff"), noise(5_000, 95)).unwrap();
    fs::write(music.join("empty.flac"), b"").unwrap();
    fs::write(
        music.join("text.m4a"),
        "이것은 오디오가 아닙니다\n".repeat(50),
    )
    .unwrap();
    // A WAV header that promises far more data than the file has, and a FLAC marker
    // followed by garbage.
    fs::write(music.join("truncated.wav"), b"RIFF\xff\xff\xff\x7fWAVEfmt ").unwrap();
    let mut fake_flac = b"fLaC".to_vec();
    fake_flac.extend(noise(3_000, 5));
    fs::write(music.join("fake.flac"), fake_flac).unwrap();
    bad += 9;

    let (library, _) = env.open_scanned();
    let status = library.scan_status();
    assert!(!status.running);
    assert_eq!(
        titles(&library),
        ["Good"],
        "only the valid file may be added"
    );
    assert_eq!(status.files_found, bad + 1);
    assert_eq!(status.files_done, bad + 1);
    assert_eq!(status.tracks_added, 1);
    assert_eq!(status.errors + status.unsupported, bad);

    // The library keeps working afterwards, and unchanged bad files are still counted
    // but not read again.
    let reads = library.debug_tag_reads();
    assert_eq!(reads, bad + 1);
    write_tagged_wav(&music.join("later.wav"), 300, "Later", "A", "B");
    rescan(&library);
    assert_eq!(titles(&library), ["Good", "Later"]);
    assert_eq!(library.debug_tag_reads(), reads + 1);
    let status = library.scan_status();
    assert_eq!(status.errors + status.unsupported, bad);
    assert_eq!(status.files_done, status.files_found);
}

#[test]
fn artwork_is_cached_and_regenerated() {
    if !ffmpeg_or_skip("artwork_is_cached_and_regenerated") {
        return;
    }
    let env = TestEnv::new();
    let music = env.music();
    let cover = env.dir.path().join("cover-source.png");
    write_png(&cover, 900, 600);
    ffmpeg_audio(
        &music.join("album/embedded.flac"),
        &["-c:a", "flac"],
        &[("title", "Embedded FLAC"), ("album", "Art")],
        Some(&cover),
    );
    ffmpeg_audio(
        &music.join("album/embedded.mp3"),
        &["-c:a", "libmp3lame", "-id3v2_version", "3"],
        &[("title", "Embedded MP3"), ("album", "Art")],
        Some(&cover),
    );
    // No embedded picture, but an image file in the folder (name matched ignoring case).
    write_tagged_wav(
        &music.join("folder/plain.wav"),
        300,
        "Folder Art",
        "A",
        "Folder",
    );
    write_png(&music.join("folder/Cover.PNG"), 300, 300);
    write_tagged_wav(&music.join("bare/none.wav"), 300, "No Art", "A", "Bare");
    // An unreadable "cover" must simply mean "no artwork".
    write_tagged_wav(
        &music.join("broken/x.wav"),
        300,
        "Broken Art",
        "A",
        "Broken",
    );
    fs::write(music.join("broken/cover.jpg"), noise(2_000, 3)).unwrap();

    // An embedded "picture" that is not an image: the track is added without artwork.
    let garbage = music.join("garbage/embedded.mp3");
    ffmpeg_audio(
        &garbage,
        &["-c:a", "libmp3lame"],
        &[("title", "Garbage Art"), ("album", "Garbage")],
        None,
    );
    {
        use lofty::config::WriteOptions;
        use lofty::file::{AudioFile, TaggedFileExt};
        use lofty::picture::{MimeType, Picture, PictureType};
        let mut tagged = lofty::read_from_path(&garbage).unwrap();
        let picture = Picture::unchecked(noise(4_000, 17))
            .pic_type(PictureType::CoverFront)
            .mime_type(MimeType::Jpeg)
            .build();
        tagged.primary_tag_mut().unwrap().push_picture(picture);
        tagged
            .save_to_path(&garbage, WriteOptions::default())
            .unwrap();
    }

    let (library, _) = env.open_scanned();
    assert_eq!(library.scan_status().errors, 0);
    assert_eq!(by_title(&library, "Garbage Art").art_id, None);
    let flac = by_title(&library, "Embedded FLAC");
    let mp3 = by_title(&library, "Embedded MP3");
    let art_id = flac.art_id.clone().expect("embedded cover");
    assert_eq!(art_id.len(), 16);
    assert!(art_id.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(
        mp3.art_id.as_deref(),
        Some(art_id.as_str()),
        "same image, one cache entry"
    );
    let path = library.art_path(&art_id);
    assert!(path.is_file());
    assert!(path.starts_with(env.paths().cache_dir.join("art").join(&art_id[..2])));

    // Cached at no more than 600 px, decoded at whatever size the UI asks for.
    let full = library.load_art_rgba(&art_id, 2000).unwrap();
    assert_eq!((full.width, full.height), (600, 400));
    let thumb = library.load_art_rgba(&art_id, 96).unwrap();
    assert_eq!((thumb.width, thumb.height), (96, 64));
    assert_eq!(thumb.rgba.len(), 96 * 64 * 4);

    let folder = by_title(&library, "Folder Art");
    let folder_art = folder.art_id.clone().expect("folder image");
    assert_ne!(folder_art, art_id);
    let image = library.load_art_rgba(&folder_art, 600).unwrap();
    assert_eq!((image.width, image.height), (300, 300));
    assert_eq!(by_title(&library, "No Art").art_id, None);
    assert_eq!(by_title(&library, "Broken Art").art_id, None);

    // The album carries the artwork of its tracks.
    let snapshot = library.snapshot();
    let album = &snapshot.albums()[snapshot.album_index(flac.album_key).unwrap() as usize];
    assert_eq!(album.art_id.as_deref(), Some(art_id.as_str()));

    // Evicted cache file: the load reports "nothing" now, regenerates in the
    // background and notifies; afterwards the image is back under the same id.
    let notified = Arc::new(AtomicUsize::new(0));
    {
        let notified = Arc::clone(&notified);
        library.set_notifier(Box::new(move || {
            notified.fetch_add(1, Ordering::Relaxed);
        }));
    }
    fs::remove_file(&path).unwrap();
    assert!(library.load_art_rgba(&art_id, 96).is_none());
    wait_until("artwork regeneration", TIMEOUT, || path.is_file());
    wait_until("regeneration notification", TIMEOUT, || {
        notified.load(Ordering::Relaxed) > 0
    });
    assert!(library.load_art_rgba(&art_id, 96).is_some());
    // Unknown ids are harmless.
    assert!(library.load_art_rgba("ffffffffffffffff", 96).is_none());
    assert!(library.load_art_rgba("../../../etc/passwd", 96).is_none());

    // A cover image that later appears next to files with embedded pictures does not
    // replace their artwork, and does not cause them to be read again.
    let reads = library.debug_tag_reads();
    write_png(&music.join("album/cover.png"), 50, 50);
    rescan(&library);
    assert_eq!(
        by_title(&library, "Embedded FLAC").art_id.as_deref(),
        Some(art_id.as_str())
    );
    assert_eq!(
        by_title(&library, "Embedded MP3").art_id.as_deref(),
        Some(art_id.as_str())
    );
    assert_eq!(library.debug_tag_reads(), reads);
}

#[test]
fn remove_folder_only_touches_the_database() {
    let env = TestEnv::new();
    let music = env.music();
    for i in 0..5 {
        write_tagged_wav(
            &music.join(format!("t{i}.wav")),
            200 + i * 10,
            &format!("T{i}"),
            "A",
            "B",
        );
    }
    let loose_dir = env.dir.path().join("loose");
    write_tagged_wav(&loose_dir.join("single.wav"), 300, "Single", "A", "B");
    let (library, root) = env.open_scanned();
    let loose = library.import_files(&[loose_dir.join("single.wav")]);
    assert_eq!(loose.len(), 1);
    let first = by_title(&library, "T0");
    let playlist = library.create_playlist("p").unwrap();
    library
        .add_to_playlist(playlist, &[first.id, loose[0]])
        .unwrap();
    library.save_queue(&[(1, first.id), (2, loose[0])]);
    assert_eq!(library.snapshot().len(), 6);

    assert!(matches!(
        library.remove_folder(root + 100),
        Err(LibraryError::NotFound)
    ));
    library.remove_folder(root).unwrap();
    assert!(library.roots().is_empty());
    assert_eq!(titles(&library), ["Single"]);
    assert!(library.track(first.id).is_none());
    // References to the removed tracks are gone, others stay.
    let items = library.playlist_items(playlist);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].track_id, loose[0]);
    assert_eq!(library.load_queue(), [(2, loose[0])]);
    // Every file is still on disk.
    for i in 0..5 {
        assert!(music.join(format!("t{i}.wav")).is_file());
    }

    // Persisted, and the folder can be added again.
    drop(library);
    let library = env.open();
    assert!(library.roots().is_empty());
    assert_eq!(library.snapshot().len(), 1);
    library.add_folder(&music).unwrap();
    wait_scan(&library);
    assert_eq!(library.snapshot().len(), 6);
}

#[test]
fn removing_a_folder_cancels_its_scan() {
    let env = TestEnv::new();
    let music = env.music();
    for i in 0..400 {
        write_wav(&music.join(format!("d{}/t{i}.wav", i % 20)), 100, &[]);
    }
    let library = env.open();
    let root = library.add_folder(&music).unwrap();
    // Remove it while the scan is (most likely) still running.
    library.remove_folder(root).unwrap();
    wait_scan(&library);
    assert!(library.roots().is_empty());
    assert!(
        library.snapshot().is_empty(),
        "no track of a removed folder may remain"
    );
    drop(library);
    let library = env.open();
    assert!(library.snapshot().is_empty());
    assert!(library.roots().is_empty());
    assert_eq!(fs::read_dir(music.join("d0")).unwrap().count(), 20);
}

#[test]
fn shutdown_interrupts_a_scan_and_the_next_start_completes_it() {
    let env = TestEnv::new();
    let music = env.music();
    const FILES: usize = 600;
    for i in 0..FILES {
        write_wav(&music.join(format!("d{}/t{i}.wav", i % 20)), 100, &[]);
    }
    let library = env.open();
    library.add_folder(&music).unwrap();
    let started = std::time::Instant::now();
    library.shutdown();
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "shutdown must not wait for the whole scan"
    );
    assert!(!library.scan_status().running);
    let partial = library.snapshot().len();
    assert!(partial <= FILES);
    // After shutdown nothing scans any more, but reading still works.
    library.rescan();
    assert!(!library.scan_status().running);
    drop(library);

    // Whatever was committed is intact; the next start picks up the rest and reads
    // only the files that are not in the database yet.
    let library = env.open();
    assert!(library.snapshot().len() <= FILES);
    let already = library.snapshot().len() as u64;
    rescan(&library);
    assert_eq!(library.snapshot().len(), FILES);
    assert_eq!(library.debug_tag_reads(), FILES as u64 - already);
}

#[test]
fn folders_cannot_overlap() {
    let env = TestEnv::new();
    let music = env.music();
    fs::create_dir_all(music.join("sub/deeper")).unwrap();
    let library = env.open();
    library.add_folder(&music.join("sub")).unwrap();

    assert!(matches!(
        library.add_folder(&music.join("sub")),
        Err(LibraryError::AlreadyAdded)
    ));
    assert!(matches!(
        library.add_folder(&music.join("sub/")),
        Err(LibraryError::AlreadyAdded)
    ));
    match library.add_folder(&music.join("sub/deeper")) {
        Err(LibraryError::InsideExistingFolder(existing)) => {
            assert_eq!(existing, music.join("sub"))
        }
        other => panic!("unexpected: {other:?}"),
    }
    match library.add_folder(&music) {
        Err(LibraryError::ContainsExistingFolder(existing)) => {
            assert_eq!(existing, music.join("sub"));
        }
        other => panic!("unexpected: {other:?}"),
    }
    // Reaching an existing folder through a symbolic link is the same folder.
    let alias = env.dir.path().join("alias");
    symlink(music.join("sub"), &alias).unwrap();
    assert!(matches!(
        library.add_folder(&alias),
        Err(LibraryError::AlreadyAdded)
    ));

    assert!(matches!(
        library.add_folder(&env.dir.path().join("does-not-exist")),
        Err(LibraryError::NotADirectory)
    ));
    let file = env.dir.path().join("file.txt");
    fs::write(&file, b"x").unwrap();
    assert!(matches!(
        library.add_folder(&file),
        Err(LibraryError::NotADirectory)
    ));
    assert_eq!(library.roots().len(), 1);
    // Errors are displayable for the UI.
    assert!(!LibraryError::AlreadyAdded.to_string().is_empty());
}

#[test]
fn imported_files_are_reused_and_adopted_by_folders() {
    let env = TestEnv::new();
    let music = env.music();
    let a = music.join("a.wav");
    let b = music.join("sub/b.wav");
    write_tagged_wav(&a, 300, "A", "Artist", "Album");
    write_tagged_wav(&b, 400, "B", "Artist", "Album");
    let not_audio = music.join("readme.txt");
    fs::write(&not_audio, b"text").unwrap();
    let broken = music.join("broken.mp3");
    fs::write(&broken, noise(3000, 11)).unwrap();

    let library = env.open();
    let ids = library.import_files(&[
        b.clone(),
        not_audio.clone(),
        music.join("missing.wav"),
        broken.clone(),
        a.clone(),
    ]);
    assert_eq!(ids.len(), 2, "unreadable and unsupported files are skipped");
    let (id_b, id_a) = (ids[0], ids[1]);
    assert_eq!(library.track(id_b).unwrap().title, "B");
    assert_eq!(library.track(id_a).unwrap().title, "A");
    assert_eq!(library.track(id_a).unwrap().root_id, None);
    assert_eq!(library.snapshot().len(), 2);
    assert_eq!(library.debug_tag_reads(), 3); // a, b and the broken file

    // Importing again returns the same rows without reading the files.
    assert_eq!(library.import_files(&[a.clone(), b.clone()]), [id_a, id_b]);
    assert_eq!(library.debug_tag_reads(), 3);
    library.record_play(id_a);

    // Adding the folder that contains them adopts the existing rows.
    let root = library.add_folder(&music).unwrap();
    wait_scan(&library);
    assert_eq!(library.snapshot().len(), 2);
    let adopted = library.track(id_a).unwrap();
    assert_eq!(adopted.root_id, Some(root));
    assert_eq!(adopted.play_count, 1);
    assert_eq!(library.track(id_b).unwrap().root_id, Some(root));
    assert_eq!(library.roots()[0].track_count, 2);

    // remove_tracks is database-only.
    library.remove_tracks(&[id_a]);
    assert!(library.track(id_a).is_none());
    assert!(a.is_file());
    assert_eq!(library.snapshot().len(), 1);
}

#[test]
fn loose_files_are_rechecked_on_rescan() {
    let env = TestEnv::new();
    let dir = env.dir.path().join("downloads");
    let kept = dir.join("kept.wav");
    let gone = dir.join("gone.wav");
    let edited = dir.join("edited.wav");
    write_tagged_wav(&kept, 300, "Kept", "A", "B");
    write_tagged_wav(&gone, 320, "Gone", "A", "B");
    write_tagged_wav(&edited, 340, "Edited", "A", "B");
    let library = env.open();
    let ids = library.import_files(&[kept.clone(), gone.clone(), edited.clone()]);
    assert_eq!(ids.len(), 3);
    library.set_favorite(&[ids[0]], true);

    fs::remove_file(&kept).unwrap();
    fs::remove_file(&gone).unwrap();
    write_tagged_wav(&edited, 600, "Edited Again", "A", "B");
    rescan(&library);
    // Favourite: kept but unavailable. Unreferenced: removed. Changed: re-read.
    assert!(!library.track(ids[0]).unwrap().available);
    assert!(library.track(ids[1]).is_none());
    let edited_track = library.track(ids[2]).unwrap();
    assert_eq!(edited_track.title, "Edited Again");
    assert_eq!(edited_track.duration_ms, 600);
}

#[test]
fn watcher_picks_up_changes_and_folder_availability() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("first/initial.wav"), 300, "Initial", "A", "B");
    let (library, _) = env.open_scanned();
    library.start_watching();
    library.start_watching(); // idempotent
    // Give the event stream a moment to start before changing anything.
    std::thread::sleep(Duration::from_millis(1500));
    let reads = library.debug_tag_reads();

    // A new file in a new directory.
    write_tagged_wav(&music.join("new/fresh.wav"), 350, "Fresh", "A", "B");
    wait_until("new file to appear", TIMEOUT, || {
        library.snapshot().search("fresh").len() == 1
    });
    let fresh = by_title(&library, "Fresh");
    // Only the changed directory was scanned: nothing else was read.
    assert_eq!(library.debug_tag_reads(), reads + 1);

    // Rename: the row is relinked.
    fs::rename(music.join("new/fresh.wav"), music.join("new/moved.wav")).unwrap();
    wait_until("rename to be relinked", TIMEOUT, || {
        library
            .track(fresh.id)
            .is_some_and(|t| t.path == music.join("new/moved.wav"))
    });
    assert_eq!(library.snapshot().len(), 2);

    // Delete: the unreferenced row goes away.
    fs::remove_file(music.join("new/moved.wav")).unwrap();
    wait_until("deleted file to be removed", TIMEOUT, || {
        library.track(fresh.id).is_none()
    });
    assert_eq!(titles(&library), ["Initial"]);

    // The folder disappears and comes back: noticed by the periodic check.
    let away = env.dir.path().join("music-away");
    fs::rename(&music, &away).unwrap();
    wait_until("folder to go offline", TIMEOUT, || {
        !library.roots()[0].online && library.snapshot().tracks.iter().all(|t| !t.available)
    });
    assert_eq!(library.snapshot().len(), 1);
    fs::rename(&away, &music).unwrap();
    wait_until("folder to come back", TIMEOUT, || {
        library.roots()[0].online && library.snapshot().tracks.iter().all(|t| t.available)
    });

    // Watching continues after the folder returned.
    write_tagged_wav(&music.join("first/second.wav"), 360, "Second", "A", "B");
    wait_until("file added after reconnect", TIMEOUT, || {
        library.snapshot().search("second").len() == 1
    });
    library.shutdown();
}

#[test]
fn tracks_are_published_while_a_scan_is_still_running() {
    let env = TestEnv::new();
    let music = env.music();
    const FILES: usize = 1500;
    for i in 0..FILES {
        write_wav(&music.join(format!("d{}/t{i:04}.wav", i % 30)), 50, &[]);
    }
    let library = env.open();
    // Publish after every database batch instead of at most every 500 ms, so that the
    // intermediate snapshots are observable however fast the machine is.
    library.debug_set_publish_interval(Duration::ZERO);
    let sizes = Arc::new(std::sync::Mutex::new(Vec::<usize>::new()));
    {
        let sizes = Arc::clone(&sizes);
        let weak = Arc::downgrade(&library);
        library.set_notifier(Box::new(move || {
            if let Some(library) = weak.upgrade() {
                sizes.lock().unwrap().push(library.snapshot().len());
            }
        }));
    }
    library.add_folder(&music).unwrap();
    wait_scan(&library);
    assert_eq!(library.snapshot().len(), FILES);
    assert_eq!(library.scan_status().tracks_added, FILES as u64);

    let sizes = sizes.lock().unwrap().clone();
    let partial: Vec<usize> = sizes
        .iter()
        .copied()
        .filter(|&n| n > 0 && n < FILES)
        .collect();
    assert!(
        !partial.is_empty(),
        "tracks must become visible before the scan ends: {sizes:?}"
    );
    assert!(
        sizes.windows(2).all(|pair| pair[0] <= pair[1]),
        "snapshots only grow during an initial scan: {sizes:?}"
    );
    // A track from a partial snapshot is already fully usable.
    let first = library.snapshot().tracks[0].id;
    assert!(library.track(first).is_some_and(|t| t.available));
    library.shutdown();
}

#[test]
fn files_on_disk_are_never_modified() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("a.wav"), 300, "A", "Artist", "Album");
    write_tagged_wav(&music.join("b.wav"), 400, "B", "Artist", "Album");
    let snapshot_of = |dir: &Path| -> Vec<(std::path::PathBuf, Vec<u8>, std::time::SystemTime)> {
        let mut files: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .map(|p| {
                let modified = fs::metadata(&p).unwrap().modified().unwrap();
                (p.clone(), fs::read(&p).unwrap(), modified)
            })
            .collect();
        files.sort();
        files
    };
    let before = snapshot_of(&music);
    let (library, root) = env.open_scanned();
    let ids: Vec<i64> = library.snapshot().tracks.iter().map(|t| t.id).collect();
    library.set_favorite(&ids, true);
    library.record_play(ids[0]);
    rescan(&library);
    library.remove_tracks(&ids[..1]);
    library.remove_folder(root).unwrap();
    library.shutdown();
    assert_eq!(snapshot_of(&music), before);
}

// ---- regression tests for the data-safety review ------------------------------------

/// Restores directory permissions when dropped, so that a failing test still lets the
/// temporary directory be cleaned up.
struct RestorePermissions(Vec<std::path::PathBuf>);

impl Drop for RestorePermissions {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        for dir in &self.0 {
            let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o755));
        }
    }
}

#[test]
fn unreadable_directories_make_a_scan_incomplete_not_destructive() {
    use std::os::unix::fs::PermissionsExt;
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("open/free.wav"), 300, "Free", "A", "B");
    write_tagged_wav(&music.join("open/doomed.wav"), 310, "Doomed", "A", "B");
    write_tagged_wav(&music.join("locked/inside.wav"), 320, "Inside", "A", "B");
    // A directory outside the folder, reached through a symbolic link.
    let external = env.dir.path().join("external");
    write_tagged_wav(&external.join("album/linked.wav"), 330, "Linked", "A", "B");
    symlink(&external, music.join("link")).unwrap();

    let (library, _) = env.open_scanned();
    assert_eq!(titles(&library), ["Doomed", "Free", "Inside", "Linked"]);
    let first_scan = library.roots()[0].last_scan_at;
    assert!(first_scan.is_some());
    let ids: Vec<i64> = library.snapshot().tracks.iter().map(|t| t.id).collect();

    // Both directories become unreadable (as with a permission or privacy restriction).
    let locked = vec![music.join("locked"), external.clone()];
    let restore = RestorePermissions(locked.clone());
    for dir in &locked {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o000)).unwrap();
    }
    if fs::read_dir(&locked[0]).is_ok() {
        eprintln!("note: test skipped: this user can read directories without permission");
        return;
    }
    // Meanwhile one file elsewhere really is deleted and one is added.
    fs::remove_file(music.join("open/doomed.wav")).unwrap();
    write_tagged_wav(&music.join("open/new.wav"), 340, "New", "A", "B");
    std::thread::sleep(Duration::from_millis(1100)); // scan times have 1 s resolution
    rescan(&library);

    // Nothing was removed and nothing marked missing — not the tracks behind the
    // unreadable directories, and (because the scan could not see everything) not the
    // deleted file either. The new file was still taken in.
    assert_eq!(
        titles(&library),
        ["Doomed", "Free", "Inside", "Linked", "New"]
    );
    for id in &ids {
        assert!(
            library.track(*id).is_some_and(|t| t.available),
            "track {id}"
        );
    }
    assert_eq!(
        library.roots()[0].last_scan_at,
        first_scan,
        "an incomplete scan must not be recorded as a completed one"
    );
    // The same holds when the individually registered files are checked.
    rescan(&library);
    assert_eq!(library.snapshot().len(), 5);

    // With the permissions back, a scan completes and applies the normal policy.
    drop(restore);
    rescan(&library);
    assert_eq!(titles(&library), ["Free", "Inside", "Linked", "New"]);
    assert!(library.snapshot().tracks.iter().all(|t| t.available));
    assert!(library.roots()[0].last_scan_at > first_scan);
}

#[test]
fn unreadable_individually_added_file_is_not_treated_as_deleted() {
    use std::os::unix::fs::PermissionsExt;
    let env = TestEnv::new();
    let dir = env.dir.path().join("downloads");
    write_tagged_wav(&dir.join("private/kept.wav"), 300, "Kept", "A", "B");
    write_tagged_wav(&dir.join("gone.wav"), 310, "Gone", "A", "B");
    let library = env.open();
    let ids = library.import_files(&[dir.join("private/kept.wav"), dir.join("gone.wav")]);
    assert_eq!(ids.len(), 2);

    let locked = vec![dir.join("private")];
    let _restore = RestorePermissions(locked.clone());
    fs::set_permissions(&locked[0], fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read_dir(&locked[0]).is_ok() {
        eprintln!("note: test skipped: this user can read directories without permission");
        return;
    }
    fs::remove_file(dir.join("gone.wav")).unwrap();
    rescan(&library);
    // "Permission denied" is not "deleted": the track stays exactly as it was.
    assert!(library.track(ids[0]).is_some_and(|t| t.available));
    // "Not found" in a folder that still exists is.
    assert!(library.track(ids[1]).is_none());
}

#[test]
fn folder_replaced_by_unrelated_content_does_not_wipe_its_tracks() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("a/one.wav"), 300, "One", "A", "B");
    write_tagged_wav(&music.join("a/two.wav"), 310, "Two", "A", "B");
    write_tagged_wav(&music.join("b/three.wav"), 320, "Three", "A", "B");
    let (library, _) = env.open_scanned();
    let ids: Vec<i64> = library.snapshot().tracks.iter().map(|t| t.id).collect();
    assert_eq!(ids.len(), 3);
    let scanned_at = library.roots()[0].last_scan_at;

    // The folder's path now leads somewhere else: a non-empty directory with none of
    // the known files (a stale mount point with leftovers, a recreated folder).
    let real = env.dir.path().join("music-real");
    fs::rename(&music, &real).unwrap();
    fs::create_dir_all(music.join("other")).unwrap();
    fs::write(music.join(".DS_Store"), b"finder").unwrap();
    fs::write(
        music.join("readme.txt"),
        b"not the music you are looking for",
    )
    .unwrap();
    write_tagged_wav(&music.join("other/else.wav"), 400, "Else", "X", "Y");
    std::thread::sleep(Duration::from_millis(1100));
    rescan(&library);

    // None of the unreferenced tracks was removed; they are unavailable. What is in
    // the folder now is taken in.
    assert_eq!(titles(&library), ["Else", "One", "Three", "Two"]);
    for id in &ids {
        assert!(
            library.track(*id).is_some_and(|t| !t.available),
            "track {id}"
        );
    }
    assert!(by_title(&library, "Else").available);
    assert_eq!(
        library.roots()[0].last_scan_at,
        scanned_at,
        "a folder without any of its known files is not recorded as scanned"
    );
    // Still nothing is removed on the next scan (and the next start).
    rescan(&library);
    assert_eq!(library.snapshot().len(), 4);
    drop(library);
    let library = env.open();
    rescan(&library);
    assert_eq!(library.snapshot().len(), 4);

    // The original content returns: the same rows are available again, without being
    // read again, and the leftover of the impostor folder is handled normally.
    fs::remove_dir_all(&music).unwrap();
    fs::rename(&real, &music).unwrap();
    rescan(&library);
    assert_eq!(titles(&library), ["One", "Three", "Two"]);
    for id in &ids {
        assert!(
            library.track(*id).is_some_and(|t| t.available),
            "track {id}"
        );
    }
    assert_eq!(library.debug_tag_reads(), 0);
}

#[test]
fn deleting_every_file_of_a_watched_subfolder_still_removes_its_tracks() {
    // The guard against wiped folders must not get in the way of an ordinary deletion:
    // other tracks of the folder are still there, so the folder is alive.
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("keep/stay.wav"), 300, "Stay", "A", "B");
    write_tagged_wav(&music.join("album/one.wav"), 310, "One", "A", "B");
    write_tagged_wav(&music.join("album/two.wav"), 320, "Two", "A", "B");
    let (library, _) = env.open_scanned();
    fs::remove_dir_all(music.join("album")).unwrap();
    rescan(&library);
    assert_eq!(titles(&library), ["Stay"]);
}

#[test]
fn importing_another_spelling_of_a_known_file_reuses_its_row() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("Album/Song.wav"), 300, "Song", "A", "B");
    let (library, root) = env.open_scanned();
    let song = by_title(&library, "Song");
    assert_eq!(library.debug_tag_reads(), 1);

    // Through a symbolic link to the folder.
    let alias = env.dir.path().join("alias");
    symlink(music.join("Album"), &alias).unwrap();
    assert_eq!(library.import_files(&[alias.join("Song.wav")]), [song.id]);
    // Through the fully resolved path (differs from the folder's spelling wherever a
    // parent directory is a symbolic link, as the temporary directory is on macOS).
    let resolved = fs::canonicalize(music.join("Album/Song.wav")).unwrap();
    assert_eq!(library.import_files(&[resolved]), [song.id]);
    // Through a different letter case, where the volume ignores case.
    let shouting = music.join("ALBUM/SONG.WAV");
    if shouting.exists() {
        assert_eq!(library.import_files(&[shouting]), [song.id]);
    } else {
        eprintln!("note: letter-case part skipped: the volume is case-sensitive");
    }
    assert_eq!(
        library.snapshot().len(),
        1,
        "no second row for the same file"
    );
    assert_eq!(
        library.debug_tag_reads(),
        1,
        "and the file was not read again"
    );
    rescan(&library);
    assert_eq!(library.snapshot().len(), 1);
    assert!(library.track(song.id).unwrap().available);

    // A file that is new to the library, named through the alias: it is stored under
    // the folder's own spelling, so the scan recognises it instead of adding it again.
    write_tagged_wav(&music.join("Album/Second.wav"), 310, "Second", "A", "B");
    let ids = library.import_files(&[alias.join("Second.wav")]);
    assert_eq!(ids.len(), 1);
    let second = library.track(ids[0]).unwrap();
    assert_eq!(second.path, music.join("Album/Second.wav"));
    assert_eq!(second.root_id, Some(root));
    let reads = library.debug_tag_reads();
    rescan(&library);
    assert_eq!(library.snapshot().len(), 2);
    assert_eq!(library.debug_tag_reads(), reads);
    assert!(library.track(ids[0]).unwrap().available);

    // A file the scanner reached through a link inside the folder, imported by its
    // real location.
    let external = env.dir.path().join("external");
    write_tagged_wav(&external.join("far.wav"), 320, "Far", "A", "B");
    symlink(&external, music.join("ext")).unwrap();
    rescan(&library);
    let far = by_title(&library, "Far");
    assert_eq!(far.path, music.join("ext/far.wav"));
    assert_eq!(library.import_files(&[external.join("far.wav")]), [far.id]);
    assert_eq!(library.snapshot().len(), 3);
}

#[test]
fn file_in_a_hidden_directory_of_a_music_folder_stays_an_individual_file() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("visible.wav"), 300, "Visible", "A", "B");
    write_tagged_wav(&music.join(".stash/hidden.wav"), 310, "Hidden", "A", "B");
    let (library, _) = env.open_scanned();
    assert_eq!(titles(&library), ["Visible"]);

    // The scan skips hidden directories, so a file imported from one must not be tied
    // to the folder — the scan could never confirm it.
    let ids = library.import_files(&[music.join(".stash/hidden.wav")]);
    assert_eq!(ids.len(), 1);
    assert_eq!(library.track(ids[0]).unwrap().root_id, None);
    let playlist = library.create_playlist("p").unwrap();
    library.add_to_playlist(playlist, &ids).unwrap();
    rescan(&library);
    rescan(&library);
    let hidden = library.track(ids[0]).unwrap();
    assert!(hidden.available, "the entry must not grey out after a scan");
    assert_eq!(hidden.root_id, None);
    assert_eq!(library.snapshot().len(), 2);

    // It is still looked after: when the file goes, the playlist entry is kept as
    // unavailable.
    fs::remove_file(music.join(".stash/hidden.wav")).unwrap();
    rescan(&library);
    assert!(!library.track(ids[0]).unwrap().available);
}

#[test]
fn file_moved_between_music_folders_keeps_its_row() {
    let env = TestEnv::new();
    let first = env.music();
    let second = env.dir.path().join("second");
    write_tagged_wav(&first.join("a/song.wav"), 300, "Song", "Artist", "Album");
    write_tagged_wav(&first.join("a/stay.wav"), 310, "Stay", "Artist", "Album");
    write_tagged_wav(&second.join("keep.wav"), 320, "Keep", "Artist", "Album");
    write_tagged_wav(&second.join("plain.wav"), 330, "Plain", "Artist", "Album");
    let library = env.open();
    let root1 = library.add_folder(&first).unwrap();
    let root2 = library.add_folder(&second).unwrap();
    wait_scan(&library);
    assert_eq!(library.snapshot().len(), 4);

    let song = by_title(&library, "Song");
    let plain = by_title(&library, "Plain");
    library.set_favorite(&[song.id], true);
    library.record_play(song.id);
    library.record_play(plain.id);
    let playlist = library.create_playlist("mix").unwrap();
    library.add_to_playlist(playlist, &[song.id]).unwrap();

    // First folder -> second folder. The first folder is scanned first: the track is
    // found missing there (and kept, being a favourite) before the file turns up.
    fs::create_dir_all(second.join("moved")).unwrap();
    fs::rename(first.join("a/song.wav"), second.join("moved/song.wav")).unwrap();
    rescan(&library);
    let moved = library.track(song.id).expect("the row follows the file");
    assert_eq!(moved.path, second.join("moved/song.wav"));
    assert_eq!(moved.root_id, Some(root2));
    assert!(moved.available && moved.favorite);
    assert_eq!(moved.play_count, 1);
    assert_eq!(
        library.snapshot().len(),
        4,
        "no second row for the moved file"
    );
    assert_eq!(library.playlist_items(playlist)[0].track_id, song.id);

    // Second folder -> first folder. Now the file turns up (first folder) before it is
    // found missing (second folder).
    fs::create_dir_all(first.join("b")).unwrap();
    fs::rename(second.join("moved/song.wav"), first.join("b/song.wav")).unwrap();
    rescan(&library);
    let back = library
        .track(song.id)
        .expect("the row follows the file back");
    assert_eq!(back.path, first.join("b/song.wav"));
    assert_eq!(back.root_id, Some(root1));
    assert!(back.available && back.favorite);
    assert_eq!(library.snapshot().len(), 4);
    assert_eq!(library.playlist_items(playlist)[0].track_id, song.id);
    assert_eq!(library.playlist_items(playlist).len(), 1);

    // In that order even a track nothing refers to keeps its row and play count.
    fs::rename(second.join("plain.wav"), first.join("b/plain.wav")).unwrap();
    rescan(&library);
    let plain_moved = library
        .track(plain.id)
        .expect("unreferenced row follows too");
    assert_eq!(plain_moved.path, first.join("b/plain.wav"));
    assert_eq!(plain_moved.play_count, 1);
    assert_eq!(library.snapshot().len(), 4);
}

#[test]
fn file_missing_in_one_scan_and_found_in_a_later_one_is_relinked() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("a/song.wav"), 300, "Song", "Artist", "Album");
    write_tagged_wav(&music.join("a/stay.wav"), 310, "Stay", "Artist", "Album");
    let (library, _) = env.open_scanned();
    let song = by_title(&library, "Song");
    let playlist = library.create_playlist("mix").unwrap();
    library
        .add_to_playlist(playlist, &[song.id, song.id])
        .unwrap();
    library.save_queue(&[(1, song.id)]);

    // The file leaves the library's sight for a while …
    let limbo = env.dir.path().join("limbo");
    fs::create_dir_all(&limbo).unwrap();
    fs::rename(music.join("a/song.wav"), limbo.join("song.wav")).unwrap();
    rescan(&library);
    assert!(!library.track(song.id).unwrap().available);
    assert_eq!(library.snapshot().len(), 2);

    // … and comes back somewhere else, under another name, in a later scan (and after
    // a restart, so nothing but the database remembers the missing track).
    drop(library);
    let library = env.open();
    fs::create_dir_all(music.join("c")).unwrap();
    fs::rename(limbo.join("song.wav"), music.join("c/song renamed.wav")).unwrap();
    rescan(&library);
    let found = library
        .track(song.id)
        .expect("the unavailable row is reused");
    assert_eq!(found.path, music.join("c/song renamed.wav"));
    assert!(found.available);
    assert_eq!(library.snapshot().len(), 2);
    let items: Vec<i64> = library
        .playlist_items(playlist)
        .iter()
        .map(|item| item.track_id)
        .collect();
    assert_eq!(items, [song.id, song.id]);
    assert_eq!(library.load_queue(), [(1, song.id)]);
}

#[test]
fn folder_cover_changes_reach_unchanged_tracks() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("album/one.wav"), 300, "One", "Artist", "Album");
    write_tagged_wav(&music.join("album/two.wav"), 310, "Two", "Artist", "Album");
    write_tagged_wav(
        &music.join("other/else.wav"),
        320,
        "Else",
        "Artist",
        "Other",
    );
    let (library, _) = env.open_scanned();
    let one = by_title(&library, "One");
    assert_eq!(one.art_id, None);
    let art_of = |title: &str| by_title(&library, title).art_id.clone();

    // A cover image appears after the first scan.
    let cover = music.join("album/cover.png");
    write_png(&cover, 300, 300);
    rescan(&library);
    let first = art_of("One").expect("the new cover is picked up");
    assert_eq!(art_of("Two").as_deref(), Some(first.as_str()));
    assert_eq!(art_of("Else"), None);
    assert_eq!(
        library.debug_tag_reads(),
        3,
        "no audio file is read again for this"
    );
    assert_eq!(by_title(&library, "One").id, one.id);
    let image = library.load_art_rgba(&first, 600).unwrap();
    assert_eq!((image.width, image.height), (300, 300));
    let snapshot = library.snapshot();
    let album = &snapshot.albums()[snapshot.album_index(one.album_key).unwrap() as usize];
    assert_eq!(album.art_id.as_deref(), Some(first.as_str()));
    drop(snapshot);

    // Nothing changes while the cover stays the same.
    let generation = library.generation();
    rescan(&library);
    assert_eq!(library.generation(), generation);

    // The cover is replaced.
    write_png(&cover, 240, 160);
    rescan(&library);
    let second = art_of("One").expect("the replaced cover is picked up");
    assert_ne!(second, first);
    assert_eq!(art_of("Two").as_deref(), Some(second.as_str()));
    let image = library.load_art_rgba(&second, 600).unwrap();
    assert_eq!((image.width, image.height), (240, 160));

    // A replacement that is not an image means no artwork, not stale artwork.
    fs::write(&cover, noise(3_000, 4)).unwrap();
    rescan(&library);
    assert_eq!(art_of("One"), None);

    // A good cover again, then it is removed.
    write_png(&cover, 200, 200);
    rescan(&library);
    let third = art_of("One").expect("cover is back");
    fs::remove_file(&cover).unwrap();
    rescan(&library);
    assert_eq!(art_of("One"), None);
    assert_eq!(art_of("Two"), None);
    assert_eq!(library.debug_tag_reads(), 3);

    // Rows written before the source of the artwork was recorded (schema v1) are
    // recognised as folder artwork on the next scan and then follow changes too.
    write_png(&cover, 200, 200);
    rescan(&library);
    assert!(art_of("One").is_some());
    drop(library);
    {
        let db = rusqlite::Connection::open(env.paths().data_dir.join("library.db")).unwrap();
        let changed = db
            .execute("UPDATE tracks SET art_from_folder = 0", [])
            .unwrap();
        assert_eq!(changed, 3);
    }
    let library = env.open();
    rescan(&library);
    let before = by_title(&library, "One").art_id.clone().unwrap();
    let _ = third;
    write_png(&cover, 180, 120);
    rescan(&library);
    let after = by_title(&library, "One").art_id.clone().unwrap();
    assert_ne!(after, before);
    assert_eq!(library.debug_tag_reads(), 0);
}

#[test]
fn evicted_covers_are_regenerated_through_one_queue() {
    let env = TestEnv::new();
    let music = env.music();
    const ALBUMS: usize = 12;
    for i in 0..ALBUMS {
        let dir = music.join(format!("album {i}"));
        write_tagged_wav(
            &dir.join("track.wav"),
            300,
            &format!("T{i:02}"),
            "A",
            &format!("Album {i}"),
        );
        write_png(&dir.join("cover.png"), 40 + i as u32 * 4, 40);
    }
    let (library, _) = env.open_scanned();
    let ids: Vec<String> = library
        .snapshot()
        .tracks
        .iter()
        .map(|t| t.art_id.clone().expect("every album has a cover"))
        .collect();
    let distinct: std::collections::HashSet<&String> = ids.iter().collect();
    assert_eq!(distinct.len(), ALBUMS);
    let generation = library.art_generation();
    assert!(ids.iter().all(|id| library.load_art_rgba(id, 64).is_some()));
    assert_eq!(
        library.art_generation(),
        generation,
        "plain loads do not count"
    );

    // The whole cache is evicted; the UI asks for every cover, repeatedly.
    for id in &ids {
        fs::remove_file(library.art_path(id)).unwrap();
    }
    for _ in 0..3 {
        for id in &ids {
            assert!(library.load_art_rgba(id, 64).is_none());
        }
    }
    // Each cover is regenerated exactly once and announced through the counter.
    wait_until("covers to be regenerated", TIMEOUT, || {
        library.art_generation() >= generation + ALBUMS as u64
    });
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(library.art_generation(), generation + ALBUMS as u64);
    assert!(ids.iter().all(|id| library.load_art_rgba(id, 64).is_some()));

    // An id no track uses cannot be regenerated and does not count.
    assert!(library.load_art_rgba("00000000deadbeef", 64).is_none());
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(library.art_generation(), generation + ALBUMS as u64);
    library.shutdown();
}

#[test]
fn concurrent_favourites_and_plays_end_up_published() {
    let env = TestEnv::new();
    let music = env.music();
    for i in 0..4 {
        write_tagged_wav(
            &music.join(format!("t{i}.wav")),
            300 + i * 10,
            &format!("T{i}"),
            "A",
            "B",
        );
    }
    let (library, _) = env.open_scanned();
    let ids: Vec<i64> = library.snapshot().tracks.iter().map(|t| t.id).collect();
    std::thread::scope(|scope| {
        for worker in 0..4usize {
            let library = &library;
            let ids = &ids;
            scope.spawn(move || {
                for round in 0..150usize {
                    let id = ids[(worker + round) % ids.len()];
                    if worker % 2 == 0 {
                        library.set_favorite(&[id], (worker + round) % 3 == 0);
                    } else {
                        library.record_play(id);
                    }
                }
            });
        }
    });
    // Whatever the interleaving was, the published snapshot shows the final state.
    let snapshot = library.snapshot();
    let mut plays = 0;
    for id in &ids {
        let published = snapshot.get(*id).unwrap();
        let current = library.track(*id).unwrap();
        assert_eq!(published.favorite, current.favorite, "track {id}");
        assert_eq!(published.play_count, current.play_count, "track {id}");
        assert_eq!(
            published.last_played_at, current.last_played_at,
            "track {id}"
        );
        plays += current.play_count;
    }
    assert_eq!(plays, 300);
}

/// A link that leads up and out (to the folder above the music folder, to the home
/// folder, to the top of the disk) is not followed: the scan must not wander off over
/// everything the user has because an album folder contained such a link.
#[test]
fn links_that_lead_above_the_music_folder_are_not_followed() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("a/inside.wav"), 300, "Inside", "A", "B");
    // Next to the music folder, where only a link upwards could reach it.
    write_tagged_wav(
        &env.dir.path().join("elsewhere/private.wav"),
        300,
        "Private",
        "A",
        "B",
    );
    symlink(env.dir.path(), music.join("a/up")).unwrap();
    symlink("/", music.join("a/everything")).unwrap();
    // A link to a folder beside it is the ordinary case and still followed.
    let beside = env.dir.path().join("more-music");
    write_tagged_wav(&beside.join("linked.wav"), 300, "Linked", "A", "B");
    symlink(&beside, music.join("more")).unwrap();

    let (library, _) = env.open_scanned();
    assert_eq!(titles(&library), ["Inside", "Linked"]);
}

/// An MP3 as Korean Windows software of the 2000s tagged it: ID3v2.3 text frames that
/// say "Latin-1" and hold the system code page (CP949). The library shows what was meant.
#[cfg(target_os = "macos")]
#[test]
fn korean_tags_in_the_old_code_page_are_read_as_korean() {
    let env = TestEnv::new();
    let music = env.music();
    fs::create_dir_all(&music).unwrap();
    let frame = |id: &[u8; 4], text: &[u8]| {
        let mut frame = id.to_vec();
        frame.extend((text.len() as u32 + 1).to_be_bytes());
        frame.extend([0, 0]); // flags
        frame.push(0); // "ISO-8859-1"
        frame.extend(text);
        frame
    };
    // 밤편지 / 아이유 / 꽃갈피 둘 in CP949.
    let mut frames = frame(b"TIT2", b"\xb9\xe3\xc6\xed\xc1\xf6");
    frames.extend(frame(b"TPE1", b"\xbe\xc6\xc0\xcc\xc0\xaf"));
    frames.extend(frame(b"TALB", b"\xb2\xc9\xb0\xa5\xc7\xc7 \xb5\xd1"));
    // A Western title on the same disc stays what it is.
    let mut western = frame(b"TIT2", b"D\xe9j\xe0 Vu");
    western.extend(frame(b"TPE1", b"Beyonc\xe9"));
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gap_a.mp3");
    let audio = fs::read(fixture).unwrap();
    let own_tag = if audio.starts_with(b"ID3") {
        10 + audio[6..10]
            .iter()
            .fold(0usize, |n, b| (n << 7) | usize::from(*b))
    } else {
        0
    };
    for (name, frames) in [("old.mp3", &frames), ("western.mp3", &western)] {
        let size = frames.len() as u32;
        let syncsafe = [
            (size >> 21) as u8 & 0x7F,
            (size >> 14) as u8 & 0x7F,
            (size >> 7) as u8 & 0x7F,
            size as u8 & 0x7F,
        ];
        let mut file = b"ID3\x03\x00\x00".to_vec();
        file.extend(syncsafe);
        file.extend(frames);
        file.extend(&audio[own_tag..]);
        fs::write(music.join(name), file).unwrap();
    }

    let (library, _) = env.open_scanned();
    assert_eq!(titles(&library), ["Déjà Vu", "밤편지"]);
    let song = by_title(&library, "밤편지");
    assert_eq!(song.artist, "아이유");
    assert_eq!(song.album, "꽃갈피 둘");
    assert_eq!(by_title(&library, "Déjà Vu").artist, "Beyoncé");
    // Searchable the way Korean titles are, initial consonants included.
    let found = library.snapshot().search("ㅂㅍㅈ");
    assert_eq!(found.len(), 1);
}
