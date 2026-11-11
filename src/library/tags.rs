//! Reading metadata and audio properties from a file with lofty.
//!
//! Files are selected by extension and then verified by content: the container/codec
//! reported here is what the file really is, and anything the audio engine cannot play
//! (DRM-protected MP4, Opus-in-Ogg, MPEG layer 1/2, compressed WAV/AIFF …) is reported
//! as unsupported instead of being added to the library.
use crate::model::ReplayGainTags;
use lofty::config::ParseOptions;
use lofty::file::{AudioFile, FileType, TaggedFile, TaggedFileExt};
use lofty::flac::FlacFile;
use lofty::iff::aiff::{AiffCompressionType, AiffFile};
use lofty::iff::wav::{WavFile, WavFormat};
use lofty::mp4::{Mp4Codec, Mp4File};
use lofty::mpeg::{Layer, MpegFile};
use lofty::ogg::VorbisFile;
use lofty::picture::PictureType;
use lofty::probe::Probe;
use lofty::tag::{ItemKey, Tag};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

/// Extensions the scanner looks at. `m4p` is listed only so that protected purchases are
/// counted as unsupported rather than silently ignored.
const AUDIO_EXTENSIONS: [&str; 11] = [
    "mp3", "flac", "wav", "aif", "aiff", "m4a", "mp4", "m4b", "m4p", "ogg", "oga",
];

/// WAVE format tags the PCM decoder handles: integer PCM, IEEE float, A-law, µ-law.
const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_ALAW: u16 = 6;
const WAVE_FORMAT_MULAW: u16 = 7;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

/// Everything the library stores about a file's content.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FileTags {
    pub title: String,
    pub artist: String,
    pub album_artist: String,
    pub album: String,
    pub compilation: bool,
    pub disc_no: Option<u32>,
    pub track_no: Option<u32>,
    pub year: Option<i32>,
    pub genre: String,
    pub duration_ms: u64,
    pub codec: &'static str,
    pub container: &'static str,
    pub sample_rate: u32,
    pub channels: u8,
    pub bit_depth: Option<u8>,
    pub bitrate_kbps: Option<u32>,
    pub replaygain: ReplayGainTags,
    /// Raw bytes of the embedded cover (front cover preferred), if requested and present.
    pub picture: Option<Vec<u8>>,
}

/// Why a file was not added.
#[derive(Debug)]
pub(crate) enum ReadError {
    /// A recognised file the player cannot play (wrong codec, DRM …).
    Unsupported(String),
    /// The file could not be read or parsed.
    Failed(String),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Unsupported(why) => write!(f, "unsupported: {why}"),
            ReadError::Failed(why) => write!(f, "unreadable: {why}"),
        }
    }
}

/// True if the scanner should look at this file (decided by extension only).
pub(crate) fn has_audio_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            AUDIO_EXTENSIONS
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        })
}

fn type_from_extension(path: &Path) -> Option<FileType> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "mp3" => Some(FileType::Mpeg),
        "flac" => Some(FileType::Flac),
        "wav" => Some(FileType::Wav),
        "aif" | "aiff" => Some(FileType::Aiff),
        "m4a" | "mp4" | "m4b" | "m4p" => Some(FileType::Mp4),
        "ogg" | "oga" => Some(FileType::Vorbis),
        _ => None,
    }
}

fn wav_tag_is_pcm(tag: u16) -> bool {
    matches!(
        tag,
        WAVE_FORMAT_PCM | WAVE_FORMAT_IEEE_FLOAT | WAVE_FORMAT_ALAW | WAVE_FORMAT_MULAW
    )
}

fn unsupported_wav(tag: u16) -> ReadError {
    ReadError::Unsupported(format!("WAV with compressed format 0x{tag:04x}"))
}

/// Reads the format tag from the `fmt ` chunk of a RIFF/WAVE file (resolving the
/// "extensible" indirection). `None` if the file is not a well-formed WAVE.
fn wav_format_tag<R: Read + Seek>(reader: &mut R) -> Option<u16> {
    reader.seek(SeekFrom::Start(0)).ok()?;
    let mut header = [0u8; 12];
    reader.read_exact(&mut header).ok()?;
    if &header[..4] != b"RIFF" || &header[8..] != b"WAVE" {
        return None;
    }
    // Walk the chunk list; give up after a bounded number of chunks.
    for _ in 0..64 {
        let mut chunk = [0u8; 8];
        reader.read_exact(&mut chunk).ok()?;
        let size = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
        if &chunk[..4] == b"fmt " {
            let mut body = [0u8; 26];
            let wanted = (size as usize).min(body.len());
            reader.read_exact(&mut body[..wanted]).ok()?;
            if wanted < 2 {
                return None;
            }
            let tag = u16::from_le_bytes([body[0], body[1]]);
            if tag == WAVE_FORMAT_EXTENSIBLE && wanted >= 26 {
                // The real format is the first field of the sub-format GUID.
                return Some(u16::from_le_bytes([body[24], body[25]]));
            }
            return Some(tag);
        }
        let skip = i64::from(size) + i64::from(size % 2);
        reader.seek(SeekFrom::Current(skip)).ok()?;
    }
    None
}

fn failed(err: impl std::error::Error) -> ReadError {
    // lofty's top-level message is generic; the cause carries the detail.
    match err.source() {
        Some(source) => ReadError::Failed(format!("{err}: {source}")),
        None => ReadError::Failed(err.to_string()),
    }
}

/// Reads tags and audio properties of the file at `path`.
pub(crate) fn read_tags(path: &Path, want_picture: bool) -> Result<FileTags, ReadError> {
    let file = File::open(path).map_err(failed)?;
    let mut reader = BufReader::new(file);

    let guessed = Probe::new(&mut reader)
        .guess_file_type()
        .map_err(failed)?
        .file_type();
    let file_type = match guessed {
        Some(
            known @ (FileType::Mpeg
            | FileType::Flac
            | FileType::Wav
            | FileType::Aiff
            | FileType::Mp4
            | FileType::Vorbis),
        ) => known,
        Some(other) => return Err(ReadError::Unsupported(format!("{other:?} audio"))),
        None => {
            let mut magic = [0u8; 4];
            reader.seek(SeekFrom::Start(0)).map_err(failed)?;
            let is_ogg = reader.read_exact(&mut magic).is_ok() && &magic == b"OggS";
            if is_ogg {
                return Err(ReadError::Unsupported(
                    "Ogg stream with a codec other than Vorbis".into(),
                ));
            }
            type_from_extension(path)
                .ok_or_else(|| ReadError::Failed("unknown file format".into()))?
        }
    };
    reader.seek(SeekFrom::Start(0)).map_err(failed)?;

    let options = ParseOptions::new().read_cover_art(want_picture);
    let (tagged, codec, container): (TaggedFile, &'static str, &'static str) = match file_type {
        FileType::Mpeg => {
            let file = MpegFile::read_from(&mut reader, options).map_err(failed)?;
            if !matches!(file.properties().layer(), Layer::Layer3) {
                return Err(ReadError::Unsupported("MPEG audio layer 1/2".into()));
            }
            (file.into(), "MP3", "MP3")
        }
        FileType::Flac => {
            let file = FlacFile::read_from(&mut reader, options).map_err(failed)?;
            (file.into(), "FLAC", "FLAC")
        }
        FileType::Wav => {
            let file = match WavFile::read_from(&mut reader, options) {
                Ok(file) => file,
                Err(err) => {
                    // lofty rejects some compressed WAVs outright (e.g. 4-bit ADPCM);
                    // report those as unsupported rather than as broken files.
                    return Err(match wav_format_tag(&mut reader) {
                        Some(tag) if !wav_tag_is_pcm(tag) => unsupported_wav(tag),
                        _ => failed(err),
                    });
                }
            };
            match file.properties().format() {
                WavFormat::PCM | WavFormat::IEEE_FLOAT => {}
                WavFormat::Other(tag) if wav_tag_is_pcm(*tag) => {}
                WavFormat::Other(tag) => return Err(unsupported_wav(*tag)),
            }
            (file.into(), "PCM", "WAV")
        }
        FileType::Aiff => {
            let file = AiffFile::read_from(&mut reader, options).map_err(failed)?;
            match file.properties().compression_type() {
                None
                | Some(
                    AiffCompressionType::None
                    | AiffCompressionType::sowt
                    | AiffCompressionType::fl32
                    | AiffCompressionType::FL32
                    | AiffCompressionType::fl64
                    | AiffCompressionType::alaw
                    | AiffCompressionType::ALAW
                    | AiffCompressionType::ulaw
                    | AiffCompressionType::ULAW,
                ) => {}
                Some(other) => {
                    return Err(ReadError::Unsupported(format!(
                        "AIFF-C with compression {}",
                        other.compression_name()
                    )));
                }
            }
            (file.into(), "PCM", "AIFF")
        }
        FileType::Mp4 => {
            let file = Mp4File::read_from(&mut reader, options).map_err(failed)?;
            let properties = file.properties();
            if properties.is_drm_protected() {
                return Err(ReadError::Unsupported("DRM-protected MP4".into()));
            }
            let codec = match properties.codec() {
                Some(Mp4Codec::AAC) => "AAC",
                Some(Mp4Codec::ALAC) => "ALAC",
                other => {
                    return Err(ReadError::Unsupported(format!("MP4 with codec {other:?}")));
                }
            };
            (file.into(), codec, "MP4")
        }
        FileType::Vorbis => {
            let file = VorbisFile::read_from(&mut reader, options).map_err(failed)?;
            (file.into(), "Vorbis", "Ogg")
        }
        other => return Err(ReadError::Unsupported(format!("{other:?} audio"))),
    };

    let properties = tagged.properties();
    let sample_rate = properties.sample_rate().unwrap_or(0);
    let channels = properties.channels().unwrap_or(0);
    if sample_rate == 0 || channels == 0 {
        return Err(ReadError::Failed("no audio stream found".into()));
    }
    let duration_ms = u64::try_from(properties.duration().as_millis()).unwrap_or(u64::MAX);
    let bit_depth = properties.bit_depth().filter(|&bits| bits > 0);
    let bitrate_kbps = properties
        .audio_bitrate()
        .filter(|&kbps| kbps > 0)
        .or(properties.overall_bitrate().filter(|&kbps| kbps > 0));

    // Primary tag first (e.g. ID3v2 before ID3v1), then whatever else the file carries.
    let mut tags: Vec<&Tag> = Vec::new();
    if let Some(primary) = tagged.primary_tag() {
        tags.push(primary);
    }
    for tag in tagged.tags() {
        if !tags.iter().any(|known| std::ptr::eq(*known, tag)) {
            tags.push(tag);
        }
    }

    let mut title = first_text(&tags, ItemKey::TrackTitle);
    if title.is_empty() {
        title = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
    }
    let compilation = tags.iter().any(|tag| {
        tag.get_string(ItemKey::FlagCompilation)
            .is_some_and(|flag| matches!(flag.trim(), "1" | "true" | "True" | "TRUE"))
    });
    let replaygain = ReplayGainTags {
        track_gain_db: first_parsed(&tags, ItemKey::ReplayGainTrackGain, parse_gain_db),
        track_peak: first_parsed(&tags, ItemKey::ReplayGainTrackPeak, parse_peak),
        album_gain_db: first_parsed(&tags, ItemKey::ReplayGainAlbumGain, parse_gain_db),
        album_peak: first_parsed(&tags, ItemKey::ReplayGainAlbumPeak, parse_peak),
    };
    let picture = if want_picture {
        pick_picture(&tags)
    } else {
        None
    };

    Ok(FileTags {
        title,
        artist: joined_text(&tags, ItemKey::TrackArtist),
        album_artist: joined_text(&tags, ItemKey::AlbumArtist),
        album: first_text(&tags, ItemKey::AlbumTitle),
        compilation,
        disc_no: first_parsed(&tags, ItemKey::DiscNumber, leading_number),
        track_no: first_parsed(&tags, ItemKey::TrackNumber, leading_number),
        year: [
            ItemKey::RecordingDate,
            ItemKey::Year,
            ItemKey::ReleaseDate,
            ItemKey::OriginalReleaseDate,
        ]
        .into_iter()
        .find_map(|key| first_parsed(&tags, key, parse_year)),
        genre: joined_text(&tags, ItemKey::Genre),
        duration_ms,
        codec,
        container,
        sample_rate,
        channels,
        bit_depth,
        bitrate_kbps,
        replaygain,
        picture,
    })
}

/// Reads only the embedded cover of a file (used to regenerate an evicted cache entry).
pub(crate) fn read_picture(path: &Path) -> Option<Vec<u8>> {
    read_tags(path, true).ok().and_then(|tags| tags.picture)
}

fn clean(text: &str) -> String {
    text.replace('\0', " ").trim().to_string()
}

/// First non-empty value of `key` across the tags.
fn first_text(tags: &[&Tag], key: ItemKey) -> String {
    tags.iter()
        .flat_map(|tag| tag.get_strings(key))
        .map(clean)
        .find(|value| !value.is_empty())
        .unwrap_or_default()
}

/// All values of `key` from the first tag that has any, joined with ", " (multi-valued
/// artist and genre fields).
fn joined_text(tags: &[&Tag], key: ItemKey) -> String {
    for tag in tags {
        let mut values: Vec<String> = Vec::new();
        for value in tag.get_strings(key).map(clean) {
            if !value.is_empty() && !values.contains(&value) {
                values.push(value);
            }
        }
        if !values.is_empty() {
            return values.join(", ");
        }
    }
    String::new()
}

fn first_parsed<T>(tags: &[&Tag], key: ItemKey, parse: fn(&str) -> Option<T>) -> Option<T> {
    tags.iter()
        .flat_map(|tag| tag.get_strings(key))
        .find_map(parse)
}

fn pick_picture(tags: &[&Tag]) -> Option<Vec<u8>> {
    let pictures = || tags.iter().flat_map(|tag| tag.pictures());
    pictures()
        .find(|picture| picture.pic_type() == PictureType::CoverFront && !picture.data().is_empty())
        .or_else(|| pictures().find(|picture| !picture.data().is_empty()))
        .map(|picture| picture.data().to_vec())
}

/// Parses the leading integer of values such as "3", "03" or "3/12". Zero means "unset".
fn leading_number(text: &str) -> Option<u32> {
    let digits: String = text
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse::<u32>().ok().filter(|&n| n > 0)
}

/// Finds a four-digit year in values such as "2017", "2017-04-21" or "21.04.2017".
fn parse_year(text: &str) -> Option<i32> {
    let bytes = text.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        if !bytes[start].is_ascii_digit() {
            start += 1;
            continue;
        }
        let mut end = start;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end - start == 4 {
            return text[start..end].parse::<i32>().ok().filter(|&y| y > 0);
        }
        start = end;
    }
    None
}

/// Parses a ReplayGain gain such as "-6.54 dB", "+1.2dB" or "-6,54".
pub(crate) fn parse_gain_db(text: &str) -> Option<f32> {
    let trimmed = text.trim();
    let lower = trimmed.to_ascii_lowercase();
    let number = lower.strip_suffix("db").unwrap_or(&lower).trim();
    let value: f32 = number.replace(',', ".").parse().ok()?;
    (value.is_finite() && value.abs() <= 100.0).then_some(value)
}

/// Parses a ReplayGain peak (linear, 1.0 = full scale).
pub(crate) fn parse_peak(text: &str) -> Option<f32> {
    let value: f32 = text.trim().replace(',', ".").parse().ok()?;
    (value.is_finite() && (0.0..=100.0).contains(&value)).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_forms() {
        assert_eq!(parse_gain_db("-6.54 dB"), Some(-6.54));
        assert_eq!(parse_gain_db("+1.20 dB"), Some(1.2));
        assert_eq!(parse_gain_db("-6.54dB"), Some(-6.54));
        assert_eq!(parse_gain_db("  -6.54 DB "), Some(-6.54));
        assert_eq!(parse_gain_db("-6,54 dB"), Some(-6.54));
        assert_eq!(parse_gain_db("0"), Some(0.0));
        assert_eq!(parse_gain_db("-7"), Some(-7.0));
        assert_eq!(parse_gain_db(""), None);
        assert_eq!(parse_gain_db("dB"), None);
        assert_eq!(parse_gain_db("loud"), None);
        assert_eq!(parse_gain_db("NaN dB"), None);
        assert_eq!(parse_gain_db("inf"), None);
        assert_eq!(parse_gain_db("-900 dB"), None);
    }

    #[test]
    fn peak_forms() {
        assert_eq!(parse_peak("0.987654"), Some(0.987654));
        assert_eq!(parse_peak(" 1.000000 "), Some(1.0));
        assert_eq!(parse_peak("1,05"), Some(1.05));
        assert_eq!(parse_peak("-0.5"), None);
        assert_eq!(parse_peak("x"), None);
        assert_eq!(parse_peak("NaN"), None);
    }

    #[test]
    fn numbers_and_years() {
        assert_eq!(leading_number("3"), Some(3));
        assert_eq!(leading_number("03/12"), Some(3));
        assert_eq!(leading_number(" 7 "), Some(7));
        assert_eq!(leading_number("0"), None);
        assert_eq!(leading_number("A1"), None);
        assert_eq!(leading_number(""), None);
        assert_eq!(leading_number("99999999999999"), None);

        assert_eq!(parse_year("2017"), Some(2017));
        assert_eq!(parse_year("2017-04-21"), Some(2017));
        assert_eq!(parse_year("21.04.2017"), Some(2017));
        assert_eq!(parse_year("20170421"), None);
        assert_eq!(parse_year("0000"), None);
        assert_eq!(parse_year("n/a"), None);
    }

    #[test]
    fn extensions() {
        for name in [
            "a.mp3", "a.MP3", "a.flac", "a.wav", "a.aif", "a.AIFF", "a.m4a", "a.mp4", "a.m4b",
            "a.m4p", "a.ogg", "a.oga",
        ] {
            assert!(has_audio_extension(Path::new(name)), "{name}");
        }
        for name in [
            "a.txt",
            "a.jpg",
            "a.opus",
            "a.wma",
            "mp3",
            "a.mp3.part",
            "a",
        ] {
            assert!(!has_audio_extension(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn wav_format_tag_is_found() {
        use std::io::Cursor;
        let wav = |fmt: &[u8]| {
            let mut bytes = b"RIFF\0\0\0\0WAVE".to_vec();
            // An unrelated chunk with an odd size (padded) before the format chunk.
            bytes.extend_from_slice(b"JUNK\x03\0\0\0abc\0");
            bytes.extend_from_slice(b"fmt ");
            bytes.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
            bytes.extend_from_slice(fmt);
            bytes
        };
        let mut pcm = vec![1u8, 0];
        pcm.extend_from_slice(&[0; 14]);
        assert_eq!(wav_format_tag(&mut Cursor::new(wav(&pcm))), Some(1));
        let mut adpcm = vec![2u8, 0];
        adpcm.extend_from_slice(&[0; 14]);
        assert_eq!(wav_format_tag(&mut Cursor::new(wav(&adpcm))), Some(2));
        // Extensible: the tag is taken from the sub-format GUID.
        let mut extensible = vec![0xFEu8, 0xFF];
        extensible.extend_from_slice(&[0; 22]);
        extensible.extend_from_slice(&[3, 0]);
        extensible.extend_from_slice(&[0; 14]);
        assert_eq!(wav_format_tag(&mut Cursor::new(wav(&extensible))), Some(3));
        assert_eq!(
            wav_format_tag(&mut Cursor::new(b"RIFF\0\0\0\0AVI ".to_vec())),
            None
        );
        assert_eq!(wav_format_tag(&mut Cursor::new(b"RIFF".to_vec())), None);
        assert_eq!(wav_format_tag(&mut Cursor::new(wav(&[]))), None);
        assert!(wav_tag_is_pcm(1) && wav_tag_is_pcm(3) && wav_tag_is_pcm(6) && wav_tag_is_pcm(7));
        assert!(!wav_tag_is_pcm(2) && !wav_tag_is_pcm(0x55));
    }

    #[test]
    fn missing_file_is_a_failure_not_a_panic() {
        match read_tags(Path::new("/definitely/not/here.mp3"), true) {
            Err(ReadError::Failed(_)) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }
}
