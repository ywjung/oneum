//! Lyrics from the internet against a lyrics service that runs here: what the app
//! asks, how often, what it makes of the answers, and that a song the file of which
//! has its own synced lyrics is not asked for at all. The stand-in reads the requests
//! with code of its own, not with the app's.
mod net_support;

use net_support::{HttpServer, Reply, Request, response};
use oneum::library::details::{self, Details};
use oneum::library::lyrics::Lyrics;
use oneum::library::online_lyrics::Source;
use oneum::net::lyrics::Song;
use std::sync::{Arc, Mutex};

const SYNCED: &str = "[00:01.00] 첫 줄\n[00:03.50] 둘째 줄\n[00:06.00] third line";
const PLAIN: &str = "첫 줄\n둘째 줄\nthird line";

/// A value of a query as it was meant: `%XX` back to bytes.
fn unescaped(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).expect("hex");
            out.push(u8::from_str_radix(hex, 16).expect("hex digits"));
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).expect("utf-8")
}

/// The names and values of a request's query.
fn query(request: &Request) -> Vec<(String, String)> {
    let (_, query) = request.path.split_once('?').unwrap_or((&request.path, ""));
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (name.to_string(), unescaped(value))
        })
        .collect()
}

fn value<'a>(query: &'a [(String, String)], name: &str) -> &'a str {
    query
        .iter()
        .find(|(key, _)| key == name)
        .map_or("", |(_, value)| value.as_str())
}

/// A lyrics service that knows "밤편지" by 아이유 (with times), "Words Only" (without)
/// and "Humming" (no words at all), and keeps every request it gets.
fn service() -> (HttpServer, Arc<Mutex<Vec<Request>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let server = {
        let seen = seen.clone();
        HttpServer::start(move |request| {
            seen.lock().unwrap().push(request.clone());
            if request.method != "GET" || !request.path.starts_with("/api/get?") {
                return Reply::Bytes(response(400, "Bad Request", &[], b""));
            }
            let query = query(request);
            // As the real service: a length of more than an hour is refused, and a
            // song is found under an album only if the album is the one it has.
            let seconds = value(&query, "duration").parse::<u64>().unwrap_or(1);
            if !(1..=3600).contains(&seconds) {
                return Reply::Bytes(response(400, "Bad Request", &[], b"duration"));
            }
            let album = query.iter().find(|(key, _)| key == "album_name");
            if album.is_some_and(|(_, album)| album != "Palette") {
                let body = br#"{"statusCode":404,"name":"TrackNotFound","message":"none"}"#;
                return Reply::Bytes(response(404, "Not Found", &[], body));
            }
            let body = match (value(&query, "track_name"), value(&query, "artist_name")) {
                ("밤편지", "아이유 (IU)") => serde_json::json!({
                    "id": 7, "trackName": "밤편지", "artistName": "아이유 (IU)",
                    "instrumental": false, "plainLyrics": PLAIN, "syncedLyrics": SYNCED,
                }),
                ("Words Only", _) => serde_json::json!({
                    "id": 8, "instrumental": false, "plainLyrics": PLAIN, "syncedLyrics": null,
                }),
                ("Humming", _) => serde_json::json!({
                    "id": 9, "instrumental": true, "plainLyrics": null, "syncedLyrics": null,
                }),
                ("Garbage", _) => {
                    return Reply::Bytes(response(200, "OK", &[], b"<html>not the service</html>"));
                }
                ("Endless", _) => {
                    let body = vec![b' '; (1 << 20) + 1];
                    return Reply::Bytes(response(200, "OK", &[], &body));
                }
                _ => {
                    let body = br#"{"code":404,"name":"TrackNotFound","message":"none"}"#;
                    return Reply::Bytes(response(404, "Not Found", &[], body));
                }
            };
            let json = [("Content-Type", "application/json")];
            Reply::Bytes(response(200, "OK", &json, body.to_string().as_bytes()))
        })
    };
    (server, seen)
}

fn night_letter() -> Song<'static> {
    Song {
        title: "밤편지",
        artist: "아이유 (IU)",
        album: "Palette",
        duration_ms: 253_400,
    }
}

fn song(title: &'static str) -> Song<'static> {
    Song {
        title,
        artist: "Somebody",
        album: "",
        duration_ms: 0,
    }
}

#[test]
fn a_song_is_asked_for_once_by_its_title_artist_album_and_length_and_nothing_else() {
    let (mut server, seen) = service();
    let cache = tempfile::tempdir().unwrap();
    let source = Source::new(cache.path(), &server.url(""));
    let lyrics = source
        .find(&night_letter())
        .expect("the service knows the song");
    let lines = lyrics.timed.as_ref().expect("with times");
    assert_eq!(lines.len(), 3);
    assert_eq!((lines[1].at_ms, lines[1].text.as_str()), (3_500, "둘째 줄"));
    assert_eq!(lyrics.plain, PLAIN);

    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let request = &seen[0];
        assert_eq!(
            query(request),
            [
                ("track_name".to_string(), "밤편지".to_string()),
                ("artist_name".to_string(), "아이유 (IU)".to_string()),
                ("album_name".to_string(), "Palette".to_string()),
                ("duration".to_string(), "253".to_string()),
            ]
        );
        // The request says who asks only as far as "the app": no account, nothing
        // that tells one listener from another, nothing about the device.
        let mut names: Vec<&str> = request.headers.iter().map(|(n, _)| n.as_str()).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            ["accept-encoding", "connection", "host", "user-agent"]
        );
        // (The app's name and version, as the service asks of its clients.)
        let agent = request.header("user-agent").expect("a user agent");
        assert_eq!(agent, format!("OnEum/{}", env!("CARGO_PKG_VERSION")));
        assert!(request.body.is_empty());
    }

    // What came back is kept: the same song is not asked for again, by this source or
    // by one that starts later (the app after a restart).
    assert_eq!(source.find(&night_letter()), Some(lyrics.clone()));
    let later = Source::new(cache.path(), &server.url(""));
    assert_eq!(later.find(&night_letter()), Some(lyrics));
    assert_eq!(seen.lock().unwrap().len(), 1);
    // Nothing that is kept is named after the song.
    for entry in std::fs::read_dir(cache.path().join("lyrics")).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        assert!(name.is_ascii() && !name.contains("IU"), "{name}");
    }
    server.stop();
}

#[test]
fn what_the_service_does_not_have_is_remembered_and_what_it_cannot_say_is_not() {
    let (mut server, seen) = service();
    let cache = tempfile::tempdir().unwrap();
    let source = Source::new(cache.path(), &server.url("/"));
    let asked = || seen.lock().unwrap().len();

    // A song it does not know, and one without words: asked once each.
    assert_eq!(source.find(&song("Nobody Knows This")), None);
    assert_eq!(source.find(&song("Nobody Knows This")), None);
    assert_eq!(asked(), 1);
    assert_eq!(source.find(&song("Humming")), None);
    assert_eq!(source.find(&song("Humming")), None);
    assert_eq!(asked(), 2);
    // Words without times are words all the same.
    let words = source.find(&song("Words Only")).expect("plain lyrics");
    assert!(words.timed.is_none());
    assert_eq!(words.plain, PLAIN);
    assert_eq!(asked(), 3);
    // A song without an artist, or without a title, is not asked for.
    let unnamed = Song {
        artist: "  ",
        ..song("Untitled")
    };
    assert_eq!(source.find(&unnamed), None);
    assert_eq!(source.find(&song("")), None);
    assert_eq!(asked(), 3);

    // An answer that is not the service's is no lyrics, nothing is kept of it, and
    // the service is left alone for a while after it: the next song is not asked for.
    assert_eq!(source.find(&song("Garbage")), None);
    assert_eq!(asked(), 4);
    assert_eq!(source.find(&song("Another One")), None);
    assert_eq!(asked(), 4);
    // (What is kept still answers meanwhile.)
    assert!(source.find(&song("Words Only")).is_some());
    // An answer without an end is not read to its end.
    let fresh = Source::new(cache.path(), &server.url(""));
    assert_eq!(fresh.find(&song("Endless")), None);
    assert_eq!(asked(), 5);
    // Neither of the two was kept: a source that starts later asks again.
    let later = Source::new(cache.path(), &server.url(""));
    assert_eq!(later.find(&song("Garbage")), None);
    assert_eq!(asked(), 6);
    server.stop();
}

#[test]
fn a_file_with_synced_lyrics_of_its_own_is_not_asked_for() {
    let (mut server, seen) = service();
    let cache = tempfile::tempdir().unwrap();
    let source = Source::new(cache.path(), &server.url(""));
    let asked = || seen.lock().unwrap().len();
    let of_the_file = |text: &str| Details {
        lyrics: Lyrics::from_text(text),
        ..Details::default()
    };

    // The file's own lyrics follow the song: they stay, and nothing is asked.
    let own = of_the_file("[00:02.00] the file's own line\n[00:04.00] and its second");
    assert!(!own.wants_online_lyrics());
    let kept = details::with_online_lyrics(own.clone(), &source, &night_letter());
    assert_eq!(kept, own);
    assert!(!kept.lyrics_online);
    assert_eq!(asked(), 0);

    // The file has words without times and the service has them with times: those
    // of the service are shown, and said to be from there.
    let plain = of_the_file("the file's words\nwithout times");
    assert!(plain.wants_online_lyrics());
    let found = details::with_online_lyrics(plain.clone(), &source, &night_letter());
    assert!(found.lyrics_online);
    assert_eq!(
        found
            .lyrics
            .as_ref()
            .and_then(|l| l.timed.as_ref())
            .map(Vec::len),
        Some(3)
    );
    assert_eq!(asked(), 1);

    // The service has only words too: the file's own stay.
    let same = details::with_online_lyrics(plain.clone(), &source, &song("Words Only"));
    assert_eq!(same, plain);
    assert_eq!(asked(), 2);

    // The file has nothing: whatever the service has is shown; a song it does not
    // know stays without lyrics.
    let none = Details::default();
    let words = details::with_online_lyrics(none.clone(), &source, &song("Words Only"));
    assert!(words.lyrics_online && words.lyrics.is_some());
    let unknown = details::with_online_lyrics(none.clone(), &source, &song("Unknown"));
    assert_eq!(unknown, none);
    assert_eq!(asked(), 3);
    server.stop();
}

#[test]
fn a_service_that_is_not_there_costs_one_try_and_no_lyrics() {
    // A port nothing listens on.
    let port = {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.local_addr().unwrap().port()
    };
    let cache = tempfile::tempdir().unwrap();
    let source = Source::new(cache.path(), &format!("http://127.0.0.1:{port}"));
    let started = std::time::Instant::now();
    assert_eq!(source.find(&night_letter()), None);
    assert_eq!(source.find(&song("Words Only")), None);
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    // Nothing was kept: the songs are asked for again when the service is back.
    assert!(
        !cache.path().join("lyrics").exists()
            || std::fs::read_dir(cache.path().join("lyrics"))
                .unwrap()
                .next()
                .is_none()
    );
    // An address that is none is no lyrics either.
    let nowhere = Source::new(cache.path(), "ftp://lyrics.example");
    assert_eq!(nowhere.find(&night_letter()), None);
}

/// The service finds a song under an album only as it spells the album, and takes no
/// length of more than an hour: a song whose album is written otherwise is found
/// without it, and a long one is asked for without its length.
#[test]
fn another_spelling_of_the_album_and_a_very_long_song_are_found_all_the_same() {
    let (mut server, seen) = service();
    let cache = tempfile::tempdir().unwrap();
    let source = Source::new(cache.path(), &server.url(""));
    let asked = || seen.lock().unwrap().len();
    let other_album = Song {
        album: "Palette (Deluxe Edition)",
        ..night_letter()
    };
    let lyrics = source.find(&other_album).expect("found without the album");
    assert_eq!(lyrics.timed.as_ref().map(Vec::len), Some(3));
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "once with the album, once without");
        assert_eq!(
            value(&query(&seen[0]), "album_name"),
            "Palette (Deluxe Edition)"
        );
        let second = query(&seen[1]);
        assert!(second.iter().all(|(key, _)| key != "album_name"));
        assert_eq!(value(&second, "track_name"), "밤편지");
        assert_eq!(value(&second, "duration"), "253");
    }
    // Kept: not asked for again.
    assert!(source.find(&other_album).is_some());
    assert_eq!(asked(), 2);

    // A song of an hour and a half: asked for without a length, and found.
    let long = Song {
        duration_ms: 90 * 60 * 1000,
        ..song("Words Only")
    };
    assert!(source.find(&long).is_some());
    let last = seen.lock().unwrap().last().cloned().unwrap();
    assert!(query(&last).iter().all(|(key, _)| key != "duration"));
    assert_eq!(asked(), 3);

    // A song the service knows under no album: asked for twice, remembered as none.
    let unknown = Song {
        album: "Some Album",
        ..song("Nobody Knows This")
    };
    assert_eq!(source.find(&unknown), None);
    assert_eq!(source.find(&unknown), None);
    assert_eq!(asked(), 5);
    server.stop();
}

/// A service that says it is asked too much is left alone for a good while, and
/// nothing is kept of the song that was asked for.
#[test]
fn a_service_that_is_asked_too_much_is_left_alone() {
    let seen = Arc::new(Mutex::new(0u32));
    let mut server = {
        let seen = seen.clone();
        HttpServer::start(move |_| {
            *seen.lock().unwrap() += 1;
            let wait = [("Retry-After", "30")];
            Reply::Bytes(response(429, "Too Many Requests", &wait, b""))
        })
    };
    let cache = tempfile::tempdir().unwrap();
    let source = Source::new(cache.path(), &server.url(""));
    assert_eq!(source.find(&night_letter()), None);
    assert_eq!(source.find(&song("Words Only")), None);
    assert_eq!(source.find(&night_letter()), None);
    assert_eq!(*seen.lock().unwrap(), 1, "one request, then none");
    assert!(!cache.path().join("lyrics").exists());
    server.stop();
}
