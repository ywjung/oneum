//! Demuxing and decoding of one audio file: with Symphonia, or for DSD with
//! [`super::dsd`].
use super::dsd::DsdDecoder;
use super::mp4_edit::{self, Trim};
use super::source::FileInMemory;
use super::{PlaybackError, PlaybackErrorKind};
use std::path::Path;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase};

/// Give up on a stream after this many undecodable packets in a row.
const MAX_CONSECUTIVE_DECODE_ERRORS: u32 = 64;
/// Sample rates outside this range are a damaged header, not music.
const SAMPLE_RATE_RANGE: std::ops::RangeInclusive<u32> = 4_000..=768_000;
/// More channels than any real layout: a damaged header.
const MAX_CHANNELS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceInfo {
    pub sample_rate: u32,
    pub channels: usize,
    /// Playable frames (encoder delay/padding excluded) when the container knows it.
    pub total_frames: Option<u64>,
    /// Short codec name as reported by the decoder (e.g. "flac", "mp3", "aac").
    pub codec: String,
    /// True when the container carried encoder delay/padding information that is trimmed.
    pub has_gapless_info: bool,
    /// True when the file is played from memory (see `source.rs`).
    pub in_memory: bool,
    /// For DSD: the one-bit rate of the file (`sample_rate` is the PCM it is converted to).
    pub dsd_rate: Option<u32>,
}

/// Decodes one file into PCM (or, for a track of a CUE sheet, one stretch of a file).
pub struct TrackDecoder {
    inner: Inner,
    range: Option<Range>,
}

/// A track of a CUE sheet: the frames of the file it is, as positions in the file.
struct Range {
    start: u64,
    end: Option<u64>,
    /// The file position of the next frame `next` gives.
    next: u64,
    /// The file's info with the track's length.
    info: SourceInfo,
}

enum Inner {
    Media(Box<MediaDecoder>),
    Dsd(Box<DsdDecoder>, SourceInfo),
}

/// What Symphonia decodes.
struct MediaDecoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    time_base: Option<TimeBase>,
    info: SourceInfo,
    /// For AAC in an MP4 file: which frames of the decoded stream are the music (the
    /// reader does not act on the file's edit list or Apple's gapless note itself).
    trim: Option<Trim>,
    /// After a seek: source frame the output must start at. Decoded audio before it is
    /// dropped using packet timestamps. `Some(u64::MAX)` means "at end of stream".
    drop_until: Option<u64>,
    consecutive_errors: u32,
    /// Corrupt packets skipped so far (reported for diagnostics).
    skipped_packets: u32,
    /// Where (in source frames, from the packet timestamps) the audio `next` gave last
    /// starts. Unlike a count of what was given, this stays right when a damaged packet
    /// is skipped.
    position: u64,
    /// Bytes read since the last packet that decoded (see [`Watched`]).
    since_audio: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

fn err(kind: PlaybackErrorKind, detail: impl Into<String>) -> PlaybackError {
    PlaybackError {
        kind,
        detail: detail.into(),
    }
}

fn map_open_error(e: SymError) -> PlaybackError {
    match e {
        SymError::IoError(io) => err(
            PlaybackErrorKind::Corrupt,
            format!("io while probing: {}", io.kind()),
        ),
        SymError::Unsupported(what) => err(PlaybackErrorKind::Unsupported, what),
        other => err(PlaybackErrorKind::Corrupt, other.to_string()),
    }
}

/// The largest sample value let into the pipeline: 36 dB above full scale. No music is
/// stored that far over (float files keep a few dB of headroom at most); what is, is
/// damage, and numbers like 1e38 overflow in the rate converter and the filters.
pub(crate) const MAX_SAMPLE: f32 = 64.0;

/// Makes samples safe to compute with: what is not a number becomes silence, what is
/// absurdly large is brought down to [`MAX_SAMPLE`].
pub(crate) fn sanitize(samples: &mut [f32]) {
    for s in samples {
        if !s.is_finite() {
            *s = 0.0;
        } else if s.abs() > MAX_SAMPLE {
            *s = MAX_SAMPLE.copysign(*s);
        }
    }
}

/// Why a file could not be opened.
pub(crate) fn map_file_error(e: &std::io::Error) -> PlaybackError {
    match e.kind() {
        std::io::ErrorKind::NotFound => err(PlaybackErrorKind::FileMissing, "file not found"),
        kind => err(
            PlaybackErrorKind::FileUnreadable,
            format!("open failed: {kind}"),
        ),
    }
}

/// Whether the decoder can play the file at all: opens it and decodes its first packet.
/// For the scanner, to tell apart what a file's header calls by one name: AAC-LC in
/// stereo plays, HE-AAC and 5.1 AAC do not. `Err` says why not.
pub fn can_play(path: &Path) -> Result<(), PlaybackError> {
    let mut decoder = TrackDecoder::open(path)?;
    let mut first = Vec::new();
    decoder.next(&mut first).map(|_| ())
}

/// Bytes the demuxer may read without a packet of audio coming of it (when opening the
/// file, and between packets). Real files have packets every few kilobytes and their
/// largest stretch of non-audio is a cover of some megabytes; a file that has turned
/// into zeros (or a sparse file of gigabytes) would otherwise be searched to its end.
const MAX_BYTES_WITHOUT_AUDIO: u64 = 128 * 1024 * 1024;

/// The counter's value while a seek is under way: reads are not counted.
const SEEKING: u64 = u64::MAX;

/// A source that stops giving bytes once [`MAX_BYTES_WITHOUT_AUDIO`] have been read
/// since the counter was last reset (by a packet that decoded).
struct Watched {
    inner: Box<dyn MediaSource>,
    since_audio: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

/// The error a [`Watched`] source gives: no audio in what may be read.
fn no_audio_error() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "no audio in the last 128 MB of the file",
    )
}

impl std::io::Read for Watched {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::sync::atomic::Ordering;
        let read = self.since_audio.load(Ordering::Relaxed);
        if read == SEEKING {
            return self.inner.read(buf);
        }
        if read >= MAX_BYTES_WITHOUT_AUDIO {
            return Err(no_audio_error());
        }
        let n = self.inner.read(buf)?;
        self.since_audio.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

impl std::io::Seek for Watched {
    fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(to)
    }
}

impl MediaSource for Watched {
    fn is_seekable(&self) -> bool {
        self.inner.is_seekable()
    }

    fn byte_len(&self) -> Option<u64> {
        self.inner.byte_len()
    }
}

/// ID3v2 tags in front of the audio looked past before giving up (one is usual, two
/// happen; more is not music).
const MAX_LEADING_TAGS: usize = 16;

/// A source seen from `start` on.
struct FromOffset {
    inner: Box<dyn MediaSource>,
    start: u64,
}

impl std::io::Read for FromOffset {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl std::io::Seek for FromOffset {
    fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
        use std::io::SeekFrom;
        let at = match to {
            SeekFrom::Start(n) => self
                .inner
                .seek(SeekFrom::Start(self.start.saturating_add(n)))?,
            other => self.inner.seek(other)?,
        };
        if at < self.start {
            // Not before the start of what is shown.
            self.inner.seek(SeekFrom::Start(self.start))?;
            return Ok(0);
        }
        Ok(at - self.start)
    }
}

impl MediaSource for FromOffset {
    fn is_seekable(&self) -> bool {
        self.inner.is_seekable()
    }

    fn byte_len(&self) -> Option<u64> {
        self.inner
            .byte_len()
            .map(|len| len.saturating_sub(self.start))
    }
}

/// The source with the ID3v2 tags at its start left out. Playback takes a song's tags
/// from the library and never from the demuxer, which would read every frame of a tag
/// into memory: a crafted tag of a million small frames is hundreds of megabytes there.
fn after_leading_tags(mut source: Box<dyn MediaSource>) -> Box<dyn MediaSource> {
    use std::io::{Read, Seek, SeekFrom};
    let mut start = 0u64;
    for _ in 0..MAX_LEADING_TAGS {
        let mut header = [0u8; 10];
        if source.seek(SeekFrom::Start(start)).is_err() || source.read_exact(&mut header).is_err() {
            break;
        }
        let size = &header[6..10];
        let is_tag = &header[..3] == b"ID3"
            && header[3] < 0xFF
            && header[4] < 0xFF
            && size.iter().all(|b| b & 0x80 == 0);
        if !is_tag {
            break;
        }
        let len = size.iter().fold(0u64, |n, b| n << 7 | u64::from(*b));
        let footer = if header[5] & 0x10 != 0 { 10 } else { 0 };
        start += 10 + len + footer;
    }
    if source.seek(SeekFrom::Start(start)).is_err() {
        start = 0;
        let _ = source.seek(SeekFrom::Start(0));
    }
    if start == 0 {
        return source;
    }
    Box::new(FromOffset {
        inner: source,
        start,
    })
}

/// True if what `reader` holds starts like a DSD file. Leaves it at the start.
fn starts_as_dsd<R: std::io::Read + std::io::Seek>(reader: &mut R) -> Result<bool, PlaybackError> {
    let mut magic = [0u8; 4];
    let read = std::io::Read::read(reader, &mut magic).map_err(|e| map_file_error(&e))?;
    std::io::Seek::rewind(reader).map_err(|e| map_file_error(&e))?;
    Ok(read == 4 && super::dsd::is_dsd_magic(&magic))
}

fn dsd_info(decoder: &DsdDecoder, in_memory: bool) -> SourceInfo {
    let info = decoder.info();
    SourceInfo {
        sample_rate: info.pcm_rate(),
        channels: info.out_channels,
        total_frames: Some(info.frames()),
        codec: "dsd".into(),
        has_gapless_info: true,
        in_memory,
        dsd_rate: Some(info.dsd_rate),
    }
}

impl TrackDecoder {
    /// Opens `path`, reading from the file as decoding goes. A track of a CUE sheet
    /// (see `library::cue`) opens its stretch of the sheet's audio file.
    pub fn open(path: &Path) -> Result<Self, PlaybackError> {
        Self::open_located(path, crate::library::cue::resolve(path).as_ref())
    }

    /// Like [`TrackDecoder::open`] for a track whose stretch of a sheet's file was
    /// looked up already (`None`: the file `path` whole).
    pub fn open_located(
        path: &Path,
        segment: Option<&crate::library::cue::Segment>,
    ) -> Result<Self, PlaybackError> {
        match segment {
            Some(segment) => Self::open_file(&segment.audio)?.limited_to(segment),
            None => Self::open_file(path),
        }
    }

    fn open_file(path: &Path) -> Result<Self, PlaybackError> {
        let mut file = std::fs::File::open(path).map_err(|e| map_file_error(&e))?;
        let inner = if starts_as_dsd(&mut file)? {
            let decoder = DsdDecoder::new(Box::new(file))?;
            let info = dsd_info(&decoder, false);
            Inner::Dsd(Box::new(decoder), info)
        } else {
            Inner::Media(Box::new(MediaDecoder::from_source(
                path,
                Box::new(file),
                false,
                false,
            )?))
        };
        Ok(TrackDecoder { inner, range: None })
    }

    /// Opens `path` for checking it: like [`TrackDecoder::open`], with the decoder
    /// computing the checksum the file carries for its audio (FLAC's MD5), to be
    /// compared by [`TrackDecoder::checksum_matches`] at the end.
    /// `segment`: the track's stretch of a sheet's file, looked up already (`None`: the
    /// file `path` whole).
    pub fn open_verifying(
        path: &Path,
        segment: Option<&crate::library::cue::Segment>,
    ) -> Result<Self, PlaybackError> {
        // A sheet's track is part of the file: the checksum is the whole file's.
        if let Some(segment) = segment {
            return Self::open_file(&segment.audio)?.limited_to(segment);
        }
        let mut file = std::fs::File::open(path).map_err(|e| map_file_error(&e))?;
        let inner = if starts_as_dsd(&mut file)? {
            let decoder = DsdDecoder::new(Box::new(file))?;
            let info = dsd_info(&decoder, false);
            Inner::Dsd(Box::new(decoder), info)
        } else {
            Inner::Media(Box::new(MediaDecoder::from_source(
                path,
                Box::new(file),
                false,
                true,
            )?))
        };
        Ok(TrackDecoder { inner, range: None })
    }

    /// After decoding a file opened with [`TrackDecoder::open_verifying`] to its end:
    /// whether the checksum it carries matches the decoded audio. `None` when it
    /// carries none (or the format has none).
    pub fn checksum_matches(&mut self) -> Option<bool> {
        if self.range.is_some() {
            return None;
        }
        match &mut self.inner {
            Inner::Media(m) => m.decoder.finalize().verify_ok,
            Inner::Dsd(..) => None,
        }
    }

    /// Opens a file that is being loaded into memory (memory playback).
    pub fn open_in_memory(file: FileInMemory) -> Result<Self, PlaybackError> {
        let path = file.path().to_path_buf();
        let mut source = file.into_source();
        let inner = if starts_as_dsd(&mut source)? {
            let decoder = DsdDecoder::from_memory(source)?;
            let info = dsd_info(&decoder, true);
            Inner::Dsd(Box::new(decoder), info)
        } else {
            Inner::Media(Box::new(MediaDecoder::from_source(
                &path,
                Box::new(source),
                true,
                false,
            )?))
        };
        Ok(TrackDecoder { inner, range: None })
    }

    /// Plays only `segment` of the file, if given (a track of a CUE sheet, for a decoder
    /// opened from the sheet's file in memory).
    pub fn within(
        self,
        segment: Option<&crate::library::cue::Segment>,
    ) -> Result<Self, PlaybackError> {
        match segment {
            Some(segment) => self.limited_to(segment),
            None => Ok(self),
        }
    }

    /// Plays only `segment` of the file: from its first frame (sought to exactly) to
    /// the frame before the next track's first.
    fn limited_to(mut self, segment: &crate::library::cue::Segment) -> Result<Self, PlaybackError> {
        let rate = self.info().sample_rate;
        let start = segment.start_frame(rate);
        // Not beyond the end of the file (a sheet may say the next track starts later
        // than the file lasts).
        let file_frames = self.info().total_frames;
        let end = segment
            .end_frame(rate)
            .map(|end| file_frames.map_or(end, |total| end.min(total)));
        if start > 0 {
            self.seek_file(start)?;
        }
        let mut info = self.info().clone();
        info.total_frames = end.or(file_frames).map(|end| end.saturating_sub(start));
        self.range = Some(Range {
            start,
            end,
            next: start,
            info,
        });
        Ok(self)
    }

    pub fn info(&self) -> &SourceInfo {
        if let Some(range) = &self.range {
            return &range.info;
        }
        match &self.inner {
            Inner::Media(m) => &m.info,
            Inner::Dsd(_, info) => info,
        }
    }

    /// Seeks so that the next decoded frame is `frame` (in source frames). Seeking past
    /// the end leaves the decoder at end of stream.
    pub fn seek(&mut self, frame: u64) -> Result<(), PlaybackError> {
        let Some(range) = &self.range else {
            return self.seek_file(frame);
        };
        let target = range.start.saturating_add(frame);
        let target = range.end.map_or(target, |end| target.min(end));
        self.seek_file(target)?;
        if let Some(range) = &mut self.range {
            range.next = target;
        }
        Ok(())
    }

    fn seek_file(&mut self, frame: u64) -> Result<(), PlaybackError> {
        match &mut self.inner {
            Inner::Media(m) => m.seek(frame),
            Inner::Dsd(d, _) => d.seek(frame),
        }
    }

    /// Decodes the next stretch into `out` (cleared first) as interleaved f32 at the
    /// source channel count. Returns `Ok(false)` at end of stream.
    pub fn next(&mut self, out: &mut Vec<f32>) -> Result<bool, PlaybackError> {
        if let Some(range) = &self.range
            && range.end.is_some_and(|end| range.next >= end)
        {
            out.clear();
            return Ok(false);
        }
        let more = match &mut self.inner {
            Inner::Media(m) => m.next(out)?,
            Inner::Dsd(d, _) => d.next(out)?,
        };
        let Some(range) = &mut self.range else {
            return Ok(more);
        };
        if !more {
            return Ok(false);
        }
        let channels = range.info.channels.max(1);
        // Where this audio is in the file: from the packet timestamps (a damaged packet
        // that was skipped leaves a hole a count would not see); DSD has no packets to
        // skip and is counted.
        let at = match &self.inner {
            Inner::Media(m) => m.position,
            Inner::Dsd(..) => range.next,
        };
        if let Some(end) = range.end {
            let left = end.saturating_sub(at) as usize;
            out.truncate(left.saturating_mul(channels).min(out.len()));
        }
        range.next = at + (out.len() / channels) as u64;
        Ok(!out.is_empty())
    }

    /// Corrupt packets skipped so far (reported for diagnostics).
    pub fn skipped_packets(&self) -> u32 {
        match &self.inner {
            Inner::Media(m) => m.skipped_packets,
            Inner::Dsd(..) => 0,
        }
    }

    /// For playback: the last frames of a DSD file depend on what follows it, so they
    /// are held when the file runs out (`next` says "end", [`TrackDecoder::holds_tail`]
    /// says there is more) until [`TrackDecoder::finish`] is told what follows. Other
    /// files are not affected.
    pub fn defer_tail(&mut self) {
        if let Inner::Dsd(d, _) = &mut self.inner {
            d.defer_tail(true);
        }
    }

    /// True when frames are held for [`TrackDecoder::finish`].
    pub fn holds_tail(&self) -> bool {
        matches!(&self.inner, Inner::Dsd(d, _) if d.holds_tail())
    }

    /// The held frames, into `out`: computed on into `following` when it is DSD that
    /// continues this file (which then starts from where this one leaves off), with
    /// silence otherwise.
    pub fn finish(
        &mut self,
        following: Option<&mut TrackDecoder>,
        out: &mut Vec<f32>,
    ) -> Result<(), PlaybackError> {
        out.clear();
        let Inner::Dsd(d, _) = &mut self.inner else {
            return Ok(());
        };
        let next = following.and_then(|f| match &mut f.inner {
            Inner::Dsd(next, _) => Some(next.as_mut()),
            Inner::Media(_) => None,
        });
        d.finish(next, out)
    }
}

impl MediaDecoder {
    fn from_source(
        path: &Path,
        source: Box<dyn MediaSource>,
        in_memory: bool,
        verify: bool,
    ) -> Result<Self, PlaybackError> {
        let since_audio = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let watched = Watched {
            inner: after_leading_tags(source),
            since_audio: std::sync::Arc::clone(&since_audio),
        };
        let mss = MediaSourceStream::new(Box::new(watched), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let format = symphonia::default::get_probe()
            .probe(
                &hint,
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .map_err(|e| match e {
                // An unrecognised container is reported as an I/O "end of stream" by the probe.
                SymError::IoError(_) | SymError::Unsupported(_) => {
                    err(PlaybackErrorKind::Unsupported, "unrecognised container")
                }
                other => map_open_error(other),
            })?;
        let track = format
            .default_track(TrackType::Audio)
            .ok_or_else(|| err(PlaybackErrorKind::Unsupported, "no audio track"))?;
        let track_id = track.id;
        let time_base = track.time_base;
        let total_frames = track.num_frames;
        let has_gapless_info = track.delay.is_some() || track.padding.is_some();
        let Some(CodecParameters::Audio(params)) = track.codec_params.clone() else {
            return Err(err(PlaybackErrorKind::Unsupported, "not an audio stream"));
        };
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(
                &params,
                &AudioDecoderOptions::default().gapless(true).verify(verify),
            )
            .map_err(|e| match e {
                SymError::Unsupported(what) => err(PlaybackErrorKind::Unsupported, what),
                other => err(PlaybackErrorKind::Unsupported, other.to_string()),
            })?;
        let sample_rate = params
            .sample_rate
            .filter(|r| SAMPLE_RATE_RANGE.contains(r))
            .ok_or_else(|| err(PlaybackErrorKind::Unsupported, "unusable sample rate"))?;
        let channels = params
            .channels
            .as_ref()
            .map(|c| c.count())
            .filter(|c| (1..=MAX_CHANNELS).contains(c))
            .ok_or_else(|| err(PlaybackErrorKind::Unsupported, "unusable channel layout"))?;
        // A container that knows only its own length (Matroska): the frames from that.
        let total_frames = total_frames.or_else(|| {
            let media = format.media_info();
            let (base, length) = (media.time_base?, media.duration?);
            let frames =
                i128::from(length.get()) * i128::from(base.numer.get()) * i128::from(sample_rate)
                    / i128::from(base.denom.get());
            u64::try_from(frames).ok().filter(|f| *f > 0)
        });
        let codec = decoder.codec_info().short_name.to_string();
        // AAC's priming and padding: the reader passes them on as audio. What the file
        // says about them is read here and applied while decoding.
        let trim = (codec == "aac" && !has_gapless_info)
            .then(|| std::fs::File::open(path).ok())
            .flatten()
            .and_then(|file| mp4_edit::read(std::io::BufReader::new(file), sample_rate))
            // A note that claims more music than the track holds is wrong about the
            // length shown and sought in: the track's own length (less the priming) wins.
            .map(
                |trim| match total_frames.map(|n| n.saturating_sub(trim.delay)) {
                    Some(held) if held > 0 && trim.valid > held => Trim {
                        valid: held,
                        ..trim
                    },
                    _ => trim,
                },
            );
        let (total_frames, has_gapless_info) = match trim {
            Some(trim) => (Some(trim.valid), true),
            None => (total_frames, has_gapless_info),
        };
        Ok(Self {
            format,
            decoder,
            track_id,
            time_base,
            info: SourceInfo {
                sample_rate,
                channels,
                total_frames,
                codec,
                has_gapless_info,
                in_memory,
                dsd_rate: None,
            },
            trim,
            drop_until: None,
            consecutive_errors: 0,
            skipped_packets: 0,
            position: 0,
            since_audio,
        })
    }

    /// Converts a track timestamp to a source frame index.
    fn ts_to_frame(&self, ts: i64) -> i64 {
        match self.time_base {
            Some(tb) => {
                let num =
                    i128::from(ts) * i128::from(tb.numer.get()) * i128::from(self.info.sample_rate);
                (num / i128::from(tb.denom.get())) as i64
            }
            None => ts,
        }
    }

    /// Seeks so that the next decoded frame is `frame` (in source frames). Seeking past the
    /// end leaves the decoder at end of stream.
    ///
    /// The reader is positioned a little before the target and the surplus is dropped by
    /// timestamp while decoding. This gives codecs that need a warm-up packet after a
    /// reset (Vorbis, MP3 bit reservoir) valid audio at the target.
    fn seek(&mut self, frame: u64) -> Result<(), PlaybackError> {
        // A seek may read far to find its place (an MP3 without a table of contents is
        // walked frame by frame): what it reads does not count as reading without audio.
        self.since_audio
            .store(SEEKING, std::sync::atomic::Ordering::Relaxed);
        let result = self.seek_inner(frame);
        self.since_audio
            .store(0, std::sync::atomic::Ordering::Relaxed);
        result
    }

    fn seek_inner(&mut self, frame: u64) -> Result<(), PlaybackError> {
        // Positions are counted in the music; the decoded stream has the priming in front.
        let frame = frame.saturating_add(self.trim.map_or(0, |t| t.delay));
        let preroll = u64::from(self.info.sample_rate) / 8;
        let secs = frame.saturating_sub(preroll) as f64 / f64::from(self.info.sample_rate);
        let Some(time) = Time::try_from_secs_f64(secs) else {
            return Err(err(
                PlaybackErrorKind::Corrupt,
                "seek position out of range",
            ));
        };
        match self.format.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time,
                track_id: Some(self.track_id),
            },
        ) {
            Ok(_) => {
                self.decoder.reset();
                self.drop_until = Some(frame);
                Ok(())
            }
            Err(SymError::SeekError(_)) | Err(SymError::IoError(_)) => {
                // Out of range or unseekable: treat as "play from the end", i.e. finish the track.
                self.drop_until = Some(u64::MAX);
                Ok(())
            }
            Err(other) => Err(err(PlaybackErrorKind::Corrupt, other.to_string())),
        }
    }

    /// Decodes the next packet into `out` (cleared first) as interleaved f32 at the source
    /// channel count. Returns `Ok(false)` at end of stream. Damaged packets are skipped.
    fn next(&mut self, out: &mut Vec<f32>) -> Result<bool, PlaybackError> {
        out.clear();
        loop {
            if self.drop_until == Some(u64::MAX) {
                return Ok(false);
            }
            let packet = match self.format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(false),
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(false);
                }
                // Far past the last audio there is only damage: the track ends there.
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::InvalidData => {
                    tracing::debug!("{e}: ending the track");
                    return Ok(false);
                }
                Err(SymError::ResetRequired) => return Ok(false),
                Err(SymError::IoError(e)) => {
                    return Err(err(
                        PlaybackErrorKind::FileUnreadable,
                        format!("read failed: {}", e.kind()),
                    ));
                }
                Err(other) => return Err(err(PlaybackErrorKind::Corrupt, other.to_string())),
            };
            if packet.track_id != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(buf) => {
                    self.consecutive_errors = 0;
                    self.since_audio
                        .store(0, std::sync::atomic::Ordering::Relaxed);
                    let channels = buf.spec().channels().count();
                    if channels != self.info.channels || buf.spec().rate() != self.info.sample_rate
                    {
                        return Err(err(
                            PlaybackErrorKind::Unsupported,
                            "stream parameters change mid-file",
                        ));
                    }
                    let frames = buf.frames();
                    if frames == 0 {
                        continue;
                    }
                    buf.copy_to_vec_interleaved::<f32>(out);
                    // Float files can carry NaN/infinity; those would poison the
                    // resampler and every gain stage after it.
                    sanitize(out);
                    // First frame of this buffer in the decoded stream (what the decoder
                    // trimmed of the encoder delay itself included).
                    let trimmed = i64::try_from(packet.trim_start.get()).unwrap_or(0);
                    let mut start = self.ts_to_frame(packet.pts.get().saturating_add(trimmed));
                    if let Some(trim) = self.trim {
                        // Keep what lies inside the music: after the priming, before
                        // the padding.
                        let first = i64::try_from(trim.delay).unwrap_or(i64::MAX);
                        let end =
                            first.saturating_add(i64::try_from(trim.valid).unwrap_or(i64::MAX));
                        if start >= end {
                            out.clear();
                            return Ok(false);
                        }
                        let buffer_end = start.saturating_add(frames as i64);
                        if buffer_end > end {
                            out.truncate((end - start) as usize * channels);
                        }
                        if buffer_end <= first {
                            out.clear();
                            continue;
                        }
                        if start < first {
                            out.drain(..(first - start) as usize * channels);
                            start = first;
                        }
                    }
                    if let Some(target) = self.drop_until {
                        let target = i64::try_from(target).unwrap_or(i64::MAX);
                        let left = (out.len() / channels) as i64;
                        if start.saturating_add(left) <= target {
                            out.clear();
                            continue;
                        }
                        if start < target {
                            out.drain(..(target - start) as usize * channels);
                            start = target;
                        }
                        self.drop_until = None;
                    }
                    if out.is_empty() {
                        continue;
                    }
                    // In the music's frames (the stream has the priming in front).
                    let delay = self.trim.map_or(0, |t| i64::try_from(t.delay).unwrap_or(0));
                    self.position = u64::try_from(start.saturating_sub(delay)).unwrap_or(0);
                    return Ok(true);
                }
                Err(SymError::DecodeError(_)) | Err(SymError::IoError(_)) => {
                    self.consecutive_errors += 1;
                    self.skipped_packets += 1;
                    if self.consecutive_errors > MAX_CONSECUTIVE_DECODE_ERRORS {
                        return Err(err(
                            PlaybackErrorKind::Corrupt,
                            "too many undecodable packets",
                        ));
                    }
                }
                Err(SymError::ResetRequired) => return Ok(false),
                Err(other) => return Err(err(PlaybackErrorKind::Corrupt, other.to_string())),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::sanitize;

    #[test]
    fn non_finite_samples_become_silence() {
        let mut samples = [0.25, f32::NAN, -0.5, f32::INFINITY, f32::NEG_INFINITY, 1.0];
        sanitize(&mut samples);
        assert_eq!(samples, [0.25, 0.0, -0.5, 0.0, 0.0, 1.0]);
        // Far outside the range but still a number: brought down, sign kept. A float
        // file's few dB of headroom are left alone.
        let mut wild = [3.0e38, -3.0e38, 2.5, -63.9, 64.0];
        sanitize(&mut wild);
        assert_eq!(wild, [64.0, -64.0, 2.5, -63.9, 64.0]);
    }
}

/// Tags of a container read through Symphonia: for files the tag library does not know
/// (Matroska, WebM). Text values only, as they are written.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct MediaTags {
    pub title: String,
    pub artist: String,
    pub album_artist: String,
    pub album: String,
    pub genre: String,
    pub date: String,
    pub track_no: Option<u32>,
    pub disc_no: Option<u32>,
    pub compilation: bool,
    /// Track gain, track peak, album gain, album peak, as written.
    pub replaygain: [Option<String>; 4],
    pub picture: Option<Vec<u8>>,
}

/// Reads the tags of `path` with Symphonia (the newest revision, the file's own and its
/// audio track's). Nothing for a file it cannot open.
pub(crate) fn read_media_tags(path: &Path) -> MediaTags {
    use symphonia::core::meta::{StandardTag, StandardVisualKey, Tag};
    let mut tags = MediaTags::default();
    let Ok(file) = std::fs::File::open(path) else {
        return tags;
    };
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let Ok(mut format) = symphonia::default::get_probe().probe(
        &hint,
        mss,
        FormatOptions::default(),
        MetadataOptions::default(),
    ) else {
        return tags;
    };
    let mut metadata = format.metadata();
    let Some(revision) = metadata.skip_to_latest() else {
        return tags;
    };
    let mut all: Vec<&Tag> = revision.media.tags.iter().collect();
    let mut visuals: Vec<_> = revision.media.visuals.iter().collect();
    for track in &revision.per_track {
        all.extend(&track.metadata.tags);
        visuals.extend(&track.metadata.visuals);
    }
    let set = |slot: &mut String, value: &str| {
        if slot.is_empty() {
            *slot = value.to_string();
        }
    };
    for tag in all {
        // Matroska names its tags by level: "ALBUM@ARTIST", "TRACK@TITLE"; the album
        // level's title is the album and its artist the album artist.
        let (level, name) = match tag.raw.key.split_once('@') {
            Some((level, name)) => (level.to_ascii_uppercase(), name.to_ascii_uppercase()),
            None => (String::new(), tag.raw.key.to_ascii_uppercase()),
        };
        let album_level = level == "ALBUM";
        let text = tag.raw.value.to_string();
        match (&tag.std, name.as_str()) {
            (_, "TITLE") if album_level => set(&mut tags.album, &text),
            (_, "ARTIST") if album_level => set(&mut tags.album_artist, &text),
            (Some(StandardTag::TrackTitle(v)), _) => set(&mut tags.title, v),
            (_, "TITLE") => set(&mut tags.title, &text),
            (Some(StandardTag::Artist(v)), _) => set(&mut tags.artist, v),
            (_, "ARTIST") => set(&mut tags.artist, &text),
            (Some(StandardTag::AlbumArtist(v)), _) => set(&mut tags.album_artist, v),
            (Some(StandardTag::Album(v)), _) => set(&mut tags.album, v),
            (_, "ALBUM") => set(&mut tags.album, &text),
            (Some(StandardTag::Genre(v)), _) => set(&mut tags.genre, v),
            (_, "GENRE") => set(&mut tags.genre, &text),
            (Some(StandardTag::RecordingDate(v) | StandardTag::ReleaseDate(v)), _) => {
                set(&mut tags.date, v)
            }
            (_, "DATE_RECORDED" | "DATE_RELEASED" | "DATE") => set(&mut tags.date, &text),
            (Some(StandardTag::TrackNumber(n)), _) => {
                tags.track_no = tags.track_no.or(u32::try_from(*n).ok())
            }
            (_, "PART_NUMBER") if !album_level => {
                tags.track_no = tags.track_no.or(text.trim().parse().ok())
            }
            (Some(StandardTag::DiscNumber(n)), _) => {
                tags.disc_no = tags.disc_no.or(u32::try_from(*n).ok())
            }
            (Some(StandardTag::CompilationFlag(on)), _) => tags.compilation |= *on,
            (Some(StandardTag::ReplayGainTrackGain(v)), _) => {
                tags.replaygain[0].get_or_insert_with(|| v.to_string());
            }
            (Some(StandardTag::ReplayGainTrackPeak(v)), _) => {
                tags.replaygain[1].get_or_insert_with(|| v.to_string());
            }
            (Some(StandardTag::ReplayGainAlbumGain(v)), _) => {
                tags.replaygain[2].get_or_insert_with(|| v.to_string());
            }
            (Some(StandardTag::ReplayGainAlbumPeak(v)), _) => {
                tags.replaygain[3].get_or_insert_with(|| v.to_string());
            }
            _ => {}
        }
    }
    // A file tagged at the album level only: its artist is the album's.
    if tags.artist.is_empty() {
        tags.artist = tags.album_artist.clone();
    }
    // The front cover if one is marked as such, else the first picture.
    tags.picture = visuals
        .iter()
        .find(|v| v.usage == Some(StandardVisualKey::FrontCover))
        .or(visuals.first())
        .map(|v| v.data.to_vec());
    tags
}
