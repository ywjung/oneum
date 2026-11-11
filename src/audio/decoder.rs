//! Demuxing and decoding of one audio file with Symphonia.
use super::{PlaybackError, PlaybackErrorKind};
use std::path::Path;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase};

/// Give up on a stream after this many undecodable packets in a row.
const MAX_CONSECUTIVE_DECODE_ERRORS: u32 = 64;

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
}

pub struct TrackDecoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    time_base: Option<TimeBase>,
    info: SourceInfo,
    /// After a seek: source frame the output must start at. Decoded audio before it is
    /// dropped using packet timestamps. `Some(u64::MAX)` means "at end of stream".
    drop_until: Option<u64>,
    consecutive_errors: u32,
    /// Corrupt packets skipped so far (reported for diagnostics).
    pub skipped_packets: u32,
}

fn err(kind: PlaybackErrorKind, detail: impl Into<String>) -> PlaybackError {
    PlaybackError { kind, detail: detail.into() }
}

fn map_open_error(e: SymError) -> PlaybackError {
    match e {
        SymError::IoError(io) => err(PlaybackErrorKind::Corrupt, format!("io while probing: {}", io.kind())),
        SymError::Unsupported(what) => err(PlaybackErrorKind::Unsupported, what),
        other => err(PlaybackErrorKind::Corrupt, other.to_string()),
    }
}

impl TrackDecoder {
    pub fn open(path: &Path) -> Result<Self, PlaybackError> {
        let file = std::fs::File::open(path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => err(PlaybackErrorKind::FileMissing, "file not found"),
            kind => err(PlaybackErrorKind::FileUnreadable, format!("open failed: {kind}")),
        })?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let format = symphonia::default::get_probe()
            .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
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
            .make_audio_decoder(&params, &AudioDecoderOptions::default().gapless(true))
            .map_err(|e| match e {
                SymError::Unsupported(what) => err(PlaybackErrorKind::Unsupported, what),
                other => err(PlaybackErrorKind::Unsupported, other.to_string()),
            })?;
        let sample_rate = params
            .sample_rate
            .filter(|r| *r > 0)
            .ok_or_else(|| err(PlaybackErrorKind::Unsupported, "unknown sample rate"))?;
        let channels = params
            .channels
            .as_ref()
            .map(|c| c.count())
            .filter(|c| *c > 0)
            .ok_or_else(|| err(PlaybackErrorKind::Unsupported, "unknown channel layout"))?;
        let codec = decoder.codec_info().short_name.to_string();
        Ok(Self {
            format,
            decoder,
            track_id,
            time_base,
            info: SourceInfo { sample_rate, channels, total_frames, codec, has_gapless_info },
            drop_until: None,
            consecutive_errors: 0,
            skipped_packets: 0,
        })
    }

    pub fn info(&self) -> &SourceInfo {
        &self.info
    }

    /// Converts a track timestamp to a source frame index.
    fn ts_to_frame(&self, ts: i64) -> i64 {
        match self.time_base {
            Some(tb) => {
                let num = i128::from(ts) * i128::from(tb.numer.get()) * i128::from(self.info.sample_rate);
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
    pub fn seek(&mut self, frame: u64) -> Result<(), PlaybackError> {
        let preroll = u64::from(self.info.sample_rate) / 8;
        let secs = frame.saturating_sub(preroll) as f64 / f64::from(self.info.sample_rate);
        let Some(time) = Time::try_from_secs_f64(secs) else {
            return Err(err(PlaybackErrorKind::Corrupt, "seek position out of range"));
        };
        match self.format.seek(SeekMode::Accurate, SeekTo::Time { time, track_id: Some(self.track_id) }) {
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
    pub fn next(&mut self, out: &mut Vec<f32>) -> Result<bool, PlaybackError> {
        out.clear();
        loop {
            if self.drop_until == Some(u64::MAX) {
                return Ok(false);
            }
            let packet = match self.format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(false),
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(false),
                Err(SymError::ResetRequired) => return Ok(false),
                Err(SymError::IoError(e)) => {
                    return Err(err(PlaybackErrorKind::FileUnreadable, format!("read failed: {}", e.kind())));
                }
                Err(other) => return Err(err(PlaybackErrorKind::Corrupt, other.to_string())),
            };
            if packet.track_id != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(buf) => {
                    self.consecutive_errors = 0;
                    let channels = buf.spec().channels().count();
                    if channels != self.info.channels || buf.spec().rate() != self.info.sample_rate {
                        return Err(err(PlaybackErrorKind::Unsupported, "stream parameters change mid-file"));
                    }
                    let frames = buf.frames();
                    if frames == 0 {
                        continue;
                    }
                    buf.copy_to_vec_interleaved::<f32>(out);
                    if let Some(target) = self.drop_until {
                        // First audible frame of this buffer (encoder-delay trim included).
                        let trim = i64::try_from(packet.trim_start.get()).unwrap_or(0);
                        let start = self.ts_to_frame(packet.pts.get().saturating_add(trim));
                        let target = i64::try_from(target).unwrap_or(i64::MAX);
                        if start.saturating_add(frames as i64) <= target {
                            out.clear();
                            continue;
                        }
                        if start < target {
                            out.drain(..(target - start) as usize * channels);
                        }
                        self.drop_until = None;
                    }
                    return Ok(true);
                }
                Err(SymError::DecodeError(_)) | Err(SymError::IoError(_)) => {
                    self.consecutive_errors += 1;
                    self.skipped_packets += 1;
                    if self.consecutive_errors > MAX_CONSECUTIVE_DECODE_ERRORS {
                        return Err(err(PlaybackErrorKind::Corrupt, "too many undecodable packets"));
                    }
                }
                Err(SymError::ResetRequired) => return Ok(false),
                Err(other) => return Err(err(PlaybackErrorKind::Corrupt, other.to_string())),
            }
        }
    }
}
