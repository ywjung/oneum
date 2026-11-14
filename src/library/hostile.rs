//! Damaged and hostile text given to the library: playlists, lyrics, tag values in old
//! encodings, searches. Nothing may panic or grow without bound, and a playlist may not
//! lead anywhere but to local files.
use super::search::{PreparedQuery, choseong_form, normalize};
use super::{details, legacy, lyrics, m3u};
use crate::testing::Noise;
use std::path::Path;

/// Random bytes, and text built from `pieces`, by turns.
fn inputs(seed: u64, rounds: usize, pieces: &[&str]) -> impl Iterator<Item = Vec<u8>> {
    let mut noise = Noise::new(seed);
    let pieces: Vec<String> = pieces.iter().map(|p| p.to_string()).collect();
    (0..rounds).map(move |round| {
        if round % 3 == 0 {
            let len = noise.below(400);
            noise.bytes(len)
        } else {
            let refs: Vec<&str> = pieces.iter().map(String::as_str).collect();
            let count = noise.below(50);
            noise.text(&refs, count).into_bytes()
        }
    })
}

#[test]
fn playlists_of_any_content_name_local_files_only() {
    let pieces = [
        "../",
        "..\\",
        "/",
        "\\\\server\\share\\",
        "//server/share/",
        "file://",
        "file://host/",
        "file:///",
        "http://",
        "smb://",
        "C:\\",
        "~",
        "%2e%2e%2f",
        "%",
        "%zz",
        "%00",
        "%ff",
        "#EXTM3U",
        "#EXTINF:1,",
        "#",
        "\r\n",
        "\n",
        "\r",
        " ",
        "\t",
        "\u{feff}",
        "\u{0}",
        "노래",
        "song.flac",
        "a",
    ];
    let dir = tempfile::tempdir().unwrap();
    for bytes in inputs(21, 3_000, &pieces) {
        let text = m3u::decode(&bytes);
        let entries = m3u::parse(&text, dir.path());
        assert!(entries.len() <= m3u::MAX_ENTRIES);
        for entry in entries {
            let Some(path) = entry.path else { continue };
            // What is followed is a place on this machine, fully spelled out.
            assert!(path.is_absolute(), "{:?} from {text:?}", path);
            let shown = path.to_string_lossy();
            assert!(
                !shown.starts_with("//") && !shown.starts_with("\\\\"),
                "{shown:?}"
            );
            assert!(!shown.contains("://"), "{shown:?}");
            assert!(
                !path.components().any(|c| c.as_os_str() == ".."),
                "{shown:?}"
            );
        }
    }
}

#[test]
fn lyrics_of_any_content_stay_within_limits() {
    let pieces = [
        "[",
        "]",
        "<",
        ">",
        "00:",
        "59",
        "99999999999999999999",
        ":",
        ".",
        ",",
        "-",
        "+",
        "[offset:",
        "[offset:-99999999999999999999]",
        "[ar:",
        "[00:01.00]",
        "[99:99.999]",
        "[1:2]",
        "\n",
        "\r\n",
        "\r",
        " ",
        "\u{feff}",
        "가사",
        "la",
        "\u{0}",
    ];
    for bytes in inputs(23, 4_000, &pieces) {
        let text = String::from_utf8_lossy(&bytes);
        if let Some(lines) = lyrics::timed(&text) {
            assert!(lines.len() <= 5_000);
            assert!(lines.iter().all(|l| !l.text.contains('\n')), "{text:?}");
        }
        if let Some(plain) = lyrics::clean(&text) {
            assert!(!plain.is_empty());
            assert!(plain.chars().count() <= 60_001, "{}", plain.len());
        }
    }
}

#[test]
fn bytes_in_any_encoding_are_text_or_nothing() {
    let mut noise = Noise::new(29);
    for round in 0..6_000 {
        let len = noise.below(300);
        let mut bytes = noise.bytes(len);
        // Byte order marks in front of noise, and an odd number of bytes after one.
        let mark: &[u8] = match round % 4 {
            0 => &[0xFF, 0xFE],
            1 => &[0xFE, 0xFF],
            2 => &[0xEF, 0xBB, 0xBF],
            _ => &[],
        };
        bytes.splice(0..0, mark.iter().copied());
        let _ = legacy::is_korean_cp949(&bytes);
        if let Some(text) = legacy::decode_file(&bytes) {
            // No conversion makes more characters than there were bytes.
            assert!(text.chars().count() <= bytes.len());
        }
        let as_latin1: String = bytes.iter().map(|b| char::from(*b)).collect();
        if let Some(text) = legacy::repair(&as_latin1) {
            assert!(text.chars().count() <= bytes.len());
        }
    }
}

#[test]
fn names_in_tags_are_few_and_short_whatever_the_tag_holds() {
    let pieces = [
        ";",
        " / ",
        "/",
        "\0",
        ",",
        " ",
        "Bach, Johann Sebastian",
        "한",
        "\n",
        "a",
    ];
    for bytes in inputs(31, 2_000, &pieces) {
        let text = String::from_utf8_lossy(&bytes).repeat(8);
        let found = details::names([text.as_str(), text.as_str()]);
        assert!(found.len() <= 12);
        assert!(
            found
                .iter()
                .all(|n| n.chars().count() <= 120 && !n.is_empty())
        );
    }
}

#[test]
fn any_search_is_understood_or_finds_nothing() {
    let pieces = [
        "artist:",
        "아티스트:",
        "year:",
        "plays:",
        "fav:",
        "added:",
        "played:",
        "format:",
        "genre:",
        ":",
        "::",
        "\"",
        "-",
        "..",
        ">=",
        "<=",
        ">",
        "<",
        "=",
        "1990",
        "1990-1999",
        "99999999999999999999999",
        "-1",
        "0",
        "30d",
        "9999999999999999y",
        "99999999999w",
        "3달",
        "1년",
        "일",
        "yes",
        "never",
        " ",
        "\t",
        "ㄱㄴ",
        "가",
        "é",
        "\u{0}",
        "\u{1f}",
        "Re:Zero",
        "a",
    ];
    for now in [i64::MIN, -1, 0, 1_800_000_000, i64::MAX] {
        for bytes in inputs(37, 3_000, &pieces) {
            let text = String::from_utf8_lossy(&bytes);
            let query = PreparedQuery::new_at(&text, now);
            let _ = query.is_empty();
            let normalized = normalize(&text);
            let _ = choseong_form(&normalized);
        }
    }
}

/// CUE sheets of any content: nothing panics, and what comes out is a sheet the rest of
/// the library can rely on: its tracks start one after the other, each number once, few
/// enough, with names of bounded length; and the lengths and sample positions worked
/// out from it stay in range whatever the audio's length is.
#[test]
fn sheets_of_any_content_are_sheets_or_nothing() {
    use super::cue;
    let pieces = [
        "FILE ",
        "\"image.wav\" WAVE",
        "image.wav WAVE",
        "\"..\\..\\x.flac\"",
        "TRACK ",
        "01",
        "02",
        "99",
        "4294967295",
        "18446744073709551616",
        " AUDIO",
        " MODE1/2352",
        "INDEX 01 ",
        "INDEX 00 ",
        "00:00:00",
        "01:02:03",
        "99:59:74",
        "10000:00:00",
        "10001:00:00",
        "00:60:00",
        "00:00:75",
        "-1:00:00",
        ":",
        "TITLE ",
        "PERFORMER ",
        "REM DATE ",
        "REM GENRE ",
        "REM DISCNUMBER ",
        "REM REPLAYGAIN_TRACK_GAIN ",
        "1969",
        "가나다",
        "-7.5 dB",
        "nan",
        "inf",
        "\"",
        "\r\n",
        "\n",
        " ",
        "\t",
        "\u{feff}",
        "\u{0}",
    ];
    for bytes in inputs(33, 4_000, &pieces) {
        let text = super::m3u::decode(&bytes);
        let Some(sheet) = cue::parse(&text) else {
            continue;
        };
        assert!(!sheet.files.is_empty() && !sheet.tracks.is_empty());
        assert!(sheet.tracks.len() <= 255);
        let mut numbers = std::collections::HashSet::new();
        for (i, track) in sheet.tracks.iter().enumerate() {
            assert!(numbers.insert(track.number), "a number twice: {text:?}");
            assert!(track.title.chars().count() <= 1_000);
            assert!(track.performer.chars().count() <= 1_000);
            let next = sheet.tracks.get(i + 1).map(|next| next.start);
            assert!(next.is_none_or(|next| next > track.start), "{text:?}");
            let segment = cue::Segment {
                audio: std::path::PathBuf::from("a.flac"),
                start: track.start,
                end: next,
            };
            for rate in [8_000, 44_100, 768_000] {
                let start = segment.start_frame(rate);
                assert!(segment.end_frame(rate).is_none_or(|end| end >= start));
            }
            for total_ms in [0, 1, 180_000, u64::MAX / 2] {
                let length = super::scan::sheet_track_length(track.start, next, total_ms);
                assert!(length.is_none_or(|ms| total_ms == 0 || ms <= total_ms));
            }
            let _ = cue::stretch_stamp(i64::MIN, track.start, next);
        }
        assert!(sheet.title.chars().count() <= 1_000);
        assert!(sheet.genre.chars().count() <= 200);
    }
}

/// An Apple Music library export of any content: nothing panics, it is read in a
/// time that does not blow up with its size, and only songs with a local file come out.
#[test]
fn apple_exports_of_any_content_are_read_or_refused() {
    use super::apple;
    let pieces = [
        "<?xml version=\"1.0\"?>",
        "<plist version=\"1.0\">",
        "</plist>",
        "<dict>",
        "</dict>",
        "<array>",
        "</array>",
        "<key>",
        "</key>",
        "Tracks",
        "Playlists",
        "Track ID",
        "Location",
        "Play Count",
        "Play Date UTC",
        "Date Added",
        "Loved",
        "Playlist Items",
        "Name",
        "<integer>",
        "</integer>",
        "<string>",
        "</string>",
        "<date>",
        "</date>",
        "<true/>",
        "<false/>",
        "42",
        "-1",
        "99999999999999999999",
        "2024-01-02T03:04:05Z",
        "0000-13-40T99:99:99Z",
        "file:///Users/me/a%20b.mp3",
        "file://localhost/x",
        "file://other/x",
        "http://x/y.mp3",
        "%",
        "%zz",
        "&amp;",
        "&#x41;",
        "&#99999999;",
        "&",
        "<",
        ">",
        "<!--",
        "-->",
        "\n",
        " ",
        "노래",
    ];
    for bytes in inputs(34, 4_000, &pieces) {
        let text = String::from_utf8_lossy(&bytes);
        let Some(library) = apple::parse(&text) else {
            continue;
        };
        for track in &library.tracks {
            assert!(track.path.is_absolute(), "{:?}", track.path);
        }
    }
    // Deep nesting and long runs of one piece: read, or refused, in good time.
    let started = std::time::Instant::now();
    for piece in ["<dict>", "<array>", "<key>", "&", "<", "<!--", "<string>"] {
        let _ = apple::parse(&piece.repeat(200_000));
    }
    let nested = format!(
        "<plist><dict><key>Tracks</key>{}{}</dict></plist>",
        "<dict><key>k</key>".repeat(50_000),
        "</dict>".repeat(50_000)
    );
    let _ = apple::parse(&nested);
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
}

/// A path that does not exist, in a folder that does not exist: the readers that take a
/// path say "nothing" for it.
#[test]
fn a_file_that_is_not_there_has_no_lyrics_and_no_details() {
    let gone = Path::new("/nowhere/at/all/song.flac");
    assert_eq!(details::read(gone), details::Details::default());
    assert!(lyrics::from_sidecar(gone).is_none());
}

/// A pipe in place of a song (opening it would wait for a writer forever) and a CUE
/// sheet that is a pipe: the song's details and the sheet are not waited for.
#[cfg(unix)]
#[test]
fn a_pipe_is_not_waited_for() {
    let dir = tempfile::tempdir().expect("tempdir");
    let song = dir.path().join("song.flac");
    let sheet = dir.path().join("album.cue");
    for pipe in [&song, &sheet] {
        let made = std::process::Command::new("mkfifo")
            .arg(pipe)
            .status()
            .is_ok_and(|s| s.success());
        assert!(made, "mkfifo");
    }
    let started = std::time::Instant::now();
    let details = details::read(&song);
    assert!(details.credits.is_empty() && details.facts.is_empty());
    assert!(super::cue::read(&sheet).is_none());
    assert!(super::cue::resolve(&super::cue::track_path(&sheet, 1)).is_none());
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}
