//! Playlists, M3U8 import/export, favourites, history, queue and key-value persistence.
#[path = "library_support.rs"]
mod support;

use oneum::library::{Library, LibraryError};
use oneum::model::TrackId;
use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use support::*;

/// A library with five scanned tracks; returns their ids in title order (T1..T5).
fn library_with_tracks(env: &TestEnv) -> (Arc<Library>, Vec<TrackId>) {
    for i in 1..=5u32 {
        write_tagged_wav(
            &env.music().join(format!("앨범/곡 {i}.wav")),
            200 + i * 100,
            &format!("T{i}"),
            "아이유",
            "앨범",
        );
    }
    let (library, _) = env.open_scanned();
    let snapshot = library.snapshot();
    let mut tracks: Vec<_> = snapshot.tracks.iter().collect();
    tracks.sort_by(|a, b| a.title.cmp(&b.title));
    let ids = tracks.iter().map(|t| t.id).collect();
    (library, ids)
}

fn track_ids(library: &Library, playlist: i64) -> Vec<TrackId> {
    library
        .playlist_items(playlist)
        .iter()
        .map(|item| item.track_id)
        .collect()
}

#[test]
fn playlist_crud() {
    let env = TestEnv::new();
    let (library, ids) = library_with_tracks(&env);
    let generation = library.playlists_generation();
    assert!(library.playlists().is_empty());

    assert!(matches!(
        library.create_playlist(""),
        Err(LibraryError::InvalidName)
    ));
    assert!(matches!(
        library.create_playlist("  \t "),
        Err(LibraryError::InvalidName)
    ));
    let first = library.create_playlist("  드라이브 🚗  ").unwrap();
    let second = library.create_playlist("드라이브 🚗").unwrap();
    assert_ne!(first, second, "duplicate names are allowed");
    assert!(library.playlists_generation() > generation);
    let lists = library.playlists();
    assert_eq!(lists.len(), 2);
    assert_eq!(lists[0].name, "드라이브 🚗", "the name is trimmed");
    assert_eq!((lists[0].id, lists[0].item_count), (first, 0));

    // Items keep their order and may repeat; unknown track ids are skipped.
    library
        .add_to_playlist(first, &[ids[2], ids[0], ids[2], 987_654, ids[4]])
        .unwrap();
    assert_eq!(track_ids(&library, first), [ids[2], ids[0], ids[2], ids[4]]);
    let items = library.playlist_items(first);
    let unique: std::collections::HashSet<i64> = items.iter().map(|i| i.item_id).collect();
    assert_eq!(unique.len(), 4, "every entry has its own item id");
    library.add_to_playlist(first, &[ids[1]]).unwrap();
    assert_eq!(
        track_ids(&library, first),
        [ids[2], ids[0], ids[2], ids[4], ids[1]]
    );
    assert_eq!(library.playlists()[0].item_count, 5);
    assert!(library.playlist_items(second).is_empty());

    // Removing one of the two entries of the same track leaves the other.
    let generation = library.playlists_generation();
    library
        .remove_playlist_items(first, &[items[0].item_id])
        .unwrap();
    assert_eq!(track_ids(&library, first), [ids[0], ids[2], ids[4], ids[1]]);
    assert!(library.playlists_generation() > generation);
    // Item ids of another playlist are not affected.
    library.add_to_playlist(second, &[ids[3]]).unwrap();
    let foreign = library.playlist_items(second)[0].item_id;
    library.remove_playlist_items(first, &[foreign]).unwrap();
    assert_eq!(library.playlist_items(second).len(), 1);

    library.rename_playlist(first, " 새 이름 ").unwrap();
    assert_eq!(library.playlists()[0].name, "새 이름");
    assert!(matches!(
        library.rename_playlist(first, " "),
        Err(LibraryError::InvalidName)
    ));

    // Unknown playlists.
    assert!(matches!(
        library.rename_playlist(999, "x"),
        Err(LibraryError::NotFound)
    ));
    assert!(matches!(
        library.delete_playlist(999),
        Err(LibraryError::NotFound)
    ));
    assert!(matches!(
        library.add_to_playlist(999, &ids),
        Err(LibraryError::NotFound)
    ));
    assert!(matches!(
        library.remove_playlist_items(999, &[1]),
        Err(LibraryError::NotFound)
    ));
    assert!(matches!(
        library.move_playlist_items(999, &[1], 0),
        Err(LibraryError::NotFound)
    ));
    assert!(library.playlist_items(999).is_empty());

    // Everything is persistent.
    drop(library);
    let library = env.open();
    assert_eq!(library.playlists().len(), 2);
    assert_eq!(track_ids(&library, first), [ids[0], ids[2], ids[4], ids[1]]);

    library.delete_playlist(first).unwrap();
    assert_eq!(library.playlists().len(), 1);
    assert!(library.playlist_items(first).is_empty());
    // Deleting a playlist does not delete tracks.
    assert_eq!(library.snapshot().len(), 5);
}

#[test]
fn moving_playlist_items() {
    let env = TestEnv::new();
    let (library, ids) = library_with_tracks(&env);
    let playlist = library.create_playlist("order").unwrap();
    library.add_to_playlist(playlist, &ids).unwrap();
    let item = |index: usize| library.playlist_items(playlist)[index].item_id;
    let order = |library: &Library| -> Vec<usize> {
        track_ids(library, playlist)
            .iter()
            .map(|id| ids.iter().position(|x| x == id).unwrap() + 1)
            .collect()
    };
    assert_eq!(order(&library), [1, 2, 3, 4, 5]);

    // One item down: T1 lands at index 2 of [2,3,4,5].
    library
        .move_playlist_items(playlist, &[item(0)], 2)
        .unwrap();
    assert_eq!(order(&library), [2, 3, 1, 4, 5]);
    // One item to the front.
    library
        .move_playlist_items(playlist, &[item(4)], 0)
        .unwrap();
    assert_eq!(order(&library), [5, 2, 3, 1, 4]);
    // Several items keep their relative order in the list, whatever order they are
    // named in: moving {T4, T2} to the front gives T2, T4.
    library
        .move_playlist_items(playlist, &[item(4), item(1)], 0)
        .unwrap();
    assert_eq!(order(&library), [2, 4, 5, 3, 1]);
    // An index past the end appends.
    library
        .move_playlist_items(playlist, &[item(0), item(2)], 99)
        .unwrap();
    assert_eq!(order(&library), [4, 3, 1, 2, 5]);
    // Into the middle: [4,1,5] with {3,2} inserted at index 1.
    library
        .move_playlist_items(playlist, &[item(1), item(3)], 1)
        .unwrap();
    assert_eq!(order(&library), [4, 3, 2, 1, 5]);
    // Unknown item ids and an empty selection change nothing.
    let generation = library.playlists_generation();
    library
        .move_playlist_items(playlist, &[123_456], 0)
        .unwrap();
    library.move_playlist_items(playlist, &[], 0).unwrap();
    assert_eq!(order(&library), [4, 3, 2, 1, 5]);
    assert_eq!(library.playlists_generation(), generation);

    // The order is what gets stored.
    drop(library);
    let library = env.open();
    assert_eq!(order(&library), [4, 3, 2, 1, 5]);
}

#[test]
fn deleting_tracks_cascades_to_playlists_history_and_queue() {
    let env = TestEnv::new();
    let (library, ids) = library_with_tracks(&env);
    let playlist = library.create_playlist("p").unwrap();
    library
        .add_to_playlist(playlist, &[ids[0], ids[1], ids[0], ids[2]])
        .unwrap();
    library.record_play(ids[0]);
    library.record_play(ids[1]);
    assert!(library.save_session(
        Some(&[(10, ids[0]), (11, ids[1]), (12, ids[0])][..]),
        "test_session",
        "{}"
    ));
    let generation = library.playlists_generation();

    library.remove_tracks(&[ids[0]]);
    assert_eq!(track_ids(&library, playlist), [ids[1], ids[2]]);
    assert!(library.playlists_generation() > generation);
    assert_eq!(library.recent_plays(10), [ids[1]]);
    assert_eq!(library.load_queue(), [(11, ids[1])]);
    assert_eq!(library.snapshot().len(), 4);
    assert!(library.snapshot().get(ids[0]).is_none());
}

/// The count the app uses to decide whether the play queue must be checked again: it
/// moves when tracks leave the library, not on the many other notifications.
#[test]
fn only_removals_move_the_removal_count() {
    let env = TestEnv::new();
    let (library, ids) = library_with_tracks(&env);
    let start = library.removals();
    library.set_favorite(&[ids[0]], true).unwrap();
    library.record_play(ids[1]);
    let playlist = library.create_playlist("mix").unwrap();
    library.add_to_playlist(playlist, &ids).unwrap();
    library.rescan();
    assert!(library.wait_for_scan(std::time::Duration::from_secs(30)));
    assert_eq!(library.removals(), start, "nothing left the library");
    library.remove_tracks(&[ids[0], ids[1]]);
    assert!(library.removals() > start);
}

#[test]
fn favourites_history_and_play_counts() {
    let env = TestEnv::new();
    let (library, ids) = library_with_tracks(&env);
    let notified = Arc::new(AtomicUsize::new(0));
    {
        let notified = Arc::clone(&notified);
        library.set_notifier(Box::new(move || {
            notified.fetch_add(1, Ordering::Relaxed);
        }));
    }

    let generation = library.generation();
    library
        .set_favorite(&[ids[0], ids[3], 424_242], true)
        .unwrap();
    assert!(library.generation() > generation);
    assert!(notified.load(Ordering::Relaxed) > 0);
    let snapshot = library.snapshot();
    assert!(snapshot.get(ids[0]).unwrap().favorite);
    assert!(snapshot.get(ids[3]).unwrap().favorite);
    assert!(!snapshot.get(ids[1]).unwrap().favorite);
    // The index is intact after the cheap update.
    assert_eq!(snapshot.search("t4").len(), 1);
    assert_eq!(snapshot.search("ㅇㅇㅇ").len(), 5);
    library.set_favorite(&[ids[0]], false).unwrap();
    assert!(!library.track(ids[0]).unwrap().favorite);
    assert!(!library.snapshot().get(ids[0]).unwrap().favorite);
    // Setting the value it already has does not publish anything.
    let generation = library.generation();
    library.set_favorite(&[ids[0]], false).unwrap();
    assert_eq!(library.generation(), generation);

    assert!(library.recent_plays(10).is_empty());
    library.record_play(ids[1]);
    library.record_play(ids[2]);
    library.record_play(ids[1]);
    library.record_play(777_777); // unknown ids are ignored
    assert_eq!(
        library.recent_plays(10),
        [ids[1], ids[2]],
        "most recent first, distinct"
    );
    assert_eq!(library.recent_plays(1), [ids[1]]);
    assert!(library.recent_plays(0).is_empty());
    let played = library.snapshot().get(ids[1]).cloned().unwrap();
    assert_eq!(played.play_count, 2);
    assert!(played.last_played_at.is_some());
    assert_eq!(library.snapshot().get(ids[0]).unwrap().play_count, 0);

    drop(snapshot);
    drop(library);
    let library = env.open();
    assert_eq!(library.recent_plays(10), [ids[1], ids[2]]);
    assert_eq!(library.track(ids[1]).unwrap().play_count, 2);
    assert!(library.track(ids[3]).unwrap().favorite);
}

/// The list of what was played last is asked for by the window in a frame: it comes
/// from memory and does not wait for the database, also not while a write is held up
/// (another program has the database locked).
#[test]
fn recent_plays_do_not_wait_for_the_database() {
    let env = TestEnv::new();
    let (library, ids) = library_with_tracks(&env);
    library.record_play(ids[1]);
    library.record_play(ids[0]);
    let other = rusqlite::Connection::open(env.paths().data_dir.join("library.db")).unwrap();
    other.execute_batch("BEGIN IMMEDIATE").unwrap();
    // A write that waits for the lock (and holds the library's own connection).
    let writer = {
        let library = library.clone();
        let id = ids[2];
        std::thread::spawn(move || library.record_play(id))
    };
    std::thread::sleep(std::time::Duration::from_millis(300));
    let started = std::time::Instant::now();
    assert_eq!(library.recent_plays(10), [ids[0], ids[1]]);
    assert!(
        started.elapsed() < std::time::Duration::from_millis(500),
        "waited {:?}",
        started.elapsed()
    );
    other.execute_batch("ROLLBACK").unwrap();
    writer.join().unwrap();
    // After a restart the list is read from the history.
    library.shutdown();
    let (reopened, _) = Library::open(env.paths()).unwrap();
    let recent = reopened.recent_plays(10);
    assert_eq!(&recent[recent.len() - 2..], [ids[0], ids[1]]);
}

#[test]
fn kv_and_queue_persistence() {
    let env = TestEnv::new();
    let (library, ids) = library_with_tracks(&env);
    assert_eq!(library.kv_get("player.volume"), None);
    library.kv_set("player.volume", "0.8");
    library.kv_set("player.volume", "0.5");
    library.kv_set("상태", "{\"반복\": \"전체\", \"note\": \"it's\"}");
    library.kv_set("empty", "");
    assert_eq!(library.kv_get("player.volume").as_deref(), Some("0.5"));
    assert_eq!(library.kv_get("empty").as_deref(), Some(""));

    assert!(library.load_queue().is_empty());
    assert!(library.save_session(Some(&[(1, ids[0]), (2, ids[1])][..]), "test_session", "{}"));
    // Saving replaces the previous queue; the same track may be queued twice and item
    // ids are opaque 64-bit values. Unknown tracks are dropped.
    let queue = [(u64::MAX, ids[4]), (7, ids[4]), (3, 555_555), (0, ids[2])];
    assert!(library.save_session(Some(&queue[..]), "test_session", "{}"));
    assert_eq!(
        library.load_queue(),
        [(u64::MAX, ids[4]), (7, ids[4]), (0, ids[2])]
    );

    drop(library);
    let library = env.open();
    assert_eq!(library.kv_get("player.volume").as_deref(), Some("0.5"));
    assert_eq!(
        library.kv_get("상태").as_deref(),
        Some("{\"반복\": \"전체\", \"note\": \"it's\"}")
    );
    assert_eq!(
        library.load_queue(),
        [(u64::MAX, ids[4]), (7, ids[4]), (0, ids[2])]
    );
    assert!(library.save_session(Some(&[][..]), "test_session", "{}"));
    assert!(library.load_queue().is_empty());
}

#[test]
fn m3u8_export_and_import_round_trip() {
    let env = TestEnv::new();
    let (library, ids) = library_with_tracks(&env);
    let playlist = library.create_playlist("원본").unwrap();
    library
        .add_to_playlist(playlist, &[ids[1], ids[0], ids[1]])
        .unwrap();

    let dest = env.dir.path().join("exports/내 목록.m3u8");
    assert_eq!(library.export_m3u8(playlist, &dest).unwrap(), 3);
    let text = fs::read_to_string(&dest).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "#EXTM3U");
    assert_eq!(lines.len(), 7);
    // Durations: T2 is 400 ms (rounds to 0 s), T1 is 300 ms.
    assert_eq!(lines[1], "#EXTINF:0,아이유 - T2");
    let t2_path = library.track(ids[1]).unwrap().path.clone();
    assert!(t2_path.is_absolute());
    assert_eq!(lines[2], t2_path.to_str().unwrap());
    assert_eq!(lines[3], "#EXTINF:0,아이유 - T1");
    assert!(matches!(
        library.export_m3u8(999, &dest),
        Err(LibraryError::NotFound)
    ));

    let imported = library.import_m3u8(&dest).unwrap();
    assert_eq!(imported.name, "내 목록");
    assert_eq!(imported.matched, 3);
    assert!(imported.missing.is_empty());
    assert_ne!(imported.playlist_id, playlist);
    assert_eq!(
        track_ids(&library, imported.playlist_id),
        [ids[1], ids[0], ids[1]]
    );
    assert_eq!(
        library.snapshot().len(),
        5,
        "known files are not imported twice"
    );
}

#[test]
fn m3u8_import_tolerates_real_world_files() {
    let env = TestEnv::new();
    let (library, ids) = library_with_tracks(&env);
    let music = env.music();
    let path_of = |i: usize| library.track(ids[i]).unwrap().path.clone();

    // A file outside the library that exists on disk: it gets imported.
    let lists = env.dir.path().join("lists");
    write_tagged_wav(
        &lists.join("extra/밖의 곡.wav"),
        250,
        "Outside",
        "Other",
        "Elsewhere",
    );
    // A file that exists but is not audio the library can read.
    fs::write(lists.join("extra/broken.mp3"), noise(2000, 21)).unwrap();

    let file_url = format!(
        "file://{}",
        path_of(2)
            .to_str()
            .unwrap()
            .bytes()
            .map(|b| match b {
                b'/' | b'.' | b'-' | b'_' => (b as char).to_string(),
                b if b.is_ascii_alphanumeric() => (b as char).to_string(),
                b => format!("%{b:02X}"),
            })
            .collect::<String>()
    );
    let relative_to_music = "../music/앨범/곡 4.wav";
    let mut text = String::from("\u{feff}#EXTM3U\r\n");
    text.push_str("#EXTINF:1,아이유 - T1\r\n");
    text.push_str(&format!("{}\r\n", path_of(0).display()));
    text.push_str("\r\n   \r\n# a plain comment\r\n");
    text.push_str(&format!("{file_url}\r\n"));
    text.push_str(&format!("{relative_to_music}\r\n"));
    text.push_str("extra/밖의 곡.wav\r\n");
    text.push_str("extra/broken.mp3\r\n");
    text.push_str("extra/없는 파일.flac\r\n");
    text.push_str("http://example.com/stream.mp3\r\n");
    text.push_str(&format!("{}", path_of(0).display())); // no trailing newline
    let list_path = lists.join("섞인 목록.m3u");
    fs::write(&list_path, text).unwrap();
    assert!(music.join("앨범/곡 4.wav").is_file());

    let generation = library.playlists_generation();
    let imported = library.import_m3u8(&list_path).unwrap();
    assert_eq!(imported.name, "섞인 목록");
    assert_eq!(
        imported.missing,
        [
            "extra/broken.mp3",
            "extra/없는 파일.flac",
            "http://example.com/stream.mp3"
        ]
    );
    assert_eq!(imported.matched, 5);
    assert!(library.playlists_generation() > generation);

    let outside = library
        .snapshot()
        .tracks
        .iter()
        .find(|t| t.title == "Outside")
        .cloned()
        .expect("the file outside the library was imported");
    assert_eq!(outside.root_id, None);
    assert_eq!(
        track_ids(&library, imported.playlist_id),
        [ids[0], ids[2], ids[3], outside.id, ids[0]]
    );
    assert_eq!(library.snapshot().len(), 6);

    // A missing playlist file is an I/O error, not a panic or an empty playlist.
    let count = library.playlists().len();
    assert!(matches!(
        library.import_m3u8(&lists.join("nope.m3u8")),
        Err(LibraryError::Io(_))
    ));
    assert_eq!(library.playlists().len(), count);
}

#[test]
fn m3u8_import_matches_paths_across_unicode_normal_forms() {
    use unicode_normalization::UnicodeNormalization;
    let env = TestEnv::new();
    let (library, ids) = library_with_tracks(&env);
    // The library has the path as the file system reported it. Write the playlist with
    // the opposite normal form; the entry must resolve to the existing row.
    let original = library
        .track(ids[0])
        .unwrap()
        .path
        .to_str()
        .unwrap()
        .to_string();
    let nfd: String = original.nfd().collect();
    let nfc: String = original.nfc().collect();
    let other = if original == nfc { nfd } else { nfc };
    assert_ne!(other, original, "the fixture path must contain Hangul");
    let list = env.dir.path().join("forms.m3u8");
    fs::write(&list, format!("#EXTM3U\n{other}\n")).unwrap();

    let imported = library.import_m3u8(&list).unwrap();
    assert_eq!(imported.matched, 1);
    assert_eq!(track_ids(&library, imported.playlist_id), [ids[0]]);
    assert_eq!(
        library.snapshot().len(),
        5,
        "no duplicate row for the same file"
    );
}

#[test]
fn m3u8_import_handles_awkward_names_duplicates_and_many_new_files() {
    let env = TestEnv::new();
    let (library, ids) = library_with_tracks(&env);
    let lists = env.dir.path().join("lists");
    // Names that a careless parser gets wrong: a leading '#', and surrounding spaces.
    write_tagged_wav(&lists.join("#1 Hits/01.wav"), 300, "Hash", "A", "B");
    write_tagged_wav(&lists.join(" padded .wav"), 310, "Padded", "A", "B");
    // Enough new files to need more than one database batch.
    const NEW: usize = 230;
    for i in 0..NEW {
        write_wav(&lists.join(format!("new/n{i:03}.wav")), 100, &[]);
    }

    let mut text: Vec<u8> = Vec::new();
    text.extend_from_slice(b"#EXTM3U\n");
    text.extend_from_slice(b"#1 Hits/01.wav\n");
    text.extend_from_slice(b"# just a remark\n");
    text.extend_from_slice(b" padded .wav\n");
    for i in 0..NEW {
        text.extend_from_slice(format!("#EXTINF:0,n{i}\nnew/n{i:03}.wav\n").as_bytes());
    }
    // The same new file again, and a library track through another spelling of its path.
    text.extend_from_slice(b"new/n000.wav\n");
    let known = library.track(ids[0]).unwrap().path.clone();
    let resolved = fs::canonicalize(&known).unwrap();
    text.extend_from_slice(resolved.to_str().unwrap().as_bytes());
    text.push(b'\n');
    // A name in a legacy encoding (CP949): not valid UTF-8, cannot be resolved.
    text.extend_from_slice(b"new/\xb9\xe3\xc6\xed\xc1\xf6.wav\n");
    let list = lists.join("awkward.m3u");
    fs::write(&list, &text).unwrap();

    let before = library.snapshot().len();
    let imported = library.import_m3u8(&list).unwrap();
    assert_eq!(imported.matched, 2 + NEW + 2);
    assert_eq!(imported.missing.len(), 1, "{:?}", imported.missing);
    assert!(imported.missing[0].starts_with("new/"));
    assert!(imported.missing[0].contains('\u{fffd}'));

    let items = track_ids(&library, imported.playlist_id);
    assert_eq!(items.len(), 2 + NEW + 2);
    let title = |id: TrackId| library.track(id).unwrap().title.clone();
    assert_eq!(title(items[0]), "Hash");
    assert_eq!(title(items[1]), "Padded");
    assert_eq!(title(items[2]), "n000");
    assert_eq!(title(items[2 + NEW - 1]), format!("n{:03}", NEW - 1));
    assert_eq!(items[2 + NEW], items[2], "a file listed twice is one track");
    assert_eq!(
        items[2 + NEW + 1],
        ids[0],
        "known under another spelling of its path"
    );
    assert_eq!(library.snapshot().len(), before + 2 + NEW);
    // The spaces belong to the name.
    assert!(
        library
            .track(items[1])
            .unwrap()
            .path
            .ends_with(" padded .wav")
    );
}

#[test]
fn m3u8_import_refuses_files_too_large_to_be_playlists() {
    let env = TestEnv::new();
    let (library, _) = library_with_tracks(&env);
    let huge = env.dir.path().join("huge.m3u8");
    {
        use std::io::Write;
        let mut file = fs::File::create(&huge).unwrap();
        let line = [b'x'; 1024];
        for _ in 0..(17 * 1024) {
            file.write_all(&line).unwrap();
        }
    }
    let playlists = library.playlists().len();
    match library.import_m3u8(&huge) {
        Err(LibraryError::Io(err)) => {
            assert_eq!(err.kind(), std::io::ErrorKind::FileTooLarge);
        }
        other => panic!("unexpected: {:?}", other.map(|i| i.matched)),
    }
    assert_eq!(
        library.playlists().len(),
        playlists,
        "no playlist is left behind"
    );
}

/// An Apple Music library export: counts, dates and loved songs are merged into the
/// library's songs (matched by file, whatever the Unicode form of the path), the user's
/// playlists are made, and importing again changes nothing.
#[test]
fn an_apple_music_library_export_is_taken_over() {
    use unicode_normalization::UnicodeNormalization;
    let env = TestEnv::new();
    let (library, ids) = library_with_tracks(&env);
    let path_of = |id: TrackId| library.track(id).unwrap().path.clone();
    let url = |path: &std::path::Path, nfd: bool| {
        let text = path.to_string_lossy();
        let text: String = if nfd {
            text.nfd().collect()
        } else {
            text.nfc().collect()
        };
        let mut url = String::from("file://");
        for byte in text.bytes() {
            if byte.is_ascii_alphanumeric() || b"/-_.".contains(&byte) {
                url.push(byte as char);
            } else {
                url.push_str(&format!("%{byte:02X}"));
            }
        }
        url
    };
    library.record_play(ids[0]);
    let before = library.track(ids[0]).unwrap();
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>Tracks</key><dict>
<key>1</key><dict><key>Track ID</key><integer>1</integer><key>Play Count</key><integer>7</integer>
  <key>Play Date UTC</key><date>2020-01-01T00:00:00Z</date><key>Date Added</key><date>2001-01-01T00:00:00Z</date>
  <key>Location</key><string>{}</string></dict>
<key>2</key><dict><key>Track ID</key><integer>2</integer><key>Loved</key><true/>
  <key>Location</key><string>{}</string></dict>
<key>3</key><dict><key>Track ID</key><integer>3</integer><key>Play Count</key><integer>3</integer>
  <key>Location</key><string>file:///nowhere/gone.mp3</string></dict>
</dict><key>Playlists</key><array>
<dict><key>Name</key><string>Library</string><key>Master</key><true/><key>Playlist Items</key><array>
  <dict><key>Track ID</key><integer>1</integer></dict></array></dict>
<dict><key>Name</key><string>드라이브</string><key>Playlist Items</key><array>
  <dict><key>Track ID</key><integer>2</integer></dict><dict><key>Track ID</key><integer>3</integer></dict>
  <dict><key>Track ID</key><integer>1</integer></dict></array></dict>
</array></dict></plist>"#,
        url(&path_of(ids[0]), false),
        url(&path_of(ids[1]), true),
    );
    let file = env.dir.path().join("Library.xml");
    fs::write(&file, xml).unwrap();

    let result = library.import_apple_music(&file).expect("imports");
    assert_eq!(
        (result.matched, result.unmatched, result.playlists),
        (2, 1, 1)
    );
    let first = library.track(ids[0]).unwrap();
    assert_eq!(first.play_count, 7, "the higher count");
    assert_eq!(
        first.last_played_at, before.last_played_at,
        "the later play stays"
    );
    assert_eq!(first.added_at, 978_307_200, "the earlier date added");
    assert!(library.track(ids[1]).unwrap().favorite, "loved → favourite");
    let playlist = library
        .playlists()
        .into_iter()
        .find(|p| p.name == "드라이브")
        .expect("playlist made");
    assert_eq!(track_ids(&library, playlist.id), [ids[1], ids[0]]);
    assert_eq!(library.snapshot().get(ids[0]).unwrap().play_count, 7);
    // What the window shows (the snapshot) has the date too: sorting by date added
    // and `added:` searches go by it.
    let snapshot = library.snapshot();
    assert_eq!(snapshot.get(ids[0]).unwrap().added_at, 978_307_200);
    let recent: Vec<TrackId> = snapshot
        .search("added:30d")
        .into_iter()
        .map(|i| snapshot.tracks[i as usize].id)
        .collect();
    assert!(
        !recent.contains(&ids[0]),
        "added in 2001, not in the last 30 days"
    );
    assert!(recent.contains(&ids[1]));

    // Again: nothing more.
    let again = library.import_apple_music(&file).expect("imports");
    assert_eq!(again.playlists, 0);
    assert_eq!(library.track(ids[0]).unwrap().play_count, 7);
    assert_eq!(library.playlists().len(), 1);

    // Not an export.
    fs::write(&file, "<html></html>").unwrap();
    assert!(library.import_apple_music(&file).is_err());
}

/// An export made on another computer names files where they are there. A library
/// whose music was copied from that computer (a phone's) finds its songs by the folders
/// they are in and their names; asked to go by the place alone, it finds none.
#[test]
fn an_apple_music_export_from_another_computer_is_matched_by_folder_and_name() {
    let env = TestEnv::new();
    let (library, ids) = library_with_tracks(&env);
    // The same folders and file names, below a music folder that is not here.
    let elsewhere = |id: TrackId| {
        let path = library.track(id).unwrap().path.clone();
        let parts: Vec<String> = path
            .components()
            .rev()
            .take(2)
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let mut url = String::from("file:///Users/someone/Music/Media/Artist/");
        for (index, part) in parts.iter().rev().enumerate() {
            if index > 0 {
                url.push('/');
            }
            for byte in part.bytes() {
                if byte.is_ascii_alphanumeric() || b"-_.".contains(&byte) {
                    url.push(byte as char);
                } else {
                    url.push_str(&format!("%{byte:02X}"));
                }
            }
        }
        url
    };
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>Tracks</key><dict>
<key>1</key><dict><key>Track ID</key><integer>1</integer><key>Play Count</key><integer>9</integer>
  <key>Location</key><string>{}</string></dict>
<key>2</key><dict><key>Track ID</key><integer>2</integer><key>Loved</key><true/>
  <key>Location</key><string>{}</string></dict>
<key>3</key><dict><key>Track ID</key><integer>3</integer><key>Play Count</key><integer>3</integer>
  <key>Location</key><string>file:///Users/someone/Music/Media/Artist/Nowhere/gone.mp3</string></dict>
</dict><key>Playlists</key><array>
<dict><key>Name</key><string>Mac에서</string><key>Playlist Items</key><array>
  <dict><key>Track ID</key><integer>2</integer></dict><dict><key>Track ID</key><integer>1</integer></dict>
  <dict><key>Track ID</key><integer>3</integer></dict></array></dict>
</array></dict></plist>"#,
        elsewhere(ids[0]),
        elsewhere(ids[1]),
    );
    let file = env.dir.path().join("Library.xml");
    fs::write(&file, xml).unwrap();

    // By the place alone (as on the computer the export was made on): nothing is here.
    let by_place = library.import_apple_music(&file).expect("imports");
    assert_eq!(
        (by_place.matched, by_place.unmatched, by_place.playlists),
        (0, 3, 0)
    );
    assert_eq!(library.track(ids[0]).unwrap().play_count, 0);

    let result = library
        .import_apple_music_from(&file, true)
        .expect("imports");
    assert_eq!(
        (result.matched, result.unmatched, result.playlists),
        (2, 1, 1)
    );
    assert_eq!(library.track(ids[0]).unwrap().play_count, 9);
    assert!(library.track(ids[1]).unwrap().favorite);
    let playlist = library
        .playlists()
        .into_iter()
        .find(|p| p.name == "Mac에서")
        .expect("playlist made");
    assert_eq!(track_ids(&library, playlist.id), [ids[1], ids[0]]);
}

/// The app's own library data, written on one device and taken in on others whose
/// music is the same folders and files below a music folder that lies elsewhere.
#[test]
fn library_data_written_here_is_taken_in_on_another_device() {
    use oneum::library::backup::{Search, is_of_a_later_version};
    let env = TestEnv::new();
    let (library, ids) = library_with_tracks(&env);
    library.record_play(ids[0]);
    library.record_play(ids[2]);
    library.set_favorite(&[ids[1]], true).unwrap();
    let list = library.create_playlist("드라이브").unwrap();
    library.add_to_playlist(list, &[ids[1], ids[0]]).unwrap();
    let searches = [Search {
        name: "Loved".into(),
        query: "fav:yes".into(),
    }];
    let file = env.dir.path().join("OnEum Library.json");
    let written = library.export_data(&file, &searches).expect("writes");
    assert_eq!((written.songs, written.playlists), (5, 1));
    // The file names songs from their music folder on, and tells nothing of where that
    // folder lies on this device.
    let text = fs::read_to_string(&file).unwrap();
    assert!(text.contains("music/앨범/곡 1.wav"), "{text}");
    assert!(!text.contains(&*env.dir.path().to_string_lossy()));

    // Another device with the same music. It has heard a song itself, just now; what
    // the file tells of happened long before.
    let mut data: serde_json::Value = serde_json::from_str(&text).unwrap();
    for play in data["plays"].as_array_mut().unwrap() {
        play[1] = serde_json::json!(1_000_000_000 + play[0].as_i64().unwrap());
    }
    data["songs"][0]["added"] = serde_json::json!(978_307_200);
    fs::write(&file, data.to_string()).unwrap();
    let there = TestEnv::new();
    let (other, theirs) = library_with_tracks(&there);
    other.record_play(theirs[4]);
    let result = other.import_data(&file).expect("takes in");
    assert_eq!(
        (result.matched, result.unmatched, result.playlists),
        (5, 0, 1)
    );
    assert_eq!(result.searches, searches);
    let first = other.track(theirs[0]).unwrap();
    assert_eq!(first.play_count, 1);
    assert_eq!(
        first.last_played_at,
        library.track(ids[0]).unwrap().last_played_at
    );
    assert_eq!(first.added_at, 978_307_200, "the earlier date added");
    assert!(other.track(theirs[1]).unwrap().favorite);
    assert!(!other.track(theirs[0]).unwrap().favorite);
    let playlist = other
        .playlists()
        .into_iter()
        .find(|p| p.name == "드라이브")
        .expect("playlist made");
    assert_eq!(track_ids(&other, playlist.id), [theirs[1], theirs[0]]);
    // The plays themselves came too, each in its place in time: what this device heard
    // just now is still what was heard last.
    assert_eq!(other.plays_since(0).len(), 3);
    assert_eq!(other.recent_plays(10), [theirs[4], theirs[2], theirs[0]]);
    assert_eq!(other.snapshot().get(theirs[0]).unwrap().play_count, 1);

    // Again: nothing more.
    let again = other.import_data(&file).expect("takes in");
    assert_eq!((again.matched, again.playlists), (5, 0));
    assert_eq!(other.plays_since(0).len(), 3);
    assert_eq!(other.playlists().len(), 1);
    assert_eq!(other.recent_plays(10), [theirs[4], theirs[2], theirs[0]]);

    // A device whose music folder has another name, with one song missing and a
    // playlist of the same name that holds other songs.
    let third = TestEnv::new();
    for i in 1..=4u32 {
        write_tagged_wav(
            &third.dir.path().join(format!("노래/앨범/곡 {i}.wav")),
            200 + i * 100,
            &format!("T{i}"),
            "아이유",
            "앨범",
        );
    }
    let last = third.open();
    last.add_folder(&third.dir.path().join("노래")).unwrap();
    wait_scan(&last);
    let by_title = |title: &str| {
        let snapshot = last.snapshot();
        snapshot
            .tracks
            .iter()
            .find(|t| t.title == title)
            .map(|t| t.id)
            .unwrap()
    };
    let own = last.create_playlist("드라이브").unwrap();
    last.add_to_playlist(own, &[by_title("T3")]).unwrap();
    let result = last.import_data(&file).expect("takes in");
    assert_eq!(
        (result.matched, result.unmatched, result.playlists),
        (4, 1, 1),
        "found by the folders they lie in and their names"
    );
    assert!(last.track(by_title("T2")).unwrap().favorite);
    let marked = last
        .playlists()
        .into_iter()
        .find(|p| p.name == "드라이브 (2)")
        .expect("the name was taken: the list that came in is marked");
    assert_eq!(
        track_ids(&last, marked.id),
        [by_title("T2"), by_title("T1")]
    );
    assert_eq!(track_ids(&last, own), [by_title("T3")]);

    // What is not library data, and data of a later version of the app.
    fs::write(&file, "<html></html>").unwrap();
    let error = last.import_data(&file).unwrap_err();
    assert!(!is_of_a_later_version(&error));
    fs::write(&file, r#"{"format":"oneum-library-data","version":99}"#).unwrap();
    let error = last.import_data(&file).unwrap_err();
    assert!(is_of_a_later_version(&error));
    assert!(matches!(
        last.import_data(&env.dir.path().join("no such file")),
        Err(LibraryError::Io(_))
    ));
}
