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
        library.set_favorite(&[id], true).unwrap();
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
    if has_encoder("libopus") {
        // Opus in WebM: the container is read, the codec is not played.
        ffmpeg_audio(
            &music.join("opus.webm"),
            &["-c:a", "libopus", "-ar", "48000"],
            &[("title", "Opus")],
            None,
        );
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
    library.set_favorite(&[a.id], true).unwrap();
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
    library.set_favorite(&[favourite.id], true).unwrap();

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
    assert!(library.save_session(
        Some(&[(1, first.id), (2, loose[0])][..]),
        "test_session",
        "{}"
    ));
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
    library.set_favorite(&[ids[0]], true).unwrap();

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
    library.set_favorite(&ids, true).unwrap();
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
    library.set_favorite(&[song.id], true).unwrap();
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
    assert!(library.save_session(Some(&[(1, song.id)][..]), "test_session", "{}"));

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
                        library
                            .set_favorite(&[id], (worker + round) % 3 == 0)
                            .unwrap();
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

/// The ID3v2 tag a DSD file carries, made with the tag library.
fn id3_tag(title: &str, artist: &str, album: &str) -> Vec<u8> {
    use lofty::config::WriteOptions;
    use lofty::id3::v2::Id3v2Tag;
    use lofty::tag::{Accessor, TagExt};
    let mut tag = Id3v2Tag::new();
    tag.set_title(title.into());
    tag.set_artist(artist.into());
    tag.set_album(album.into());
    tag.set_track(3);
    let mut bytes = Vec::new();
    tag.dump_to(&mut bytes, WriteOptions::default()).unwrap();
    bytes
}

/// Formats that play since DSD, Matroska and ADPCM were added: each is in the library
/// as what it is, with its tags, its length and (DSD) its own rate.
#[test]
fn dsd_matroska_and_adpcm_are_added_as_what_they_are() {
    use oneum::audio::dsd::encode::{Modulator, dff, dsf};
    let env = TestEnv::new();
    let music = env.music();
    // Two seconds of DSD64 and one of DSD128, stereo, with tags.
    let bits = |rate: u32, seconds: f64| {
        let samples = (f64::from(rate) * seconds) as u64;
        let mut m = Modulator::new();
        for n in 0..samples {
            m.push(0.4 * (2.0 * std::f64::consts::PI * 440.0 * n as f64 / f64::from(rate)).sin());
        }
        (m.finish(), samples)
    };
    let (b64, n64) = bits(2_822_400, 2.0);
    let tag = id3_tag("밤의 노래", "Pianist", "SACD Album");
    fs::write(
        music.join("one.dsf"),
        dsf(2_822_400, &[b64.clone(), b64.clone()], n64, Some(&tag)),
    )
    .unwrap();
    fs::write(
        music.join("two.dff"),
        dff(
            2_822_400,
            &[b64.clone(), b64],
            Some(&id3_tag("Two", "Pianist", "SACD Album")),
        ),
    )
    .unwrap();
    let (b128, n128) = bits(5_644_800, 1.0);
    fs::write(
        music.join("untagged.dsf"),
        dsf(5_644_800, &[b128.clone(), b128], n128, None),
    )
    .unwrap();
    let mut expected = vec!["Two", "untagged", "밤의 노래"];
    if ffmpeg_or_skip("dsd_matroska_and_adpcm_are_added_as_what_they_are (Matroska, ADPCM)") {
        // ffmpeg puts a Matroska file's title in the segment header, not in its tags,
        // so this one is known by its file name; artist and album are album-level tags.
        ffmpeg_audio(
            &music.join("In Matroska.mka"),
            &["-c:a", "flac"],
            &[
                ("title", "Segment title"),
                ("artist", "Band"),
                ("album", "Live"),
            ],
            None,
        );
        ffmpeg_audio(
            &music.join("ima.wav"),
            &["-c:a", "adpcm_ima_wav"],
            &[],
            None,
        );
        ffmpeg_audio(&music.join("ms.wav"), &["-c:a", "adpcm_ms"], &[], None);
        expected.extend(["In Matroska", "ima", "ms"]);
        if let Some(vorbis) = vorbis_args() {
            ffmpeg_audio(&music.join("In WebM.webm"), &vorbis, &[], None);
            expected.push("In WebM");
        }
    }
    let (library, _) = env.open_scanned();
    let mut want: Vec<String> = expected.iter().map(|t| t.to_string()).collect();
    want.sort();
    assert_eq!(titles(&library), want);
    assert_eq!(library.scan_status().unsupported, 0);

    let one = by_title(&library, "밤의 노래");
    assert_eq!(
        (one.codec.as_str(), one.container.as_str()),
        ("DSD64", "DSF")
    );
    assert_eq!(
        (one.artist.as_str(), one.album.as_str()),
        ("Pianist", "SACD Album")
    );
    assert_eq!(one.track_no, Some(3));
    assert_eq!(one.sample_rate, 2_822_400);
    assert_eq!(one.playback_rate(), 88_200);
    assert_eq!(one.bit_depth, Some(1));
    assert_eq!(one.channels, 2);
    assert!(
        (1_990..=2_000).contains(&one.duration_ms),
        "{}",
        one.duration_ms
    );
    assert!(one.is_lossless());
    let two = by_title(&library, "Two");
    assert_eq!(
        (two.codec.as_str(), two.container.as_str()),
        ("DSD64", "DSDIFF")
    );
    let fast = by_title(&library, "untagged");
    assert_eq!(
        (fast.codec.as_str(), fast.sample_rate),
        ("DSD128", 5_644_800)
    );
    assert_eq!(fast.playback_rate(), 88_200);
    assert!((990..=1_000).contains(&fast.duration_ms));

    if expected.contains(&"ima") {
        let mka = by_title(&library, "In Matroska");
        assert_eq!(
            (mka.codec.as_str(), mka.container.as_str()),
            ("FLAC", "Matroska")
        );
        assert_eq!((mka.artist.as_str(), mka.album.as_str()), ("Band", "Live"));
        assert_eq!(mka.album_artist, "Band");
        assert!(
            (990..=1_010).contains(&mka.duration_ms),
            "{}",
            mka.duration_ms
        );
        for name in ["ima", "ms"] {
            let wav = by_title(&library, name);
            assert_eq!(
                (wav.codec.as_str(), wav.container.as_str()),
                ("ADPCM", "WAV")
            );
            assert!(!wav.is_lossless());
        }
    }
    if expected.contains(&"In WebM") {
        let webm = by_title(&library, "In WebM");
        assert_eq!(
            (webm.codec.as_str(), webm.container.as_str()),
            ("Vorbis", "WebM")
        );
    }
}

#[test]
fn cue_sheet_tracks_take_the_place_of_their_audio_file() {
    let env = TestEnv::new();
    let music = env.music();
    let image = music.join("Live/image.wav");
    write_tagged_wav(&image, 6000, "Whole Concert", "Image Artist", "Image Album");
    write_tagged_wav(&music.join("other.wav"), 300, "Other", "Someone", "Else");
    let library = env.open();
    library.add_folder(&music).unwrap();
    wait_scan(&library);
    assert_eq!(titles(&library), ["Other", "Whole Concert"]);
    let reads = library.debug_tag_reads();

    // A sheet appears next to the file: its tracks replace the file.
    let sheet_path = music.join("Live/image.cue");
    let sheet = |first: &str| {
        format!(
            "PERFORMER \"시트 아티스트\"\nTITLE \"시트 앨범\"\nREM GENRE Jazz\nREM DATE 1999\n\
             REM REPLAYGAIN_ALBUM_GAIN -7.50 dB\nFILE \"image.wav\" WAVE\n\
             \x20 TRACK 01 AUDIO\n    TITLE \"{first}\"\n    INDEX 01 00:00:00\n\
             \x20 TRACK 02 AUDIO\n    TITLE \"둘째 곡\"\n    PERFORMER \"Guest\"\n\
             \x20   REM REPLAYGAIN_TRACK_GAIN -3.00 dB\n    INDEX 00 00:01:50\n    INDEX 01 00:02:00\n\
             \x20 TRACK 03 AUDIO\n    INDEX 01 00:04:30\n"
        )
    };
    fs::write(&sheet_path, sheet("첫 곡")).unwrap();
    rescan(&library);
    // A track without a title of its own is named by its number.
    assert_eq!(titles(&library), ["Other", "Track 03", "둘째 곡", "첫 곡"]);
    assert_eq!(
        library.debug_tag_reads(),
        reads + 1,
        "the file is read once"
    );
    let snapshot = library.snapshot();
    assert!(
        snapshot.tracks.iter().all(|t| t.path != image),
        "the file is not also there alone"
    );
    let second = by_title(&library, "둘째 곡");
    assert_eq!(second.path, sheet_path.join("02"));
    assert_eq!(
        oneum::library::cue::backing_file(&second.path),
        sheet_path.as_path()
    );
    assert_eq!(
        oneum::library::cue::real_file(&second.path).as_ref(),
        image.as_path()
    );
    assert_eq!(
        (second.artist.as_str(), second.album_artist.as_str()),
        ("Guest", "시트 아티스트")
    );
    assert_eq!(second.album, "시트 앨범");
    assert_eq!((second.track_no, second.year), (Some(2), Some(1999)));
    assert_eq!(second.genre, "Jazz");
    // INDEX 01 is where a track starts: 2.0 s to 4.4 s.
    assert_eq!(second.duration_ms, 2400);
    assert_eq!(second.replaygain.track_gain_db, Some(-3.0));
    assert_eq!(second.replaygain.album_gain_db, Some(-7.5));
    assert_eq!((second.codec.as_str(), second.sample_rate), ("PCM", 8000));
    let third = by_title(&library, "Track 03");
    assert_eq!(third.duration_ms, 1600, "the last track runs to the end");
    assert_eq!(third.artist, "시트 아티스트");
    assert_eq!(third.album_key, second.album_key, "one album");

    // Nothing changed: nothing is read again.
    let generation = library.generation();
    rescan(&library);
    assert_eq!(library.debug_tag_reads(), reads + 1);
    assert_eq!(library.generation(), generation);
    let status = library.scan_status();
    assert_eq!((status.files_found, status.files_done), (4, 4));

    // The sheet is edited: its tracks are read again and keep their identity.
    fs::write(&sheet_path, sheet("Opening")).unwrap();
    rescan(&library);
    assert_eq!(library.debug_tag_reads(), reads + 2);
    let opening = by_title(&library, "Opening");
    assert_eq!(opening.path, sheet_path.join("01"));
    assert_eq!(by_title(&library, "둘째 곡").id, second.id);
    assert_eq!(library.snapshot().len(), 4);

    // A second sheet for the same file adds nothing.
    fs::write(music.join("Live/zz.cue"), sheet("Again")).unwrap();
    rescan(&library);
    assert_eq!(library.snapshot().len(), 4);
    assert!(
        library.snapshot().tracks.iter().all(|t| t.title != "Again"),
        "the first sheet by name has the file"
    );
    fs::remove_file(music.join("Live/zz.cue")).unwrap();

    // A track taken out of the sheet goes; the one before it now runs to the end.
    let two_tracks = sheet("Opening");
    let two_tracks = &two_tracks[..two_tracks.find("  TRACK 03").unwrap()];
    fs::write(&sheet_path, two_tracks).unwrap();
    rescan(&library);
    assert_eq!(titles(&library), ["Opening", "Other", "둘째 곡"]);
    assert_eq!(by_title(&library, "둘째 곡").duration_ms, 4000);

    // The sheet goes: the file is there alone again.
    fs::remove_file(&sheet_path).unwrap();
    rescan(&library);
    assert_eq!(titles(&library), ["Other", "Whole Concert"]);
}

/// Trouble with a sheet or its file never costs tracks that are there: a sheet that
/// cannot be read keeps its tracks, and a file whose sheet's tracks could not be read
/// stays in the library as it was.
#[test]
fn cue_sheet_trouble_keeps_the_tracks_that_are_there() {
    use std::os::unix::fs::PermissionsExt;
    let set_mode = |path: &Path, mode: u32| {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    };
    let env = TestEnv::new();
    let music = env.music();
    let image = music.join("Live/image.wav");
    write_tagged_wav(&image, 3000, "Whole Concert", "Image Artist", "Image Album");
    let library = env.open();
    library.add_folder(&music).unwrap();
    wait_scan(&library);
    let whole = by_title(&library, "Whole Concert");
    let sheet_path = music.join("Live/image.cue");
    let sheet = "FILE \"image.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"One\"\n    INDEX 01 00:00:00\n\
                 \x20 TRACK 02 AUDIO\n    TITLE \"Two\"\n    INDEX 01 00:01:00\n";

    // The file cannot be read when its sheet appears: no tracks come of the sheet, and
    // the file stays (it is not taken as replaced).
    set_mode(&image, 0o000);
    fs::write(&sheet_path, sheet).unwrap();
    rescan(&library);
    set_mode(&image, 0o644);
    assert_eq!(titles(&library), ["Whole Concert"]);
    assert_eq!(by_title(&library, "Whole Concert").id, whole.id);
    // Readable again: now the sheet's tracks take its place.
    rescan(&library);
    assert_eq!(titles(&library), ["One", "Two"]);
    let one = by_title(&library, "One");

    // The sheet cannot be read for a while: its tracks are kept as they are, and its
    // file is not taken in alone meanwhile (known by the size the tracks recorded).
    set_mode(&sheet_path, 0o000);
    let unreadable = fs::read(&sheet_path).is_err();
    rescan(&library);
    set_mode(&sheet_path, 0o644);
    if unreadable {
        assert_eq!(titles(&library), ["One", "Two"], "not the file alone");
        assert_eq!(by_title(&library, "One").id, one.id);
    }
    // Readable again: the same rows.
    rescan(&library);
    assert_eq!(titles(&library), ["One", "Two"]);
    assert_eq!(by_title(&library, "One").id, one.id);

    // A sheet made unusable (not a sheet any more) is the sheet gone.
    fs::write(&sheet_path, "not a sheet").unwrap();
    rescan(&library);
    assert_eq!(titles(&library), ["Whole Concert"]);

    // The only album of the music folder: deleting its sheet also brings the file back
    // alone (the file, found by its size, shows that the folder is the one it was).
    fs::write(&sheet_path, sheet).unwrap();
    rescan(&library);
    assert_eq!(titles(&library), ["One", "Two"]);
    fs::remove_file(&sheet_path).unwrap();
    rescan(&library);
    assert_eq!(titles(&library), ["Whole Concert"]);
    assert!(library.snapshot().tracks.iter().all(|t| t.available));
}

/// A sheet written on another system may spell its file's name in other letter case
/// (or another Unicode form) than the folder does. Its tracks are taken in, and the file
/// is not taken in alone beside them.
#[test]
fn a_sheet_that_spells_its_file_otherwise_still_covers_it() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(
        &music.join("Live/Image.wav"),
        3000,
        "Whole Concert",
        "Band",
        "Live",
    );
    fs::write(
        music.join("Live/image.cue"),
        "FILE \"IMAGE.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"One\"\n    INDEX 01 00:00:00\n\
         \x20 TRACK 02 AUDIO\n    TITLE \"Two\"\n    INDEX 01 00:01:00\n",
    )
    .unwrap();
    let (library, _) = env.open_scanned();
    assert_eq!(titles(&library), ["One", "Two"]);
    rescan(&library);
    assert_eq!(titles(&library), ["One", "Two"]);
}

/// A sheet may say tracks start beyond the end of its file (a sheet for a longer rip):
/// those are no tracks, and the one before them ends with the file (its length, what
/// plays of it, and the file check agree). The sheet is not read again for that at
/// every scan, nor for a track the user removed from the library, which stays out as
/// a file's does.
#[test]
fn a_sheet_track_past_the_end_is_not_read_again_and_again() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("Live/image.wav"), 3000, "Whole", "Band", "Live");
    fs::write(
        music.join("Live/image.cue"),
        "FILE \"image.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"One\"\n    INDEX 01 00:00:00\n\
         \x20 TRACK 02 AUDIO\n    TITLE \"Two\"\n    INDEX 01 00:01:00\n\
         \x20 TRACK 03 AUDIO\n    TITLE \"Beyond\"\n    INDEX 01 00:10:00\n\
         \x20 TRACK 04 AUDIO\n    TITLE \"Further\"\n    INDEX 01 00:20:00\n",
    )
    .unwrap();
    let (library, _) = env.open_scanned();
    assert_eq!(titles(&library), ["One", "Two"]);
    assert_eq!(
        by_title(&library, "Two").duration_ms,
        2000,
        "to the end of the file"
    );
    assert!(library.check_files());
    assert!(library.wait_for_checks(Duration::from_secs(60)));
    for title in ["One", "Two"] {
        let check = by_title(&library, title).check.expect("checked");
        assert_ne!(
            check.verdict,
            oneum::model::CheckVerdict::Damaged,
            "{title}"
        );
    }
    let reads = library.debug_tag_reads();
    rescan(&library);
    rescan(&library);
    assert_eq!(library.debug_tag_reads(), reads, "not read again");

    library.remove_tracks(&[by_title(&library, "Two").id]);
    rescan(&library);
    rescan(&library);
    assert_eq!(titles(&library), ["One"], "what was removed stays out");
    assert_eq!(
        library.debug_tag_reads(),
        reads,
        "and the sheet is not read for it"
    );
}

/// A song removed from the library is not taken in again by the next scan of its
/// folder (which runs at every start): the file is still there, but it is remembered
/// as removed, also by the next run. (It used to be back as a new song.) Opening the
/// file by hand takes it in again.
#[test]
fn a_removed_song_stays_out_until_its_file_is_opened_again() {
    let env = TestEnv::new();
    let music = env.music();
    // Two files with the same tags: removing one must not depend on the other.
    write_tagged_wav(&music.join("a/song.wav"), 300, "Same", "Artist", "Album");
    write_tagged_wav(&music.join("b/song.wav"), 320, "Same", "Artist", "Album");
    write_tagged_wav(&music.join("a/other.wav"), 340, "Other", "Artist", "Album");
    let removed_file = music.join("a/song.wav");
    {
        let (library, _) = env.open_scanned();
        assert_eq!(titles(&library), ["Other", "Same", "Same"]);
        let reads = library.debug_tag_reads();
        let id = library
            .snapshot()
            .tracks
            .iter()
            .find(|t| t.path == removed_file)
            .unwrap()
            .id;
        library.remove_tracks(&[id]);
        assert!(removed_file.is_file(), "the file is not touched");
        assert_eq!(titles(&library), ["Other", "Same"]);

        rescan(&library);
        rescan(&library);
        assert_eq!(titles(&library), ["Other", "Same"], "a scan leaves it out");
        assert_eq!(library.debug_tag_reads(), reads, "without reading it again");
        library.shutdown();
    }

    // The next run scans the folder as every start does: still out.
    let library = env.open();
    rescan(&library);
    assert_eq!(titles(&library), ["Other", "Same"]);
    assert!(
        library
            .snapshot()
            .tracks
            .iter()
            .all(|t| t.path != removed_file),
        "the one that was removed, not its namesake"
    );

    // Opened by hand, it is in again, and stays in.
    assert_eq!(
        library
            .import_files(std::slice::from_ref(&removed_file))
            .len(),
        1
    );
    assert_eq!(titles(&library), ["Other", "Same", "Same"]);
    rescan(&library);
    assert_eq!(titles(&library), ["Other", "Same", "Same"]);
    library.shutdown();
    let library = env.open();
    rescan(&library);
    assert_eq!(titles(&library), ["Other", "Same", "Same"]);
}

/// An iPhone app's container moves when the app is updated: what was removed from the
/// library is still removed in the new place (it is known by its path, which moves
/// with the tracks'), and can still be brought back.
#[test]
fn removed_songs_stay_out_when_the_apps_container_moves() {
    use oneum::library::LibraryPaths;
    let dir = tempfile::tempdir().unwrap();
    let paths = |home: &Path| LibraryPaths {
        data_dir: home.join("Library/data"),
        cache_dir: home.join("Library/cache"),
    };
    let before = dir.path().join("Application/A");
    let music = before.join("Documents");
    write_tagged_wav(&music.join("Album/one.wav"), 1000, "One", "Band", "Album");
    write_tagged_wav(&music.join("Album/two.wav"), 1100, "Two", "Band", "Album");
    write_tagged_wav(&music.join("Live/image.wav"), 3000, "Whole", "Band", "Live");
    fs::write(
        music.join("Live/image.cue"),
        "FILE \"image.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"First\"\n    INDEX 01 00:00:00\n\
         \x20 TRACK 02 AUDIO\n    TITLE \"Second\"\n    INDEX 01 00:01:00\n",
    )
    .unwrap();
    {
        let (library, _) = Library::open_in_home(paths(&before), &before).unwrap();
        library.add_folder(&music).unwrap();
        wait_scan(&library);
        assert_eq!(titles(&library), ["First", "One", "Second", "Two"]);
        library.remove_tracks(&[
            by_title(&library, "Two").id,
            by_title(&library, "Second").id,
        ]);
        rescan(&library);
        assert_eq!(titles(&library), ["First", "One"]);
        library.shutdown();
    }

    let after = dir.path().join("Application/B");
    fs::rename(&before, &after).unwrap();
    let (library, _) = Library::open_in_home(paths(&after), &after).unwrap();
    assert_eq!(library.removed_count(), 2);
    rescan(&library);
    rescan(&library);
    assert_eq!(
        titles(&library),
        ["First", "One"],
        "still out in the new place"
    );
    assert_eq!(library.removed_count(), 2, "and still known as removed");

    library.restore_removed();
    wait_scan(&library);
    assert_eq!(titles(&library), ["First", "One", "Second", "Two"]);
    assert_eq!(library.removed_count(), 0);
}

/// "Bring Back Removed Songs": everything that was removed is in the library again
/// with one scan, and nothing is remembered as removed any more.
#[test]
fn removed_songs_can_all_be_brought_back() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("one.wav"), 300, "One", "Artist", "Album");
    write_tagged_wav(&music.join("two.wav"), 320, "Two", "Artist", "Album");
    write_tagged_wav(&music.join("three.wav"), 340, "Three", "Artist", "Album");
    let (library, _) = env.open_scanned();
    assert_eq!(library.removed_count(), 0);
    library.remove_tracks(&[by_title(&library, "One").id, by_title(&library, "Two").id]);
    rescan(&library);
    assert_eq!(titles(&library), ["Three"]);
    assert_eq!(library.removed_count(), 2);
    library.shutdown();

    let library = env.open();
    assert_eq!(library.removed_count(), 2, "the next run still knows");
    library.restore_removed();
    wait_scan(&library);
    assert_eq!(titles(&library), ["One", "Three", "Two"]);
    assert_eq!(library.removed_count(), 0);
}

/// What was removed is forgotten with its folder (added again, the folder is taken in
/// whole), and when the file itself is gone: a file that turns up under that name
/// later is a new one.
#[test]
fn removed_songs_are_forgotten_with_their_folder_or_their_file() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("one.wav"), 300, "One", "Artist", "Album");
    write_tagged_wav(&music.join("two.wav"), 320, "Two", "Artist", "Album");
    let (library, _) = env.open_scanned();
    library.remove_tracks(&[by_title(&library, "Two").id]);
    rescan(&library);
    assert_eq!(titles(&library), ["One"]);

    // The file goes, a full scan sees that, and another file comes under its name.
    fs::remove_file(music.join("two.wav")).unwrap();
    rescan(&library);
    write_tagged_wav(&music.join("two.wav"), 500, "Two Again", "Artist", "Album");
    rescan(&library);
    assert_eq!(titles(&library), ["One", "Two Again"]);

    // Removed, and then the folder is removed and added again: all of it is there.
    library.remove_tracks(&[by_title(&library, "One").id]);
    rescan(&library);
    assert_eq!(titles(&library), ["Two Again"]);
    let root = library.roots()[0].id;
    library.remove_folder(root).unwrap();
    library.add_folder(&music).unwrap();
    wait_scan(&library);
    assert_eq!(titles(&library), ["One", "Two Again"]);
}

/// Every track of a CUE sheet removed: the sheet's file does not come back in their
/// place as one long song.
#[test]
fn removing_every_track_of_a_sheet_does_not_bring_its_file_back_whole() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("Live/image.wav"), 3000, "Whole", "Band", "Live");
    fs::write(
        music.join("Live/image.cue"),
        "FILE \"image.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"One\"\n    INDEX 01 00:00:00\n\
         \x20 TRACK 02 AUDIO\n    TITLE \"Two\"\n    INDEX 01 00:01:00\n",
    )
    .unwrap();
    write_tagged_wav(
        &music.join("Live/single.wav"),
        300,
        "Single",
        "Band",
        "Live",
    );
    {
        let (library, _) = env.open_scanned();
        assert_eq!(titles(&library), ["One", "Single", "Two"]);
        let ids: Vec<_> = ["One", "Two"]
            .iter()
            .map(|title| by_title(&library, title).id)
            .collect();
        library.remove_tracks(&ids);
        rescan(&library);
        assert_eq!(titles(&library), ["Single"]);
        library.shutdown();
    }
    let library = env.open();
    rescan(&library);
    assert_eq!(titles(&library), ["Single"], "after a restart too");
}

/// A sheet none of whose tracks lie within its file (another disc's sheet under this
/// disc's name) is no sheet for it: the file is there alone, as without the sheet, and
/// stays so without being read again. Once the file is one the sheet fits, its tracks
/// take its place.
#[test]
fn a_sheet_with_no_track_within_its_file_leaves_the_file_alone() {
    let env = TestEnv::new();
    let music = env.music();
    let image = music.join("Live/image.wav");
    write_tagged_wav(&image, 3000, "Whole", "Band", "Live");
    fs::write(
        music.join("Live/image.cue"),
        "FILE \"image.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"One\"\n    INDEX 01 00:05:00\n\
         \x20 TRACK 02 AUDIO\n    TITLE \"Two\"\n    INDEX 01 00:06:00\n",
    )
    .unwrap();
    let (library, _) = env.open_scanned();
    assert_eq!(titles(&library), ["Whole"]);
    let whole = by_title(&library, "Whole");
    let reads = library.debug_tag_reads();
    rescan(&library);
    rescan(&library);
    assert_eq!(titles(&library), ["Whole"]);
    assert_eq!(by_title(&library, "Whole").id, whole.id);
    assert_eq!(library.debug_tag_reads(), reads, "not read again");

    // The whole concert after all: the sheet fits now.
    write_tagged_wav(&image, 8000, "Whole", "Band", "Live");
    rescan(&library);
    assert_eq!(titles(&library), ["One", "Two"]);
}

/// The file of a sheet becomes shorter (a re-rip of less): its tracks that now start
/// beyond its end leave the library, and once none fits, the file is there alone, with
/// none of the old tracks beside it. Nothing is read again at the scans after that.
#[test]
fn sheet_tracks_past_a_shorter_file_leave() {
    let env = TestEnv::new();
    let music = env.music();
    let image = music.join("Live/image.wav");
    write_tagged_wav(&image, 3000, "Whole", "Band", "Live");
    fs::write(
        music.join("Live/image.cue"),
        "FILE \"image.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"One\"\n    INDEX 01 00:01:00\n\
         \x20 TRACK 02 AUDIO\n    TITLE \"Two\"\n    INDEX 01 00:02:00\n",
    )
    .unwrap();
    let (library, _) = env.open_scanned();
    assert_eq!(titles(&library), ["One", "Two"]);
    let one = by_title(&library, "One");

    write_tagged_wav(&image, 1500, "Whole", "Band", "Live");
    rescan(&library);
    assert_eq!(titles(&library), ["One"], "Two starts beyond the end now");
    assert_eq!(by_title(&library, "One").id, one.id);
    assert_eq!(by_title(&library, "One").duration_ms, 500);
    let reads = library.debug_tag_reads();
    rescan(&library);
    assert_eq!(library.debug_tag_reads(), reads, "not read again");

    write_tagged_wav(&image, 800, "Whole", "Band", "Live");
    rescan(&library);
    assert_eq!(
        titles(&library),
        ["Whole"],
        "the file alone, no old track beside it"
    );
    let reads = library.debug_tag_reads();
    rescan(&library);
    assert_eq!(titles(&library), ["Whole"]);
    assert_eq!(library.debug_tag_reads(), reads, "not read again");
}

/// A track that a shorter re-rip leaves out, but that the library keeps (a favourite),
/// stays as unavailable: it is not brought back as playable, and does not have its
/// sheet read again at every scan. A longer rip brings it back.
#[test]
fn a_kept_track_past_a_shorter_file_is_left_alone() {
    let env = TestEnv::new();
    let music = env.music();
    let image = music.join("Live/image.wav");
    write_tagged_wav(&image, 3000, "Whole", "Band", "Live");
    fs::write(
        music.join("Live/image.cue"),
        "FILE \"image.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"One\"\n    INDEX 01 00:01:00\n\
         \x20 TRACK 02 AUDIO\n    TITLE \"Two\"\n    INDEX 01 00:02:00\n",
    )
    .unwrap();
    let (library, _) = env.open_scanned();
    let two = by_title(&library, "Two");
    library.set_favorite(&[two.id], true).unwrap();

    write_tagged_wav(&image, 1500, "Whole", "Band", "Live");
    rescan(&library);
    assert_eq!(titles(&library), ["One", "Two"], "kept, as a favourite");
    assert!(!by_title(&library, "Two").available);
    let reads = library.debug_tag_reads();
    rescan(&library);
    rescan(&library);
    assert_eq!(library.debug_tag_reads(), reads, "not read again");
    assert!(!by_title(&library, "Two").available, "not brought back");

    write_tagged_wav(&image, 3000, "Whole", "Band", "Live");
    rescan(&library);
    let back = by_title(&library, "Two");
    assert_eq!(back.id, two.id);
    assert!(back.available && back.favorite);
}

/// A folder none of whose known files were found is in doubt (another drive at its
/// path?): its tracks are kept as unavailable. That doubt is about the tracks from
/// before. A file added since, and deleted later, leaves the library like any other,
/// while other files added since are still there.
#[test]
fn a_file_added_since_a_folder_came_into_doubt_can_go() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("old.wav"), 1000, "Old", "Band", "Then");
    let (library, _) = env.open_scanned();
    fs::remove_file(music.join("old.wav")).unwrap();
    write_tagged_wav(&music.join("new1.wav"), 1000, "New One", "Band", "Now");
    write_tagged_wav(&music.join("new2.wav"), 1200, "New Two", "Band", "Now");
    rescan(&library);
    assert_eq!(titles(&library), ["New One", "New Two", "Old"]);
    assert!(!by_title(&library, "Old").available, "kept, in doubt");

    fs::remove_file(music.join("new1.wav")).unwrap();
    rescan(&library);
    assert_eq!(titles(&library), ["New Two", "Old"], "New One is gone");
    assert!(!by_title(&library, "Old").available, "still kept");
    assert!(by_title(&library, "New Two").available);
}

/// What the randomized test below knows of one album folder.
#[derive(Clone, Debug)]
struct ModelAlbum {
    /// Length of `image.wav`, if it is there.
    audio_ms: Option<u64>,
    /// `image.cue` and `zz.cue`, in the order the scan meets them.
    sheets: [ModelSheet; 2],
}

#[derive(Clone, Debug, PartialEq)]
enum ModelSheet {
    None,
    /// A `.cue` that is not a sheet.
    Garbage,
    /// A sheet for a file that is not there.
    OtherFile,
    /// The letter its titles begin with, and `(number, start in seconds)`.
    Tracks(char, Vec<(u32, u64)>),
}

impl ModelAlbum {
    /// The titles the album's folder gives, with their lengths. The first sheet that is
    /// for the file has it; with no track of that sheet within the file, or no such
    /// sheet, the file is there alone.
    /// Each with the path the library knows it by (`dir` is the album's folder): the
    /// file itself, or the sheet's path with the track's number.
    fn expected_at(&self, album: usize, dir: &Path) -> Vec<(std::path::PathBuf, String, u64)> {
        let Some(total) = self.audio_ms else {
            return Vec::new();
        };
        let alone = vec![(dir.join("image.wav"), format!("W{album}"), total)];
        let claiming = self
            .sheets
            .iter()
            .zip(["image.cue", "zz.cue"])
            .find_map(|(sheet, name)| match sheet {
                ModelSheet::Tracks(letter, tracks) => Some((letter, tracks, name)),
                _ => None,
            });
        let Some((letter, tracks, name)) = claiming else {
            return alone;
        };
        let fitting: Vec<(std::path::PathBuf, String, u64)> = tracks
            .iter()
            .enumerate()
            .filter(|(_, (_, start))| start * 1000 < total)
            .map(|(i, (number, start))| {
                let end = tracks
                    .get(i + 1)
                    .map_or(total, |(_, next)| (next * 1000).min(total));
                (
                    dir.join(name).join(format!("{number:02}")),
                    format!("{letter}{album}-{number}"),
                    end - start * 1000,
                )
            })
            .collect();
        if fitting.is_empty() { alone } else { fitting }
    }
}

fn write_model_sheet(path: &Path, album: usize, sheet: &ModelSheet, spelling: usize) {
    match sheet {
        ModelSheet::None => {
            let _ = fs::remove_file(path);
        }
        ModelSheet::Garbage => fs::write(path, "not a sheet").unwrap(),
        ModelSheet::OtherFile => fs::write(
            path,
            "FILE \"elsewhere.wav\" WAVE\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n",
        )
        .unwrap(),
        ModelSheet::Tracks(letter, tracks) => {
            // The file's name as written elsewhere: other letter case, or the name it
            // had before it was converted.
            let name = ["image.wav", "IMAGE.WAV", "Image.Wav", "image.flac"][spelling % 4];
            let mut text = format!("FILE \"{name}\" WAVE\n");
            for (number, start) in tracks {
                text.push_str(&format!(
                    "TRACK {number:02} AUDIO\nTITLE \"{letter}{album}-{number}\"\nINDEX 01 00:{start:02}:00\n"
                ));
            }
            fs::write(path, text).unwrap();
        }
    }
}

/// The scan of CUE sheets and their files, driven through random changes: files that
/// get longer, shorter or go; sheets that change, break, move or go, one or two for a
/// file; tracks made favourites, put in a playlist, removed from the library; scans
/// that cannot finish (a folder that cannot be read). After every scan that finished,
/// the library has to be exactly what the folders say: the tracks that are there, each
/// once, with the right lengths; nothing else available; nothing kept that nobody
/// refers to. Except what was removed from the library: that stays out, until its
/// file is opened by hand or is gone. And a scan right after it has to find nothing
/// to read.
#[test]
fn random_changes_to_sheets_and_files_leave_the_library_right() {
    // More seeds for a longer search: ONEUM_MODEL_SEEDS=2000 cargo test … random_changes
    let seeds = std::env::var("ONEUM_MODEL_SEEDS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(24u64);
    for seed in 1..=seeds {
        sheet_model_run(seed, 90);
    }
}

fn sheet_model_run(seed: u64, steps: usize) {
    use std::os::unix::fs::PermissionsExt;
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut roll = move |n: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) % n
    };
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("anchor.wav"), 1000, "Anchor", "Band", "Anchor");
    // A folder that can be made unreadable: a scan that meets it concludes nothing.
    let locked_dir = music.join("locked");
    fs::create_dir_all(&locked_dir).unwrap();
    let lock_dir = |on: bool| {
        let mode = if on { 0o000 } else { 0o755 };
        fs::set_permissions(&locked_dir, fs::Permissions::from_mode(mode)).unwrap();
    };
    let (library, _) = env.open_scanned();
    let playlist = library.create_playlist("Mix").unwrap();
    let mut albums = vec![
        ModelAlbum {
            audio_ms: None,
            sheets: [ModelSheet::None, ModelSheet::None],
        };
        2
    ];
    let mut locked = false;
    // Albums whose first sheet cannot be read for now: nothing of them changes then.
    let mut unreadable = [false; 2];
    // Whether the scan before this step could finish (the library is what it should be).
    let mut settled = true;
    // Whether only a scan of the whole music folder can bring the library up to date
    // (something outside the folder scanned last is still open).
    let mut whole_due = false;
    // What was removed from the library, by the path it was known by: scans leave it
    // out. It is forgotten when its file is opened, or (after a scan of everything
    // that finished) when its file, for a sheet's track the sheet, is not there.
    let mut removed: std::collections::HashSet<std::path::PathBuf> = Default::default();
    let forget_what_is_gone = |removed: &mut std::collections::HashSet<std::path::PathBuf>| {
        removed.retain(|path| {
            let is_sheet_track = path
                .parent()
                .is_some_and(|sheet| sheet.extension().is_some_and(|ext| ext == "cue"));
            let file = if is_sheet_track {
                path.parent().unwrap()
            } else {
                path
            };
            file.exists()
        });
    };
    let mut log: Vec<String> = Vec::new();
    for step in 0..steps {
        let a = roll(2) as usize;
        let dir = music.join(format!("A{a}"));
        fs::create_dir_all(&dir).unwrap();
        let image = dir.join("image.wav");
        let sheet_paths = [dir.join("image.cue"), dir.join("zz.cue")];
        let op = roll(16);
        if unreadable[a] && op <= 7 {
            continue;
        }
        // Changes to an album's files are followed by a scan of its folder alone, as the
        // folder watcher asks for, or by one of the whole music folder; everything else
        // by one of the whole music folder.
        let folder_only = matches!(op, 0..=7 | 14) && roll(2) == 0;
        match op {
            0..=2 => {
                let length = [0, 800, 1500, 3000, 5000, 8000][roll(6) as usize];
                if length == 0 {
                    let _ = fs::remove_file(&image);
                    albums[a].audio_ms = None;
                } else {
                    write_tagged_wav(&image, length as u32, &format!("W{a}"), "Band", "Live");
                    albums[a].audio_ms = Some(length);
                }
                log.push(format!("{step}: A{a} audio {length} ms"));
            }
            3..=6 => {
                // Mostly the first sheet; sometimes a second one for the same file.
                let which = usize::from(roll(4) == 0);
                let sheet = match roll(6) {
                    0 => ModelSheet::None,
                    1 => ModelSheet::Garbage,
                    2 => ModelSheet::OtherFile,
                    _ => {
                        let mut tracks = Vec::new();
                        let mut start = roll(3);
                        for number in 1..=1 + roll(4) as u32 {
                            tracks.push((number, start));
                            start += 1 + roll(3);
                        }
                        ModelSheet::Tracks(['T', 'Z'][which], tracks)
                    }
                };
                write_model_sheet(&sheet_paths[which], a, &sheet, roll(6) as usize);
                log.push(format!("{step}: A{a} sheet {which} {sheet:?}"));
                albums[a].sheets[which] = sheet;
            }
            7 => {
                // A sheet moves to the other name (if that is free).
                let from = roll(2) as usize;
                let to = 1 - from;
                if albums[a].sheets[from] != ModelSheet::None
                    && albums[a].sheets[to] == ModelSheet::None
                {
                    fs::rename(&sheet_paths[from], &sheet_paths[to]).unwrap();
                    albums[a].sheets[to] =
                        std::mem::replace(&mut albums[a].sheets[from], ModelSheet::None);
                    log.push(format!("{step}: A{a} sheet {from} renamed to {to}"));
                }
            }
            8 => {
                locked = !locked;
                lock_dir(locked);
                log.push(format!("{step}: locked folder {locked}"));
            }
            15 => {
                // The album's file is opened (from the file manager, say): that gives
                // the tracks the folder stands for, and changes nothing in the library.
                // Of what was removed from the library, a file opened like this is in
                // again; the tracks of a sheet are not (they come back through
                // `restore_removed`), and stay out of what the opening gives.
                let all = albums[a].expected_at(a, &dir);
                let by_sheet = all.iter().any(|(path, _, _)| {
                    path.parent()
                        .is_some_and(|sheet| sheet.extension().is_some_and(|ext| ext == "cue"))
                });
                // (A sheet all of whose tracks were removed: opening its file is left
                // out of this model.)
                let none_left = all.iter().all(|(path, _, _)| removed.contains(path));
                if settled && albums[a].audio_ms.is_some() && !(by_sheet && none_left) {
                    let titles_of = |library: &Library| {
                        let mut all: Vec<(String, bool)> = library
                            .snapshot()
                            .tracks
                            .iter()
                            .map(|t| (t.title.clone(), t.available))
                            .collect();
                        all.sort();
                        all
                    };
                    let mut before = titles_of(&library);
                    let mut want: Vec<String> = Vec::new();
                    for (path, title, _) in all {
                        if by_sheet && removed.contains(&path) {
                            continue;
                        }
                        if removed.remove(&path) {
                            before.push((title.clone(), true));
                        }
                        want.push(title);
                    }
                    before.sort();
                    want.sort();
                    let opened = library.import_files(std::slice::from_ref(&image));
                    let mut got: Vec<String> = opened
                        .iter()
                        .filter_map(|id| library.track(*id))
                        .map(|t| t.title.clone())
                        .collect();
                    got.sort();
                    log.push(format!("{step}: A{a} file opened"));
                    let context = format!("seed {seed}, after:\n  {}", log.join("\n  "));
                    assert_eq!(got, want, "opened: {context}");
                    assert_eq!(
                        titles_of(&library),
                        before,
                        "opening changed more than what was removed of it: {context}"
                    );
                }
            }
            14 => {
                // The first sheet cannot be read for a while (and can again). Only from
                // a settled library: what an unfinished scan left open (a file not yet
                // handed over to its sheet's tracks) waits for the sheet to be readable.
                if matches!(albums[a].sheets[0], ModelSheet::Tracks(..))
                    && (settled || unreadable[a])
                {
                    let mode = if unreadable[a] { 0o644 } else { 0o000 };
                    fs::set_permissions(&sheet_paths[0], fs::Permissions::from_mode(mode)).unwrap();
                    unreadable[a] = !unreadable[a] && fs::read(&sheet_paths[0]).is_err();
                    log.push(format!("{step}: A{a} sheet unreadable {}", unreadable[a]));
                }
            }
            op => {
                let mut tracks = library.snapshot().tracks.clone();
                tracks.sort_by(|x, y| x.path.cmp(&y.path));
                let Some(track) = tracks.get(roll(tracks.len().max(1) as u64) as usize) else {
                    continue;
                };
                match op {
                    9 | 10 => library.set_favorite(&[track.id], true).unwrap(),
                    11 => library.add_to_playlist(playlist, &[track.id]).unwrap(),
                    // (Not the anchor: with no file left that was known, the folder
                    // would come into doubt, which keeps what is missing on purpose.)
                    // (Nor a track of a sheet that cannot be read: it cannot be read
                    // back in until the sheet can.)
                    12 if track.title != "Anchor"
                        && !(0..2).any(|a| {
                            unreadable[a] && track.path.starts_with(music.join(format!("A{a}")))
                        }) =>
                    {
                        library.remove_tracks(&[track.id]);
                        removed.insert(track.path.clone());
                    }
                    12 => {}
                    _ => library.set_favorite(&[track.id], false).unwrap(),
                }
                log.push(format!("{step}: op {op} on {}", track.path.display()));
            }
        }

        // A scan that could not read the locked folder concludes nothing about what is
        // gone: what the library holds then is checked once a scan of everything can
        // finish. (A scan of an album's folder alone does not meet the locked folder.)
        let cannot_finish = locked && fs::read_dir(&locked_dir).is_err();
        if folder_only && !whole_due {
            library.debug_rescan_dirs(std::slice::from_ref(&dir));
            wait_scan(&library);
            log.push(format!("{step}: (scan of A{a} alone)"));
        } else {
            rescan(&library);
            whole_due = cannot_finish;
            if !cannot_finish {
                forget_what_is_gone(&mut removed);
            }
        }
        settled = !whole_due;
        if whole_due {
            continue;
        }
        let mut expected: Vec<(String, u64)> = vec![("Anchor".into(), 1000)];
        for (a, album) in albums.iter().enumerate() {
            expected.extend(
                album
                    .expected_at(a, &music.join(format!("A{a}")))
                    .into_iter()
                    .filter(|(path, _, _)| !removed.contains(path))
                    .map(|(_, title, length)| (title, length)),
            );
        }
        expected.sort();
        let look = |library: &Library| {
            let snapshot = library.snapshot();
            let in_playlist: Vec<_> = library
                .playlist_items(playlist)
                .into_iter()
                .map(|item| item.track_id)
                .collect();
            let mut available: Vec<(String, u64)> = Vec::new();
            let mut orphans: Vec<String> = Vec::new();
            for track in &snapshot.tracks {
                // (What is kept of a sheet that cannot be read stays until it can be:
                // nothing is concluded about its tracks meanwhile.)
                let waits = (0..2)
                    .any(|a| unreadable[a] && track.path.starts_with(music.join(format!("A{a}"))));
                if track.available {
                    available.push((track.title.clone(), track.duration_ms));
                } else if !track.favorite && !in_playlist.contains(&track.id) && !waits {
                    orphans.push(track.path.display().to_string());
                }
            }
            available.sort();
            (available, orphans)
        };
        let (available, orphans) = look(&library);
        let context = || {
            lock_dir(false);
            for a in 0..2 {
                let sheet = music.join(format!("A{a}/image.cue"));
                let _ = fs::set_permissions(sheet, fs::Permissions::from_mode(0o644));
            }
            format!("seed {seed}, after:\n  {}", log.join("\n  "))
        };
        assert_eq!(available, expected, "{}", context());
        assert!(
            orphans.is_empty(),
            "kept for nobody: {orphans:?}\n{}",
            context()
        );
        let reads = library.debug_tag_reads();
        rescan(&library);
        assert_eq!(
            library.debug_tag_reads(),
            reads,
            "read again: {}",
            context()
        );
        assert_eq!(look(&library).0, expected, "second scan: {}", context());
        forget_what_is_gone(&mut removed);
    }
    lock_dir(false);
    for a in 0..2 {
        let sheet = music.join(format!("A{a}/image.cue"));
        let _ = fs::set_permissions(sheet, fs::Permissions::from_mode(0o644));
    }
}

/// The file a sheet's tracks now stand for leaves the library, and what the user gave
/// it goes to them: its places in playlists hold the album's tracks, in their order,
/// and if it was a favourite, they are.
#[test]
fn a_replaced_files_favourite_and_playlists_go_to_the_sheets_tracks() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("Live/image.wav"), 3000, "Whole", "Band", "Live");
    write_tagged_wav(
        &music.join("Other/song.wav"),
        1000,
        "Other",
        "Band",
        "Other",
    );
    let (library, _) = env.open_scanned();
    let whole = by_title(&library, "Whole");
    let other = by_title(&library, "Other");
    library.set_favorite(&[whole.id], true).unwrap();
    let playlist = library.create_playlist("Mix").unwrap();
    library
        .add_to_playlist(playlist, &[other.id, whole.id, other.id])
        .unwrap();

    fs::write(
        music.join("Live/image.cue"),
        "FILE \"image.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"One\"\n    INDEX 01 00:00:00\n\
         \x20 TRACK 02 AUDIO\n    TITLE \"Two\"\n    INDEX 01 00:01:00\n",
    )
    .unwrap();
    rescan(&library);
    assert_eq!(
        titles(&library),
        ["One", "Other", "Two"],
        "the file is gone"
    );
    let (one, two) = (by_title(&library, "One"), by_title(&library, "Two"));
    assert!(one.favorite && two.favorite);
    let items: Vec<_> = library
        .playlist_items(playlist)
        .into_iter()
        .map(|item| item.track_id)
        .collect();
    assert_eq!(items, [other.id, one.id, two.id, other.id]);
    assert!(library.snapshot().tracks.iter().all(|t| t.available));
}

/// Opening the audio file of a sheet in a music folder (from the file manager, say)
/// opens the sheet's tracks: the file is not added alone, which its scan would undo.
#[test]
fn an_opened_sheet_file_is_its_tracks() {
    let env = TestEnv::new();
    let music = env.music();
    let image = music.join("Live/image.wav");
    write_tagged_wav(&image, 3000, "Whole", "Band", "Live");
    fs::write(
        music.join("Live/image.cue"),
        "FILE \"image.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"One\"\n    INDEX 01 00:00:00\n\
         \x20 TRACK 02 AUDIO\n    TITLE \"Two\"\n    INDEX 01 00:01:00\n",
    )
    .unwrap();
    let (library, _) = env.open_scanned();
    let tracks = [by_title(&library, "One").id, by_title(&library, "Two").id];
    assert_eq!(library.import_files(std::slice::from_ref(&image)), tracks);
    assert_eq!(titles(&library), ["One", "Two"]);
    // A playlist naming the file holds the tracks.
    let list = env.dir.path().join("live.m3u8");
    fs::write(&list, format!("{}\n", image.display())).unwrap();
    let imported = library.import_m3u8(&list).unwrap();
    assert_eq!(imported.matched, 2);
    assert!(imported.missing.is_empty());

    // Tracks kept while their file was away (they are in that playlist) are available
    // again when the file is opened, as a known file opened again is.
    let away = env.dir.path().join("image.wav");
    fs::rename(&image, &away).unwrap();
    rescan(&library);
    assert!(!by_title(&library, "One").available);
    assert!(!by_title(&library, "Two").available);
    fs::rename(&away, &image).unwrap();
    assert_eq!(library.import_files(std::slice::from_ref(&image)), tracks);
    for id in tracks {
        assert!(library.track(id).expect("kept").available);
    }
}

/// A playlist with tracks of a sheet in it, written out and read in again (on the same
/// computer): the same tracks, in their order. The sheet's tracks are written as the
/// paths the library knows them by.
#[test]
fn a_playlist_with_sheet_tracks_comes_back_as_it_was() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("Live/image.wav"), 3000, "Whole", "Band", "Live");
    write_tagged_wav(&music.join("single.wav"), 1000, "Single", "Band", "Singles");
    fs::write(
        music.join("Live/image.cue"),
        "FILE \"image.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"One\"\n    INDEX 01 00:00:00\n\
         \x20 TRACK 02 AUDIO\n    TITLE \"Two\"\n    INDEX 01 00:01:00\n",
    )
    .unwrap();
    let (library, _) = env.open_scanned();
    let ids = [
        by_title(&library, "Two").id,
        by_title(&library, "Single").id,
        by_title(&library, "One").id,
        by_title(&library, "Two").id,
    ];
    let playlist = library.create_playlist("Mix").unwrap();
    library.add_to_playlist(playlist, &ids).unwrap();
    let file = env.dir.path().join("mix.m3u8");
    assert_eq!(library.export_m3u8(playlist, &file).unwrap(), 4);
    let imported = library.import_m3u8(&file).unwrap();
    assert!(imported.missing.is_empty(), "{:?}", imported.missing);
    let back: Vec<_> = library
        .playlist_items(imported.playlist_id)
        .into_iter()
        .map(|item| item.track_id)
        .collect();
    assert_eq!(back, ids);
    assert_eq!(titles(&library), ["One", "Single", "Two"], "nothing added");
}

/// A sheet that appears, changes and goes in a watched folder is followed without a
/// rescan being asked for: the folder watcher sees `.cue` files too.
#[test]
fn the_watcher_follows_a_sheet() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("Live/image.wav"), 3000, "Whole", "Band", "Live");
    write_tagged_wav(&music.join("other.wav"), 1000, "Other", "Band", "Other");
    let (library, _) = env.open_scanned();
    library.start_watching();
    // Give the event stream a moment to start before changing anything.
    std::thread::sleep(Duration::from_millis(1500));
    let sheet = music.join("Live/image.cue");
    let write = |second: &str| {
        fs::write(
            &sheet,
            format!(
                "FILE \"image.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"One\"\n    INDEX 01 00:00:00\n\
                 \x20 TRACK 02 AUDIO\n    TITLE \"{second}\"\n    INDEX 01 00:01:00\n"
            ),
        )
        .unwrap();
    };
    write("Two");
    wait_until("the sheet's tracks", TIMEOUT, || {
        titles(&library) == ["One", "Other", "Two"]
    });
    write("Second");
    wait_until("the new title", TIMEOUT, || {
        titles(&library) == ["One", "Other", "Second"]
    });
    fs::remove_file(&sheet).unwrap();
    wait_until("the file alone again", TIMEOUT, || {
        titles(&library) == ["Other", "Whole"]
    });
    assert!(library.snapshot().tracks.iter().all(|t| t.available));
}

/// A file that appears just as a folder begins to be watched (the system's event
/// stream is not running yet, and the scan that came with the folder is over) is not
/// missed: the folder is scanned once more shortly after.
#[test]
fn a_change_made_as_watching_begins_is_not_missed() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("a/one.wav"), 500, "One", "Band", "A");
    let (library, _) = env.open_scanned();
    library.start_watching();
    write_tagged_wav(&music.join("a/two.wav"), 500, "Two", "Band", "A");
    wait_until("the file from before the watch was live", TIMEOUT, || {
        titles(&library) == ["One", "Two"]
    });
}

/// A folder that cannot be removed (the database is locked by another program, the
/// disk is full) stays a music folder with its tracks: the library does not forget in
/// memory what the database still holds. Once the database can be written, it goes.
#[test]
fn a_folder_that_cannot_be_removed_stays() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("a.wav"), 500, "A", "Band", "Album");
    write_tagged_wav(&music.join("b.wav"), 500, "B", "Band", "Album");
    let (library, root) = env.open_scanned();
    {
        // Another connection holds the write lock: nothing can be deleted.
        let other = rusqlite::Connection::open(env.paths().data_dir.join("library.db")).unwrap();
        other.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert!(library.remove_folder(root).is_err());
        other.execute_batch("ROLLBACK").unwrap();
    }
    assert_eq!(library.roots().len(), 1, "still a music folder");
    wait_scan(&library);
    assert_eq!(titles(&library), ["A", "B"]);
    assert!(library.snapshot().tracks.iter().all(|t| t.available));
    // The same after a restart: memory and database agree.
    library.shutdown();
    let (reopened, _) = Library::open(env.paths()).unwrap();
    assert_eq!(reopened.roots().len(), 1);
    assert_eq!(titles(&reopened), ["A", "B"]);

    reopened.remove_folder(root).unwrap();
    assert!(reopened.roots().is_empty());
    assert!(titles(&reopened).is_empty());
}

/// Removing a folder that fails part of the way loses nothing: the folder is deleted
/// whole or not at all. (Deleted five hundred songs at a time, a failure at the last
/// song left the first five hundred gone; the scan that followed found the files
/// again, but as new songs, without their favourites and playlist entries.)
#[test]
fn a_folder_removal_that_fails_part_way_loses_nothing() {
    const SONGS: usize = 501;
    let env = TestEnv::new();
    let music = env.music();
    for n in 0..SONGS {
        write_tagged_wav(
            &music.join(format!("{n:03}.wav")),
            20,
            &format!("Song {n:03}"),
            "Band",
            "Album",
        );
    }
    let (library, root) = env.open_scanned();
    let ids: Vec<_> = library.snapshot().tracks.iter().map(|t| t.id).collect();
    assert_eq!(ids.len(), SONGS);
    library.set_favorite(&ids, true).unwrap();
    let playlist = library.create_playlist("all of it").unwrap();
    library.add_to_playlist(playlist, &ids).unwrap();

    let favourites = |library: &Library| {
        library
            .snapshot()
            .tracks
            .iter()
            .filter(|t| t.favorite)
            .count()
    };
    // The deletion fails when it comes to the last song that is left.
    let other = rusqlite::Connection::open(env.paths().data_dir.join("library.db")).unwrap();
    other
        .execute_batch(
            "CREATE TRIGGER refuse_the_last BEFORE DELETE ON tracks \
             WHEN (SELECT COUNT(*) FROM tracks) <= 1 \
             BEGIN SELECT RAISE(ABORT, 'refused, as a full disk might'); END;",
        )
        .unwrap();
    assert!(library.remove_folder(root).is_err());
    assert_eq!(library.roots().len(), 1, "still a music folder");
    wait_scan(&library);
    assert_eq!(library.snapshot().tracks.len(), SONGS);
    assert_eq!(favourites(&library), SONGS, "favourites are kept");
    assert_eq!(
        library.playlist_items(playlist).len(),
        SONGS,
        "and so is the playlist"
    );
    let same: Vec<_> = library.snapshot().tracks.iter().map(|t| t.id).collect();
    assert_eq!(
        same, ids,
        "the songs are the ones that were there, not new ones"
    );
    // The same after a restart: memory and database agree.
    library.shutdown();
    let (reopened, _) = Library::open(env.paths()).unwrap();
    assert_eq!(reopened.snapshot().tracks.len(), SONGS);
    assert_eq!(favourites(&reopened), SONGS);
    assert_eq!(reopened.playlist_items(playlist).len(), SONGS);

    // Once it can be deleted, all of it goes.
    other.execute_batch("DROP TRIGGER refuse_the_last").unwrap();
    reopened.remove_folder(root).unwrap();
    assert!(reopened.roots().is_empty());
    assert!(reopened.snapshot().tracks.is_empty());
    assert!(reopened.playlist_items(playlist).is_empty());
}

/// A playlist with tracks of a sheet in it, read into a library that does not have the
/// music folder (a new library, another profile): the tracks are found through the
/// sheet, like files a playlist names. They are looked after like other individually
/// registered files: read again when the sheet changes, gone with it; and they become
/// the folder's own when the folder is added.
#[test]
fn a_playlist_with_sheet_tracks_is_read_into_an_empty_library() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("Live/image.wav"), 3000, "Whole", "Band", "Live");
    let sheet = music.join("Live/image.cue");
    let write = |second: &str| {
        fs::write(
            &sheet,
            format!(
                "FILE \"image.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"One\"\n    INDEX 01 00:00:00\n\
                 \x20 TRACK 02 AUDIO\n    TITLE \"{second}\"\n    INDEX 01 00:01:00\n"
            ),
        )
        .unwrap();
    };
    write("Two");
    let list = env.dir.path().join("mix.m3u8");
    {
        let (library, _) = env.open_scanned();
        let playlist = library.create_playlist("Mix").unwrap();
        let ids = [by_title(&library, "Two").id, by_title(&library, "One").id];
        library.add_to_playlist(playlist, &ids).unwrap();
        assert_eq!(library.export_m3u8(playlist, &list).unwrap(), 2);
        library.shutdown();
    }

    // Another library, with no music folder.
    let other = TestEnv::new();
    let library = other.open();
    let imported = library.import_m3u8(&list).unwrap();
    assert!(imported.missing.is_empty(), "{:?}", imported.missing);
    assert_eq!(imported.matched, 2);
    assert_eq!(titles(&library), ["One", "Two"]);
    let order: Vec<String> = library
        .playlist_items(imported.playlist_id)
        .into_iter()
        .filter_map(|item| library.track(item.track_id))
        .map(|t| t.title.clone())
        .collect();
    assert_eq!(order, ["Two", "One"]);
    let one = by_title(&library, "One");
    assert!(one.available && one.root_id.is_none());
    assert_eq!(one.duration_ms, 1000);

    // Nothing to do while nothing changes; a changed sheet is read again; a sheet
    // that goes takes its tracks along (kept as unavailable: the playlist has them).
    let reads = library.debug_tag_reads();
    rescan(&library);
    assert_eq!(library.debug_tag_reads(), reads, "not read again");
    write("Second");
    rescan(&library);
    assert_eq!(titles(&library), ["One", "Second"]);
    assert_eq!(by_title(&library, "One").id, one.id);
    let away = other.dir.path().join("image.cue");
    fs::rename(&sheet, &away).unwrap();
    rescan(&library);
    assert!(library.snapshot().tracks.iter().all(|t| !t.available));
    fs::rename(&away, &sheet).unwrap();
    rescan(&library);
    assert!(library.snapshot().tracks.iter().all(|t| t.available));

    // The folder becomes a music folder: the same tracks, now its own.
    library.add_folder(&music).unwrap();
    wait_scan(&library);
    assert_eq!(titles(&library), ["One", "Second"]);
    let adopted = by_title(&library, "One");
    assert_eq!(adopted.id, one.id);
    assert!(adopted.root_id.is_some());
}

/// A music folder that is removed takes the record of its sheets with it.
#[test]
fn removing_a_folder_forgets_its_sheets() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("Live/image.wav"), 3000, "Whole", "Band", "Live");
    fs::write(
        music.join("Live/image.cue"),
        "FILE \"image.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"One\"\n    INDEX 01 00:00:00\n",
    )
    .unwrap();
    let (library, root) = env.open_scanned();
    let recorded = || library.kv_get("cue_sheets_read").unwrap_or_default();
    assert!(recorded().contains("image.cue"));
    library.remove_folder(root).unwrap();
    assert!(!recorded().contains("image.cue"), "{}", recorded());
}

/// A sheet's tracks are in the sheet's folder: with a file beside them of the same
/// album (no album artist, where the sheet names one) they are one album, in the
/// library's lists and when shuffling by album.
#[test]
fn a_sheet_and_a_file_beside_it_are_one_album() {
    use oneum::app::host::LibraryHost;
    use oneum::playback::PlayerHost;
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("Live/image.wav"), 3000, "Whole", "", "Live");
    fs::write(
        music.join("Live/image.cue"),
        "PERFORMER \"Band\"\nTITLE \"Live\"\nFILE \"image.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"One\"\n    INDEX 01 00:00:00\n\
         \x20 TRACK 02 AUDIO\n    TITLE \"Two\"\n    INDEX 01 00:01:00\n",
    )
    .unwrap();
    write_wav(
        &music.join("Live/bonus.wav"),
        1000,
        &[("INAM", "Bonus"), ("IART", "Band"), ("IPRD", "Live")],
    );
    let (library, _) = env.open_scanned();
    let snapshot = library.snapshot();
    let bonus = by_title(&library, "Bonus");
    let one = by_title(&library, "One");
    assert!(bonus.album_artist.is_empty() && !one.album_artist.is_empty());
    assert_eq!(snapshot.albums().len(), 1, "one album");
    let host = LibraryHost::new(library.clone(), || {});
    assert!(host.has_track(one.id));
    assert!(!host.has_track(987_654));
    assert_eq!(host.album_of(one.id), host.album_of(bonus.id));
}

/// The cover beside a sheet is its tracks' artwork, and comes back when its cache file
/// is evicted (also when the sheet cannot be read then: the folder is the sheet's).
#[test]
fn a_sheets_folder_cover_is_regenerated() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("Live/image.wav"), 3000, "Whole", "Band", "Live");
    write_png(&music.join("Live/cover.png"), 50, 50);
    let sheet_path = music.join("Live/image.cue");
    fs::write(
        &sheet_path,
        "FILE \"image.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"One\"\n    INDEX 01 00:00:00\n",
    )
    .unwrap();
    let (library, _) = env.open_scanned();
    let art_id = by_title(&library, "One")
        .art_id
        .clone()
        .expect("the folder's cover");
    let path = library.art_path(&art_id);
    assert!(path.is_file());
    for sheet_readable in [true, false] {
        if !sheet_readable {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&sheet_path, fs::Permissions::from_mode(0o000)).unwrap();
        }
        fs::remove_file(&path).unwrap();
        assert!(library.load_art_rgba(&art_id, 96).is_none());
        wait_until("artwork regeneration", TIMEOUT, || path.is_file());
    }
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&sheet_path, fs::Permissions::from_mode(0o644)).unwrap();
}

/// What was found out about a sheet's tracks (the file check here; the loudness
/// measurement the same way) stays when only names in the sheet change, and goes for a
/// track whose stretch of the file moves. Which sheets were read is kept across a
/// restart: an unchanged sheet is not read again then.
#[test]
fn a_sheet_edit_keeps_what_was_measured_of_unmoved_tracks() {
    let env = TestEnv::new();
    let music = env.music();
    write_tagged_wav(&music.join("Live/image.wav"), 6000, "Whole", "Band", "Live");
    let sheet_path = music.join("Live/image.cue");
    let sheet = |second_title: &str, third_start: &str| {
        format!(
            "FILE \"image.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"One\"\n    INDEX 01 00:00:00\n\
             \x20 TRACK 02 AUDIO\n    TITLE \"{second_title}\"\n    INDEX 01 00:02:00\n\
             \x20 TRACK 03 AUDIO\n    TITLE \"Three\"\n    INDEX 01 {third_start}\n"
        )
    };
    fs::write(&sheet_path, sheet("Two", "00:04:00")).unwrap();
    {
        let (library, _) = env.open_scanned();
        assert_eq!(titles(&library), ["One", "Three", "Two"]);
        assert!(library.check_files());
        assert!(library.wait_for_checks(Duration::from_secs(60)));
        assert!(library.snapshot().tracks.iter().all(|t| t.check.is_some()));
        library.shutdown();
    }

    // Reopened: the sheet is known to be read, nothing is read again.
    let (library, _) = Library::open(env.paths()).unwrap();
    rescan(&library);
    assert_eq!(
        library.debug_tag_reads(),
        0,
        "nothing read again after a restart"
    );
    let one = by_title(&library, "One");

    // Only a title changes: the names are read again, the results stay.
    fs::write(&sheet_path, sheet("Second", "00:04:00")).unwrap();
    rescan(&library);
    assert_eq!(library.debug_tag_reads(), 1, "the sheet's file read once");
    assert_eq!(titles(&library), ["One", "Second", "Three"]);
    assert!(
        library.snapshot().tracks.iter().all(|t| t.check.is_some()),
        "a title change keeps what was found out"
    );
    assert_eq!(by_title(&library, "One").mtime_ns, one.mtime_ns);

    // The third track starts later: tracks two and three have other stretches now.
    fs::write(&sheet_path, sheet("Second", "00:05:00")).unwrap();
    rescan(&library);
    assert!(
        by_title(&library, "One").check.is_some(),
        "track one did not move"
    );
    assert!(by_title(&library, "Second").check.is_none());
    assert!(by_title(&library, "Three").check.is_none());
}

/// An iPhone app's container is given another path when the app is updated or restored
/// from a backup, and the music in it moves with it. The library follows: the same
/// songs, with what is known about them, at the paths they have now; nothing is read
/// again, and what lies outside the container is left where it is.
#[test]
fn a_library_follows_its_folder_when_that_moves() {
    use oneum::library::LibraryPaths;
    let dir = tempfile::tempdir().unwrap();
    let paths = |home: &Path| LibraryPaths {
        data_dir: home.join("Library/data"),
        cache_dir: home.join("Library/cache"),
    };
    let before = dir.path().join("Application/A");
    let music = before.join("Documents");
    write_tagged_wav(&music.join("Album/one.wav"), 1000, "One", "Band", "Album");
    write_tagged_wav(&music.join("Live/image.wav"), 3000, "Whole", "Band", "Live");
    fs::write(
        music.join("Live/image.cue"),
        "FILE \"image.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"First\"\n    INDEX 01 00:00:00\n\
         \x20 TRACK 02 AUDIO\n    TITLE \"Second\"\n    INDEX 01 00:01:00\n",
    )
    .unwrap();
    let outside = dir.path().join("elsewhere/loose.wav");
    write_tagged_wav(&outside, 1000, "Loose", "Band", "Singles");

    let (ids, playlist, loose) = {
        let (library, _) = Library::open_in_home(paths(&before), &before).unwrap();
        library.add_folder(&music).unwrap();
        wait_scan(&library);
        assert_eq!(
            library.import_files(std::slice::from_ref(&outside)).len(),
            1
        );
        assert_eq!(titles(&library), ["First", "Loose", "One", "Second"]);
        let ids: Vec<_> = ["One", "First", "Second", "Loose"]
            .iter()
            .map(|title| by_title(&library, title).id)
            .collect();
        library.set_favorite(&ids[..1], true).unwrap();
        let playlist = library.create_playlist("Mix").unwrap();
        library.add_to_playlist(playlist, &ids).unwrap();
        let loose = by_title(&library, "Loose").path.clone();
        library.shutdown();
        (ids, playlist, loose)
    };

    let after = dir.path().join("Application/B");
    fs::rename(&before, &after).unwrap();
    let (library, _) = Library::open_in_home(paths(&after), &after).unwrap();
    // As it is opened, before anything is scanned: the songs are where the files are.
    let snapshot = library.snapshot();
    for (title, id) in ["One", "First", "Second", "Loose"].iter().zip(&ids) {
        let track = snapshot
            .get(*id)
            .unwrap_or_else(|| panic!("{title} is gone"));
        assert_eq!(&track.title, title);
        assert!(track.available, "{title} is not available");
        assert!(track.file().exists(), "{}", track.path.display());
    }
    assert!(snapshot.get(ids[0]).unwrap().path.starts_with(&after));
    assert_eq!(
        snapshot.get(ids[3]).unwrap().path,
        loose,
        "left where it is"
    );
    assert!(snapshot.get(ids[0]).unwrap().favorite);
    let roots = library.roots();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].path, after.join("Documents"));

    rescan(&library);
    assert_eq!(library.debug_tag_reads(), 0, "nothing is read again");
    assert_eq!(titles(&library), ["First", "Loose", "One", "Second"]);
    let items: Vec<_> = library
        .playlist_items(playlist)
        .into_iter()
        .map(|item| item.track_id)
        .collect();
    assert_eq!(items, ids);

    // Opened again where it is: nothing to follow.
    library.shutdown();
    let (library, _) = Library::open_in_home(paths(&after), &after).unwrap();
    rescan(&library);
    assert_eq!(library.debug_tag_reads(), 0);
    assert_eq!(library.snapshot().len(), 4);
}

/// A music folder outside the app's own (one linked on an iPhone and found again by its
/// bookmark) that was renamed since the last run: the library is told where it was and
/// where it is, and keeps the songs with what is known about them. Nothing is read
/// again. A folder that is not one of the library's is not followed, and one whose new
/// place is a music folder already is left as it was.
#[test]
fn a_library_follows_a_music_folder_that_was_moved() {
    use oneum::library::LibraryPaths;
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("Application/A");
    let paths = || LibraryPaths {
        data_dir: home.join("Library/data"),
        cache_dir: home.join("Library/cache"),
    };
    let own = home.join("Documents");
    write_tagged_wav(&own.join("Album/one.wav"), 1000, "One", "Band", "Album");
    let linked = dir.path().join("Drive/Music");
    write_tagged_wav(&linked.join("Live/two.wav"), 1000, "Two", "Band", "Live");
    write_tagged_wav(
        &linked.join("Live/three.wav"),
        1000,
        "Three",
        "Band",
        "Live",
    );

    let ids = {
        let (library, _) = Library::open_in_home(paths(), &home).unwrap();
        library.add_folder(&own).unwrap();
        library.add_folder(&linked).unwrap();
        wait_scan(&library);
        assert_eq!(titles(&library), ["One", "Three", "Two"]);
        let ids: Vec<_> = ["One", "Two", "Three"]
            .iter()
            .map(|title| by_title(&library, title).id)
            .collect();
        library.set_favorite(&ids[1..2], true).unwrap();
        library.shutdown();
        ids
    };

    let renamed = dir.path().join("Drive/Records");
    fs::rename(&linked, &renamed).unwrap();
    // A folder the library does not know is not followed (and does no harm).
    let moves = [
        (linked.clone(), renamed.clone()),
        (dir.path().join("Drive/Other"), dir.path().join("Drive/New")),
    ];
    let (library, _) = Library::open_following(paths(), &home, &moves).unwrap();
    let snapshot = library.snapshot();
    for (title, id) in ["One", "Two", "Three"].iter().zip(&ids) {
        let track = snapshot
            .get(*id)
            .unwrap_or_else(|| panic!("{title} is gone"));
        assert_eq!(&track.title, title);
        assert!(track.file().exists(), "{}", track.path.display());
    }
    assert!(snapshot.get(ids[1]).unwrap().path.starts_with(&renamed));
    assert!(snapshot.get(ids[1]).unwrap().favorite);
    assert!(snapshot.get(ids[0]).unwrap().path.starts_with(&own));
    let mut roots: Vec<_> = library.roots().into_iter().map(|root| root.path).collect();
    roots.sort();
    let mut expected = vec![own.clone(), renamed.clone()];
    expected.sort();
    assert_eq!(roots, expected);
    rescan(&library);
    assert_eq!(library.debug_tag_reads(), 0, "nothing is read again");
    assert_eq!(titles(&library), ["One", "Three", "Two"]);
    library.shutdown();

    // Its new place is a music folder already: it cannot be followed there, and the
    // library opens as it was.
    let (library, _) =
        Library::open_following(paths(), &home, &[(renamed.clone(), own.clone())]).unwrap();
    let mut roots: Vec<_> = library.roots().into_iter().map(|root| root.path).collect();
    roots.sort();
    assert_eq!(roots, expected);
    assert_eq!(library.snapshot().len(), 3);
}
