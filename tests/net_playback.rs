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
