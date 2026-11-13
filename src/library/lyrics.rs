//! Lyrics of a song, for the info panel: from the file's own tags, or from a text file
//! of the same name next to it (`Song.lrc`, `Song.txt`). Read when asked for, never
//! stored: nothing about a file is kept that the scan did not need.
use lofty::file::TaggedFileExt;
use lofty::tag::ItemKey;
use std::path::Path;

/// Larger than any lyrics; a bigger file of that name is something else.
const MAX_SIDECAR_BYTES: u64 = 512 * 1024;
/// Lyrics longer than this are cut (a tag that holds a whole book).
const MAX_CHARS: usize = 60_000;

/// The lyrics of the file at `path`, as plain text. `None` when there are none (or the
/// file cannot be read: lyrics are an extra, not worth an error message).
pub fn read(path: &Path) -> Option<String> {
    from_tags(path).or_else(|| from_sidecar(path))
}

fn from_tags(path: &Path) -> Option<String> {
    let file = lofty::read_from_path(path).ok()?;
    let mut tags = Vec::new();
    tags.extend(file.primary_tag());
    tags.extend(file.tags());
    tags.iter()
        .flat_map(|tag| {
            [ItemKey::Lyrics, ItemKey::UnsyncLyrics]
                .into_iter()
                .filter_map(|key| tag.get_string(key))
        })
        .find_map(clean)
}

fn from_sidecar(path: &Path) -> Option<String> {
    use std::io::Read;
    ["lrc", "txt"].into_iter().find_map(|extension| {
        // Opened once and asked about through the handle, so that what is checked
        // is what is read: only a regular file (a pipe of that name would never
        // finish), and never more than the limit however the file grows meanwhile.
        let file = std::fs::File::open(path.with_extension(extension)).ok()?;
        let meta = file.metadata().ok()?;
        if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_SIDECAR_BYTES {
            return None;
        }
        let mut bytes = Vec::new();
        file.take(MAX_SIDECAR_BYTES).read_to_end(&mut bytes).ok()?;
        clean(&decode(&bytes)?)
    })
}

/// The text of one line without the time stamps that synchronised lyrics put in front
/// of it (`[01:23.45]`, also several in a row). `None` for a line that only carries
/// information about the file (`[ar:Artist]`, `[offset:120]`).
fn without_time_stamps(line: &str) -> Option<&str> {
    let mut rest = line.trim_start();
    let mut stamped = false;
    while let Some(inner) = rest.strip_prefix('[') {
        let Some((tag, after)) = inner.split_once(']') else {
            break;
        };
        let is_time = tag.contains(':')
            && tag
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, ':' | '.'));
        if is_time {
            stamped = true;
            rest = after.trim_start();
        } else if !stamped && after.trim().is_empty() && is_lrc_field(tag) {
            // "[ar:Artist]" alone on a line.
            return None;
        } else {
            break;
        }
    }
    Some(rest)
}

/// True for the inside of an LRC information line: one of the format's field names
/// before the colon. Anything else in brackets is part of the words: `[Chorus]`,
/// `[Verse 1: Eminem]`.
fn is_lrc_field(tag: &str) -> bool {
    const FIELDS: [&str; 12] = [
        "ti", "ar", "al", "au", "by", "re", "ve", "la", "id", "offset", "length", "tool",
    ];
    tag.split_once(':').is_some_and(|(name, _)| {
        FIELDS
            .iter()
            .any(|field| name.trim().eq_ignore_ascii_case(field))
    }) || tag.starts_with('#')
}

/// The text of a lyrics file: UTF-8, or UTF-16 when it says so with a byte order mark.
/// Anything else (the legacy code pages old lyric files were often saved in) cannot be
/// read without guessing, and is not shown rather than shown as garbage.
fn decode(bytes: &[u8]) -> Option<String> {
    let utf16 = |bytes: &[u8], big_endian: bool| {
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                if big_endian {
                    u16::from_be_bytes(*pair)
                } else {
                    u16::from_le_bytes(*pair)
                }
            })
            .collect();
        String::from_utf16(&units).ok()
    };
    match bytes {
        [0xFF, 0xFE, rest @ ..] => utf16(rest, false),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, true),
        _ => String::from_utf8(bytes.to_vec()).ok(),
    }
}

/// Lyrics as they are shown: one line per line whatever the line endings were, time
/// stamps removed, no more than one empty line in a row, none at either end. `None`
/// when nothing is left.
pub(crate) fn clean(raw: &str) -> Option<String> {
    let raw = raw
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut lines: Vec<&str> = Vec::new();
    for line in raw.split('\n').filter_map(without_time_stamps) {
        let line = line.trim_end();
        if line.is_empty() && lines.last().is_none_or(|last| last.is_empty()) {
            continue;
        }
        lines.push(line);
    }
    while lines.last().is_some_and(|last| last.is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        return None;
    }
    let text = lines.join("\n");
    Some(match text.char_indices().nth(MAX_CHARS) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_lyrics_keep_their_lines() {
        let text = clean("  첫 줄  \r\n둘째 줄\r\n\r\n\r\n\r\n다음 연\n").expect("lyrics");
        assert_eq!(text, "첫 줄\n둘째 줄\n\n다음 연");
        assert_eq!(clean(""), None);
        assert_eq!(clean(" \n\r\n\t\n"), None);
        // Old Mac line endings.
        assert_eq!(clean("a\rb").as_deref(), Some("a\nb"));
    }

    #[test]
    fn synchronised_lyrics_are_shown_without_their_time_stamps() {
        let lrc = "\u{feff}[ti:노래]\n[ar:가수]\n[offset:+120]\n[00:01.00]첫 줄\n\
                   [00:05.20][01:10.00] 반복되는 줄\n[00:09.00]\n[00:12]끝 [괄호]는 가사\n";
        assert_eq!(
            clean(lrc).as_deref(),
            Some("첫 줄\n반복되는 줄\n\n끝 [괄호]는 가사")
        );
        // Brackets that are part of the words stay.
        assert_eq!(clean("[Chorus]\nla la").as_deref(), Some("[Chorus]\nla la"));
        assert_eq!(
            clean("[Verse 1: Eminem]\nwords\n[Chorus: Rihanna]").as_deref(),
            Some("[Verse 1: Eminem]\nwords\n[Chorus: Rihanna]")
        );
        assert_eq!(
            clean("[Offset: 250]\n[#comment]\nwords").as_deref(),
            Some("words")
        );
        assert_eq!(clean("[1:2] x [not a tag").as_deref(), Some("x [not a tag"));
    }

    #[test]
    fn endless_lyrics_are_cut() {
        let long = "가나다라\n".repeat(30_000);
        let text = clean(&long).expect("lyrics");
        assert_eq!(text.chars().count(), MAX_CHARS + 1);
        assert!(text.ends_with('…'));
    }

    #[test]
    fn a_text_file_next_to_the_song_is_used_when_the_tags_have_none() {
        let dir = tempfile::tempdir().expect("temp dir");
        let song = dir.path().join("노래.flac");
        // Not an audio file at all: the tags give nothing, quietly.
        std::fs::write(&song, b"not audio").expect("write");
        assert_eq!(read(&song), None);
        std::fs::write(dir.path().join("노래.txt"), "텍스트 가사").expect("write");
        assert_eq!(read(&song).as_deref(), Some("텍스트 가사"));
        // A synchronised file wins over a plain one.
        std::fs::write(dir.path().join("노래.lrc"), "[00:01.00]싱크 가사").expect("write");
        assert_eq!(read(&song).as_deref(), Some("싱크 가사"));
        // A file that is far too large for lyrics is left alone.
        std::fs::write(dir.path().join("노래.lrc"), vec![b'a'; 600 * 1024]).expect("write");
        assert_eq!(read(&song).as_deref(), Some("텍스트 가사"));
        assert_eq!(read(&dir.path().join("없는 파일.mp3")), None);

        // UTF-16 with a byte order mark is read; a legacy code page is not guessed at.
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("유니코드 가사".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        std::fs::write(dir.path().join("노래.lrc"), utf16).expect("write");
        assert_eq!(read(&song).as_deref(), Some("유니코드 가사"));
        // "가사" in EUC-KR: not valid UTF-8, so the next candidate is used instead.
        std::fs::write(dir.path().join("노래.lrc"), [0xB0, 0xA1, 0xBB, 0xE7]).expect("write");
        assert_eq!(read(&song).as_deref(), Some("텍스트 가사"));
    }

    #[test]
    fn lyrics_in_the_tags_of_a_file_are_read() {
        use lofty::config::WriteOptions;
        use lofty::tag::{Tag, TagExt, TagType};
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let dir = tempfile::tempdir().expect("temp dir");
        for (name, kind, key) in [
            ("gap_a.flac", TagType::VorbisComments, ItemKey::Lyrics),
            ("gap_a.mp3", TagType::Id3v2, ItemKey::UnsyncLyrics),
            ("gap_a.alac.m4a", TagType::Mp4Ilst, ItemKey::Lyrics),
        ] {
            // A folder of its own: the three songs share the name "gap_a".
            let folder = dir.path().join(name);
            std::fs::create_dir(&folder).expect("folder");
            let song = folder.join(name);
            std::fs::copy(fixtures.join(name), &song).expect("copy fixture");
            assert_eq!(read(&song), None, "{name}: no lyrics yet");
            let mut tag = Tag::new(kind);
            tag.insert_text(key, "첫 줄\r\n둘째 줄\n".to_string());
            tag.save_to_path(&song, WriteOptions::default())
                .expect("tag written");
            assert_eq!(read(&song).as_deref(), Some("첫 줄\n둘째 줄"), "{name}");
            // The tags win over a text file next to the song.
            std::fs::write(song.with_extension("txt"), "다른 가사").expect("write");
            assert_eq!(read(&song).as_deref(), Some("첫 줄\n둘째 줄"), "{name}");
        }
    }
}
