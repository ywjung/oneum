//! The glue between the player and the library: session persistence and track lookup.
use oneum::app::host::LibraryHost;
use oneum::library::{Library, LibraryPaths};
use oneum::playback::{PlayerHost, QueueItem, QueueState, RepeatMode, Session};
use std::path::Path;
use std::sync::Arc;

fn write_wav(path: &Path, frames: u32) {
    let data: Vec<u8> = (0..frames)
        .flat_map(|i| (((i % 64) as i16 - 32) * 200).to_le_bytes())
        .collect();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
    bytes.extend_from_slice(&8000u32.to_le_bytes());
    bytes.extend_from_slice(&16000u32.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&data);
    std::fs::write(path, bytes).expect("write wav");
}

fn open(dir: &Path) -> Arc<Library> {
    let paths = LibraryPaths {
        data_dir: dir.join("data"),
        cache_dir: dir.join("cache"),
    };
    std::fs::create_dir_all(&paths.data_dir).expect("data dir");
    std::fs::create_dir_all(&paths.cache_dir).expect("cache dir");
    Library::open(paths).expect("library opens").0
}

#[test]
fn session_survives_a_restart_and_tracks_resolve() {
    let dir = tempfile::tempdir().expect("temp dir");
    let music = dir.path().join("music");
    std::fs::create_dir_all(&music).expect("music dir");
    write_wav(&music.join("하나.wav"), 4000);
    write_wav(&music.join("둘.wav"), 8000);

    let library = open(dir.path());
    let ids = library.import_files(&[music.join("하나.wav"), music.join("둘.wav")]);
    assert_eq!(ids.len(), 2);
    let host = LibraryHost::new(library.clone(), || {});
    assert!(
        host.load_session().is_none(),
        "a fresh profile has no session"
    );

    let playable = host.track(ids[1]).expect("track resolves");
    assert!(playable.available);
    assert_eq!(playable.duration_ms, 1000);
    assert!(playable.path.ends_with("둘.wav"));
    assert!(host.track(987_654).is_none());

    let session = Session {
        queue: QueueState {
            // The same track twice: queue items are distinct from tracks.
            items: vec![
                QueueItem {
                    id: 11,
                    track: ids[1],
                },
                QueueItem {
                    id: 12,
                    track: ids[0],
                },
                QueueItem {
                    id: 13,
                    track: ids[1],
                },
            ],
            current: Some(2),
            shuffle: true,
            repeat: RepeatMode::One,
            unshuffled: vec![12, 11, 13],
        },
        position_ms: 432,
        volume: 0.37,
    };
    assert!(host.save_session(&session, true));
    host.record_play(ids[0]);
    // A later save without queue changes must not lose the stored queue.
    let mut moved = session.clone();
    moved.position_ms = 777;
    assert!(host.save_session(&moved, false));
    drop(host);
    library.shutdown();
    drop(library);

    let library = open(dir.path());
    let host = LibraryHost::new(library.clone(), || {});
    assert_eq!(host.load_session(), Some(moved));
    assert_eq!(library.recent_plays(10), vec![ids[0]]);
    assert_eq!(library.track(ids[0]).expect("track").play_count, 1);
    library.shutdown();
}

/// A session whose queue cannot be written is not saved in part: what is stored stays
/// the previous session, whole, and the next successful save brings both up to date.
#[test]
fn a_session_is_saved_whole_or_not_at_all() {
    let dir = tempfile::tempdir().expect("temp dir");
    let music = dir.path().join("music");
    std::fs::create_dir_all(&music).expect("music dir");
    for name in ["a.wav", "b.wav", "c.wav"] {
        write_wav(&music.join(name), 4000);
    }
    let library = open(dir.path());
    let ids = library.import_files(&[
        music.join("a.wav"),
        music.join("b.wav"),
        music.join("c.wav"),
    ]);
    let host = LibraryHost::new(library.clone(), || {});
    let session = |tracks: &[i64], current: usize, position_ms: u64| Session {
        queue: QueueState {
            items: tracks
                .iter()
                .enumerate()
                .map(|(i, &track)| QueueItem {
                    id: i as u64 + 1,
                    track,
                })
                .collect(),
            current: Some(current),
            shuffle: false,
            repeat: RepeatMode::Off,
            unshuffled: Vec::new(),
        },
        position_ms,
        volume: 0.5,
    };
    let first = session(&[ids[0], ids[1], ids[2]], 2, 100);
    assert!(host.save_session(&first, true));

    // Writing the queue fails (here: a trigger stands in for a full disk).
    let db = rusqlite::Connection::open(dir.path().join("data/library.db")).expect("db");
    db.execute_batch(
        "CREATE TRIGGER fail_queue BEFORE INSERT ON queue \
         BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
    )
    .expect("trigger");
    let second = session(&[ids[1]], 0, 5000);
    assert!(!host.save_session(&second, true), "the failure is reported");
    // Not "the new current item of the old queue": the old session, unchanged.
    assert_eq!(host.load_session(), Some(first));

    db.execute_batch("DROP TRIGGER fail_queue")
        .expect("drop trigger");
    assert!(host.save_session(&second, true));
    assert_eq!(host.load_session(), Some(second));
    library.shutdown();
}

/// Quitting waits for the player thread, but not for ever.
#[test]
fn quitting_does_not_wait_for_a_thread_that_never_ends() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};
    // A thread that ends in time is joined.
    let quick = std::thread::spawn(|| std::thread::sleep(Duration::from_millis(30)));
    assert!(oneum::app::join_within(quick, Duration::from_secs(5)));
    // One that does not is given up on after the patience runs out, not earlier.
    let release = Arc::new(AtomicBool::new(false));
    let flag = release.clone();
    let stuck = std::thread::spawn(move || {
        while !flag.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    let started = Instant::now();
    assert!(!oneum::app::join_within(stuck, Duration::from_millis(200)));
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_millis(200) && waited < Duration::from_secs(2),
        "waited {waited:?}"
    );
    release.store(true, Ordering::Relaxed);
}

/// What the app measured for an untagged file is what the player levels it with.
#[test]
fn the_player_is_given_measured_loudness() {
    let dir = tempfile::tempdir().expect("temp dir");
    let music = dir.path().join("music");
    std::fs::create_dir_all(&music).expect("music dir");
    write_wav(&music.join("untagged.wav"), 16_000);
    let library = open(dir.path());
    let ids = library.import_files(&[music.join("untagged.wav")]);
    let host = LibraryHost::new(library.clone(), || {});
    let before = host.track(ids[0]).expect("track resolves").replaygain;
    assert_eq!(before.track_gain_db, None);

    assert!(library.measure_loudness());
    assert!(library.wait_for_loudness(std::time::Duration::from_secs(30)));
    let after = host.track(ids[0]).expect("track resolves").replaygain;
    let gain = after.track_gain_db.expect("a measured gain");
    assert!(gain.is_finite() && gain.abs() < 30.0, "{gain}");
    assert!(
        after
            .track_peak
            .is_some_and(|peak| peak > 0.0 && peak <= 1.0)
    );
}
