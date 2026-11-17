//! Playing a track of a media server on the network (`audio::remote`): what comes out
//! of the decoder is what comes out of it for the same file on disk, whether the file
//! is taken into memory as it arrives or read as it comes, whether or not the server
//! gives parts of a file, and whatever the network does once the file has arrived.
mod net_support;

use net_support::{FakeServer, Track};
use oneum::audio::decoder::TrackDecoder;
use oneum::audio::remote::RemoteFile;
use oneum::audio::source::Budget;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The formats of the fixtures and what a server calls them.
const KINDS: [(&str, &str); 10] = [
    ("gap_a.flac", "audio/flac"),
    ("gap_a.wav", "audio/wav"),
    ("gap_a.aiff", "audio/aiff"),
    ("gap_a.alac.m4a", "audio/mp4"),
    ("gap_a.aac.m4a", "audio/mp4"),
    ("gap_a.mp3", "audio/mpeg"),
    ("gap_a.ogg", "audio/ogg"),
    ("gap_a.opus", "audio/opus"),
    ("gap_a.wv", "audio/x-wavpack"),
    ("silence.ape", "audio/x-ape"),
];

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn serve(names: &[(&str, &str)]) -> FakeServer {
    FakeServer::start(
        names
            .iter()
            .map(|(name, mime)| {
                let body = std::fs::read(fixture(name)).expect("fixture");
                Track::new(name, mime, body).into()
            })
            .collect(),
    )
}

fn rest_of(mut decoder: TrackDecoder) -> Vec<f32> {
    let (mut all, mut block) = (Vec::new(), Vec::new());
    while decoder.next(&mut block).expect("decodes") {
        all.extend_from_slice(&block);
    }
    all
}

fn from_file(name: &str, seek: Option<u64>) -> Vec<f32> {
    let mut decoder = TrackDecoder::open(&fixture(name)).expect("the file opens");
    if let Some(frame) = seek {
        decoder.seek(frame).expect("seeks");
    }
    rest_of(decoder)
}

/// Memory for everything, or for nothing (the file is then read as it comes).
fn budget(in_memory: bool) -> Arc<Budget> {
    Arc::new(Budget::new(if in_memory { 1 << 30 } else { 0 }))
}

fn from_server(url: &str, in_memory: bool, seek: Option<u64>) -> Vec<f32> {
    let file = RemoteFile::open(url).expect("the server answers");
    let (mut decoder, load) =
        TrackDecoder::open_remote_within(file, &budget(in_memory)).expect("opens");
    assert_eq!(load.is_some(), in_memory);
    assert_eq!(decoder.info().in_memory, in_memory);
    if let Some(frame) = seek {
        decoder.seek(frame).expect("seeks");
    }
    rest_of(decoder)
}

#[test]
fn a_servers_file_gives_the_same_samples_as_the_file_itself() {
    let server = serve(&KINDS);
    for (n, (name, _)) in KINDS.iter().enumerate() {
        let expected = from_file(name, None);
        assert!(!expected.is_empty());
        for in_memory in [true, false] {
            let heard = from_server(&server.media_url(n), in_memory, None);
            assert!(heard == expected, "{name}, in memory: {in_memory}");
        }
    }
}

#[test]
fn seeking_in_a_servers_file_lands_where_it_does_in_the_file() {
    let server = serve(&KINDS);
    for (n, (name, _)) in KINDS.iter().enumerate() {
        for frame in [1, 4_000, 20_000] {
            let expected = from_file(name, Some(frame));
            for in_memory in [true, false] {
                let heard = from_server(&server.media_url(n), in_memory, Some(frame));
                assert!(
                    heard == expected,
                    "{name} from frame {frame}, in memory: {in_memory}"
                );
            }
        }
    }
}

/// Some servers send a file only whole. Taken into memory it plays (and seeks) all the
/// same: what the decoder asks for ahead of the bytes that have arrived is waited for.
#[test]
fn a_server_that_gives_no_parts_of_a_file_still_plays_and_seeks() {
    let server = serve(&KINDS);
    server.set_refuse_ranges(true);
    for (n, (name, _)) in KINDS.iter().enumerate() {
        let heard = from_server(&server.media_url(n), true, None);
        assert!(heard == from_file(name, None), "{name}");
        let heard = from_server(&server.media_url(n), true, Some(20_000));
        assert!(
            heard == from_file(name, Some(20_000)),
            "{name} after a seek"
        );
    }
    assert_eq!(
        server.offset_requests(),
        0,
        "nothing was asked for that the server cannot give"
    );
}

/// Once the file is in memory the server is not needed any more: it can go away.
#[test]
fn a_file_that_has_arrived_plays_on_without_the_server() {
    let mut server = serve(&[("gap_a.flac", "audio/flac")]);
    let file = RemoteFile::open(&server.media_url(0)).expect("the server answers");
    let (decoder, load) = TrackDecoder::open_remote_within(file, &budget(true)).expect("opens");
    let load = load.expect("taken into memory");
    let started = Instant::now();
    while !load.is_complete() {
        assert!(started.elapsed() < Duration::from_secs(10), "never arrived");
        std::thread::sleep(Duration::from_millis(5));
    }
    server.stop();
    assert!(rest_of(decoder) == from_file("gap_a.flac", None));
}

/// A server that is gone, or does not have the file, is an error at once, not a wait.
#[test]
fn a_file_the_server_does_not_have_fails_at_once() {
    let mut server = serve(&[("gap_a.flac", "audio/flac")]);
    let missing = server.url("/media/99");
    let started = Instant::now();
    let error = RemoteFile::open(&missing).err().expect("no such file");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    let url = server.media_url(0);
    server.stop();
    assert!(RemoteFile::open(&url).is_err(), "nobody listens there now");
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// A file that is read as it comes and whose server goes away in the middle ends in
/// an error (or early), within the time a read is given, not in a hang.
#[test]
fn a_server_that_goes_away_mid_file_does_not_hang_the_decoder() {
    let mut server = serve(&[("gap_a.wav", "audio/wav")]);
    let file = RemoteFile::open(&server.media_url(0)).expect("the server answers");
    let (mut decoder, _) = TrackDecoder::open_remote_within(file, &budget(false)).expect("opens");
    let mut block = Vec::new();
    assert!(decoder.next(&mut block).expect("the start decodes"));
    server.stop();
    let started = Instant::now();
    let mut frames = block.len();
    // To its end, or to the error the missing server makes of it.
    while let Ok(true) = decoder.next(&mut block) {
        frames += block.len();
    }
    assert!(started.elapsed() < Duration::from_secs(15));
    assert!(frames <= from_file("gap_a.wav", None).len());
}

/// A connection that is cut in the middle of a song (the network away for a moment, a
/// server that closes what has been open for long) is made anew at the place it
/// stopped at: the song is whole, in memory and read as it comes.
#[test]
fn a_connection_cut_in_the_middle_of_a_song_is_made_anew() {
    use net_support::{HttpServer, Reply};
    use std::sync::Mutex;
    let body = std::fs::read(fixture("gap_a.flac")).expect("fixture");
    let seen = Arc::new(Mutex::new(Vec::<usize>::new()));
    let server = {
        let (body, seen) = (body.clone(), seen.clone());
        HttpServer::start(move |request| {
            let from: usize = request
                .header("range")
                .and_then(|range| range.strip_prefix("bytes="))
                .and_then(|range| range.trim_end_matches('-').parse().ok())
                .unwrap_or(0);
            seen.lock().unwrap().push(from);
            let part = &body[from.min(body.len())..];
            let head = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Type: audio/flac\r\n\
                 Accept-Ranges: bytes\r\nContent-Range: bytes {from}-{}/{}\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len() - 1,
                body.len(),
                part.len()
            );
            let mut bytes = head.into_bytes();
            // What is asked for from the start ends after a third of what was
            // announced; what is asked for from a place in the file arrives whole.
            bytes.extend_from_slice(if from == 0 {
                &part[..part.len() / 3]
            } else {
                part
            });
            Reply::Bytes(bytes)
        })
    };
    let expected = from_file("gap_a.flac", None);
    for in_memory in [true, false] {
        seen.lock().unwrap().clear();
        let heard = from_server(&server.url("/song.flac"), in_memory, None);
        assert!(heard == expected, "in memory: {in_memory}");
        let seen = seen.lock().unwrap();
        assert!(
            seen.contains(&(body.len() / 3)),
            "asked anew where the first connection ended: {seen:?}"
        );
    }
}

/// A track of a server that is fed anew (a seek, the next round of a repeated section,
/// a setting that begins the feed again) is read on from where it is in memory: the
/// server is not asked for the file a second time.
#[test]
fn a_servers_track_that_is_fed_anew_is_not_fetched_again() {
    use oneum::audio::feed::{NextSlot, NextTrack, TrackRequest};
    use oneum::audio::{Engine, EngineEvent, ReplayGainSettings};
    use oneum::model::ReplayGainTags;
    let server = serve(&[("gap_a.flac", "audio/flac")]);
    let (mut engine, mut renderer) =
        Engine::new_offline(44_100, 1.0, ReplayGainSettings::default());
    let request = TrackRequest {
        item_id: 1,
        track_id: -1,
        path: PathBuf::from(server.media_url(0)),
        replaygain: ReplayGainTags::default(),
        crossfade_ms: 0,
    };
    let slot = NextSlot::new();
    slot.set(NextTrack::End);
    // Plays until `frames` have been heard of the feed that is on, or to its end.
    let mut play = |engine: &mut Engine, frames: u64| -> bool {
        let mut out = vec![0f32; 1024];
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            assert!(Instant::now() < deadline, "the feed did not get there");
            let mut events = Vec::new();
            engine.poll(&mut events);
            if events
                .iter()
                .any(|event| matches!(event, EngineEvent::Drained { .. }))
            {
                return true;
            }
            match engine.active_buffered() {
                Some((buffered, finished)) if buffered >= 512 || finished => {
                    renderer.render(&mut out, 2);
                    if engine.shared().position().1 >= frames {
                        return false;
                    }
                }
                _ => std::thread::sleep(Duration::from_millis(1)),
            }
        }
    };
    engine
        .start_feed(request.clone(), 0, slot.clone())
        .expect("feed starts");
    engine.set_playing(true);
    assert!(!play(&mut engine, 8_000), "still playing");
    let asked = server.media_requests();
    assert!(asked >= 1);
    for start_ms in [500, 100, 900] {
        engine
            .start_feed(request.clone(), start_ms, slot.clone())
            .expect("feed starts");
        assert!(!play(&mut engine, 2_000), "plays on from {start_ms} ms");
    }
    assert!(play(&mut engine, u64::MAX), "plays to its end");
    assert_eq!(
        server.media_requests(),
        asked,
        "nothing more was asked of the server"
    );
    // Stopped, the file is let go of: played again, it is asked for again.
    engine.stop_feeds();
    engine.start_feed(request, 0, slot).expect("feed starts");
    assert!(play(&mut engine, u64::MAX));
    assert_eq!(server.media_requests(), asked + 1);
}
