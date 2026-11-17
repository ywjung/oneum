//! Lyrics of a song, for the info panel: from the file's own tags, or from a text file
//! of the same name next to it (`Song.lrc`, `Song.txt`). Read when asked for, never
//! stored: nothing about a file is kept that the scan did not need.
//!
//! Lyrics with time stamps (the LRC format, in a file or pasted into a tag) are kept
//! with their times as well, so that the line being sung can be shown as it is sung.
use lofty::file::{TaggedFile, TaggedFileExt};
use lofty::tag::ItemKey;
use std::path::Path;

/// Larger than any lyrics; a bigger file of that name is something else.
const MAX_SIDECAR_BYTES: u64 = 512 * 1024;
/// Lyrics longer than this are cut (a tag that holds a whole book).
const MAX_CHARS: usize = 60_000;
/// How much of a tag or file is looked at for lyrics at all.
const MAX_RAW_CHARS: usize = 200_000;

/// One line of lyrics and when it is sung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimedLine {
    /// Milliseconds from the start of the song.
    pub at_ms: u32,
    /// May be empty: a pause in the singing.
    pub text: String,
    /// Other lines written for the same moment: a translation or a pronunciation
    /// that the file gives right after the original.
    pub also: Vec<String>,
}

/// The lyrics of one song.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lyrics {
    /// The words as plain text, one line per line.
    pub plain: String,
    /// The lines with their times, in order, when the lyrics came with time stamps.
    pub timed: Option<Vec<TimedLine>>,
}

impl Lyrics {
    fn from_raw(raw: &str) -> Option<Self> {
        // A tag can hold megabytes; no song's words do.
        let raw = match raw.char_indices().nth(MAX_RAW_CHARS) {
            Some((at, _)) => &raw[..at],
            None => raw,
        };
        Some(Self {
            plain: clean(raw)?,
            timed: timed(raw),
        })
    }

    /// Lyrics from a text as it comes (with or without time stamps): for those found
    /// on the internet. `None` when the text holds no words.
    pub fn from_text(raw: &str) -> Option<Self> {
        Self::from_raw(raw)
    }

    /// Index of the line that is being sung at `position_ms`: the last one whose time
    /// has come. `None` before the first line, and for lyrics without times.
    pub fn line_at(&self, position_ms: u64) -> Option<usize> {
        let lines = self.timed.as_ref()?;
        lines
            .partition_point(|line| u64::from(line.at_ms) <= position_ms)
            .checked_sub(1)
    }
}

/// Lyrics from the tags of an opened file.
pub(crate) fn from_tags(file: &TaggedFile) -> Option<Lyrics> {
    let mut tags = Vec::new();
    tags.extend(file.primary_tag());
    tags.extend(file.tags());
    tags.iter()
        .flat_map(|tag| {
            [ItemKey::Lyrics, ItemKey::UnsyncLyrics]
                .into_iter()
                .filter_map(|key| tag.get_string(key))
        })
        // Korean from an old code page, declared as Latin-1 by the tag.
        .find_map(|raw| match super::legacy::repair(raw) {
            Some(korean) => Lyrics::from_raw(&korean),
            None => Lyrics::from_raw(raw),
        })
}

/// Lyrics from a text file next to the song: a regular file (a pipe of that name would
/// never deliver) no larger than any song's words.
pub(crate) fn from_sidecar(path: &Path) -> Option<Lyrics> {
    ["lrc", "txt"].into_iter().find_map(|extension| {
        let sidecar = path.with_extension(extension);
        let bytes = crate::storage::files::read_bounded(&sidecar, MAX_SIDECAR_BYTES).ok()??;
        Lyrics::from_raw(&super::legacy::decode_file(&bytes)?)
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

/// A time stamp's inside (`01:23.45`, `1:02:03`, `00:12`) in milliseconds.
fn stamp_ms(tag: &str) -> Option<u32> {
    let mut parts = tag.split(':');
    let (first, second, third) = (parts.next()?, parts.next()?, parts.next());
    if parts.next().is_some() {
        return None;
    }
    let whole = |text: &str| {
        (!text.is_empty() && text.len() <= 3 && text.bytes().all(|b| b.is_ascii_digit()))
            .then(|| text.parse::<u32>().ok())
            .flatten()
    };
    // The last part is seconds, with or without a fraction.
    let seconds_text = third.unwrap_or(second);
    let (seconds, fraction) = seconds_text.split_once('.').unwrap_or((seconds_text, ""));
    let seconds = whole(seconds).filter(|s| *s < 60)?;
    let millis = match fraction.len() {
        0 => 0,
        1..=3 if fraction.bytes().all(|b| b.is_ascii_digit()) => {
            fraction.parse::<u32>().ok()? * 10u32.pow(3 - fraction.len() as u32)
        }
        _ => return None,
    };
    let minutes = match third {
        Some(_) => whole(first)? * 60 + whole(second).filter(|m| *m < 60)?,
        None => whole(first)?,
    };
    Some((minutes * 60 + seconds) * 1000 + millis)
}

/// The text without the per-word time stamps of "enhanced" LRC (`<00:12.34>`).
fn without_word_stamps(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        let (before, after) = rest.split_at(open);
        out.push_str(before);
        let inner = &after[1..];
        // A stamp is a dozen characters: the closing bracket is looked for that far
        // and no farther (a line of nothing but "<" would otherwise be searched to its
        // end once for each of them).
        let close = inner.bytes().take(MAX_STAMP_BYTES).position(|b| b == b'>');
        match close {
            Some(close) if stamp_ms(&inner[..close]).is_some() => rest = &inner[close + 1..],
            _ => {
                out.push('<');
                rest = inner;
            }
        }
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The fewest stamped lines that make lyrics "synchronised" (one stamp can be chance).
const MIN_TIMED_LINES: usize = 2;
/// More lines than any song has; a file that claims more is cut here.
const MAX_TIMED_LINES: usize = 5_000;
/// More time stamps than one line is ever sung at, and more text than one line has.
/// Every stamp gets the line's text, so the two together bound what the lines take.
const MAX_STAMPS_PER_LINE: usize = 64;
const MAX_LINE_CHARS: usize = 500;
/// Lines kept for one moment besides the first (translation, pronunciation, …).
const MAX_ALSO_LINES: usize = 3;
/// The longest a time stamp's inside is taken to be (`1:02:03.456` is eleven bytes).
const MAX_STAMP_BYTES: usize = 16;

/// The lines of synchronised lyrics with their times, in the order they are sung.
/// `None` when the text carries no time stamps.
pub(crate) fn timed(raw: &str) -> Option<Vec<TimedLine>> {
    let raw = raw
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut lines: Vec<TimedLine> = Vec::new();
    // "[offset:+250]": the whole text is 250 ms early, i.e. shown that much sooner.
    let mut offset_ms: i64 = 0;
    for line in raw.split('\n') {
        let mut rest = line.trim_start();
        let mut stamps: Vec<u32> = Vec::new();
        while let Some(inner) = rest.strip_prefix('[') {
            let Some((tag, after)) = inner.split_once(']') else {
                break;
            };
            if let Some(at) = stamp_ms(tag) {
                if stamps.len() < MAX_STAMPS_PER_LINE {
                    stamps.push(at);
                }
                rest = after.trim_start();
            } else {
                if stamps.is_empty()
                    && let Some((name, value)) = tag.split_once(':')
                    && name.trim().eq_ignore_ascii_case("offset")
                    && let Ok(value) = value.trim().parse::<i64>()
                {
                    offset_ms = value.clamp(-600_000, 600_000);
                }
                break;
            }
        }
        if stamps.is_empty() {
            continue;
        }
        let text: String = without_word_stamps(rest)
            .chars()
            .take(MAX_LINE_CHARS)
            .collect();
        for at_ms in stamps {
            if lines.len() < MAX_TIMED_LINES {
                lines.push(TimedLine {
                    at_ms,
                    text: text.clone(),
                    also: Vec::new(),
                });
            }
        }
    }
    for line in &mut lines {
        line.at_ms = (i64::from(line.at_ms) - offset_ms).clamp(0, i64::from(u32::MAX)) as u32;
    }
    // A repeated line is written once with all its times: bring them in order. The
    // sort is stable, so lines of the same moment keep the order of the file.
    lines.sort_by_key(|line| line.at_ms);
    // Lines that share a moment are sung together: an original and its translation or
    // pronunciation, written one after the other. They become one entry.
    // More lines than that at one moment (the credits some files stack at the start)
    // are not dropped: they start another entry of the same moment.
    let mut merged: Vec<TimedLine> = Vec::with_capacity(lines.len());
    for line in lines {
        match merged.last_mut() {
            Some(last) if last.at_ms == line.at_ms && last.also.len() < MAX_ALSO_LINES => {
                if line.text.is_empty() || line.text == last.text || last.also.contains(&line.text)
                {
                    continue;
                }
                if last.text.is_empty() {
                    last.text = line.text;
                } else {
                    last.also.push(line.text);
                }
            }
            Some(last) if last.at_ms == line.at_ms && line.text.is_empty() => {}
            _ => merged.push(line),
        }
    }
    if merged.len() < MIN_TIMED_LINES {
        return None;
    }
    Some(merged)
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

    /// The lyrics of the song at `path`, as the info panel gets them.
    fn read(path: &Path) -> Option<Lyrics> {
        crate::library::details::read(path).lyrics
    }

    /// The words of the song at `path`, as plain text.
    fn plain(path: &Path) -> Option<String> {
        read(path).map(|lyrics| lyrics.plain)
    }

    fn lines(raw: &str) -> Vec<(u32, String)> {
        timed(raw)
            .expect("synchronised lyrics")
            .into_iter()
            .map(|line| (line.at_ms, line.text))
            .collect()
    }

    /// An original and its translation share a time stamp: one entry, both shown.
    #[test]
    fn lines_of_the_same_moment_go_together() {
        let raw = "[00:01.00]夜に駆ける\n[00:01.00]밤을 달리다\n[00:01.00]yoru ni kakeru\n\
                   [00:01.00]밤을 달리다\n[00:05.00]\n[00:05.00]두 번째 줄\n[00:09.00]마지막";
        let lines = timed(raw).expect("timed");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].text, "夜に駆ける");
        assert_eq!(lines[0].also, ["밤을 달리다", "yoru ni kakeru"]);
        // A pause stamp followed by words at the same moment: the words.
        assert_eq!(lines[1].text, "두 번째 줄");
        assert!(lines[1].also.is_empty());
        let lyrics = Lyrics::from_raw(raw).expect("lyrics");
        assert_eq!(lyrics.line_at(1_500), Some(0));
        assert_eq!(lyrics.line_at(6_000), Some(1));
        // Five credit lines stacked at the start: none is lost.
        let credits = "[00:00.00]제목\n[00:00.00]가수\n[00:00.00]작사\n[00:00.00]작곡\n\
                       [00:00.00]편곡\n[00:03.00]첫 줄\n[00:06.00]둘째 줄";
        let lines = timed(credits).expect("timed");
        let shown: Vec<&str> = lines
            .iter()
            .flat_map(|l| std::iter::once(l.text.as_str()).chain(l.also.iter().map(String::as_str)))
            .collect();
        assert_eq!(
            shown,
            ["제목", "가수", "작사", "작곡", "편곡", "첫 줄", "둘째 줄"]
        );
    }

    #[test]
    fn time_stamps_in_their_usual_forms() {
        assert_eq!(stamp_ms("00:12.34"), Some(12_340));
        assert_eq!(stamp_ms("01:02.5"), Some(62_500));
        assert_eq!(stamp_ms("3:07.123"), Some(187_123));
        assert_eq!(stamp_ms("00:12"), Some(12_000));
        assert_eq!(stamp_ms("1:02:03"), Some(3_723_000));
        assert_eq!(stamp_ms("99:59.99"), Some(5_999_990));
        // Not time stamps.
        for not_one in [
            "ar:Artist",
            "00:61",
            "1:2:3:4",
            "12",
            "ab:cd",
            "00:12.3456",
            "",
            ":",
            "1:",
        ] {
            assert_eq!(stamp_ms(not_one), None, "{not_one:?}");
        }
    }

    #[test]
    fn synchronised_lyrics_keep_their_times() {
        let lrc =
            "[ti:노래]\n[ar:가수]\n[00:01.00]첫 줄\n[00:05.20] 둘째 줄 \n[00:09.00]\n[00:12]끝";
        assert_eq!(
            lines(lrc),
            [
                (1_000, "첫 줄".to_string()),
                (5_200, "둘째 줄".to_string()),
                (9_000, String::new()),
                (12_000, "끝".to_string()),
            ]
        );
        // A chorus written once with all its times comes out in the order it is sung.
        let repeated = "[00:30.00][01:10.00]후렴\n[00:10.00]1절\n[00:50.00]2절";
        assert_eq!(
            lines(repeated)
                .iter()
                .map(|l| l.1.as_str())
                .collect::<Vec<_>>(),
            ["1절", "후렴", "2절", "후렴"]
        );
        // "[offset:+500]": everything half a second sooner, never before the start.
        assert_eq!(
            lines("[offset:+500]\n[00:00.20]a\n[00:02.00]b"),
            [(0, "a".to_string()), (1_500, "b".to_string())]
        );
        // Per-word stamps of "enhanced" lyrics are taken out of the words.
        assert_eq!(
            lines("[00:01.00]<00:01.00>Hold <00:01.50>the <00:02.00>line\n[00:04.00]a < b")[0].1,
            "Hold the line"
        );
        assert_eq!(lines("[00:01.00]x\n[00:04.00]a < b > c")[1].1, "a < b > c");
        // Plain lyrics, and a text with a single stamp-like line, are not synchronised.
        assert_eq!(timed("첫 줄\n둘째 줄"), None);
        assert_eq!(timed("[Chorus]\nla la\n[1:23] once"), None);
        assert_eq!(timed(""), None);
    }

    #[test]
    fn the_line_being_sung() {
        let lyrics =
            Lyrics::from_raw("[00:10.00]하나\n[00:20.00]둘\n[00:30.00]셋").expect("lyrics");
        assert_eq!(lyrics.plain, "하나\n둘\n셋");
        assert_eq!(lyrics.line_at(0), None, "before the first line");
        assert_eq!(lyrics.line_at(9_999), None);
        assert_eq!(lyrics.line_at(10_000), Some(0));
        assert_eq!(lyrics.line_at(19_999), Some(0));
        assert_eq!(lyrics.line_at(20_000), Some(1));
        assert_eq!(lyrics.line_at(3_600_000), Some(2), "the last line stays");
        // Lyrics without times have no "line being sung".
        let plain = Lyrics::from_raw("하나\n둘").expect("lyrics");
        assert_eq!(
            (plain.timed.is_some(), plain.line_at(15_000)),
            (false, None)
        );
    }

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
        assert_eq!(plain(&song).as_deref(), Some("텍스트 가사"));
        // A synchronised file wins over a plain one.
        std::fs::write(dir.path().join("노래.lrc"), "[00:01.00]싱크 가사").expect("write");
        assert_eq!(plain(&song).as_deref(), Some("싱크 가사"));
        // A file that is far too large for lyrics is left alone.
        std::fs::write(dir.path().join("노래.lrc"), vec![b'a'; 600 * 1024]).expect("write");
        assert_eq!(plain(&song).as_deref(), Some("텍스트 가사"));
        assert_eq!(read(&dir.path().join("없는 파일.mp3")), None);

        // UTF-16 with a byte order mark is read; a legacy code page is not guessed at.
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("유니코드 가사".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        std::fs::write(dir.path().join("노래.lrc"), utf16).expect("write");
        assert_eq!(plain(&song).as_deref(), Some("유니코드 가사"));
        // "가사" in the Korean code page of old Windows, as many lyric files are.
        std::fs::write(dir.path().join("노래.lrc"), [0xB0, 0xA1, 0xBB, 0xE7]).expect("write");
        #[cfg(any(target_os = "macos", windows))]
        assert_eq!(plain(&song).as_deref(), Some("가사"));
        // Bytes that are neither Unicode nor Korean: the next candidate is used.
        std::fs::write(dir.path().join("노래.lrc"), [0xE9, 0x20, 0xE8, 0x20]).expect("write");
        assert_eq!(plain(&song).as_deref(), Some("텍스트 가사"));
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
            assert_eq!(plain(&song).as_deref(), Some("첫 줄\n둘째 줄"), "{name}");
            // The tags win over a text file next to the song.
            std::fs::write(song.with_extension("txt"), "다른 가사").expect("write");
            assert_eq!(plain(&song).as_deref(), Some("첫 줄\n둘째 줄"), "{name}");
        }
    }

    /// Every stamp on a line gets the line's words. A line with thousands of stamps
    /// and half a megabyte of words would be gigabytes: both are bounded.
    #[test]
    fn a_line_with_thousands_of_time_stamps_and_endless_words_stays_small() {
        let stamps: String = (0..5_000)
            .map(|i| format!("[{}:{:02}]", 10 + i / 60, i % 60))
            .collect();
        let raw = format!("{stamps}{}\n[00:01.00]second\n", "word ".repeat(100_000));
        let lines = timed(&raw).expect("timed");
        assert_eq!(lines.len(), MAX_STAMPS_PER_LINE + 1);
        let held: usize = lines.iter().map(|l| l.text.len()).sum();
        assert!(held <= (MAX_STAMPS_PER_LINE + 1) * MAX_LINE_CHARS, "{held}");
        // What is looked at of a tag is bounded as well.
        let lyrics = Lyrics::from_raw(&"la\n".repeat(1_000_000)).expect("lyrics");
        assert!(lyrics.plain.len() <= MAX_RAW_CHARS);
    }

    /// The closing bracket of a word stamp is looked for a few bytes ahead, not to the
    /// end of the line once for every opening one.
    #[test]
    fn a_line_of_opening_brackets_is_read_in_no_time() {
        let started = std::time::Instant::now();
        let brackets = "<".repeat(400_000);
        assert_eq!(without_word_stamps(&brackets).len(), 400_000);
        assert_eq!(without_word_stamps(&format!("{brackets}>")).len(), 400_001);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        // Word stamps are still taken out, and what merely looks like one is kept.
        assert_eq!(without_word_stamps("<00:01.00>la <00:02.50>la"), "la la");
        assert_eq!(without_word_stamps("a < b > c <3"), "a < b > c <3");
        assert_eq!(
            without_word_stamps("<한글 가사가 여기에 길게 이어진다>"),
            "<한글 가사가 여기에 길게 이어진다>"
        );
    }

    /// A pipe named like the lyrics file: opening it would wait for a writer for ever.
    #[cfg(unix)]
    #[test]
    fn a_pipe_in_the_place_of_the_lyrics_file_is_not_opened() {
        let dir = tempfile::tempdir().unwrap();
        let song = dir.path().join("Song.flac");
        let made = std::process::Command::new("mkfifo")
            .arg(dir.path().join("Song.lrc"))
            .status()
            .expect("mkfifo");
        assert!(made.success());
        std::fs::write(dir.path().join("Song.txt"), "words in the text file\n").unwrap();
        let (done, result) = std::sync::mpsc::channel();
        std::thread::spawn(move || done.send(from_sidecar(&song).map(|l| l.plain)));
        let found = result
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("reading the lyrics came back");
        assert_eq!(found.as_deref(), Some("words in the text file"));
    }

    /// A lyrics file far larger than any song's words is not lyrics: it is left alone
    /// (not read half-way), and the text file is taken.
    #[test]
    fn an_oversized_lyrics_file_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let song = dir.path().join("Song.flac");
        let huge = "la la la\n".repeat(MAX_SIDECAR_BYTES as usize / 9 + 1);
        std::fs::write(dir.path().join("Song.lrc"), huge).unwrap();
        assert!(from_sidecar(&song).is_none());
        std::fs::write(dir.path().join("Song.txt"), "the words\n").unwrap();
        assert_eq!(
            from_sidecar(&song).map(|l| l.plain).as_deref(),
            Some("the words")
        );
    }
}
