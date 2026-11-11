//! Scanning real files in real temporary directories: formats, incremental rescans,
//! offline folders, moved/missing files, symlinks, broken files, artwork, watching.
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
    assert_eq!(album.tracks.len(), full.len());
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
    if has_encoder("mp2") {
        // MPEG layer 2 audio named .mp3.
        ffmpeg_audio(
            &music.join("layer2.mp3"),
            &["-c:a", "mp2", "-f", "mp2"],
            &[],
            None,
        );
        expected_unsupported += 1;
    }
    if has_encoder("adpcm_ms") {
        ffmpeg_audio(&music.join("adpcm.wav"), &["-c:a", "adpcm_ms"], &[], None);
        expected_unsupported += 1;
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
