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

/// A path that does not exist, in a folder that does not exist: the readers that take a
/// path say "nothing" for it.
#[test]
fn a_file_that_is_not_there_has_no_lyrics_and_no_details() {
    let gone = Path::new("/nowhere/at/all/song.flac");
    assert!(lyrics::read(gone).is_none());
    let _ = details::read(gone);
}
