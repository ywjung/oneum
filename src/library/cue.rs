//! CUE sheets: a whole album in one audio file (a CD image) and a small text file that
//! says where each track begins.
//!
//! Each track of a sheet becomes a track of the library under the path
//! `<sheet>/<number>`, e.g. `…/Abbey Road.cue/03`. No real file can have that path (the
//! sheet is a file, not a folder), so it never meets one. Whoever opens a track's audio
//! asks [`resolve`] (or [`real_file`]) for the file and the stretch of it to play.
//!
//! Only sheets for one audio file are taken: a sheet that names a file per track
//! describes files the scan finds on their own anyway.
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

/// The largest sheet read (real ones are a few kilobytes).
pub const MAX_SHEET_BYTES: u64 = 1 << 20;
/// A sheet counts time in CD frames: 75 to the second.
pub const FRAMES_PER_SECOND: u64 = 75;
/// Tracks of one sheet, at most (a CD has 99).
const MAX_TRACKS: usize = 255;

/// A sheet as written.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sheet {
    pub title: String,
    pub performer: String,
    pub genre: String,
    pub year: Option<i32>,
    pub disc: Option<u32>,
    pub album_gain_db: Option<f32>,
    pub album_peak: Option<f32>,
    /// The audio files it names, as written.
    pub files: Vec<String>,
    /// The audio tracks that have a start, in order of their start.
    pub tracks: Vec<SheetTrack>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SheetTrack {
    pub number: u32,
    pub title: String,
    pub performer: String,
    /// Where the track begins (its `INDEX 01`), in CD frames from the start of the file.
    pub start: u64,
    pub gain_db: Option<f32>,
    pub peak: Option<f32>,
}

/// The stretch of an audio file that one track of a sheet is.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub audio: PathBuf,
    /// In CD frames.
    pub start: u64,
    /// In CD frames; `None`: to the end of the file.
    pub end: Option<u64>,
}

impl Segment {
    /// The first sample frame of the track at `rate` (exact for the usual rates: 75
    /// divides 44,100 and 48,000 and their multiples).
    pub fn start_frame(&self, rate: u32) -> u64 {
        self.start * u64::from(rate) / FRAMES_PER_SECOND
    }

    /// The sample frame the track ends before, if it does not run to the end of the file.
    pub fn end_frame(&self, rate: u32) -> Option<u64> {
        self.end
            .map(|end| end * u64::from(rate) / FRAMES_PER_SECOND)
    }
}

/// What stands for a sheet track's state on disk (its "modification time" in the
/// library): the audio file's, marked with the track's stretch of it. It changes when
/// the file changes or when the sheet moves the track's start or end, but not when only
/// names in the sheet change, so that what was measured of the track stays valid.
pub fn stretch_stamp(audio_mtime_ns: i64, start: u64, end: Option<u64>) -> i64 {
    let mut bytes = [0u8; 17];
    bytes[..8].copy_from_slice(&start.to_le_bytes());
    if let Some(end) = end {
        bytes[8] = 1;
        bytes[9..].copy_from_slice(&end.to_le_bytes());
    }
    let mark = (super::fnv1a64(&bytes) & 0x00FF_FFFF) as i64;
    audio_mtime_ns ^ mark
}

/// CD frames as milliseconds.
pub fn frames_to_ms(frames: u64) -> u64 {
    frames * 1000 / FRAMES_PER_SECOND
}

/// The library path of track `number` of the sheet at `sheet`.
pub fn track_path(sheet: &Path, number: u32) -> PathBuf {
    sheet.join(format!("{number:02}"))
}

/// The sheet a track path belongs to, if it is a sheet's track (no file is read).
pub fn sheet_of(path: &Path) -> Option<&Path> {
    let sheet = path.parent()?;
    let is_sheet = sheet
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cue"));
    let numbered = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    (is_sheet && numbered).then_some(sheet)
}

/// The number of a sheet's track (the last part of its path), if it is one.
pub fn track_number(path: &Path) -> Option<u32> {
    sheet_of(path)?;
    path.file_name()?.to_str()?.parse().ok()
}

/// The file that stands for a track on disk without reading anything: the sheet for a
/// sheet's track, the path itself otherwise. (Its folder is the album's folder.)
pub fn backing_file(path: &Path) -> &Path {
    sheet_of(path).unwrap_or(path)
}

/// The audio file to open for a track: the image of a sheet's track (the sheet is read),
/// the path itself otherwise.
pub fn real_file(path: &Path) -> Cow<'_, Path> {
    match resolve(path) {
        Some(segment) => Cow::Owned(segment.audio),
        None => Cow::Borrowed(path),
    }
}

/// For a sheet's track: the audio file and the stretch of it. `None` for any other
/// path, and for a sheet that cannot be read or no longer has that track.
pub fn resolve(path: &Path) -> Option<Segment> {
    let sheet_path = sheet_of(path)?;
    let number = track_number(path)?;
    let sheet = read(sheet_path)?;
    let audio = audio_of(sheet_path, &sheet)?;
    let at = sheet.tracks.iter().position(|t| t.number == number)?;
    Some(Segment {
        audio,
        start: sheet.tracks[at].start,
        end: sheet.tracks.get(at + 1).map(|next| next.start),
    })
}

/// Reads and parses the sheet at `path` (bounded in size; Unicode or the Korean code page
/// of old Windows, like playlists).
pub fn read(path: &Path) -> Option<Sheet> {
    read_checked(path).ok().flatten()
}

/// Like [`read`], telling apart what is there but is not a usable sheet (`Ok(None)`)
/// from what could not be read (`Err`: says nothing about what the sheet holds).
pub fn read_checked(path: &Path) -> std::io::Result<Option<Sheet>> {
    let bytes = crate::storage::files::read_bounded(path, MAX_SHEET_BYTES)?;
    Ok(bytes.and_then(|bytes| parse(&super::m3u::decode(&bytes))))
}

/// The one audio file a sheet is for (see [`audio_of_checked`]); `None` also when that
/// cannot be found out just now.
pub fn audio_of(sheet_path: &Path, sheet: &Sheet) -> Option<PathBuf> {
    audio_of_checked(sheet_path, sheet).ok().flatten()
}

/// A name as compared with the names a sheet writes: letter case and Unicode form left
/// aside (a sheet written on Windows says `abbey road.wav`, or holds composed Hangul
/// where the file's name on a Mac is decomposed).
fn folded(name: &str) -> String {
    name.nfc().collect::<String>().to_lowercase()
}

/// The one audio file a sheet is for: the file it names in the sheet's folder, or one
/// there of the same name with another audio extension, as spelled in the folder (the
/// sheet's spelling may differ in letter case or Unicode form; the volume may not mind,
/// but the library compares paths as written). `Ok(None)` for a sheet that names
/// several files, or whose file is not there; `Err` when the folder or a file that may
/// be it cannot be examined just now, which says nothing about whether it is there.
pub fn audio_of_checked(sheet_path: &Path, sheet: &Sheet) -> std::io::Result<Option<PathBuf>> {
    let Some(first) = sheet.files.first() else {
        return Ok(None);
    };
    if sheet.files.iter().any(|f| f != first) {
        return Ok(None);
    }
    let Some(dir) = sheet_path.parent() else {
        return Ok(None);
    };
    // Only the name: a sheet does not lead into other folders (also not with a path
    // written on Windows).
    let name = first.rsplit(['/', '\\']).next().unwrap_or("").trim();
    if name.is_empty() || name == "." || name == ".." {
        return Ok(None);
    }
    // Usually the file is there under the name written: then the system gives its
    // spelling in the folder (a volume that ignores letter case or Unicode form opens
    // it under the sheet's), without the folder being listed. Not for a link, whose
    // resolved name would be its target's.
    let named = dir.join(name);
    if super::tags::has_audio_extension(&named)
        && std::fs::symlink_metadata(&named).is_ok_and(|meta| meta.is_file())
        && let Some(spelled) = std::fs::canonicalize(&named)
            .ok()
            .and_then(|real| real.file_name().map(|n| n.to_os_string()))
    {
        return Ok(Some(dir.join(spelled)));
    }
    let wanted = folded(name);
    // A rip to WAV that was converted afterwards still names the `.wav`: the same name
    // with another extension.
    let wanted_stem = Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(folded);
    // The best match so far: (0 the name as written, 1 the name spelled otherwise, 2 the
    // same name with another extension; then by name), and the best kind of match that
    // could not be examined (an entry that could not be listed might be any).
    let mut best: Option<(u8, PathBuf)> = None;
    let mut trouble: Option<(u8, std::io::Error)> = None;
    let mut note = |rank: u8, err: std::io::Error| {
        if trouble.as_ref().is_none_or(|(known, _)| rank < *known) {
            trouble = Some((rank, err));
        }
    };
    for entry in std::fs::read_dir(dir)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                note(0, err);
                continue;
            }
        };
        let path = entry.path();
        // Only a file the app plays: of an original kept beside its conversion (`.ape`
        // and `.flac`), the one that plays.
        if !super::tags::has_audio_extension(&path) {
            continue;
        }
        let Some(entry_name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let rank = if entry_name == name {
            0
        } else if folded(&entry_name) == wanted {
            1
        } else if wanted_stem.is_some()
            && Path::new(&entry_name)
                .file_stem()
                .and_then(|s| s.to_str())
                .map(folded)
                == wanted_stem
        {
            2
        } else {
            continue;
        };
        if best
            .as_ref()
            .is_some_and(|(known, known_path)| (*known, known_path) <= (rank, &path))
        {
            continue;
        }
        match std::fs::metadata(&path) {
            Ok(meta) if meta.is_file() => best = Some((rank, path)),
            Ok(_) => {}
            // A link to nothing: not there, as for the scan.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => note(rank, err),
        }
    }
    // A file that matches at least as well but cannot be examined may be the one: the
    // answer is not known then (rather than another file for a while).
    match (best, trouble) {
        (Some((rank, _)), Some((trouble_rank, err))) if trouble_rank <= rank => Err(err),
        (Some((_, path)), _) => Ok(Some(path)),
        (None, Some((_, err))) => Err(err),
        (None, None) => Ok(None),
    }
}

/// The value of a line's arguments: a quoted string up to its closing quote, or the
/// rest of the line.
fn value(rest: &str) -> String {
    let rest = rest.trim();
    match rest.strip_prefix('"') {
        Some(quoted) => quoted.split('"').next().unwrap_or("").trim().to_string(),
        None => rest.to_string(),
    }
}

/// `mm:ss:ff` as CD frames (minutes may go past 99).
fn index_time(text: &str) -> Option<u64> {
    let mut parts = text.trim().split(':');
    let minutes: u64 = parts.next()?.trim().parse().ok()?;
    let seconds: u64 = parts.next()?.trim().parse().ok()?;
    let frames: u64 = parts.next()?.trim().parse().ok()?;
    if parts.next().is_some() || seconds >= 60 || frames >= FRAMES_PER_SECOND || minutes > 10_000 {
        return None;
    }
    Some((minutes * 60 + seconds) * FRAMES_PER_SECOND + frames)
}

/// `-7.89 dB` or `0.988` as a number.
fn number(text: &str) -> Option<f32> {
    let token = text.split_whitespace().next()?;
    token.parse::<f32>().ok().filter(|v| v.is_finite())
}

/// Parses a sheet. `None` when it names no file or has no audio track with a start.
pub fn parse(text: &str) -> Option<Sheet> {
    let mut sheet = Sheet::default();
    // The track being read; `None` before the first, and for a track that is not audio.
    let mut track: Option<SheetTrack> = None;
    let mut in_track = false;
    let mut tracks: Vec<SheetTrack> = Vec::new();
    let finish = |track: Option<SheetTrack>, tracks: &mut Vec<SheetTrack>| {
        if let Some(track) = track
            && track.start != u64::MAX
            && tracks.len() < MAX_TRACKS
        {
            tracks.push(track);
        }
    };
    for line in text.lines() {
        let line = line.trim_start_matches('\u{feff}').trim();
        let (command, rest) = match line.split_once(char::is_whitespace) {
            Some((command, rest)) => (command, rest.trim()),
            None => (line, ""),
        };
        match command.to_ascii_uppercase().as_str() {
            "FILE" => {
                let name = if rest.starts_with('"') {
                    value(rest)
                } else {
                    // `FILE name.wav WAVE`: the type is the last word.
                    rest.rsplit_once(char::is_whitespace)
                        .map_or(rest, |(name, _)| name)
                        .trim()
                        .to_string()
                };
                if !name.is_empty() {
                    sheet.files.push(name);
                }
            }
            "TRACK" => {
                finish(track.take(), &mut tracks);
                in_track = true;
                let mut words = rest.split_whitespace();
                let number = words.next().and_then(|n| n.parse::<u32>().ok());
                let audio = words
                    .next()
                    .is_some_and(|kind| kind.eq_ignore_ascii_case("AUDIO"));
                if let (Some(number), true) = (number, audio) {
                    track = Some(SheetTrack {
                        number,
                        start: u64::MAX,
                        ..SheetTrack::default()
                    });
                }
            }
            "INDEX" => {
                let mut words = rest.split_whitespace();
                if words.next().and_then(|n| n.parse::<u32>().ok()) == Some(1)
                    && let (Some(track), Some(at)) =
                        (track.as_mut(), words.next().and_then(index_time))
                {
                    track.start = at;
                }
            }
            "TITLE" | "PERFORMER" => {
                let text: String = value(rest).chars().take(1_000).collect();
                let field = match (in_track, track.as_mut()) {
                    (false, _) => Some(if command.eq_ignore_ascii_case("TITLE") {
                        &mut sheet.title
                    } else {
                        &mut sheet.performer
                    }),
                    (true, Some(track)) => Some(if command.eq_ignore_ascii_case("TITLE") {
                        &mut track.title
                    } else {
                        &mut track.performer
                    }),
                    (true, None) => None,
                };
                if let Some(field) = field {
                    *field = text;
                }
            }
            "REM" => {
                let (key, rest) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                match key.to_ascii_uppercase().as_str() {
                    "GENRE" if !in_track => {
                        sheet.genre = value(rest).chars().take(200).collect();
                    }
                    "DATE" if !in_track => {
                        sheet.year = value(rest)
                            .get(..4)
                            .and_then(|y| y.parse().ok())
                            .filter(|y| (1000..=9999).contains(y));
                    }
                    "DISCNUMBER" if !in_track => {
                        sheet.disc = value(rest)
                            .split('/')
                            .next()
                            .and_then(|d| d.trim().parse().ok());
                    }
                    "REPLAYGAIN_ALBUM_GAIN" => sheet.album_gain_db = number(rest),
                    "REPLAYGAIN_ALBUM_PEAK" => sheet.album_peak = number(rest),
                    "REPLAYGAIN_TRACK_GAIN" => {
                        if let Some(track) = track.as_mut() {
                            track.gain_db = number(rest);
                        }
                    }
                    "REPLAYGAIN_TRACK_PEAK" => {
                        if let Some(track) = track.as_mut() {
                            track.peak = number(rest);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    finish(track.take(), &mut tracks);
    // In the order they play; a track that does not start after the one before it, or
    // has the number of one before it (two tracks would be one in the library, whose
    // tracks are known by number), makes no sense and is left out.
    let mut ordered: Vec<SheetTrack> = Vec::with_capacity(tracks.len());
    let mut numbers = std::collections::HashSet::new();
    for track in tracks {
        let fits = ordered.last().is_none_or(|last| track.start > last.start)
            && !numbers.contains(&track.number);
        if fits {
            numbers.insert(track.number);
            ordered.push(track);
        }
    }
    sheet.tracks = ordered;
    (!sheet.files.is_empty() && !sheet.tracks.is_empty()).then_some(sheet)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHEET: &str = "\u{feff}REM GENRE \"Rock\"\r
REM DATE 1969\r
REM DISCID 7A0B4C09\r
REM REPLAYGAIN_ALBUM_GAIN -7.89 dB\r
REM REPLAYGAIN_ALBUM_PEAK 0.988647\r
PERFORMER \"The Beatles\"\r
TITLE \"Abbey Road\"\r
FILE \"Abbey Road.wav\" WAVE\r
  TRACK 01 AUDIO\r
    TITLE \"Come Together\"\r
    REM REPLAYGAIN_TRACK_GAIN -6.50 dB\r
    REM REPLAYGAIN_TRACK_PEAK 0.95\r
    INDEX 01 00:00:00\r
  TRACK 02 AUDIO\r
    TITLE \"Something\"\r
    PERFORMER \"George Harrison\"\r
    INDEX 00 04:19:30\r
    INDEX 01 04:20:12\r
  TRACK 03 MODE1/2352\r
    INDEX 01 07:23:00\r
  TRACK 04 AUDIO\r
    TITLE \"Maxwell's Silver Hammer\"\r
    INDEX 01 07:23:00\r
";

    #[test]
    fn a_sheet_is_read() {
        let sheet = parse(SHEET).expect("parses");
        assert_eq!(sheet.title, "Abbey Road");
        assert_eq!(sheet.performer, "The Beatles");
        assert_eq!(sheet.genre, "Rock");
        assert_eq!(sheet.year, Some(1969));
        assert_eq!(sheet.album_gain_db, Some(-7.89));
        assert_eq!(sheet.files, ["Abbey Road.wav"]);
        // The data track is not audio.
        let numbers: Vec<u32> = sheet.tracks.iter().map(|t| t.number).collect();
        assert_eq!(numbers, [1, 2, 4]);
        assert_eq!(sheet.tracks[0].title, "Come Together");
        assert_eq!(sheet.tracks[0].gain_db, Some(-6.5));
        // INDEX 01 is where a track starts; the pregap (INDEX 00) belongs to the one before.
        assert_eq!(sheet.tracks[1].start, (4 * 60 + 20) * 75 + 12);
        assert_eq!(sheet.tracks[1].performer, "George Harrison");
        assert_eq!(sheet.tracks[2].start, (7 * 60 + 23) * 75);
    }

    #[test]
    fn segments_in_samples() {
        let segment = Segment {
            audio: PathBuf::from("a.flac"),
            start: 75 * 60 + 1,
            end: Some(75 * 120),
        };
        assert_eq!(segment.start_frame(44_100), 60 * 44_100 + 588);
        assert_eq!(segment.end_frame(48_000), Some(120 * 48_000));
        assert_eq!(frames_to_ms(75 * 3 + 15), 3_200);
    }

    #[test]
    fn what_is_not_a_usable_sheet() {
        assert_eq!(parse(""), None);
        assert_eq!(
            parse("TITLE \"x\"\nTRACK 01 AUDIO\nINDEX 01 00:00:00"),
            None,
            "no file"
        );
        assert_eq!(
            parse("FILE \"a.wav\" WAVE\nTRACK 01 AUDIO"),
            None,
            "no start"
        );
        // Unquoted file names, times out of range, starts that go backwards.
        let sheet = parse(
            "FILE a b.flac WAVE\nTRACK 1 AUDIO\nINDEX 01 00:10:00\nTRACK 2 AUDIO\nINDEX 01 00:05:00\n\
             TRACK 3 AUDIO\nINDEX 01 00:61:00\nTRACK 4 AUDIO\nINDEX 01 01:00:00",
        )
        .expect("parses");
        assert_eq!(sheet.files, ["a b.flac"]);
        let numbers: Vec<u32> = sheet.tracks.iter().map(|t| t.number).collect();
        assert_eq!(numbers, [1, 4]);
        // A number that comes again later (with a later start) is left out: the library
        // knows a sheet's tracks by number.
        let sheet = parse(
            "FILE \"a.wav\" WAVE\nTRACK 01 AUDIO\nINDEX 01 00:00:00\nTRACK 02 AUDIO\nINDEX 01 00:10:00\n\
             TRACK 01 AUDIO\nINDEX 01 00:20:00\nTRACK 03 AUDIO\nINDEX 01 00:30:00",
        )
        .expect("parses");
        let numbers: Vec<u32> = sheet.tracks.iter().map(|t| t.number).collect();
        assert_eq!(numbers, [1, 2, 3]);
    }

    /// A sheet written elsewhere may spell its file's name in other letter case or
    /// another Unicode form than the folder does: the file is found, under the folder's
    /// spelling (the one the scan meets it by).
    #[test]
    fn the_image_is_named_as_the_folder_spells_it() {
        let dir = tempfile::tempdir().unwrap();
        let sheet_path = dir.path().join("Album.cue");
        // Decomposed Hangul, as file names written by older Macs are.
        let on_disk = "\u{1112}\u{1161}\u{11ab}\u{1100}\u{1173}\u{11af} Live.flac";
        std::fs::write(dir.path().join(on_disk), b"x").unwrap();
        let written = |name: &str| Sheet {
            files: vec![name.to_string()],
            ..Sheet::default()
        };
        for name in ["한글 LIVE.flac", "한글 live.wav", on_disk] {
            assert_eq!(
                audio_of(&sheet_path, &written(name)),
                Some(dir.path().join(on_disk)),
                "{name}"
            );
        }
        // The name as written comes first where the folder has both spellings (a volume
        // that tells letter case apart).
        let upper = dir.path().join("ABBEY.flac");
        let lower = dir.path().join("abbey.flac");
        std::fs::write(&upper, b"x").unwrap();
        let both = std::fs::write(&lower, b"x").is_ok()
            && std::fs::read_dir(dir.path()).unwrap().count() == 3;
        if both {
            assert_eq!(audio_of(&sheet_path, &written("abbey.flac")), Some(lower));
            assert_eq!(audio_of(&sheet_path, &written("ABBEY.flac")), Some(upper));
        }
    }

    /// What cannot be examined is not taken as absent.
    #[cfg(unix)]
    #[test]
    fn an_image_that_cannot_be_examined_is_not_absent() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let sheet_path = dir.path().join("Album.cue");
        // The image is a link into a folder that cannot be entered (a share that does
        // not answer behaves alike).
        let closed = dir.path().join("closed");
        std::fs::create_dir(&closed).unwrap();
        std::fs::write(closed.join("image.flac"), b"x").unwrap();
        std::os::unix::fs::symlink(closed.join("image.flac"), dir.path().join("image.flac"))
            .unwrap();
        std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o000)).unwrap();
        let entered = std::fs::metadata(closed.join("image.flac")).is_ok();
        let sheet = Sheet {
            files: vec!["image.flac".into()],
            ..Sheet::default()
        };
        let result = audio_of_checked(&sheet_path, &sheet);
        std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o755)).unwrap();
        if entered {
            eprintln!("skipped: permissions do not apply to this user");
            return;
        }
        assert_eq!(
            result.expect_err("not known").kind(),
            std::io::ErrorKind::PermissionDenied
        );
        // A link to nothing is no file, as for the scan.
        std::fs::remove_file(closed.join("image.flac")).unwrap();
        assert_eq!(audio_of_checked(&sheet_path, &sheet).unwrap(), None);
    }

    #[test]
    fn track_paths_and_their_sheets() {
        let sheet = Path::new("/m/Album/Album.cue");
        let path = track_path(sheet, 3);
        assert_eq!(path, Path::new("/m/Album/Album.cue/03"));
        assert_eq!(sheet_of(&path), Some(sheet));
        assert_eq!(backing_file(&path), sheet);
        assert_eq!(sheet_of(Path::new("/m/Album/03 Song.flac")), None);
        assert_eq!(sheet_of(Path::new("/m/Album.cue/notes")), None);
        assert_eq!(backing_file(Path::new("/m/a.flac")), Path::new("/m/a.flac"));
    }

    #[test]
    fn the_image_is_found_in_the_sheets_folder() {
        let dir = tempfile::tempdir().unwrap();
        let sheet_path = dir.path().join("Album.cue");
        std::fs::write(&sheet_path, SHEET).unwrap();
        let sheet = read(&sheet_path).expect("reads");
        // Named .wav, converted to .flac afterwards.
        assert_eq!(audio_of(&sheet_path, &sheet), None);
        std::fs::write(dir.path().join("Abbey Road.flac"), b"x").unwrap();
        assert_eq!(
            audio_of(&sheet_path, &sheet),
            Some(dir.path().join("Abbey Road.flac"))
        );
        std::fs::write(dir.path().join("Abbey Road.wav"), b"x").unwrap();
        assert_eq!(
            audio_of(&sheet_path, &sheet),
            Some(dir.path().join("Abbey Road.wav")),
            "the file named comes first"
        );
        // A path in the sheet does not lead elsewhere.
        let mut elsewhere = sheet.clone();
        elsewhere.files = vec!["..\\..\\secret\\Abbey Road.wav".into()];
        assert_eq!(
            audio_of(&sheet_path, &elsewhere),
            Some(dir.path().join("Abbey Road.wav"))
        );
        // An original that is not played, beside its conversion: the conversion.
        std::fs::remove_file(dir.path().join("Abbey Road.wav")).unwrap();
        std::fs::write(dir.path().join("Abbey Road.ape"), b"x").unwrap();
        assert_eq!(
            audio_of(&sheet_path, &sheet),
            Some(dir.path().join("Abbey Road.flac")),
            "not the .ape, which comes first by name"
        );
        let mut named_ape = sheet.clone();
        named_ape.files = vec!["Abbey Road.ape".into()];
        assert_eq!(
            audio_of(&sheet_path, &named_ape),
            Some(dir.path().join("Abbey Road.flac"))
        );
        std::fs::remove_file(dir.path().join("Abbey Road.flac")).unwrap();
        assert_eq!(
            audio_of(&sheet_path, &sheet),
            None,
            "only the .ape: nothing to play"
        );
        std::fs::write(dir.path().join("Abbey Road.wav"), b"x").unwrap();
        // A sheet for several files is not an image's.
        let mut several = sheet.clone();
        several.files = vec!["1.wav".into(), "2.wav".into()];
        assert_eq!(audio_of(&sheet_path, &several), None);
        // Resolving a track path.
        let segment = resolve(&track_path(&sheet_path, 2)).expect("resolves");
        assert_eq!(segment.audio, dir.path().join("Abbey Road.wav"));
        assert_eq!(segment.start, (4 * 60 + 20) * 75 + 12);
        assert_eq!(segment.end, Some((7 * 60 + 23) * 75));
        assert_eq!(
            resolve(&track_path(&sheet_path, 4)).expect("last").end,
            None
        );
        assert_eq!(resolve(&track_path(&sheet_path, 3)), None, "a data track");
        assert_eq!(
            &*real_file(&track_path(&sheet_path, 1)),
            dir.path().join("Abbey Road.wav")
        );
        // A sheet in the old Korean code page.
        let korean = b"PERFORMER \"\xbe\xc6\xc0\xcc\xc0\xaf\"\nFILE \"a.wav\" WAVE\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n";
        std::fs::write(&sheet_path, korean).unwrap();
        assert_eq!(read(&sheet_path).expect("reads").performer, "아이유");
    }
}
