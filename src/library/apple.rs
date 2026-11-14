//! Reading the library file Apple Music exports (File ▸ Library ▸ Export Library…): an
//! XML property list. What OnEum takes from it: per song, the play count, when it was
//! last played, when it was added and whether it was loved; and the playlists the user
//! made (not the built-in ones, folders or smart playlists). Songs are matched to the
//! library by their file's location.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The largest file read (a library of hundreds of thousands of songs stays below it).
pub(crate) const MAX_FILE_BYTES: u64 = 1 << 30;
/// Nesting deeper than this is not an Apple Music library.
const MAX_DEPTH: usize = 64;

/// A value of a property list.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    Dict(Vec<(String, Value)>),
    Array(Vec<Value>),
    String(String),
    Integer(i64),
    /// ISO 8601 in UTC, as written (`2024-01-02T03:04:05Z`).
    Date(String),
    Bool(bool),
    /// Reals and data: kept only so that the structure stays whole.
    Other,
}

impl Value {
    fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Dict(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn integer(&self, key: &str) -> Option<i64> {
        match self.get(key)? {
            Value::Integer(n) => Some(*n),
            _ => None,
        }
    }

    fn string(&self, key: &str) -> Option<&str> {
        match self.get(key)? {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    fn flag(&self, key: &str) -> bool {
        matches!(self.get(key), Some(Value::Bool(true)))
    }

    fn date(&self, key: &str) -> Option<i64> {
        match self.get(key)? {
            Value::Date(text) => parse_date(text),
            _ => None,
        }
    }
}

/// A song as the export describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct AppleTrack {
    /// The export's own id (what playlists refer to).
    pub id: i64,
    pub path: PathBuf,
    pub play_count: u32,
    /// Unix seconds.
    pub last_played: Option<i64>,
    pub added: Option<i64>,
    pub loved: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApplePlaylist {
    pub name: String,
    /// Export ids of its songs, in order.
    pub items: Vec<i64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AppleLibrary {
    pub tracks: Vec<AppleTrack>,
    pub playlists: Vec<ApplePlaylist>,
}

/// Reads an exported library. `None` when the text is not one.
pub fn parse(xml: &str) -> Option<AppleLibrary> {
    let root = Parser { text: xml, at: 0 }.document()?;
    let Value::Dict(tracks) = root.get("Tracks")? else {
        return None;
    };
    let mut library = AppleLibrary::default();
    for (_, track) in tracks {
        let Some(id) = track.integer("Track ID") else {
            continue;
        };
        // Songs without a file (streamed, in the cloud) have no location.
        let Some(path) = track.string("Location").and_then(file_url_to_path) else {
            continue;
        };
        library.tracks.push(AppleTrack {
            id,
            path,
            play_count: track
                .integer("Play Count")
                .map_or(0, |n| u32::try_from(n.max(0)).unwrap_or(u32::MAX)),
            last_played: track.date("Play Date UTC"),
            added: track.date("Date Added"),
            loved: track.flag("Loved") || track.flag("Favorited"),
        });
    }
    if let Some(Value::Array(playlists)) = root.get("Playlists") {
        for playlist in playlists {
            // The library itself, the built-in lists (music, films, …), folders and
            // smart playlists are not the user's own lists of songs.
            if playlist.flag("Master")
                || playlist.get("Distinguished Kind").is_some()
                || playlist.flag("Folder")
                || playlist.get("Smart Info").is_some()
            {
                continue;
            }
            let Some(name) = playlist
                .string("Name")
                .map(str::trim)
                .filter(|n| !n.is_empty())
            else {
                continue;
            };
            let items: Vec<i64> = match playlist.get("Playlist Items") {
                Some(Value::Array(items)) => {
                    items.iter().filter_map(|i| i.integer("Track ID")).collect()
                }
                _ => Vec::new(),
            };
            if !items.is_empty() {
                library.playlists.push(ApplePlaylist {
                    name: name.to_string(),
                    items,
                });
            }
        }
    }
    Some(library)
}

/// Finds the songs of an export among files that lie elsewhere than the export says:
/// a library that was copied to another computer or to a phone keeps the folders its
/// files are in (artist, album) and their names, not the place those folders were in.
///
/// A song is taken for the file whose last three parts (two folders and the name) are
/// the same, or failing that the last two; never for the name alone, and only where
/// that end belongs to exactly one song of the export and one file here (two discs
/// with a "01 Intro" in folders of the same name stay unmatched rather than guessed).
/// Letter case and the way accents and Hangul are composed do not count.
pub fn match_by_tail<A: Copy + Eq + std::hash::Hash, B: Copy + Eq + std::hash::Hash>(
    export: &[(A, &Path)],
    files: &[(B, &Path)],
) -> HashMap<A, B> {
    use unicode_normalization::UnicodeNormalization;
    let tail = |path: &Path, parts: usize| -> Option<String> {
        let all: Vec<String> = path
            .components()
            .filter_map(|c| match c {
                std::path::Component::Normal(part) => Some(
                    part.to_string_lossy()
                        .nfc()
                        .collect::<String>()
                        .to_lowercase(),
                ),
                _ => None,
            })
            .collect();
        (all.len() >= parts).then(|| all[all.len() - parts..].join("/"))
    };
    let mut found: HashMap<A, B> = HashMap::new();
    let mut taken: std::collections::HashSet<B> = std::collections::HashSet::new();
    for parts in [3, 2] {
        let mut theirs: HashMap<String, Vec<A>> = HashMap::new();
        for (id, path) in export {
            if !found.contains_key(id)
                && let Some(key) = tail(path, parts)
            {
                theirs.entry(key).or_default().push(*id);
            }
        }
        let mut ours: HashMap<String, Vec<B>> = HashMap::new();
        for (id, path) in files {
            if !taken.contains(id)
                && let Some(key) = tail(path, parts)
            {
                ours.entry(key).or_default().push(*id);
            }
        }
        for (key, songs) in theirs {
            if let ([song], Some([file])) = (songs.as_slice(), ours.get(&key).map(Vec::as_slice)) {
                found.insert(*song, *file);
                taken.insert(*file);
            }
        }
    }
    found
}

/// `file:///Users/me/Music/a%20b.mp3` (or `file://localhost/…`) as a path. Other
/// schemes and hosts (another computer) are not files of this one.
fn file_url_to_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let bytes = percent_decode(rest)?;
    let text = String::from_utf8(bytes).ok()?;
    #[cfg(windows)]
    {
        // file:///C:/Users/… → C:\Users\…
        let text = text.strip_prefix('/').unwrap_or(&text).replace('/', "\\");
        Some(PathBuf::from(text))
    }
    #[cfg(not(windows))]
    Some(PathBuf::from(text))
}

fn percent_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(out)
}

/// `2024-01-02T03:04:05Z` as Unix seconds.
fn parse_date(text: &str) -> Option<i64> {
    let text = text.trim().strip_suffix('Z')?;
    let (date, time) = text.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (year, month, day) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.split(':').map(|p| p.parse::<i64>().ok());
    let (hour, minute, second) = (t.next()??, t.next()??, t.next()??);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    // Days from the civil calendar (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Just enough XML for a property list: elements, text, entities, comments, the
/// declaration and the doctype.
struct Parser<'a> {
    text: &'a str,
    at: usize,
}

impl<'a> Parser<'a> {
    fn rest(&self) -> &'a str {
        &self.text[self.at..]
    }

    fn skip_space_and_comments(&mut self) {
        loop {
            let trimmed = self.rest().trim_start();
            self.at = self.text.len() - trimmed.len();
            if let Some(after) = trimmed.strip_prefix("<!--") {
                match after.find("-->") {
                    Some(end) => self.at += 4 + end + 3,
                    None => self.at = self.text.len(),
                }
            } else if trimmed.starts_with("<?") || trimmed.starts_with("<!") {
                match trimmed.find('>') {
                    Some(end) => self.at += end + 1,
                    None => self.at = self.text.len(),
                }
            } else {
                return;
            }
        }
    }

    /// The next tag: its name and whether it closes (`</x>`) or is empty (`<x/>`).
    fn tag(&mut self) -> Option<(String, bool, bool)> {
        self.skip_space_and_comments();
        let rest = self.rest();
        if !rest.starts_with('<') {
            return None;
        }
        let end = rest.find('>')?;
        let inner = &rest[1..end];
        self.at += end + 1;
        let closing = inner.starts_with('/');
        let empty = inner.ends_with('/');
        let name = inner
            .trim_start_matches('/')
            .trim_end_matches('/')
            .split_whitespace()
            .next()?
            .to_string();
        Some((name, closing, empty))
    }

    /// Text up to the closing tag `name`, entities decoded.
    fn text_until(&mut self, name: &str) -> Option<String> {
        let close = format!("</{name}>");
        let end = self.rest().find(&close)?;
        let raw = &self.rest()[..end];
        self.at += end + close.len();
        Some(decode_entities(raw))
    }

    fn document(mut self) -> Option<Value> {
        let (name, closing, _) = self.tag()?;
        if name != "plist" || closing {
            return None;
        }
        self.value(0)
    }

    fn value(&mut self, depth: usize) -> Option<Value> {
        if depth > MAX_DEPTH {
            return None;
        }
        let (name, closing, empty) = self.tag()?;
        if closing {
            return None;
        }
        Some(match name.as_str() {
            "dict" if empty => Value::Dict(Vec::new()),
            "array" if empty => Value::Array(Vec::new()),
            "dict" => {
                let mut entries = Vec::new();
                loop {
                    let (tag, closing, _) = self.tag()?;
                    match (tag.as_str(), closing) {
                        ("dict", true) => break,
                        ("key", false) => {
                            let key = self.text_until("key")?;
                            entries.push((key, self.value(depth + 1)?));
                        }
                        _ => return None,
                    }
                }
                Value::Dict(entries)
            }
            "array" => {
                let mut items = Vec::new();
                loop {
                    self.skip_space_and_comments();
                    if self.rest().starts_with("</array>") {
                        self.at += "</array>".len();
                        break;
                    }
                    items.push(self.value(depth + 1)?);
                }
                Value::Array(items)
            }
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            "string" if empty => Value::String(String::new()),
            "string" => Value::String(self.text_until("string")?),
            "integer" => Value::Integer(self.text_until("integer")?.trim().parse().ok()?),
            "date" => Value::Date(self.text_until("date")?),
            "real" | "data" if !empty => {
                self.text_until(&name)?;
                Value::Other
            }
            _ if empty => Value::Other,
            _ => return None,
        })
    }
}

fn decode_entities(raw: &str) -> String {
    if !raw.contains('&') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        // Only the next few bytes can end an entity: looking further would make text
        // full of `&` take quadratic time.
        let window = &rest.as_bytes()[..rest.len().min(11)];
        let Some(end) = window.iter().position(|&b| b == b';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Major Version</key><integer>1</integer>
	<key>Application Version</key><string>1.5.0.75</string>
	<key>Tracks</key>
	<dict>
		<key>101</key>
		<dict>
			<key>Track ID</key><integer>101</integer>
			<key>Name</key><string>밤편지</string>
			<key>Play Count</key><integer>42</integer>
			<key>Play Date UTC</key><date>2024-01-02T03:04:05Z</date>
			<key>Date Added</key><date>2019-03-24T00:00:00Z</date>
			<key>Loved</key><true/>
			<key>Normalization</key><integer>1234</integer>
			<key>Location</key><string>file:///Users/me/Music/%EC%95%84%EC%9D%B4%EC%9C%A0/Palette/01%20Night%20Letter.flac</string>
		</dict>
		<key>102</key>
		<dict>
			<key>Track ID</key><integer>102</integer>
			<key>Name</key><string>Rock &amp; Roll</string>
			<key>Favorited</key><true/>
			<key>Location</key><string>file://localhost/Users/me/Music/R%26R.mp3</string>
		</dict>
		<key>103</key>
		<dict>
			<key>Track ID</key><integer>103</integer>
			<key>Name</key><string>Streamed</string>
			<key>Play Count</key><integer>9</integer>
		</dict>
	</dict>
	<key>Playlists</key>
	<array>
		<dict><key>Name</key><string>Library</string><key>Master</key><true/>
			<key>Playlist Items</key><array><dict><key>Track ID</key><integer>101</integer></dict></array></dict>
		<dict><key>Name</key><string>Music</string><key>Distinguished Kind</key><integer>4</integer>
			<key>Playlist Items</key><array><dict><key>Track ID</key><integer>101</integer></dict></array></dict>
		<dict><key>Name</key><string>Recently Played</string><key>Smart Info</key><data>AQEA</data>
			<key>Playlist Items</key><array><dict><key>Track ID</key><integer>101</integer></dict></array></dict>
		<dict><key>Name</key><string>Folder</string><key>Folder</key><true/></dict>
		<dict><key>Name</key><string>드라이브 &lt;2024&gt;</string>
			<key>Playlist Items</key>
			<array>
				<dict><key>Track ID</key><integer>102</integer></dict>
				<dict><key>Track ID</key><integer>101</integer></dict>
			</array>
		</dict>
		<dict><key>Name</key><string>Empty</string><key>Playlist Items</key><array/></dict>
	</array>
</dict>
</plist>
"#;

    #[test]
    fn an_exported_library_is_read() {
        let library = parse(SAMPLE).expect("parses");
        assert_eq!(library.tracks.len(), 2, "the streamed song has no file");
        let first = &library.tracks[0];
        assert_eq!(first.id, 101);
        assert_eq!(
            first.path,
            PathBuf::from("/Users/me/Music/아이유/Palette/01 Night Letter.flac")
        );
        assert_eq!(first.play_count, 42);
        assert_eq!(first.last_played, Some(1_704_164_645));
        assert_eq!(first.added, Some(1_553_385_600));
        assert!(first.loved);
        let second = &library.tracks[1];
        assert_eq!(second.path, PathBuf::from("/Users/me/Music/R&R.mp3"));
        assert_eq!((second.play_count, second.last_played), (0, None));
        assert!(second.loved, "Favorited counts as loved");
        assert_eq!(
            library.playlists,
            [ApplePlaylist {
                name: "드라이브 <2024>".into(),
                items: vec![102, 101],
            }]
        );
    }

    #[test]
    fn songs_copied_elsewhere_are_found_by_the_end_of_their_path() {
        let mac = |rest: &str| PathBuf::from(format!("/Users/me/Music/Media/{rest}"));
        let phone = |rest: &str| PathBuf::from(format!("/var/mobile/Documents/{rest}"));
        let export = [
            (1, mac("IU/Flower Bookmark/01 Song.m4a")),
            // Another case, and Hangul written in parts as a Mac's disk keeps it.
            (2, mac("Beatles/Abbey Road/02 SOMETHING.mp3")),
            (
                3,
                mac("\u{1112}\u{1161}\u{11ab}/\u{1100}\u{1161}\u{11bc}/03.flac"),
            ),
            // In two albums of the same name here: not guessed.
            (4, mac("Various/Hits/01 Intro.mp3")),
            // Not here at all.
            (5, mac("Gone/Album/09.mp3")),
            // The artist folder has another name here: the album and file still say it.
            (6, mac("Nova Lane/Late Night/05 Drive.flac")),
        ];
        let files = [
            (10, phone("IU/Flower Bookmark/01 Song.m4a")),
            (20, phone("Beatles/Abbey Road/02 Something.mp3")),
            (30, phone("한/강/03.flac")),
            (40, phone("Various/Hits/01 Intro.mp3")),
            (41, phone("Backup/Various/Hits/01 Intro.mp3")),
            (60, phone("Lane, Nova/Late Night/05 Drive.flac")),
            (70, phone("01 Song.m4a")),
        ];
        let export: Vec<(i32, &Path)> = export.iter().map(|(id, p)| (*id, p.as_path())).collect();
        let files: Vec<(i32, &Path)> = files.iter().map(|(id, p)| (*id, p.as_path())).collect();
        let found = match_by_tail(&export, &files);
        assert_eq!(found.get(&1), Some(&10));
        assert_eq!(found.get(&2), Some(&20));
        assert_eq!(found.get(&3), Some(&30));
        assert_eq!(found.get(&4), None, "two files end the same way");
        assert_eq!(found.get(&5), None);
        assert_eq!(found.get(&6), Some(&60));
        assert_eq!(found.len(), 4);
        // A file's name alone is never enough.
        let lone = [(1, Path::new("/Users/me/Music/01 Song.m4a"))];
        let named = [(70, Path::new("/var/mobile/Documents/Other/01 Song.m4a"))];
        assert!(match_by_tail(&lone, &named).is_empty());
    }

    #[test]
    fn what_is_not_a_library_is_refused() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("<html><body>no</body></html>"), None);
        assert_eq!(parse("<plist><dict><key>Tracks</key>"), None);
        // Nesting without end does not exhaust the stack.
        let deep = format!("<plist>{}", "<array>".repeat(100_000));
        assert_eq!(parse(&deep), None);
        assert_eq!(parse_date("2024-13-01T00:00:00Z"), None);
        assert_eq!(parse_date("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(file_url_to_path("http://example.com/a.mp3"), None);
        assert_eq!(file_url_to_path("file://server/share/a.mp3"), None);
        assert_eq!(
            decode_entities("a &amp; b &#xAC00;&#44032; &bogus; &"),
            "a & b 가가 &bogus; &"
        );
        // Text full of `&` without `;` near: each one looks only a few bytes ahead
        // (looking to the end every time took minutes for a few megabytes).
        let ampersands = "&".repeat(2_000_000);
        let started = std::time::Instant::now();
        assert_eq!(decode_entities(&ampersands), ampersands);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        assert_eq!(decode_entities("&amp;&&&lt;"), "&&&<");
    }
}
