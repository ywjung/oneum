//! Reading and writing extended M3U (UTF-8, `.m3u8`) playlists.
use crate::model::Track;
use std::path::{Component, Path, PathBuf};

/// One playlist line that names a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct M3uEntry {
    /// The line as written in the playlist (reported back when it cannot be resolved).
    pub raw: String,
    /// Absolute local path, or `None` for entries that are not local files (e.g. URLs).
    pub path: Option<PathBuf>,
}

/// Decodes playlist bytes: UTF-8 or UTF-16 (a byte order mark is removed), or Korean in
/// the code page of old Windows, which is what a plain `.m3u` written there holds.
/// Anything else is read as UTF-8 with what is invalid replaced.
pub(crate) fn decode(bytes: &[u8]) -> String {
    super::legacy::decode_file(bytes).unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned())
}

/// Largest playlist file that is read. Real playlists are a few kilobytes; this keeps a
/// mistaken selection (a huge file, an HLS stream manifest dump) from being loaded whole.
pub(crate) const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// Parses playlist text. Relative paths are resolved against `base_dir`; CRLF line ends
/// and `file://` URLs are accepted. Skipped: blank lines, `#EXT…` directives, and other
/// `#` lines unless they name an existing file (`#1 Hits/01.mp3` is a legal path).
///
/// File names may begin or end with spaces, so a line is first taken without its
/// surrounding whitespace and, if no such file exists but the line as written does,
/// as written.
///
/// A playlist is a file from anywhere, so what it names is not trusted: lines that
/// point at another machine are not followed (see [`is_remote`]), and no more than
/// [`MAX_ENTRIES`] lines are taken.
pub(crate) fn parse(text: &str, base_dir: &Path) -> Vec<M3uEntry> {
    let exists = |path: &Option<PathBuf>| path.as_deref().is_some_and(Path::exists);
    let mut entries = Vec::new();
    for raw in text.split('\n') {
        if entries.len() >= MAX_ENTRIES {
            tracing::warn!("playlist has more than {MAX_ENTRIES} entries; the rest is ignored");
            break;
        }
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let trimmed = line.trim();
        if trimmed.is_empty() || is_directive(trimmed) {
            continue;
        }
        let mut path = resolve(trimmed, base_dir);
        if line != trimmed && !exists(&path) {
            let as_written = resolve(line, base_dir);
            if exists(&as_written) {
                path = as_written;
            }
        }
        if trimmed.starts_with('#') && !exists(&path) {
            continue; // a comment
        }
        entries.push(M3uEntry {
            raw: trimmed.to_string(),
            path,
        });
    }
    entries
}

/// The most entries taken from one playlist. Every entry costs a look at the disk; a
/// file of millions of two-letter lines must not become minutes of that.
pub(crate) const MAX_ENTRIES: usize = 100_000;

/// True for a path on another machine that merely looking at would connect to: a
/// Windows share (`\\host\share\…`, where the system would also send the user's
/// credentials to that host) or device path, or the automounter's `/net/host/…` on
/// Unix. A playlist that came from somewhere must not be able to make the app do that.
/// (A share that is mounted, `/Volumes/…` or a drive letter, is a local path.)
fn is_remote(path: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        matches!(
            path.components().next(),
            Some(Component::Prefix(prefix)) if matches!(
                prefix.kind(),
                Prefix::UNC(..) | Prefix::VerbatimUNC(..) | Prefix::DeviceNS(..)
            )
        )
    }
    #[cfg(not(windows))]
    {
        let bytes = path.as_os_str().as_encoded_bytes();
        bytes.starts_with(b"//") || bytes.starts_with(b"/net/")
    }
}

/// The path, unless it leads to another machine. A playlist that itself lies on that
/// machine may name files next to it: the user has been there already.
fn local_only(path: PathBuf, base_dir: &Path) -> Option<PathBuf> {
    (!is_remote(&path) || path.starts_with(base_dir)).then_some(path)
}

/// True for extended-M3U directives such as `#EXTM3U`, `#EXTINF:…`, `#EXT-X-…`.
fn is_directive(line: &str) -> bool {
    line.get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("#EXT"))
}

/// The path of `file:///C:/Music/a.mp3` is `/C:/Music/a.mp3`: on Windows the slash in
/// front of the drive letter is not part of the file's path.
fn strip_slash_before_drive(path: &mut Vec<u8>) {
    if let [b'/', drive, b':', ..] = path.as_slice()
        && drive.is_ascii_alphabetic()
    {
        path.remove(0);
    }
}

fn resolve(line: &str, base_dir: &Path) -> Option<PathBuf> {
    if let Some(prefix) = line.get(..7)
        && prefix.eq_ignore_ascii_case("file://")
    {
        let rest = &line[7..];
        let rest = match rest.get(..9) {
            Some(host) if host.eq_ignore_ascii_case("localhost") => &rest[9..],
            _ => rest,
        };
        if !rest.starts_with('/') {
            return None; // a file URL on another host
        }
        let mut bytes = percent_decode(rest);
        if cfg!(windows) {
            strip_slash_before_drive(&mut bytes);
        }
        let decoded = crate::platform::path_from_bytes(bytes);
        return local_only(lexical_normalize(&decoded), base_dir);
    }
    if is_url(line) {
        return None;
    }
    let path = Path::new(line);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    };
    // Before anything looks at the disk.
    let absolute = local_only(lexical_normalize(&absolute), base_dir)?;
    if !absolute.exists() && line.contains('\\') {
        // Playlists written on Windows use backslashes for relative paths.
        let converted = line.replace('\\', "/");
        let candidate = lexical_normalize(&base_dir.join(converted));
        if candidate.exists() {
            return Some(candidate);
        }
    }
    Some(absolute)
}

/// True for `scheme://…` lines (http streams and the like).
fn is_url(line: &str) -> bool {
    line.split_once("://").is_some_and(|(scheme, _)| {
        !scheme.is_empty()
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    })
}

fn percent_decode(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = text.get(i + 1..i + 3)
            && let Ok(value) = u8::from_str_radix(hex, 16)
        {
            out.push(value);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// Removes `.` components and resolves `..` without touching the file system.
pub(crate) fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // `..` at the root stays at the root; in a relative path it is kept.
                if !out.pop() && !out.has_root() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Renders tracks as extended M3U with absolute paths.
pub(crate) fn render<'a>(tracks: impl Iterator<Item = &'a Track>) -> (String, usize) {
    let mut out = String::from("#EXTM3U\n");
    let mut count = 0;
    for track in tracks {
        let seconds = (track.duration_ms + 500) / 1000;
        let label = if track.artist.is_empty() {
            single_line(&track.title)
        } else {
            format!(
                "{} - {}",
                single_line(&track.artist),
                single_line(&track.title)
            )
        };
        out.push_str(&format!("#EXTINF:{seconds},{label}\n"));
        out.push_str(&track.path.to_string_lossy());
        out.push('\n');
        count += 1;
    }
    (out, count)
}

fn single_line(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_that_point_at_another_machine_are_not_followed() {
        let base = Path::new("/Users/me/Playlists");
        let path_of = |line: &str| parse(line, base).into_iter().next().and_then(|e| e.path);
        // Ordinary entries resolve as before.
        assert_eq!(path_of("a.mp3"), Some(base.join("a.mp3")));
        assert_eq!(
            path_of("/Volumes/NAS/Music/a.mp3"),
            Some(PathBuf::from("/Volumes/NAS/Music/a.mp3"))
        );
        assert_eq!(
            path_of("file:///Users/me/Music/a%20b.mp3"),
            Some(PathBuf::from("/Users/me/Music/a b.mp3"))
        );
        // A file URL on another host, and the automounter's way to any host.
        assert_eq!(path_of("file://evil.example/share/a.mp3"), None);
        #[cfg(not(windows))]
        {
            assert_eq!(path_of("/net/evil.example/share/a.mp3"), None);
            assert_eq!(path_of("file:///net/evil.example/share/a.mp3"), None);
            assert!(is_remote(Path::new("//evil.example/share/a.mp3")));
            assert!(!is_remote(Path::new("/network/a.mp3")) && !is_remote(Path::new("/Volumes/x")));
            // A playlist that lies there itself may name its neighbours.
            let there = Path::new("/net/nas/music");
            let entry = parse("/net/nas/music/album/a.mp3", there).remove(0);
            assert_eq!(
                entry.path,
                Some(PathBuf::from("/net/nas/music/album/a.mp3"))
            );
        }
        #[cfg(windows)]
        {
            for line in [
                r"\\evil.example\share\a.mp3",
                "//evil.example/share/a.mp3",
                "file:////evil.example/share/a.mp3",
                r"\\?\UNC\evil.example\share\a.mp3",
                r"\\.\pipe\x",
            ] {
                assert_eq!(path_of(line), None, "{line}");
            }
            assert!(!is_remote(Path::new(r"C:\Music\a.mp3")));
        }
        // The entry is still listed (it is reported as not found), just never looked at.
        assert_eq!(parse("file://evil.example/share/a.mp3", base).len(), 1);
    }

    #[test]
    fn a_playlist_of_millions_of_lines_is_cut_short() {
        let text = "a\n".repeat(MAX_ENTRIES + 50);
        let entries = parse(&text, Path::new("/nowhere/at/all"));
        assert_eq!(entries.len(), MAX_ENTRIES);
    }

    #[test]
    fn windows_file_urls_lose_the_slash_before_the_drive() {
        let strip = |text: &str| {
            let mut bytes = text.as_bytes().to_vec();
            strip_slash_before_drive(&mut bytes);
            String::from_utf8(bytes).expect("utf-8")
        };
        assert_eq!(strip("/C:/Music/a.mp3"), "C:/Music/a.mp3");
        assert_eq!(strip("/d:/x"), "d:/x");
        // Anything else is left alone: a server path, a relative path, a lone slash.
        assert_eq!(strip("/Music/a.mp3"), "/Music/a.mp3");
        assert_eq!(strip("//server/share/a.mp3"), "//server/share/a.mp3");
        assert_eq!(strip("/1:/a"), "/1:/a");
        assert_eq!(strip("/"), "/");
        assert_eq!(strip(""), "");
    }

    #[test]
    fn decode_strips_bom_and_tolerates_bad_bytes() {
        assert_eq!(decode(b"\xef\xbb\xbf#EXTM3U\n"), "#EXTM3U\n");
        assert_eq!(decode(b"a\xffb"), "a\u{fffd}b");
    }

    #[test]
    fn parses_comments_crlf_relative_and_urls() {
        let base = Path::new("/lists");
        let text = "#EXTM3U\r\n\
                    #EXTINF:253,아이유 - 밤편지\r\n\
                    /music/아이유/밤편지.flac\r\n\
                    \r\n\
                    # a comment\r\n\
                    sub/track 02.mp3\r\n\
                    ../other/./x.m4a\r\n\
                    file:///music/My%20Song%20%EC%95%84.mp3\r\n\
                    FILE://localhost/music/b.mp3\r\n\
                    file://server/share/c.mp3\r\n\
                    http://example.com/stream.mp3\r\n";
        let entries = parse(text, base);
        let paths: Vec<Option<&str>> = entries
            .iter()
            .map(|e| e.path.as_deref().and_then(Path::to_str))
            .collect();
        assert_eq!(
            paths,
            [
                Some("/music/아이유/밤편지.flac"),
                Some("/lists/sub/track 02.mp3"),
                Some("/other/x.m4a"),
                Some("/music/My Song 아.mp3"),
                Some("/music/b.mp3"),
                None,
                None,
            ]
        );
        assert_eq!(entries[1].raw, "sub/track 02.mp3");
        assert_eq!(entries[6].raw, "http://example.com/stream.mp3");
    }

    #[test]
    fn hash_names_and_padded_names_are_found_when_they_exist() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        std::fs::create_dir_all(base.join("#1 Hits")).unwrap();
        std::fs::write(base.join("#1 Hits/01.mp3"), b"x").unwrap();
        std::fs::write(base.join(" padded .mp3"), b"x").unwrap();
        std::fs::write(base.join("plain.mp3"), b"x").unwrap();
        let text = "#EXTM3U\n\
                    #EXTINF:1,x\n\
                    #1 Hits/01.mp3\n\
                    # not a file, just a remark\n\
                    #2 Hits/missing.mp3\n\
                    \x20padded .mp3\n\
                    \x20\x20plain.mp3\x20\r\n\
                    \x20gone .mp3\n";
        let entries = parse(text, base);
        let paths: Vec<PathBuf> = entries.iter().filter_map(|e| e.path.clone()).collect();
        assert_eq!(
            paths,
            [
                base.join("#1 Hits/01.mp3"),
                // The name really has the spaces: taken as written.
                base.join(" padded .mp3"),
                // Stray whitespace around a normal name is ignored.
                base.join("plain.mp3"),
                // Neither form exists: reported with the trimmed form.
                base.join("gone .mp3"),
            ]
        );
        assert_eq!(entries[3].raw, "gone .mp3");
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn a_playlist_in_the_old_korean_code_page_is_read() {
        // A plain .m3u written on Korean Windows: the name is CP949, not UTF-8.
        let bytes = b"#EXTM3U\n/music/\xb9\xe3\xc6\xed\xc1\xf6.mp3\n/music/ok.mp3\n";
        let entries = parse(&decode(bytes), Path::new("/lists"));
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0].path.as_deref(),
            Some(Path::new("/music/밤편지.mp3"))
        );
        assert_eq!(entries[1].path.as_deref(), Some(Path::new("/music/ok.mp3")));
        // UTF-8 with a byte order mark, as Windows editors write it.
        let marked = "\u{feff}/music/밤편지.mp3\n";
        assert_eq!(
            parse(&decode(marked.as_bytes()), Path::new("/lists"))[0]
                .path
                .as_deref(),
            Some(Path::new("/music/밤편지.mp3"))
        );
    }

    #[test]
    fn undecodable_entries_are_kept_as_entries() {
        // Bytes that are neither UTF-8 nor Korean. The name cannot be resolved, but it
        // must still come out as an entry so the caller can report it as missing.
        let bytes = b"#EXTM3U\n/music/\xe9\x20\xe8.mp3\n/music/ok.mp3\n";
        let entries = parse(&decode(bytes), Path::new("/lists"));
        assert_eq!(entries.len(), 2);
        assert!(entries[0].raw.contains('\u{fffd}'));
        assert!(entries[0].path.as_ref().is_some_and(|p| !p.exists()));
        assert_eq!(entries[1].path.as_deref(), Some(Path::new("/music/ok.mp3")));
    }

    #[test]
    fn percent_sign_without_hex_is_kept() {
        assert_eq!(percent_decode("/a/100%.mp3"), b"/a/100%.mp3");
        assert_eq!(percent_decode("/a/%zz%41"), b"/a/%zzA");
    }

    #[test]
    fn names_with_colons_are_not_urls() {
        assert!(!is_url("Artist: Title.mp3"));
        assert!(!is_url("a b://c"));
        assert!(is_url("https://example.com/a.mp3"));
    }

    #[test]
    fn lexical_normalisation() {
        assert_eq!(
            lexical_normalize(Path::new("/a/b/../c/./d")),
            PathBuf::from("/a/c/d")
        );
        assert_eq!(lexical_normalize(Path::new("/../a")), PathBuf::from("/a"));
    }
}
