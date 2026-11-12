//! Reading and writing extended M3U (UTF-8, `.m3u8`) playlists.
use crate::model::Track;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Component, Path, PathBuf};

/// One playlist line that names a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct M3uEntry {
    /// The line as written in the playlist (reported back when it cannot be resolved).
    pub raw: String,
    /// Absolute local path, or `None` for entries that are not local files (e.g. URLs).
    pub path: Option<PathBuf>,
}

/// Decodes playlist bytes: UTF-8 (invalid sequences replaced), optional BOM removed.
pub(crate) fn decode(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.strip_prefix('\u{feff}').unwrap_or(&text).to_string()
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
pub(crate) fn parse(text: &str, base_dir: &Path) -> Vec<M3uEntry> {
    let exists = |path: &Option<PathBuf>| path.as_deref().is_some_and(Path::exists);
    let mut entries = Vec::new();
    for raw in text.split('\n') {
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

/// True for extended-M3U directives such as `#EXTM3U`, `#EXTINF:…`, `#EXT-X-…`.
fn is_directive(line: &str) -> bool {
    line.get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("#EXT"))
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
        let decoded = PathBuf::from(OsString::from_vec(percent_decode(rest)));
        return Some(lexical_normalize(&decoded));
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
    let absolute = lexical_normalize(&absolute);
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

    #[test]
    fn undecodable_entries_are_kept_as_entries() {
        // A CP949-encoded name is not valid UTF-8. It cannot be resolved, but it must
        // still come out as an entry so the caller can report it as missing.
        let bytes = b"#EXTM3U\n/music/\xb9\xe3\xc6\xed\xc1\xf6.mp3\n/music/ok.mp3\n";
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
