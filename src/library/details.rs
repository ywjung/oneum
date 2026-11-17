//! What a file says about a song beyond what the library keeps: who wrote, performed
//! and produced it, the work and movement it belongs to, its label and dates, and its
//! lyrics. Read from the file when the info panel shows the song, never stored.
use super::lyrics::{self, Lyrics};
use lofty::file::{TaggedFile, TaggedFileExt};
use lofty::tag::{ItemKey, Tag};
use std::path::Path;

/// A part somebody had in a recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Composer,
    Lyricist,
    Conductor,
    Arranger,
    Performer,
    Producer,
    Engineer,
    Mixer,
    Remixer,
}

/// The people (or ensembles) named for one role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credit {
    pub role: Role,
    pub names: Vec<String>,
}

/// A fact about the recording that is not a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Fact {
    /// The work a classical track is part of ("Symphony No. 5").
    Work,
    /// The movement within it ("II. Andante con moto").
    Movement,
    Label,
    CatalogNumber,
    Recorded,
    Released,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Details {
    pub credits: Vec<Credit>,
    pub facts: Vec<(Fact, String)>,
    pub lyrics: Option<Lyrics>,
    /// The lyrics are not the file's: they were found on the internet.
    pub lyrics_online: bool,
}

/// No more names than this per role, and none longer: a tag is not a book.
const MAX_NAMES: usize = 12;
const MAX_VALUE_CHARS: usize = 120;

const ROLES: [(Role, ItemKey); 9] = [
    (Role::Composer, ItemKey::Composer),
    (Role::Lyricist, ItemKey::Lyricist),
    (Role::Conductor, ItemKey::Conductor),
    (Role::Arranger, ItemKey::Arranger),
    (Role::Performer, ItemKey::Performer),
    (Role::Producer, ItemKey::Producer),
    (Role::Engineer, ItemKey::Engineer),
    (Role::Mixer, ItemKey::MixEngineer),
    (Role::Remixer, ItemKey::Remixer),
];

const FACTS: [(Fact, &[ItemKey]); 6] = [
    (Fact::Work, &[ItemKey::Work]),
    (Fact::Movement, &[ItemKey::Movement]),
    (Fact::Label, &[ItemKey::Label, ItemKey::Publisher]),
    (Fact::CatalogNumber, &[ItemKey::CatalogNumber]),
    (Fact::Recorded, &[ItemKey::RecordingDate]),
    (Fact::Released, &[ItemKey::ReleaseDate]),
];

/// A tag value as it is shown: Korean from an old code page read as Korean, one line,
/// no longer than fits. `None` when nothing is left.
fn value(text: &str) -> Option<String> {
    let repaired = super::legacy::repair(text);
    let text = repaired.as_deref().unwrap_or(text);
    let one_line = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>();
    let tidy = one_line.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut: String = tidy.chars().take(MAX_VALUE_CHARS).collect();
    (!cut.is_empty()).then_some(cut)
}

/// The names in the values of one tag field. A field can hold several names, as
/// several values or in one separated by ";" or " / " (a comma is left alone: it is
/// part of "Bach, Johann Sebastian").
pub(crate) fn names<'a>(values: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for raw in values {
        for part in raw.split([';', '\0']).flat_map(|p| p.split(" / ")) {
            let Some(name) = value(part) else { continue };
            let known = found
                .iter()
                .any(|n| n.to_lowercase() == name.to_lowercase());
            if !known && found.len() < MAX_NAMES {
                found.push(name);
            }
        }
    }
    found
}

/// The tags of a file, the primary one first.
fn tags_of(file: &TaggedFile) -> Vec<&Tag> {
    let mut tags: Vec<&Tag> = Vec::new();
    tags.extend(file.primary_tag());
    for tag in file.tags() {
        if !tags.iter().any(|known| std::ptr::eq(*known, tag)) {
            tags.push(tag);
        }
    }
    tags
}

fn from_tagged(file: &TaggedFile) -> (Vec<Credit>, Vec<(Fact, String)>) {
    let tags = tags_of(file);
    let credits = ROLES
        .iter()
        .filter_map(|(role, key)| {
            // The first tag that names anybody for the role speaks for the file.
            let names = tags
                .iter()
                .map(|tag| names(tag.get_strings(*key)))
                .find(|names| !names.is_empty())?;
            Some(Credit { role: *role, names })
        })
        .collect();
    let facts = FACTS
        .iter()
        .filter_map(|(fact, keys)| {
            let text = keys.iter().find_map(|key| {
                tags.iter()
                    .filter_map(|tag| tag.get_string(*key))
                    .find_map(value)
            })?;
            Some((*fact, text))
        })
        .collect();
    (credits, facts)
}

impl Details {
    /// Whether lyrics from the internet would add something: the file has none that
    /// follow the song.
    pub fn wants_online_lyrics(&self) -> bool {
        !self.lyrics.as_ref().is_some_and(|l| l.timed.is_some())
    }
}

/// Everything the file of the track at `path` says beyond what the library keeps. A
/// file that cannot be read says nothing (these are extras, not worth an error
/// message); its lyrics may still come from a text file next to it.
///
/// For a track of a CUE sheet that is the sheet's audio file: its credits are the
/// album's. Lyrics in that file would be the whole album's, so none are taken from it.
pub fn read(path: &Path) -> Details {
    let sheet_track = super::cue::sheet_of(path).is_some();
    let file = super::cue::real_file(path);
    let tagged = super::tags::read_tagged(&file);
    let (credits, facts) = tagged.as_ref().map(from_tagged).unwrap_or_default();
    let lyrics = if sheet_track {
        None
    } else {
        tagged
            .as_ref()
            .and_then(lyrics::from_tags)
            .or_else(|| lyrics::from_sidecar(path))
    };
    Details {
        credits,
        facts,
        lyrics,
        lyrics_online: false,
    }
}

/// Lyrics from the internet for a song whose file has none with times (`details`:
/// what the file says). Called only for a listener who has switched online lyrics
/// on. What the file has stays unless what is found has times and the file's has not.
pub fn with_online_lyrics(
    mut details: Details,
    source: &super::online_lyrics::Source,
    song: &crate::net::lyrics::Song<'_>,
) -> Details {
    if !details.wants_online_lyrics() {
        return details;
    }
    if let Some(found) = source.find(song)
        && (found.timed.is_some() || details.lyrics.is_none())
    {
        details.lyrics = Some(found);
        details.lyrics_online = true;
    }
    details
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn several_names_in_one_field() {
        assert_eq!(
            names(["Lennon; McCartney", "  George Martin  "]),
            ["Lennon", "McCartney", "George Martin"]
        );
        assert_eq!(names(["A / B", "a", "B"]), ["A", "B"], "no name twice");
        // A comma is part of a name written surname first; AC/DC is one name.
        assert_eq!(
            names(["Bach, Johann Sebastian"]),
            ["Bach, Johann Sebastian"]
        );
        assert_eq!(names(["AC/DC"]), ["AC/DC"]);
        assert_eq!(names(["one\0two"]), ["one", "two"]);
        assert!(names(["", " ; ", "\n"]).is_empty());
        let many: Vec<String> = (0..50).map(|i| format!("Player {i}")).collect();
        assert_eq!(names(many.iter().map(String::as_str)).len(), MAX_NAMES);
        assert_eq!(
            names([&"가".repeat(500)[..]])[0].chars().count(),
            MAX_VALUE_CHARS
        );
    }

    #[test]
    fn credits_and_facts_are_read_from_the_tags() {
        use lofty::config::WriteOptions;
        use lofty::tag::{TagExt, TagType};
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let dir = tempfile::tempdir().expect("temp dir");
        for (name, kind) in [
            ("gap_a.flac", TagType::VorbisComments),
            ("gap_a.mp3", TagType::Id3v2),
        ] {
            let folder = dir.path().join(name);
            std::fs::create_dir(&folder).expect("folder");
            let song = folder.join(name);
            std::fs::copy(fixtures.join(name), &song).expect("copy fixture");
            assert_eq!(read(&song), Details::default(), "{name}: nothing yet");

            let mut tag = Tag::new(kind);
            tag.insert_text(ItemKey::Composer, "Ludwig van Beethoven".to_string());
            tag.insert_text(ItemKey::Conductor, "Carlos Kleiber".to_string());
            tag.insert_text(ItemKey::Performer, "Wiener Philharmoniker".to_string());
            tag.insert_text(
                ItemKey::Work,
                "Symphony No. 5 in C minor, Op. 67".to_string(),
            );
            tag.insert_text(ItemKey::Movement, "I. Allegro con brio".to_string());
            tag.insert_text(ItemKey::Label, "Deutsche Grammophon".to_string());
            tag.insert_text(
                ItemKey::Lyrics,
                "[00:01.00]da da da\n[00:03.00]dum".to_string(),
            );
            tag.save_to_path(&song, WriteOptions::default())
                .expect("tag written");

            let details = read(&song);
            let credit = |role| {
                details
                    .credits
                    .iter()
                    .find(|c| c.role == role)
                    .map(|c| c.names.join(", "))
            };
            assert_eq!(
                credit(Role::Composer).as_deref(),
                Some("Ludwig van Beethoven"),
                "{name}"
            );
            assert_eq!(
                credit(Role::Conductor).as_deref(),
                Some("Carlos Kleiber"),
                "{name}"
            );
            assert_eq!(credit(Role::Lyricist), None);
            let fact = |fact| {
                details
                    .facts
                    .iter()
                    .find(|(f, _)| *f == fact)
                    .map(|(_, text)| text.as_str())
            };
            assert_eq!(fact(Fact::Label), Some("Deutsche Grammophon"), "{name}");
            // Roles come out in a fixed order: composer before conductor.
            let order: Vec<Role> = details.credits.iter().map(|c| c.role).collect();
            assert!(
                order.starts_with(&[Role::Composer, Role::Conductor]),
                "{name}: {order:?}"
            );
            if kind == TagType::VorbisComments {
                assert_eq!(
                    credit(Role::Performer).as_deref(),
                    Some("Wiener Philharmoniker")
                );
                assert_eq!(fact(Fact::Work), Some("Symphony No. 5 in C minor, Op. 67"));
                assert_eq!(fact(Fact::Movement), Some("I. Allegro con brio"));
                let lyrics = details.lyrics.as_ref().expect("lyrics");
                assert_eq!(lyrics.plain, "da da da\ndum");
                assert_eq!(lyrics.line_at(3_500), Some(1));

                // The same file as the audio file of a CUE sheet: a track of it has the
                // file's credits (the album's), not its lyrics (the whole album's).
                let sheet = folder.join("album.cue");
                std::fs::write(
                    &sheet,
                    format!(
                        "FILE \"{name}\" WAVE\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n\
                         TRACK 02 AUDIO\nINDEX 01 00:00:30\n"
                    ),
                )
                .expect("sheet");
                let track = read(&super::super::cue::track_path(&sheet, 2));
                assert_eq!(track.credits, details.credits);
                assert_eq!(track.facts, details.facts);
                assert!(track.lyrics.is_none());
            }
        }
        // A file that is not audio says nothing, quietly.
        let junk = dir.path().join("junk.flac");
        std::fs::write(&junk, b"not audio").expect("write");
        assert_eq!(read(&junk), Details::default());
    }
}
