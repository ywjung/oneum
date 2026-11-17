//! Reading metadata and audio properties from a file with lofty.
//!
//! Files are selected by extension and then verified by content: the container/codec
//! reported here is what the file really is, and anything the audio engine cannot play
//! (DRM-protected MP4, Speex-in-Ogg, compressed WAV/AIFF the decoder does not know …) is
//! reported as unsupported instead of being added to the library.
//!
//! Two kinds of file are read without lofty's audio parsing: DSD (DSF and DSDIFF), whose
//! headers are read by `audio::dsd` and whose ID3v2 tag is handed to lofty on its own,
//! and Matroska/WebM, whose properties come from the decoder and tags from Symphonia.
use crate::model::ReplayGainTags;
use lofty::ape::ApeFile;
use lofty::config::ParseOptions;
use lofty::file::{AudioFile, FileType, TaggedFile, TaggedFileExt};
use lofty::flac::FlacFile;
use lofty::iff::aiff::{AiffCompressionType, AiffFile};
use lofty::iff::wav::{WavFile, WavFormat};
use lofty::mp4::{Mp4Codec, Mp4File};
use lofty::mpeg::{Layer, MpegFile};
use lofty::ogg::{OpusFile, VorbisFile};
use lofty::picture::PictureType;
use lofty::probe::Probe;
use lofty::tag::{ItemKey, Tag};
use lofty::wavpack::WavPackFile;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

/// Extensions the scanner looks at. `m4p` is listed only so that protected purchases are
/// counted as unsupported rather than silently ignored.
const AUDIO_EXTENSIONS: [&str; 20] = [
    "mp3", "mp2", "mp1", "flac", "wav", "aif", "aiff", "m4a", "mp4", "m4b", "m4p", "ogg", "oga",
    "opus", "wv", "ape", "dsf", "dff", "mka", "webm",
];

/// What reading one file's tags and properties may cost: bytes read from the file and
/// calls on it. A real file needs a few megabytes and a few hundred calls (a large
/// cover, a long sample table); a damaged one can make the tag library walk it in tiny
/// steps (a FLAC whose metadata runs into zeros reads as millions of empty blocks, a
/// file of zeros is searched to its end). Past either limit the file is not read.
const MAX_READ_BYTES: u64 = 256 * 1024 * 1024;
const MAX_READ_CALLS: u32 = 100_000;

/// A file with a limit on what reading it may cost (see [`MAX_READ_BYTES`]).
struct Budget<R> {
    inner: R,
    bytes_left: u64,
    calls_left: u32,
}

impl<R> Budget<R> {
    fn new(inner: R) -> Self {
        Budget {
            inner,
            bytes_left: MAX_READ_BYTES,
            calls_left: MAX_READ_CALLS,
        }
    }

    fn spend(&mut self) -> std::io::Result<()> {
        if self.calls_left == 0 || self.bytes_left == 0 {
            return Err(std::io::Error::other(
                "reading this file costs more than any music file needs",
            ));
        }
        self.calls_left -= 1;
        Ok(())
    }
}

impl<R: Read> Read for Budget<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.spend()?;
        let n = self.inner.read(buf)?;
        self.bytes_left = self.bytes_left.saturating_sub(n as u64);
        Ok(n)
    }
}

impl<R: Seek> Seek for Budget<R> {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.spend()?;
        self.inner.seek(to)
    }
}

/// The first four bytes of a Matroska (and WebM) file: an EBML header.
const EBML_MAGIC: [u8; 4] = [0x1A, 0x45, 0xDF, 0xA3];
/// WAVE format tags of the ADPCM kinds the decoder plays (Microsoft, IMA).
const WAVE_FORMAT_ADPCM: [u16; 2] = [0x0002, 0x0011];

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
        "mp3" | "mp2" | "mp1" => Some(FileType::Mpeg),
        "flac" => Some(FileType::Flac),
        "wav" => Some(FileType::Wav),
        "aif" | "aiff" => Some(FileType::Aiff),
        "m4a" | "mp4" | "m4b" | "m4p" => Some(FileType::Mp4),
        "ogg" | "oga" => Some(FileType::Vorbis),
        "opus" => Some(FileType::Opus),
        "wv" => Some(FileType::WavPack),
        "ape" => Some(FileType::Ape),
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
    read_file(path, want_picture, true)
}

/// Reads what the audio itself says (codec, length, sample rate) and leaves the tags
/// alone: for a file whose tags cannot be read (damaged, or stored in a way that is
/// refused, such as a compressed ID3v2 frame). The song is then known by its file name.
pub(crate) fn read_without_tags(path: &Path) -> Result<FileTags, ReadError> {
    read_file(path, false, false)
}

/// What the audio of a file is, apart from its tags.
struct Audio {
    sample_rate: u32,
    channels: u8,
    duration_ms: u64,
    bit_depth: Option<u8>,
    bitrate_kbps: Option<u32>,
}

/// A DSF or DSDIFF file: the header by `audio::dsd`, the tags from its ID3v2 tag, read
/// by lofty as the tag at the head of an otherwise empty MPEG file. The sample rate
/// recorded is the DSD rate; playback runs at the PCM rate it converts to.
fn read_dsd(
    path: &Path,
    reader: &mut BufReader<Budget<File>>,
    want_picture: bool,
    want_tags: bool,
) -> Result<FileTags, ReadError> {
    use crate::audio::dsd;
    let info = dsd::read_info(reader).map_err(|e| match e.kind {
        crate::audio::PlaybackErrorKind::Unsupported => ReadError::Unsupported(e.detail),
        _ => ReadError::Failed(e.detail),
    })?;
    let audio = Audio {
        sample_rate: info.dsd_rate,
        channels: u8::try_from(info.channels()).unwrap_or(u8::MAX),
        duration_ms: info.duration_ms(),
        bit_depth: Some(1),
        bitrate_kbps: Some(info.bitrate_kbps()),
    };
    let codec = dsd_codec_name(info.dsd_rate);
    let container = match info.container {
        dsd::Container::Dsf => "DSF",
        dsd::Container::Dsdiff => "DSDIFF",
    };
    let id3 = if want_tags || want_picture {
        dsd::read_tag(reader, &info)
    } else {
        None
    };
    let tagged = id3.and_then(|mut bytes| {
        // Room after the tag for lofty's look at the end of the "file".
        bytes.resize(bytes.len() + 256, 0);
        let options = ParseOptions::new()
            .read_properties(false)
            .read_cover_art(want_picture)
            .read_tags(true);
        MpegFile::read_from(&mut std::io::Cursor::new(bytes), options)
            .ok()
            .map(TaggedFile::from)
    });
    let tags: Vec<&Tag> = tagged
        .as_ref()
        .map_or(Vec::new(), |t| t.tags().iter().collect());
    Ok(file_tags(
        path,
        &tags,
        audio,
        codec,
        container,
        want_picture,
    ))
}

/// The name a DSD rate is known by, as a codec name ("DSD64" …).
fn dsd_codec_name(dsd_rate: u32) -> &'static str {
    match crate::audio::dsd::rate_name(dsd_rate).as_str() {
        "DSD64" => "DSD64",
        "DSD128" => "DSD128",
        "DSD256" => "DSD256",
        "DSD512" => "DSD512",
        "DSD1024" => "DSD1024",
        _ => "DSD",
    }
}

/// A file whose audio lofty does not parse (Matroska, WebM, a WAV it refuses): what the
/// decoder says about it, and the tags Symphonia finds. Refused as unsupported when the
/// decoder cannot play it (a video codec's sound track, say).
fn read_by_decoder(
    path: &Path,
    container: &'static str,
    want_picture: bool,
    want_tags: bool,
) -> Result<FileTags, ReadError> {
    use crate::audio::PlaybackErrorKind;
    let refused = |e: crate::audio::PlaybackError| match e.kind {
        PlaybackErrorKind::Unsupported => ReadError::Unsupported(e.detail),
        _ => ReadError::Failed(e.detail),
    };
    let mut decoder = crate::audio::decoder::TrackDecoder::open(path).map_err(refused)?;
    decoder.next(&mut Vec::new()).map_err(refused)?;
    let info = decoder.info().clone();
    let codec = match info.codec.as_str() {
        "flac" => "FLAC",
        "alac" => "ALAC",
        "aac" => "AAC",
        "mp3" => "MP3",
        "mp2" => "MP2",
        "mp1" => "MP1",
        "vorbis" => "Vorbis",
        "opus" => "Opus",
        c if c.starts_with("pcm") => "PCM",
        c if c.starts_with("adpcm") => "ADPCM",
        other => return Err(ReadError::Unsupported(format!("{container} with {other}"))),
    };
    let rate = info.sample_rate.max(1);
    let audio = Audio {
        sample_rate: info.sample_rate,
        channels: u8::try_from(info.channels).unwrap_or(u8::MAX),
        duration_ms: info
            .total_frames
            .map_or(0, |f| f.saturating_mul(1_000) / u64::from(rate)),
        bit_depth: None,
        bitrate_kbps: None,
    };
    let found = if want_tags || want_picture {
        crate::audio::decoder::read_media_tags(path)
    } else {
        Default::default()
    };
    let replaygain = ReplayGainTags {
        track_gain_db: found.replaygain[0].as_deref().and_then(parse_gain_db),
        track_peak: found.replaygain[1].as_deref().and_then(parse_peak),
        album_gain_db: found.replaygain[2].as_deref().and_then(parse_gain_db),
        album_peak: found.replaygain[3].as_deref().and_then(parse_peak),
    };
    let mut title = clean(&found.title);
    if title.is_empty() {
        title = file_title(path);
    }
    Ok(FileTags {
        title,
        artist: clean(&found.artist),
        album_artist: clean(&found.album_artist),
        album: clean(&found.album),
        compilation: found.compilation,
        disc_no: found.disc_no,
        track_no: found.track_no,
        year: parse_year(&found.date),
        genre: clean(&found.genre),
        duration_ms: audio.duration_ms,
        codec,
        container,
        sample_rate: audio.sample_rate,
        channels: audio.channels,
        bit_depth: audio.bit_depth,
        bitrate_kbps: audio.bitrate_kbps,
        replaygain,
        picture: if want_picture { found.picture } else { None },
    })
}

/// Whether the decoder plays the file: `Err` with its reason when the file is of a
/// kind it does not (a file it cannot read at all fails later, when it is played).
fn playable(path: &Path, format: &str) -> Result<(), ReadError> {
    match crate::audio::decoder::can_play(path) {
        Err(error) if error.kind == crate::audio::PlaybackErrorKind::Unsupported => {
            Err(ReadError::Unsupported(format!(
                "{format} the decoder does not play: {}",
                error.detail
            )))
        }
        _ => Ok(()),
    }
}

/// The title of a file without tags: its name.
fn file_title(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn read_file(path: &Path, want_picture: bool, want_tags: bool) -> Result<FileTags, ReadError> {
    // Only a regular file: a pipe of that name would never deliver its first byte.
    if !std::fs::metadata(path).map_err(failed)?.is_file() {
        return Err(ReadError::Failed("not a regular file".into()));
    }
    let file = File::open(path).map_err(failed)?;
    let mut reader = BufReader::new(Budget::new(file));

    // Recognised by content before lofty is asked.
    let mut magic = [0u8; 4];
    let got = reader.read(&mut magic).map_err(failed)?;
    reader.seek(SeekFrom::Start(0)).map_err(failed)?;
    if got == 4 && crate::audio::dsd::is_dsd_magic(&magic) {
        return read_dsd(path, &mut reader, want_picture, want_tags);
    }
    if got == 4 && magic == EBML_MAGIC {
        let webm = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("webm"));
        let container = if webm { "WebM" } else { "Matroska" };
        return read_by_decoder(path, container, want_picture, want_tags);
    }

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
            | FileType::Vorbis
            | FileType::Opus
            | FileType::WavPack
            | FileType::Ape),
        ) => known,
        Some(other) => return Err(ReadError::Unsupported(format!("{other:?} audio"))),
        None => {
            let mut magic = [0u8; 4];
            reader.seek(SeekFrom::Start(0)).map_err(failed)?;
            let is_ogg = reader.read_exact(&mut magic).is_ok() && &magic == b"OggS";
            if is_ogg {
                return Err(ReadError::Unsupported(
                    "Ogg stream with a codec other than Vorbis and Opus".into(),
                ));
            }
            type_from_extension(path)
                .ok_or_else(|| ReadError::Failed("unknown file format".into()))?
        }
    };
    reader.seek(SeekFrom::Start(0)).map_err(failed)?;

    let options = ParseOptions::new()
        .read_cover_art(want_picture)
        .read_tags(want_tags);
    // What an APE tag holds under names the tag library does not carry over (see
    // `ApeExtras`).
    let mut extras = ApeExtras::default();
    let (tagged, codec, container): (TaggedFile, &'static str, &'static str) = match file_type {
        FileType::Mpeg => {
            let file = MpegFile::read_from(&mut reader, options).map_err(failed)?;
            // Layer I and II are the older members of the family (MP2 is still common
            // in broadcast archives); they decode with the same decoder.
            let codec = match file.properties().layer() {
                Layer::Layer1 => "MP1",
                Layer::Layer2 => "MP2",
                Layer::Layer3 => "MP3",
            };
            (file.into(), codec, "MP3")
        }
        FileType::Flac => {
            let file = FlacFile::read_from(&mut reader, options).map_err(failed)?;
            (file.into(), "FLAC", "FLAC")
        }
        FileType::Wav => {
            let file = match WavFile::read_from(&mut reader, options) {
                Ok(file) => file,
                Err(err) => {
                    // lofty rejects some compressed WAVs outright (e.g. 4-bit ADPCM):
                    // the decoder may still play them; if not, they are unsupported
                    // rather than broken.
                    return match wav_format_tag(&mut reader) {
                        Some(tag) if WAVE_FORMAT_ADPCM.contains(&tag) => {
                            read_by_decoder(path, "WAV", want_picture, want_tags)
                        }
                        Some(tag) if !wav_tag_is_pcm(tag) => Err(unsupported_wav(tag)),
                        _ => Err(failed(err)),
                    };
                }
            };
            let codec = match file.properties().format() {
                WavFormat::PCM | WavFormat::IEEE_FLOAT => "PCM",
                WavFormat::Other(tag) if wav_tag_is_pcm(*tag) => "PCM",
                // ADPCM plays if the decoder takes this variant of it.
                WavFormat::Other(tag)
                    if WAVE_FORMAT_ADPCM.contains(tag)
                        && crate::audio::decoder::can_play(path).is_ok() =>
                {
                    "ADPCM"
                }
                WavFormat::Other(tag) => return Err(unsupported_wav(*tag)),
            };
            (file.into(), codec, "WAV")
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
            // "AAC" covers more than can be played: HE-AAC (which the header does not
            // always admit to) and AAC with more than two channels are refused by the
            // decoder. Asking the decoder is the only reliable way to tell.
            if codec == "AAC"
                && let Err(error) = crate::audio::decoder::can_play(path)
                && error.kind == crate::audio::PlaybackErrorKind::Unsupported
            {
                return Err(ReadError::Unsupported(format!(
                    "AAC the decoder cannot play (HE-AAC, or more than two channels): {}",
                    error.detail
                )));
            }
            (file.into(), codec, "MP4")
        }
        FileType::Vorbis => {
            let file = VorbisFile::read_from(&mut reader, options).map_err(failed)?;
            (file.into(), "Vorbis", "Ogg")
        }
        FileType::Opus => {
            let file = OpusFile::read_from(&mut reader, options).map_err(failed)?;
            playable(path, "Opus")?;
            (file.into(), "Opus", "Ogg")
        }
        // These two have kinds the decoder does not play (WavPack's lossy mode and
        // its DSD, more than two channels; Monkey's Audio in floating point): the
        // decoder is asked.
        FileType::WavPack => {
            let file = WavPackFile::read_from(&mut reader, options).map_err(failed)?;
            playable(path, "WavPack")?;
            extras = ApeExtras::of(file.ape());
            (file.into(), "WavPack", "WavPack")
        }
        FileType::Ape => {
            let file = ApeFile::read_from(&mut reader, options).map_err(failed)?;
            playable(path, "Monkey's Audio")?;
            extras = ApeExtras::of(file.ape());
            (file.into(), "APE", "APE")
        }
        other => return Err(ReadError::Unsupported(format!("{other:?} audio"))),
    };

    let properties = tagged.properties();
    // Opus is decoded at 48 kHz whatever the rate of what was encoded (which is what
    // its header names).
    let sample_rate = match file_type {
        FileType::Opus => 48_000,
        _ => properties.sample_rate().unwrap_or(0),
    };
    let channels = properties.channels().unwrap_or(0);
    if sample_rate == 0 || channels == 0 {
        return Err(ReadError::Failed("no audio stream found".into()));
    }
    let audio = Audio {
        sample_rate,
        channels,
        duration_ms: u64::try_from(properties.duration().as_millis()).unwrap_or(u64::MAX),
        bit_depth: properties.bit_depth().filter(|&bits| bits > 0),
        bitrate_kbps: properties
            .audio_bitrate()
            .filter(|&kbps| kbps > 0)
            .or(properties.overall_bitrate().filter(|&kbps| kbps > 0)),
    };

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

    let mut read = file_tags(path, &tags, audio, codec, container, want_picture);
    if read.year.is_none() {
        read.year = extras.date.as_deref().and_then(parse_year);
    }
    if read.album_artist.is_empty() {
        read.album_artist = extras
            .album_artist
            .as_deref()
            .map(clean)
            .unwrap_or_default();
    }
    Ok(read)
}

/// APE tags (WavPack and Monkey's Audio carry them) name the date "Year" and the album
/// artist "Album Artist"; files converted with ffmpeg have them as "date" and
/// "album_artist", names the tag library drops. Those two are looked up by name.
#[derive(Default)]
struct ApeExtras {
    date: Option<String>,
    album_artist: Option<String>,
}

impl ApeExtras {
    fn of(tag: Option<&lofty::ape::ApeTag>) -> Self {
        let text = |name: &str| {
            let item = tag?.get(name)?;
            match item.value() {
                lofty::tag::ItemValue::Text(text) => Some(text.clone()),
                _ => None,
            }
        };
        ApeExtras {
            date: text("date"),
            album_artist: text("album_artist"),
        }
    }
}

/// The song as the library keeps it: what its tags say and what its audio is.
fn file_tags(
    path: &Path,
    tags: &[&Tag],
    audio: Audio,
    codec: &'static str,
    container: &'static str,
    want_picture: bool,
) -> FileTags {
    let tags = tags.to_vec();
    let mut title = first_text(&tags, ItemKey::TrackTitle);
    if title.is_empty() {
        title = file_title(path);
    }
    let compilation = tags.iter().any(|tag| {
        tag.get_string(ItemKey::FlagCompilation)
            .is_some_and(|flag| matches!(flag.trim(), "1" | "true" | "True" | "TRUE"))
    });
    // Opus files are leveled with R128 tags, more often than with ReplayGain's.
    let replaygain = ReplayGainTags {
        track_gain_db: first_parsed(&tags, ItemKey::ReplayGainTrackGain, parse_gain_db)
            .or_else(|| first_parsed(&tags, ItemKey::R128TrackGain, parse_r128_gain)),
        track_peak: first_parsed(&tags, ItemKey::ReplayGainTrackPeak, parse_peak),
        album_gain_db: first_parsed(&tags, ItemKey::ReplayGainAlbumGain, parse_gain_db)
            .or_else(|| first_parsed(&tags, ItemKey::R128AlbumGain, parse_r128_gain)),
        album_peak: first_parsed(&tags, ItemKey::ReplayGainAlbumPeak, parse_peak),
    };
    let picture = if want_picture {
        pick_picture(&tags)
    } else {
        None
    };

    FileTags {
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
        duration_ms: audio.duration_ms,
        codec,
        container,
        sample_rate: audio.sample_rate,
        channels: audio.channels,
        bit_depth: audio.bit_depth,
        bitrate_kbps: audio.bitrate_kbps,
        replaygain,
        picture,
    }
}

/// Reads only the embedded cover of a file (used to regenerate an evicted cache entry).
/// Everything lofty finds in the file (for the song's details: credits, lyrics), read
/// under the same limits as a scan — a regular file only, a bounded cost of reading,
/// parser panics contained — and without the artwork. `None` if it cannot be read.
pub(crate) fn read_tagged(path: &Path) -> Option<TaggedFile> {
    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let file = File::open(path).ok()?;
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Probe::new(BufReader::new(Budget::new(file)))
            .options(ParseOptions::new().read_cover_art(false))
            .guess_file_type()
            .ok()?
            .read()
            .ok()
    }))
    .unwrap_or_else(|_| {
        tracing::warn!("the tag reader failed on a file; its details are left out");
        None
    })
}

pub(crate) fn read_picture(path: &Path) -> Option<Vec<u8>> {
    read_tags(path, true).ok().and_then(|tags| tags.picture)
}

/// Longest tag value that is kept, in characters. Longer than any title, artist or
/// album name; a tag that holds megabytes is cut here instead of being carried into
/// the database, the search index and every list that shows it.
const MAX_TAG_CHARS: usize = 1_000;

/// True if a tag value that is in the library would read differently today: Korean
/// that was stored as Latin-1 (see `legacy`). The scan reads such a file again.
pub(crate) fn would_read_differently(text: &str) -> bool {
    super::legacy::repair(text).is_some()
}

fn clean(text: &str) -> String {
    // Korean from an old code page, declared as Latin-1 by the tag.
    let repaired = super::legacy::repair(text);
    let text = repaired.as_deref().unwrap_or(text);
    let cut = text
        .char_indices()
        .nth(MAX_TAG_CHARS)
        .map_or(text.len(), |(at, _)| at);
    text[..cut].replace('\0', " ").trim().to_string()
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

/// Parses an R128 gain tag (as Opus files carry them): a whole number of 1/256 dB that
/// brings the song to −23 LUFS. Given as the ReplayGain it amounts to, whose reference
/// is 5 dB louder.
pub(crate) fn parse_r128_gain(text: &str) -> Option<f32> {
    let steps: i32 = text.trim().parse().ok()?;
    // The tag is a 16-bit number: anything else is not one.
    i16::try_from(steps)
        .ok()
        .map(|steps| f32::from(steps) / 256.0 + 5.0)
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
    fn endless_tag_values_are_cut() {
        assert_eq!(clean("  제목\0 "), "제목");
        // Korean that an old tag declares as Latin-1 is read as Korean.
        #[cfg(any(target_os = "macos", windows))]
        {
            assert_eq!(clean("°¡»ç"), "가사");
            assert!(would_read_differently("¾ÆÀÌÀ¯") && !would_read_differently("아이유"));
        }
        assert_eq!(clean("Motörhead"), "Motörhead");
        let long = "가".repeat(50_000);
        let cut = clean(&long);
        assert_eq!(cut.chars().count(), MAX_TAG_CHARS);
        assert!(
            cut.chars().all(|c| c == '가'),
            "cut on a character boundary"
        );
        assert_eq!(clean(&"a".repeat(MAX_TAG_CHARS)).len(), MAX_TAG_CHARS);
    }

    /// An ID3v2 frame can be stored compressed, and a few kilobytes can unpack to
    /// gigabytes. The tag reader is built without that support: such a frame is not
    /// unpacked, and the file is still a song.
    #[test]
    fn a_compressed_tag_frame_is_not_unpacked() {
        // zlib of an encoding byte followed by a megabyte of the letter A.
        let packed: Vec<u8> = (0..COMPRESSED_TITLE.len() / 2)
            .map(|i| u8::from_str_radix(&COMPRESSED_TITLE[i * 2..i * 2 + 2], 16).expect("hex"))
            .collect();
        let syncsafe = |n: u32| {
            [
                (n >> 21) as u8 & 0x7F,
                (n >> 14) as u8 & 0x7F,
                (n >> 7) as u8 & 0x7F,
                n as u8 & 0x7F,
            ]
        };
        // Frame: id, size, flags (compressed + data length indicator), unpacked size, data.
        let mut frame = b"TIT2".to_vec();
        frame.extend(syncsafe(packed.len() as u32 + 4));
        frame.extend([0x00, 0x09]);
        frame.extend(syncsafe(1_000_001));
        frame.extend(&packed);
        let mut file = b"ID3\x04\x00\x00".to_vec();
        file.extend(syncsafe(frame.len() as u32));
        file.extend(&frame);
        // The audio of a fixture, without the tag it came with.
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gap_a.mp3");
        let audio = std::fs::read(fixture).expect("fixture");
        let own_tag = if audio.starts_with(b"ID3") {
            10 + audio[6..10]
                .iter()
                .fold(0usize, |n, b| (n << 7) | usize::from(*b))
        } else {
            0
        };
        file.extend(&audio[own_tag..]);
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("압축된 제목.mp3");
        std::fs::write(&path, file).expect("write");

        let started = std::time::Instant::now();
        match read_tags(&path, true) {
            // The frame is passed over: the title is the file's name.
            Ok(tags) => {
                assert!(
                    tags.title.len() < 100,
                    "title of {} bytes",
                    tags.title.len()
                );
                assert_eq!(tags.codec, "MP3");
            }
            // Or the tag is refused as a whole. The scan then reads the file without
            // its tags: still a song, known by its file name.
            Err(ReadError::Failed(_)) => {
                let plain = read_without_tags(&path).expect("the audio is fine");
                assert_eq!(plain.title, "압축된 제목");
                assert_eq!(plain.codec, "MP3");
                assert!(plain.duration_ms > 900);
            }
            Err(other) => panic!("unexpected: {other:?}"),
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    const COMPRESSED_TITLE: &str = "78daedc1210100000002a0e032ff9f7288400a0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000dc1a87480c64";

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
        // R128: 1/256 dB steps toward −23 LUFS, told as ReplayGain (5 dB above).
        assert_eq!(parse_r128_gain("0"), Some(5.0));
        assert_eq!(parse_r128_gain("-1280"), Some(0.0));
        assert_eq!(parse_r128_gain(" 256 "), Some(6.0));
        assert_eq!(parse_r128_gain("-6.5 dB"), None);
        assert_eq!(parse_r128_gain("99999"), None);
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
            "a.m4p", "a.ogg", "a.oga", "a.opus", "a.wv", "a.APE",
        ] {
            assert!(has_audio_extension(Path::new(name)), "{name}");
        }
        for name in ["a.txt", "a.jpg", "a.tta", "a.wma", "mp3", "a.mp3.part", "a"] {
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

    fn fixture(name: &str) -> Vec<u8> {
        let path = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures"))
            .join(name);
        std::fs::read(path).expect("fixture")
    }

    /// A FLAC whose metadata runs into zeros (what a crash or a failed copy leaves) reads
    /// as millions of empty metadata blocks to the tag library. It is refused at once
    /// instead of holding the scan: before the read budget, 4 MB took about 2.5 s and
    /// 100 GB hours.
    #[test]
    fn a_flac_that_runs_into_zeros_is_refused_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let flac = fixture("gap_a.flac");
        let mut header = flac[..42].to_vec();
        header[4] &= 0x7F; // STREAMINFO not the last block: the zeros are "blocks"
        let path = dir.path().join("zeros.flac");
        let mut bytes = header.clone();
        bytes.resize(4 * 1024 * 1024, 0);
        std::fs::write(&path, &bytes).unwrap();
        let started = std::time::Instant::now();
        assert!(matches!(read_tags(&path, false), Err(ReadError::Failed(_))));
        assert!(matches!(
            read_without_tags(&path),
            Err(ReadError::Failed(_))
        ));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        // A sparse file of a hundred gigabytes, the same.
        let path = dir.path().join("sparse.flac");
        let file = std::fs::File::create(&path).unwrap();
        std::io::Write::write_all(&mut &file, &header).unwrap();
        file.set_len(100 * 1024 * 1024 * 1024).unwrap();
        let started = std::time::Instant::now();
        assert!(read_tags(&path, false).is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        // The real file still reads.
        let real = dir.path().join("real.flac");
        std::fs::write(&real, &flac).unwrap();
        assert_eq!(read_tags(&real, true).unwrap().codec, "FLAC");
    }

    /// The budget: what a music file needs passes, endless small steps do not.
    #[test]
    fn the_read_budget_stops_endless_small_steps() {
        let mut budget = Budget::new(std::io::Cursor::new(vec![0u8; 1024]));
        let mut byte = [0u8; 1];
        let mut steps = 0;
        while budget.seek(SeekFrom::Start(0)).is_ok() && budget.read(&mut byte).is_ok() {
            steps += 1;
        }
        assert_eq!(steps, MAX_READ_CALLS / 2);
        let mut budget = Budget::new(std::io::repeat(0));
        let mut big = vec![0u8; 1 << 20];
        let mut read = 0u64;
        while let Ok(n) = budget.read(&mut big) {
            read += n as u64;
        }
        assert_eq!(read, MAX_READ_BYTES);
    }

    /// A pipe named like a song would never deliver its first byte: it is not opened.
    #[cfg(unix)]
    #[test]
    fn a_pipe_named_like_a_song_is_not_opened() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("song.flac");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        let (done, result) = std::sync::mpsc::channel();
        let p = path.clone();
        std::thread::spawn(move || {
            let tags = read_tags(&p, false).is_err();
            let measured = crate::audio::loudness::measure_file(
                &p,
                &std::sync::atomic::AtomicBool::new(false),
            )
            .is_err();
            done.send((tags, measured))
        });
        let (tags, measured) = result
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("came back");
        assert!(tags && measured);
    }
}
