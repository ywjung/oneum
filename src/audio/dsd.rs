//! DSD (one-bit) audio: DSF and DSDIFF files, converted to PCM for playback.
//!
//! A DSD stream is one bit per sample at 64 to 512 times 44.1 kHz (or 48 kHz). It is
//! played the way most players do: converted to PCM at 88.2 kHz (96 kHz for the 48 kHz
//! family), after which everything else (gapless, leveling, the equalizer, the meters,
//! rate matching) works as for any other file. The conversion is a linear-phase low-pass
//! in two stages:
//!
//! 1. From the bits to 4 × the PCM rate (352.8 kHz): a Kaiser-windowed FIR run over
//!    whole bytes with tables (the contribution of each of the 256 values of a byte at
//!    each position in the filter), flat to 20 kHz, more than 140 dB down from where
//!    anything would fold back into 0–40 kHz.
//! 2. From there to the PCM rate: a 125-tap FIR, flat (±0.0001 dB) to 20 kHz, −6 dB at
//!    30 kHz and more than 100 dB down from 40 kHz, which takes the noise that DSD's
//!    noise shaping puts above the audio band out of the signal.
//!
//! A bit 1 is +1 and a bit 0 is −1, so full-scale DSD is full-scale PCM: no gain is
//! added (the SACD reference level, 50 % modulation, comes out at −6 dBFS).
//!
//! The filters look ahead by [`LATENCY`] output frames. At the start of a file the bits
//! before it count as DSD silence (the 0x69 pattern); at the end, what follows is either
//! the next file's bits, when the next song is DSD of the same kind and follows without
//! a gap (see [`DsdDecoder::finish`]), or silence. Seeking starts the filters a little
//! before the target, so that what comes out there is exactly what plain playback gives.
use super::source::MemorySource;
use super::{PlaybackError, PlaybackErrorKind};
use std::f64::consts::PI;
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

/// DSD silence: equal numbers of ones and zeros, as encoders write it.
pub const SILENCE: u8 = 0x69;

/// Output frames the conversion looks ahead (see the module notes): output `j` of the
/// filters is the PCM frame `j - LATENCY`.
pub const LATENCY: u64 = 16;

/// Taps of the second stage (odd: its delay is a whole number of its input samples).
const STAGE2_TAPS: usize = 125;
/// Bytes of the first stage per 2.8224 MHz (the first stage's length grows with the rate,
/// its frequencies stay where they are).
const STAGE1_BYTES_PER_64FS: usize = 12;
/// Bytes per channel read at a time.
const CHUNK_BYTES: usize = 8_192;
/// Largest ID3 tag read from a file.
const MAX_TAG_BYTES: u64 = 16 * 1024 * 1024;
/// Chunks looked at in a DSDIFF file before giving up on it.
const MAX_CHUNKS: usize = 1_024;

/// The PCM rate DSD at `dsd_rate` is converted to: 88.2 kHz for the 44.1 kHz family,
/// 96 kHz for the 48 kHz family. `None` for a rate that is not 64, 128, … 1024 times one
/// of the two.
pub fn pcm_rate(dsd_rate: u32) -> Option<u32> {
    [88_200u32, 96_000].into_iter().find(|&base| {
        dsd_rate.is_multiple_of(base) && {
            let ratio = dsd_rate / base;
            ratio.is_power_of_two() && (32..=512).contains(&ratio)
        }
    })
}

/// The usual name of a DSD rate: "DSD64" for 64 × 44.1 kHz (and 64 × 48 kHz).
pub fn rate_name(dsd_rate: u32) -> String {
    let base = if dsd_rate.is_multiple_of(44_100) {
        44_100
    } else {
        48_000
    };
    format!("DSD{}", dsd_rate / base)
}

/// True for the first four bytes of a DSF ("DSD ") or DSDIFF ("FRM8") file.
pub fn is_dsd_magic(magic: &[u8]) -> bool {
    magic.starts_with(b"DSD ") || magic.starts_with(b"FRM8")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    /// Sony's DSF: little-endian chunks, the channels in blocks, ID3v2 at the end.
    Dsf,
    /// Philips' DSDIFF (.dff): big-endian IFF, the channels interleaved byte by byte.
    Dsdiff,
}

/// Where the five channels of a 5.1 layout go, in the order the rest of the app uses
/// (front left, front right, centre, LFE, surround left, surround right).
const SURROUND: usize = 6;

/// What a DSD file holds and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsdInfo {
    pub container: Container,
    pub dsd_rate: u32,
    /// For each channel in the file, the output channel it goes to.
    slots: Vec<usize>,
    /// Channels of the PCM that comes out: 1, 2 or 6.
    pub out_channels: usize,
    /// One-bit samples per channel.
    pub samples: u64,
    data_start: u64,
    /// DSF: bytes per channel in a block; the channels follow each other block by block.
    block: Option<u64>,
    /// DSF with 1 bit per sample: the first sample is the lowest bit.
    lsb_first: bool,
    /// Where the ID3v2 tag is: offset and length.
    pub tag: Option<(u64, u64)>,
}

impl DsdInfo {
    pub fn pcm_rate(&self) -> u32 {
        pcm_rate(self.dsd_rate).unwrap_or(88_200)
    }

    /// DSD samples per PCM frame.
    fn decimation(&self) -> u64 {
        u64::from(self.dsd_rate / self.pcm_rate())
    }

    /// PCM frames the file converts to.
    pub fn frames(&self) -> u64 {
        self.samples / self.decimation()
    }

    pub fn duration_ms(&self) -> u64 {
        self.frames().saturating_mul(1_000) / u64::from(self.pcm_rate())
    }

    /// Channels in the file.
    pub fn channels(&self) -> usize {
        self.slots.len()
    }

    /// Bytes per channel that hold samples.
    fn valid_bytes(&self) -> u64 {
        self.samples.div_ceil(8)
    }

    /// Whether `other` can be converted as a continuation of this file.
    fn continues_into(&self, other: &DsdInfo) -> bool {
        self.dsd_rate == other.dsd_rate
            && self.slots == other.slots
            && self.out_channels == other.out_channels
    }

    /// Bitrate of the stream in kbit/s.
    pub fn bitrate_kbps(&self) -> u32 {
        let bits = u64::from(self.dsd_rate) * self.channels() as u64;
        u32::try_from(bits / 1_000).unwrap_or(u32::MAX)
    }
}

fn unsupported(detail: impl Into<String>) -> PlaybackError {
    PlaybackError {
        kind: PlaybackErrorKind::Unsupported,
        detail: detail.into(),
    }
}

fn corrupt(detail: impl Into<String>) -> PlaybackError {
    PlaybackError {
        kind: PlaybackErrorKind::Corrupt,
        detail: detail.into(),
    }
}

fn io(error: std::io::Error) -> PlaybackError {
    let kind = if error.kind() == std::io::ErrorKind::UnexpectedEof {
        PlaybackErrorKind::Corrupt
    } else {
        PlaybackErrorKind::FileUnreadable
    };
    PlaybackError {
        kind,
        detail: format!("dsd: {}", error.kind()),
    }
}

fn read_array<const N: usize, R: Read>(reader: &mut R) -> Result<[u8; N], PlaybackError> {
    let mut bytes = [0u8; N];
    reader.read_exact(&mut bytes).map_err(io)?;
    Ok(bytes)
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes[..4].try_into().unwrap_or_default())
}

fn le_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes[..8].try_into().unwrap_or_default())
}

/// Reads what a DSF or DSDIFF file is. Anything that does not add up is refused rather
/// than guessed at.
pub fn read_info<R: Read + Seek>(reader: &mut R) -> Result<DsdInfo, PlaybackError> {
    let file_len = reader.seek(SeekFrom::End(0)).map_err(io)?;
    reader.seek(SeekFrom::Start(0)).map_err(io)?;
    let magic: [u8; 4] = read_array(reader)?;
    match &magic {
        b"DSD " => read_dsf(reader, file_len),
        b"FRM8" => read_dsdiff(reader, file_len),
        _ => Err(unsupported("not a DSD file")),
    }
}

fn read_dsf<R: Read + Seek>(reader: &mut R, file_len: u64) -> Result<DsdInfo, PlaybackError> {
    // "DSD " chunk: size (28), file size, offset of the metadata.
    let head: [u8; 24] = read_array(reader)?;
    if le_u64(&head[0..]) != 28 {
        return Err(corrupt("dsf: DSD chunk size"));
    }
    let tag_at = le_u64(&head[16..]);
    // "fmt " chunk.
    let fmt: [u8; 52] = read_array(reader)?;
    if &fmt[0..4] != b"fmt " || le_u64(&fmt[4..]) != 52 {
        return Err(corrupt("dsf: fmt chunk"));
    }
    let format_id = le_u32(&fmt[16..]);
    let channel_type = le_u32(&fmt[20..]);
    let channels = le_u32(&fmt[24..]);
    let dsd_rate = le_u32(&fmt[28..]);
    let bits = le_u32(&fmt[32..]);
    let samples = le_u64(&fmt[36..]);
    let block = u64::from(le_u32(&fmt[44..]));
    if format_id != 0 {
        return Err(unsupported("dsf: not raw DSD"));
    }
    if pcm_rate(dsd_rate).is_none() {
        return Err(unsupported(format!("dsf: sample rate {dsd_rate}")));
    }
    if !matches!(bits, 1 | 8) {
        return Err(corrupt("dsf: bits per sample"));
    }
    if block == 0 || block > 1 << 20 {
        return Err(corrupt("dsf: block size"));
    }
    // Channel types 1–7: mono, stereo, 3 (L R C), quad (L R Ls Rs), 4 (L R C LFE),
    // 5 (L R C Ls Rs), 5.1 (L R C LFE Ls Rs).
    let (slots, out_channels): (Vec<usize>, usize) = match (channel_type, channels) {
        (1, 1) => (vec![0], 1),
        (2, 2) => (vec![0, 1], 2),
        (3, 3) => (vec![0, 1, 2], SURROUND),
        (4, 4) => (vec![0, 1, 4, 5], SURROUND),
        (5, 4) => (vec![0, 1, 2, 3], SURROUND),
        (6, 5) => (vec![0, 1, 2, 4, 5], SURROUND),
        (7, 6) => (vec![0, 1, 2, 3, 4, 5], SURROUND),
        _ => return Err(unsupported(format!("dsf: channel type {channel_type}"))),
    };
    // "data" chunk.
    let data: [u8; 12] = read_array(reader)?;
    if &data[0..4] != b"data" {
        return Err(corrupt("dsf: data chunk"));
    }
    let data_start = reader.stream_position().map_err(io)?;
    let data_len = le_u64(&data[4..])
        .checked_sub(12)
        .ok_or_else(|| corrupt("dsf: data size"))?
        .min(file_len.saturating_sub(data_start));
    // What the data holds, whatever the header claims.
    let blocks = data_len / (block * u64::from(channels));
    let samples = samples.min(blocks * block * 8);
    if samples == 0 {
        return Err(corrupt("dsf: no samples"));
    }
    let tag = (tag_at >= data_start && tag_at < file_len)
        .then(|| (tag_at, (file_len - tag_at).min(MAX_TAG_BYTES)));
    Ok(DsdInfo {
        container: Container::Dsf,
        dsd_rate,
        slots,
        out_channels,
        samples,
        data_start,
        block: Some(block),
        lsb_first: bits == 1,
        tag,
    })
}

/// A DSDIFF chunk header: its id, where its data starts and how long it is.
fn dff_chunk<R: Read + Seek>(reader: &mut R) -> Result<([u8; 4], u64, u64), PlaybackError> {
    let head: [u8; 12] = read_array(reader)?;
    let id = [head[0], head[1], head[2], head[3]];
    let len = u64::from_be_bytes(head[4..12].try_into().unwrap_or_default());
    let at = reader.stream_position().map_err(io)?;
    Ok((id, at, len))
}

fn read_dsdiff<R: Read + Seek>(reader: &mut R, file_len: u64) -> Result<DsdInfo, PlaybackError> {
    let size: [u8; 8] = read_array(reader)?;
    let form_end = (12 + u64::from_be_bytes(size)).min(file_len);
    let form: [u8; 4] = read_array(reader)?;
    if &form != b"DSD " {
        return Err(unsupported("dff: not a DSD form"));
    }
    let mut rate = None;
    let mut ids: Option<Vec<[u8; 4]>> = None;
    let mut data: Option<(u64, u64)> = None;
    let mut tag = None;
    let mut at = 16;
    for _ in 0..MAX_CHUNKS {
        if at + 12 > form_end {
            break;
        }
        reader.seek(SeekFrom::Start(at)).map_err(io)?;
        let (id, start, len) = dff_chunk(reader)?;
        let end = start
            .checked_add(len)
            .filter(|end| *end <= form_end)
            .unwrap_or(form_end);
        match &id {
            b"PROP" => {
                let kind: [u8; 4] = read_array(reader)?;
                if &kind != b"SND " {
                    return Err(corrupt("dff: PROP chunk"));
                }
                let mut sub = start + 4;
                for _ in 0..MAX_CHUNKS {
                    if sub + 12 > end {
                        break;
                    }
                    reader.seek(SeekFrom::Start(sub)).map_err(io)?;
                    let (sub_id, sub_start, sub_len) = dff_chunk(reader)?;
                    match &sub_id {
                        b"FS  " => {
                            rate = Some(u32::from_be_bytes(read_array(reader)?));
                        }
                        b"CHNL" => {
                            let count = u16::from_be_bytes(read_array(reader)?);
                            if count == 0 || count > 6 {
                                return Err(unsupported(format!("dff: {count} channels")));
                            }
                            let mut list = Vec::new();
                            for _ in 0..count {
                                list.push(read_array::<4, _>(reader)?);
                            }
                            ids = Some(list);
                        }
                        b"CMPR" => {
                            let kind: [u8; 4] = read_array(reader)?;
                            if &kind != b"DSD " {
                                return Err(unsupported("dff: DST-compressed DSD"));
                            }
                        }
                        _ => {}
                    }
                    sub = sub_start
                        .saturating_add(sub_len)
                        .saturating_add(sub_len & 1);
                }
            }
            b"DSD " => data = Some((start, end - start)),
            b"DST " => return Err(unsupported("dff: DST-compressed DSD")),
            b"ID3 " => tag = Some((start, (end - start).min(MAX_TAG_BYTES))),
            _ => {}
        }
        // Chunks are padded to an even length.
        at = start.saturating_add(len).saturating_add(len & 1);
    }
    let dsd_rate = rate.ok_or_else(|| corrupt("dff: no sample rate"))?;
    if pcm_rate(dsd_rate).is_none() {
        return Err(unsupported(format!("dff: sample rate {dsd_rate}")));
    }
    let ids = ids.ok_or_else(|| corrupt("dff: no channels"))?;
    let (data_start, data_len) = data.ok_or_else(|| corrupt("dff: no sound data"))?;
    // Stereo (SLFT SRGT), or the multichannel names (MLFT MRGT C LFE LS RS).
    let stereo = ids.len() == 2 && ids == [*b"SLFT", *b"SRGT"];
    let mono = ids.len() == 1;
    let (slots, out_channels) = if mono {
        (vec![0], 1)
    } else if stereo {
        (vec![0, 1], 2)
    } else {
        let mut slots = Vec::new();
        for id in &ids {
            slots.push(match id {
                b"MLFT" | b"SLFT" => 0,
                b"MRGT" | b"SRGT" => 1,
                b"C   " => 2,
                b"LFE " => 3,
                b"LS  " => 4,
                b"RS  " => 5,
                _ => return Err(unsupported("dff: channel layout")),
            });
        }
        let mut seen = slots.clone();
        seen.sort_unstable();
        seen.dedup();
        if seen.len() != slots.len() {
            return Err(corrupt("dff: a channel twice"));
        }
        if slots == [0, 1] {
            (slots, 2)
        } else {
            (slots, SURROUND)
        }
    };
    let samples = data_len / ids.len() as u64 * 8;
    if samples == 0 {
        return Err(corrupt("dff: no samples"));
    }
    Ok(DsdInfo {
        container: Container::Dsdiff,
        dsd_rate,
        slots,
        out_channels,
        samples,
        data_start,
        block: None,
        lsb_first: false,
        tag,
    })
}

/// The ID3v2 tag of a DSD file, if it has one.
pub fn read_tag<R: Read + Seek>(reader: &mut R, info: &DsdInfo) -> Option<Vec<u8>> {
    let (at, len) = info.tag?;
    reader.seek(SeekFrom::Start(at)).ok()?;
    let mut bytes = Vec::new();
    reader.take(len).read_to_end(&mut bytes).ok()?;
    bytes.starts_with(b"ID3").then_some(bytes)
}

/// Bessel function I0, for the Kaiser window.
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0, 1.0);
    let half = x / 2.0;
    for k in 1..200 {
        term *= half / k as f64;
        let add = term * term;
        sum += add;
        if add < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// A Kaiser-windowed sinc low-pass: `taps` coefficients, `cutoff` in cycles per sample,
/// unity gain at DC.
fn kaiser_lowpass(taps: usize, cutoff: f64, beta: f64) -> Vec<f64> {
    let middle = (taps - 1) as f64 / 2.0;
    let norm = bessel_i0(beta);
    let mut h: Vec<f64> = (0..taps)
        .map(|i| {
            let x = i as f64 - middle;
            let sinc = if x == 0.0 {
                2.0 * cutoff
            } else {
                (2.0 * PI * cutoff * x).sin() / (PI * x)
            };
            let r = x / middle;
            sinc * bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / norm
        })
        .collect();
    let sum: f64 = h.iter().sum();
    for tap in &mut h {
        *tap /= sum;
    }
    h
}

/// The filters for one DSD rate (shared between the channels and between decoders).
struct Filters {
    /// Bytes per first-stage output.
    k: usize,
    /// First stage: for each byte position (0 = newest), the sum of its eight taps for
    /// each value of the byte (bit 0 is the newest sample, 1 = +1, 0 = −1).
    tables: Vec<[f64; 256]>,
    /// Second stage.
    taps: Vec<f64>,
    /// What the first stage gives for DSD silence.
    silent: f64,
}

impl Filters {
    fn new(dsd_rate: u32) -> Self {
        let pcm = f64::from(pcm_rate(dsd_rate).unwrap_or(88_200));
        let decimation = (f64::from(dsd_rate) / pcm).round() as usize;
        // The first stage goes to 4 × the PCM rate.
        let k = decimation / 32;
        let bytes = STAGE1_BYTES_PER_64FS * k;
        let pass = 0.2268 * pcm;
        let stop = 4.0 * pcm - 0.4535 * pcm;
        let first = kaiser_lowpass(bytes * 8, (pass + stop) / 2.0 / f64::from(dsd_rate), 14.47);
        let tables = (0..bytes)
            .map(|position| {
                let mut table = [0.0; 256];
                for (value, sum) in table.iter_mut().enumerate() {
                    *sum = (0..8)
                        .map(|bit| {
                            let tap = first[position * 8 + bit];
                            if value >> bit & 1 == 1 { tap } else { -tap }
                        })
                        .sum();
                }
                table
            })
            .collect::<Vec<_>>();
        let silent = tables.iter().map(|t| t[usize::from(SILENCE)]).sum();
        let taps = kaiser_lowpass(STAGE2_TAPS, (0.2268 + 0.4535) / 2.0 / 4.0, 10.94);
        Filters {
            k,
            tables,
            taps,
            silent,
        }
    }
}

/// The conversion state of one channel. Histories are kept twice over, so that the
/// newest stretch is always one slice.
#[derive(Clone)]
struct Channel {
    bytes: Vec<u8>,
    byte_at: usize,
    since_output: usize,
    stage1: Vec<f64>,
    stage1_at: usize,
    phase: usize,
}

impl Channel {
    fn new(filters: &Filters) -> Self {
        let n = filters.tables.len();
        Channel {
            bytes: vec![SILENCE; 2 * n],
            byte_at: 0,
            since_output: 0,
            stage1: vec![filters.silent; 2 * STAGE2_TAPS],
            stage1_at: 0,
            phase: 0,
        }
    }

    /// Takes in bytes (first sample in the highest bit), adds PCM samples to `out`.
    fn push(&mut self, filters: &Filters, input: &[u8], out: &mut Vec<f32>) {
        let n = filters.tables.len();
        for &byte in input {
            // Newest at `byte_at`, stored at both `byte_at` and `byte_at + n`; the slice
            // `byte_at .. byte_at + n` read backwards is newest first.
            self.byte_at = if self.byte_at == 0 {
                n - 1
            } else {
                self.byte_at - 1
            };
            // The first sample is the highest bit, so the newest is bit 0: as the tables
            // are indexed.
            self.bytes[self.byte_at] = byte;
            self.bytes[self.byte_at + n] = byte;
            self.since_output += 1;
            if self.since_output < filters.k {
                continue;
            }
            self.since_output = 0;
            let window = &self.bytes[self.byte_at..self.byte_at + n];
            let y: f64 = window
                .iter()
                .zip(&filters.tables)
                .map(|(&b, table)| table[usize::from(b)])
                .sum();
            let m = STAGE2_TAPS;
            self.stage1_at = if self.stage1_at == 0 {
                m - 1
            } else {
                self.stage1_at - 1
            };
            self.stage1[self.stage1_at] = y;
            self.stage1[self.stage1_at + m] = y;
            self.phase += 1;
            if self.phase == 4 {
                self.phase = 0;
                let z: f64 = self.stage1[self.stage1_at..self.stage1_at + m]
                    .iter()
                    .zip(&filters.taps)
                    .map(|(s, t)| s * t)
                    .sum();
                out.push(z as f32);
            }
        }
    }
}

/// The conversion state of a whole stream.
#[derive(Clone)]
struct Converter {
    filters: Arc<Filters>,
    channels: Vec<Channel>,
}

impl Converter {
    fn new(filters: Arc<Filters>, channels: usize) -> Self {
        let channel = Channel::new(&filters);
        Converter {
            channels: vec![channel; channels],
            filters,
        }
    }
}

/// Plays one DSD file as PCM.
pub struct DsdDecoder {
    reader: Box<dyn ReadSeek>,
    info: DsdInfo,
    converter: Converter,
    /// Next byte (per channel) to be fed to the converter.
    position: u64,
    /// Outputs of the converter still to be dropped (filter warm-up).
    discard: u64,
    /// Next PCM frame to come out.
    emitted: u64,
    /// When the samples run out: stop and wait for [`DsdDecoder::finish`] instead of
    /// completing the last frames with silence.
    defer_tail: bool,
    /// The samples ran out while `defer_tail` was set; the last frames are held.
    holding: bool,
    channel_bytes: Vec<Vec<u8>>,
    channel_out: Vec<Vec<f32>>,
    raw: Vec<u8>,
}

/// What the decoder reads from.
pub trait ReadSeek: Read + Seek + Send + Sync {}
impl<T: Read + Seek + Send + Sync> ReadSeek for T {}

impl DsdDecoder {
    pub fn new(mut reader: Box<dyn ReadSeek>) -> Result<Self, PlaybackError> {
        let info = read_info(&mut reader)?;
        let filters = Arc::new(Filters::new(info.dsd_rate));
        let converter = Converter::new(filters, info.channels());
        let channels = info.channels();
        Ok(DsdDecoder {
            reader,
            info,
            converter,
            position: 0,
            discard: LATENCY,
            emitted: 0,
            defer_tail: false,
            holding: false,
            channel_bytes: vec![Vec::new(); channels],
            channel_out: vec![Vec::new(); channels],
            raw: Vec::new(),
        })
    }

    /// Opens a file that is loaded into memory.
    pub fn from_memory(source: MemorySource) -> Result<Self, PlaybackError> {
        Self::new(Box::new(source))
    }

    pub fn info(&self) -> &DsdInfo {
        &self.info
    }

    /// See the field of that name: set by playback, which asks for the last frames once
    /// it knows what follows.
    pub fn defer_tail(&mut self, defer: bool) {
        self.defer_tail = defer;
    }

    /// True once the samples have run out and the last frames are held.
    pub fn holds_tail(&self) -> bool {
        self.holding
    }

    /// Bytes per channel per PCM frame.
    fn bytes_per_frame(&self) -> u64 {
        self.info.decimation() / 8
    }

    /// Reads `count` bytes per channel from `from` on into `channel_bytes`, first sample
    /// in the highest bit. Past the samples: silence.
    fn read_bytes(&mut self, from: u64, count: usize) -> Result<(), PlaybackError> {
        let channels = self.info.channels();
        let valid = self.info.valid_bytes();
        let real = valid.saturating_sub(from).min(count as u64) as usize;
        for bytes in &mut self.channel_bytes {
            bytes.clear();
        }
        if real > 0 {
            match self.info.block {
                Some(block) => {
                    // DSF: block `b` of channel `c` is at data + (b × channels + c) × block.
                    let first = from / block;
                    let last = (from + real as u64 - 1) / block;
                    let span = (last - first + 1) * block * channels as u64;
                    let at = self.info.data_start + first * block * channels as u64;
                    self.raw.resize(span as usize, 0);
                    self.reader.seek(SeekFrom::Start(at)).map_err(io)?;
                    read_fully(&mut self.reader, &mut self.raw)?;
                    for (c, bytes) in self.channel_bytes.iter_mut().enumerate() {
                        for p in from..from + real as u64 {
                            let b = p / block - first;
                            let i = (b * channels as u64 + c as u64) * block + p % block;
                            bytes.push(self.raw[i as usize]);
                        }
                    }
                }
                None => {
                    // DSDIFF: one byte of each channel in turn.
                    let at = self.info.data_start + from * channels as u64;
                    self.raw.resize(real * channels, 0);
                    self.reader.seek(SeekFrom::Start(at)).map_err(io)?;
                    read_fully(&mut self.reader, &mut self.raw)?;
                    for (c, bytes) in self.channel_bytes.iter_mut().enumerate() {
                        bytes.extend(self.raw.iter().skip(c).step_by(channels));
                    }
                }
            }
            for bytes in &mut self.channel_bytes {
                if self.info.lsb_first {
                    for b in bytes.iter_mut() {
                        *b = b.reverse_bits();
                    }
                }
                // The bits of the last byte past the last sample are padding: silence.
                let used = (self.info.samples % 8) as u32;
                if used != 0
                    && from + real as u64 == valid
                    && let Some(last) = bytes.last_mut()
                {
                    let keep = !0u8 << (8 - used);
                    *last = (*last & keep) | (SILENCE & !keep);
                }
            }
        }
        for bytes in &mut self.channel_bytes {
            bytes.resize(count, SILENCE);
        }
        Ok(())
    }

    /// Feeds `count` bytes per channel from `from` and appends the PCM it gives, frames
    /// interleaved at the output channel count, to `out`: what the warm-up does not
    /// drop and no more than the file's frames.
    fn convert(
        &mut self,
        from: u64,
        count: usize,
        out: &mut Vec<f32>,
    ) -> Result<(), PlaybackError> {
        self.read_bytes(from, count)?;
        let filters = Arc::clone(&self.converter.filters);
        for ((channel, bytes), pcm) in self
            .converter
            .channels
            .iter_mut()
            .zip(&self.channel_bytes)
            .zip(&mut self.channel_out)
        {
            pcm.clear();
            channel.push(&filters, bytes, pcm);
        }
        let produced = self.channel_out[0].len();
        let width = self.info.out_channels;
        for i in 0..produced {
            if self.discard > 0 {
                self.discard -= 1;
                continue;
            }
            if self.emitted >= self.info.frames() {
                break;
            }
            let start = out.len();
            out.resize(start + width, 0.0);
            for (c, &slot) in self.info.slots.iter().enumerate() {
                out[start + slot] = self.channel_out[c][i];
            }
            self.emitted += 1;
        }
        Ok(())
    }

    /// The next stretch of PCM, interleaved. `Ok(false)` at the end (or, with the tail
    /// deferred, when only the held frames are left).
    pub fn next(&mut self, out: &mut Vec<f32>) -> Result<bool, PlaybackError> {
        out.clear();
        let total = self.info.frames();
        let valid = self.info.valid_bytes();
        while out.is_empty() {
            if self.emitted >= total || self.holding {
                return Ok(false);
            }
            if self.position < valid {
                let count = (valid - self.position).min(CHUNK_BYTES as u64) as usize;
                self.convert(self.position, count, out)?;
                self.position += count as u64;
            } else if self.defer_tail {
                self.holding = true;
                return Ok(false);
            } else {
                // The last frames, completed with silence after the file.
                let count = self.tail_bytes();
                self.convert(self.position, count, out)?;
                self.position += count as u64;
            }
        }
        Ok(true)
    }

    /// Bytes per channel still to be fed for the last frames to come out.
    fn tail_bytes(&self) -> usize {
        let needed = (self.info.frames() + LATENCY) * self.bytes_per_frame();
        // At least one frame's worth, so that the loop always moves on.
        needed
            .saturating_sub(self.position)
            .max(self.bytes_per_frame()) as usize
    }

    /// The frames held back at the end (see [`DsdDecoder::defer_tail`]), computed with
    /// the beginning of `following` when that file continues this one (the same rate and
    /// channels), with silence otherwise. `following`, which must not have been read
    /// from yet, then starts where this file leaves off, so that the two convert as one
    /// stream.
    pub fn finish(
        &mut self,
        following: Option<&mut DsdDecoder>,
        out: &mut Vec<f32>,
    ) -> Result<(), PlaybackError> {
        out.clear();
        if !self.holding {
            return Ok(());
        }
        self.holding = false;
        let count = self.tail_bytes();
        let following = following
            .filter(|f| self.info.continues_into(&f.info) && f.position == 0 && f.emitted == 0);
        match following {
            Some(next) => {
                // The next file starts from this one's history.
                next.converter = self.converter.clone();
                // This one ends with the next one's first bytes.
                next.read_bytes(0, count)?;
                let filters = Arc::clone(&self.converter.filters);
                for ((channel, bytes), pcm) in self
                    .converter
                    .channels
                    .iter_mut()
                    .zip(&next.channel_bytes)
                    .zip(&mut self.channel_out)
                {
                    pcm.clear();
                    channel.push(&filters, bytes, pcm);
                }
                let width = self.info.out_channels;
                let produced = self.channel_out[0].len();
                for i in 0..produced {
                    if self.emitted >= self.info.frames() {
                        break;
                    }
                    let start = out.len();
                    out.resize(start + width, 0.0);
                    for (c, &slot) in self.info.slots.iter().enumerate() {
                        out[start + slot] = self.channel_out[c][i];
                    }
                    self.emitted += 1;
                }
            }
            None => {
                let from = self.position;
                self.convert(from, count, out)?;
            }
        }
        self.position += count as u64;
        Ok(())
    }

    /// Moves to PCM frame `frame`: the filters start a little before it, so that what
    /// comes out from there is what plain playback gives.
    pub fn seek(&mut self, frame: u64) -> Result<(), PlaybackError> {
        let frame = frame.min(self.info.frames());
        // Outputs are exact once the filters have been fed this many frames' worth.
        const WARM_UP: u64 = 40;
        let wanted = frame + LATENCY;
        let start = wanted.saturating_sub(WARM_UP);
        self.converter = Converter::new(Arc::clone(&self.converter.filters), self.info.channels());
        self.position = start * self.bytes_per_frame();
        self.discard = wanted - start;
        self.emitted = frame;
        self.holding = false;
        Ok(())
    }
}

fn read_fully<R: Read + ?Sized>(reader: &mut R, buf: &mut [u8]) -> Result<(), PlaybackError> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(io(e)),
        }
    }
    // A file cut short reads as silence where it ends.
    for b in &mut buf[filled..] {
        *b = SILENCE;
    }
    Ok(())
}

/// Making DSD: a modulator and file writers, for tests and test files.
pub mod encode {
    use super::SILENCE;

    /// A second-order sigma-delta modulator in error-feedback form: the output is the
    /// input plus the quantization error shaped by (1 − z⁻¹)², so the noise is pushed
    /// above the audio band. Real DSD encoders use a higher order (less noise up to
    /// 40 kHz, more far above); for tests this one is exact and stable for input in
    /// −0.5…0.5 (the SACD reference level is 0.5). Bits come out first sample in the
    /// highest bit.
    pub struct Modulator {
        errors: [f64; 2],
        bits: Vec<u8>,
        current: u8,
        count: u8,
    }

    impl Default for Modulator {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Modulator {
        pub fn new() -> Self {
            Modulator {
                errors: [0.0; 2],
                bits: Vec::new(),
                current: 0,
                count: 0,
            }
        }

        pub fn push(&mut self, x: f64) {
            let [e1, e2] = self.errors;
            let v = x - 2.0 * e1 + e2;
            let y = if v >= 0.0 { 1.0 } else { -1.0 };
            self.errors = [y - v, e1];
            self.current = self.current << 1 | u8::from(y > 0.0);
            self.count += 1;
            if self.count == 8 {
                self.bits.push(self.current);
                self.current = 0;
                self.count = 0;
            }
        }

        /// The bytes so far; a partial byte is completed with silence.
        pub fn finish(mut self) -> Vec<u8> {
            if self.count > 0 {
                let shift = 8 - self.count;
                let keep = !0u8 << shift;
                self.bits
                    .push((self.current << shift) & keep | (SILENCE & !keep));
            }
            self.bits
        }
    }

    /// A DSF file of `channels` (1 = mono, 2 = stereo, 6 = 5.1) whose bits are
    /// `per_channel[c]` (first sample in the highest bit), with `samples` samples each
    /// and an optional ID3v2 tag.
    pub fn dsf(
        dsd_rate: u32,
        per_channel: &[Vec<u8>],
        samples: u64,
        tag: Option<&[u8]>,
    ) -> Vec<u8> {
        const BLOCK: usize = 4096;
        let channels = per_channel.len();
        let blocks = per_channel[0].len().div_ceil(BLOCK);
        let data_len = blocks * BLOCK * channels;
        let mut data = Vec::with_capacity(data_len);
        for b in 0..blocks {
            for bytes in per_channel {
                for i in 0..BLOCK {
                    // Least significant bit first; the padding after the samples is zero.
                    data.push(bytes.get(b * BLOCK + i).map_or(0, |v| v.reverse_bits()));
                }
            }
        }
        let header = 28 + 52 + 12;
        let tag_at = if tag.is_some() { header + data_len } else { 0 };
        let total = header + data_len + tag.map_or(0, <[u8]>::len);
        let channel_type: u32 = match channels {
            1 => 1,
            2 => 2,
            _ => 7,
        };
        let mut out = Vec::with_capacity(total);
        out.extend(b"DSD ");
        out.extend(28u64.to_le_bytes());
        out.extend((total as u64).to_le_bytes());
        out.extend((tag_at as u64).to_le_bytes());
        out.extend(b"fmt ");
        out.extend(52u64.to_le_bytes());
        out.extend(1u32.to_le_bytes());
        out.extend(0u32.to_le_bytes());
        out.extend(channel_type.to_le_bytes());
        out.extend((channels as u32).to_le_bytes());
        out.extend(dsd_rate.to_le_bytes());
        out.extend(1u32.to_le_bytes());
        out.extend(samples.to_le_bytes());
        out.extend((BLOCK as u32).to_le_bytes());
        out.extend(0u32.to_le_bytes());
        out.extend(b"data");
        out.extend(((12 + data_len) as u64).to_le_bytes());
        out.extend(data);
        if let Some(tag) = tag {
            out.extend(tag);
        }
        out
    }

    /// A DSDIFF file of the same, stereo or 5.1 (`MLFT MRGT C LFE LS RS`).
    pub fn dff(dsd_rate: u32, per_channel: &[Vec<u8>], tag: Option<&[u8]>) -> Vec<u8> {
        let channels = per_channel.len();
        let names: &[&[u8; 4]] = match channels {
            1 => &[b"SLFT"],
            2 => &[b"SLFT", b"SRGT"],
            _ => &[b"MLFT", b"MRGT", b"C   ", b"LFE ", b"LS  ", b"RS  "],
        };
        let chunk = |id: &[u8; 4], body: &[u8]| {
            let mut c = Vec::with_capacity(12 + body.len() + 1);
            c.extend(id);
            c.extend((body.len() as u64).to_be_bytes());
            c.extend(body);
            if body.len() % 2 == 1 {
                c.push(0);
            }
            c
        };
        let mut prop = b"SND ".to_vec();
        prop.extend(chunk(b"FS  ", &dsd_rate.to_be_bytes()));
        let mut chnl = (channels as u16).to_be_bytes().to_vec();
        for name in names {
            chnl.extend(*name);
        }
        prop.extend(chunk(b"CHNL", &chnl));
        let mut cmpr = b"DSD ".to_vec();
        cmpr.push(14);
        cmpr.extend(b"not compressed");
        prop.extend(chunk(b"CMPR", &cmpr));
        let len = per_channel[0].len();
        let mut sound = Vec::with_capacity(len * channels);
        for i in 0..len {
            for bytes in per_channel {
                sound.push(bytes[i]);
            }
        }
        let mut body = b"DSD ".to_vec();
        body.extend(chunk(b"FVER", &0x0105_0000u32.to_be_bytes()));
        body.extend(chunk(b"PROP", &prop));
        body.extend(chunk(b"DSD ", &sound));
        if let Some(tag) = tag {
            body.extend(chunk(b"ID3 ", tag));
        }
        let mut out = b"FRM8".to_vec();
        out.extend((body.len() as u64).to_be_bytes());
        out.extend(body);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::encode::{Modulator, dff, dsf};
    use super::*;
    use std::io::Cursor;

    const DSD64: u32 = 2_822_400;

    /// The bits of `seconds` of a sine (`hz`, `amplitude`) at `rate`, and the number of
    /// samples.
    fn sine_bits(rate: u32, hz: f64, amplitude: f64, samples: u64) -> Vec<u8> {
        let mut m = Modulator::new();
        for n in 0..samples {
            m.push(amplitude * (2.0 * PI * hz * n as f64 / f64::from(rate)).sin());
        }
        m.finish()
    }

    fn decode_all(bytes: Vec<u8>) -> (DsdInfo, Vec<f32>) {
        let mut d = DsdDecoder::new(Box::new(Cursor::new(bytes))).expect("opens");
        let mut all = Vec::new();
        let mut chunk = Vec::new();
        while d.next(&mut chunk).expect("decodes") {
            all.extend_from_slice(&chunk);
        }
        (d.info().clone(), all)
    }

    /// Least-squares fit of a sine of known frequency to one channel: amplitude and
    /// phase (radians), and the RMS of what is left.
    fn fit(samples: &[f32], rate: u32, hz: f64, skip: usize) -> (f64, f64, f64) {
        let w = 2.0 * PI * hz / f64::from(rate);
        let part = &samples[skip..samples.len() - skip];
        let (mut ss, mut sc, mut cc, mut ys, mut yc) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for (i, &y) in part.iter().enumerate() {
            let n = (i + skip) as f64;
            let (s, c) = (w * n).sin_cos();
            let y = f64::from(y);
            ss += s * s;
            sc += s * c;
            cc += c * c;
            ys += y * s;
            yc += y * c;
        }
        let det = ss * cc - sc * sc;
        let a = (ys * cc - yc * sc) / det;
        let b = (yc * ss - ys * sc) / det;
        let residual = part
            .iter()
            .enumerate()
            .map(|(i, &y)| {
                let n = (i + skip) as f64;
                let e = f64::from(y) - (a * (w * n).sin() + b * (w * n).cos());
                e * e
            })
            .sum::<f64>()
            / part.len() as f64;
        (a.hypot(b), b.atan2(a), residual.sqrt())
    }

    #[test]
    fn rates_and_their_pcm() {
        assert_eq!(pcm_rate(2_822_400), Some(88_200));
        assert_eq!(pcm_rate(5_644_800), Some(88_200));
        assert_eq!(pcm_rate(11_289_600), Some(88_200));
        assert_eq!(pcm_rate(22_579_200), Some(88_200));
        assert_eq!(pcm_rate(45_158_400), Some(88_200));
        assert_eq!(pcm_rate(3_072_000), Some(96_000));
        assert_eq!(pcm_rate(6_144_000), Some(96_000));
        for odd in [0, 44_100, 1_411_200, 2_822_401, 90_316_800, u32::MAX] {
            assert_eq!(pcm_rate(odd), None, "{odd}");
        }
        assert_eq!(rate_name(2_822_400), "DSD64");
        assert_eq!(rate_name(11_289_600), "DSD256");
        assert_eq!(rate_name(3_072_000), "DSD64");
    }

    /// The filters' delay is what [`LATENCY`] says, to within one DSD sample, at every
    /// rate.
    #[test]
    fn the_latency_is_the_filters_delay() {
        for multiple in [32u32, 64, 128, 256, 512] {
            let rate = 88_200 * multiple;
            let f = Filters::new(rate);
            let n1 = (f.tables.len() * 8) as f64;
            let m1 = (8 * f.k) as f64;
            let n2 = STAGE2_TAPS as f64;
            // Output j is centred on DSD sample D·j + offset.
            let offset = (4.0 - (n2 - 1.0) / 2.0) * m1 - 1.0 - (n1 - 1.0) / 2.0;
            let d = f64::from(multiple);
            assert!(
                (-offset - LATENCY as f64 * d).abs() <= 1.0,
                "{multiple}: {offset}"
            );
        }
    }

    /// Tones come out at their level, in time, with only the modulator's noise beside
    /// them, at DSD64 and DSD128; the length is the file's to the frame.
    #[test]
    fn tones_come_out_at_their_level_and_in_time() {
        for (rate, hz) in [
            (DSD64, 1_000.0),
            (DSD64, 10_000.0),
            (DSD64, 19_000.0),
            (2 * DSD64, 1_000.0),
            (2 * DSD64, 19_000.0),
        ] {
            let samples = u64::from(rate) / 4 + 13; // a quarter second, not byte-aligned
            let bits = sine_bits(rate, hz, 0.5, samples);
            let (info, pcm) = decode_all(dsf(rate, &[bits.clone(), bits], samples, None));
            assert_eq!(info.out_channels, 2);
            assert_eq!(pcm.len() as u64, 2 * info.frames());
            assert_eq!(info.frames(), samples / u64::from(rate / 88_200));
            let left: Vec<f32> = pcm.iter().step_by(2).copied().collect();
            let (amplitude, phase, rest) = fit(&left, 88_200, hz, 200);
            // The filters are flat to ±0.0001 dB (see the next test); what is left is
            // the test modulator's own distortion beside the tone.
            let db = 20.0 * (amplitude / 0.5).log10();
            assert!(db.abs() < 0.02, "{rate} {hz} Hz: {db} dB");
            // Phase 0 at frame 0: a time error of under a microsecond.
            let seconds = phase / (2.0 * PI * hz);
            assert!(seconds.abs() < 1e-6, "{rate} {hz} Hz: {seconds} s off");
            // Everything else, up to 44.1 kHz, more than 55 dB down: the harmonics and
            // noise of a second-order modulator at half scale.
            let rest_db = 20.0 * (rest / (amplitude / 2f64.sqrt())).log10();
            assert!(rest_db < -55.0, "{rate} {hz} Hz: rest at {rest_db} dB");
        }
    }

    /// DSF stores the first sample in the lowest bit, DSDIFF in the highest; the same
    /// stream in both is the same PCM. Mono and silence too.
    #[test]
    fn dsf_and_dsdiff_are_the_same_stream() {
        let samples = 300_000;
        let left = sine_bits(DSD64, 440.0, 0.4, samples);
        let right = sine_bits(DSD64, 3_000.0, 0.3, samples);
        let right_alone = right.clone();
        let (a_info, a) = decode_all(dsf(DSD64, &[left.clone(), right.clone()], samples, None));
        let (b_info, b) = decode_all(dff(DSD64, &[left.clone(), right], None));
        assert_eq!(a_info.container, Container::Dsf);
        assert_eq!(b_info.container, Container::Dsdiff);
        assert_eq!(a, b);
        // The channels are where they belong, and independent: each is what its
        // stream gives alone.
        let l: Vec<f32> = a.iter().step_by(2).copied().collect();
        let r: Vec<f32> = a.iter().skip(1).step_by(2).copied().collect();
        assert!((fit(&l, 88_200, 440.0, 200).0 - 0.4).abs() < 1e-3);
        assert!((fit(&r, 88_200, 3_000.0, 200).0 - 0.3).abs() < 1e-3);
        let (mono, alone_l) = decode_all(dsf(DSD64, &[left], samples, None));
        assert_eq!(mono.out_channels, 1);
        assert_eq!(alone_l, l);
        let (_, alone_r) = decode_all(dff(DSD64, &[right_alone], None));
        assert_eq!(alone_r, r);
        let silence = vec![SILENCE; 40_000];
        let (_, quiet) = decode_all(dsf(DSD64, &[silence.clone(), silence], 320_000, None));
        let loudest = quiet.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!(loudest < 1e-5, "{loudest}");
    }

    /// 5.1 in either file: each channel lands in its place in the order the downmix
    /// expects (L R C LFE Ls Rs).
    #[test]
    fn surround_channels_land_in_their_places() {
        let samples = 200_000;
        let tones = [500.0, 900.0, 1_300.0, 70.0, 2_100.0, 2_900.0];
        let bits: Vec<Vec<u8>> = tones
            .iter()
            .map(|&hz| sine_bits(DSD64, hz, 0.3, samples))
            .collect();
        for file in [dsf(DSD64, &bits, samples, None), dff(DSD64, &bits, None)] {
            let (info, pcm) = decode_all(file);
            assert_eq!(info.out_channels, 6);
            for (slot, &hz) in tones.iter().enumerate() {
                let channel: Vec<f32> = pcm.iter().skip(slot).step_by(6).copied().collect();
                assert!(
                    (fit(&channel, 88_200, hz, 300).0 - 0.3).abs() < 2e-3,
                    "{slot}"
                );
            }
        }
    }

    /// After a seek the samples are those plain playback gives at that frame.
    #[test]
    fn a_seek_lands_on_the_same_samples() {
        let samples = 400_000;
        let bits = sine_bits(DSD64, 1_234.0, 0.45, samples);
        let file = dsf(DSD64, &[bits.clone(), bits], samples, None);
        let (info, all) = decode_all(file.clone());
        for frame in [
            0u64,
            1,
            15,
            16,
            39,
            40,
            41,
            777,
            info.frames() - 20,
            info.frames() - 1,
        ] {
            let mut d = DsdDecoder::new(Box::new(Cursor::new(file.clone()))).unwrap();
            d.seek(frame).unwrap();
            let mut chunk = Vec::new();
            let mut got = Vec::new();
            while got.len() < 400 && d.next(&mut chunk).unwrap() {
                got.extend_from_slice(&chunk);
            }
            let want = &all[(frame as usize * 2)..];
            let n = got.len().min(400);
            assert_eq!(n, want.len().min(400), "{frame}");
            for i in 0..n {
                assert!((got[i] - want[i]).abs() < 1e-6, "{frame}: {i}");
            }
        }
        // Past the end: nothing more.
        let mut d = DsdDecoder::new(Box::new(Cursor::new(file))).unwrap();
        d.seek(info.frames() + 1_000).unwrap();
        assert!(!d.next(&mut Vec::new()).unwrap());
    }

    /// Two files cut from one stream play as that stream: the last frames of the first
    /// are computed with the first bits of the second, which starts from the history of
    /// the first. Without a follower, the end is what plain decoding gives.
    #[test]
    fn two_files_from_one_stream_join_without_a_seam() {
        let samples_a: u64 = 32 * 7_000; // a whole number of frames
        let samples_b: u64 = 260_000;
        let whole = sine_bits(DSD64, 3_000.0, 0.5, samples_a + samples_b);
        let cut = (samples_a / 8) as usize;
        let a_bits = whole[..cut].to_vec();
        let b_bits = whole[cut..].to_vec();
        let both = dsf(
            DSD64,
            &[whole.clone(), whole.clone()],
            samples_a + samples_b,
            None,
        );
        let (_, reference) = decode_all(both);

        let mut a = DsdDecoder::new(Box::new(Cursor::new(dsf(
            DSD64,
            &[a_bits.clone(), a_bits.clone()],
            samples_a,
            None,
        ))))
        .unwrap();
        let mut b = DsdDecoder::new(Box::new(Cursor::new(dsf(
            DSD64,
            &[b_bits.clone(), b_bits],
            samples_b,
            None,
        ))))
        .unwrap();
        a.defer_tail(true);
        let mut joined = Vec::new();
        let mut chunk = Vec::new();
        while a.next(&mut chunk).unwrap() {
            joined.extend_from_slice(&chunk);
        }
        assert!(a.holds_tail());
        a.finish(Some(&mut b), &mut chunk).unwrap();
        joined.extend_from_slice(&chunk);
        assert_eq!(joined.len() as u64, 2 * a.info().frames());
        while b.next(&mut chunk).unwrap() {
            joined.extend_from_slice(&chunk);
        }
        assert_eq!(joined.len(), reference.len());
        let worst = joined
            .iter()
            .zip(&reference)
            .map(|(x, y)| (x - y).abs())
            .fold(0f32, f32::max);
        assert!(worst < 1e-6, "{worst}");

        // Deferred and finished without a follower: the same as not deferring.
        let file = dsf(DSD64, &[a_bits.clone(), a_bits], samples_a, None);
        let (_, plain) = decode_all(file.clone());
        let mut d = DsdDecoder::new(Box::new(Cursor::new(file))).unwrap();
        d.defer_tail(true);
        let mut alone = Vec::new();
        while d.next(&mut chunk).unwrap() {
            alone.extend_from_slice(&chunk);
        }
        d.finish(None, &mut chunk).unwrap();
        alone.extend_from_slice(&chunk);
        assert_eq!(alone, plain);
    }

    /// The two stages together, from their coefficients: flat to 20 kHz, down from
    /// 40 kHz, and nothing that would fold into 0–40 kHz let through the first stage.
    #[test]
    fn the_filters_are_flat_and_keep_the_noise_out() {
        // Gain of an FIR at `hz` for a sample rate `rate`.
        fn gain(taps: &[f64], rate: f64, hz: f64) -> f64 {
            let w = 2.0 * PI * hz / rate;
            let (re, im) = taps
                .iter()
                .enumerate()
                .fold((0.0, 0.0), |(re, im), (n, t)| {
                    (re + t * (w * n as f64).cos(), im - t * (w * n as f64).sin())
                });
            20.0 * re.hypot(im).log10()
        }
        for multiple in [32u32, 64, 256] {
            let dsd = f64::from(88_200 * multiple);
            let f = Filters::new(88_200 * multiple);
            // The first stage's taps, back from the tables: bit `b` of position `p` is
            // half the difference between the byte with it set and with it clear.
            let first: Vec<f64> = (0..f.tables.len() * 8)
                .map(|t| (f.tables[t / 8][1 << (t % 8)] - f.tables[t / 8][0]) / 2.0)
                .collect();
            let r1 = 352_800.0;
            for hz in [10.0, 1_000.0, 10_000.0, 15_000.0, 20_000.0] {
                let total = gain(&first, dsd, hz) + gain(&f.taps, r1, hz);
                assert!(total.abs() < 1e-3, "{multiple}: {hz} Hz at {total} dB");
            }
            assert!((gain(&f.taps, r1, 30_000.0) + 6.0).abs() < 0.5);
            for hz in [40_000.0, 42_000.0, 44_100.0, 60_000.0, 100_000.0, 170_000.0] {
                let total = gain(&first, dsd, hz) + gain(&f.taps, r1, hz);
                assert!(total < -100.0, "{multiple}: {hz} Hz at {total} dB");
            }
            // Around each multiple of 352.8 kHz the first stage must stop what would
            // land within 40 kHz of zero.
            for k in 1..(multiple / 8) {
                let centre = f64::from(k) * r1;
                for off in [-40_000.0, -20_000.0, 0.0, 20_000.0, 40_000.0] {
                    let g = gain(&first, dsd, centre + off);
                    assert!(g < -135.0, "{multiple}: {} Hz at {g} dB", centre + off);
                }
            }
        }
    }

    /// The tables and the ring buffers compute what a plain convolution of the ±1
    /// samples with the two stages gives.
    #[test]
    fn the_fast_path_is_a_plain_convolution() {
        let f = Arc::new(Filters::new(DSD64));
        let first: Vec<f64> = (0..f.tables.len() * 8)
            .map(|t| (f.tables[t / 8][1 << (t % 8)] - f.tables[t / 8][0]) / 2.0)
            .collect();
        let mut seed = 0x1234_5678_u64;
        let bytes: Vec<u8> = (0..4_000)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed >> 32) as u8
            })
            .collect();
        let mut channel = Channel::new(&f);
        let mut fast = Vec::new();
        channel.push(&f, &bytes, &mut fast);
        // The same by hand: silence before, ±1 per bit, first sample highest.
        let mut x: Vec<f64> = vec![0.0; f.tables.len() * 8 + 1_000];
        let pad = x.len();
        let silence_bits: Vec<f64> = (0..8)
            .map(|i| {
                if SILENCE >> (7 - i) & 1 == 1 {
                    1.0
                } else {
                    -1.0
                }
            })
            .collect();
        for (i, v) in x.iter_mut().enumerate() {
            *v = silence_bits[(i + 8 - pad % 8) % 8];
        }
        for b in &bytes {
            for i in 0..8 {
                x.push(if b >> (7 - i) & 1 == 1 { 1.0 } else { -1.0 });
            }
        }
        let stage1 =
            |end: usize| -> f64 { first.iter().enumerate().map(|(t, h)| h * x[end - t]).sum() };
        for (j, &got) in fast.iter().enumerate().skip(40) {
            // Output j: stage-1 outputs i = 4j+3 back to 4j+3-124, each ending at the
            // last sample of byte (i+1)·k − 1.
            let z: f64 = (0..STAGE2_TAPS)
                .map(|t| {
                    let i = 4 * j + 3 - t;
                    let end = pad + (i + 1) * 8 * f.k - 1;
                    f.taps[t] * stage1(end)
                })
                .sum();
            assert!((f64::from(got) - z).abs() < 1e-6, "{j}: {got} vs {z}");
        }
        assert_eq!(fast.len(), bytes.len() / 4);
    }

    /// Damage and what is not supported: refused with a reason, never a panic.
    #[test]
    fn what_cannot_be_played_is_refused() {
        let bits = sine_bits(DSD64, 1_000.0, 0.5, 80_000);
        let good = dsf(DSD64, &[bits.clone(), bits.clone()], 80_000, None);
        let open = |bytes: Vec<u8>| DsdDecoder::new(Box::new(Cursor::new(bytes))).map(|_| ());
        assert!(open(good.clone()).is_ok());
        // Rates that are not DSD.
        let mut odd = good.clone();
        odd[28 + 28..28 + 32].copy_from_slice(&44_100u32.to_le_bytes());
        assert_eq!(open(odd).unwrap_err().kind, PlaybackErrorKind::Unsupported);
        // A channel type that does not match the count.
        let mut layout = good.clone();
        layout[28 + 20..28 + 24].copy_from_slice(&7u32.to_le_bytes());
        assert_eq!(
            open(layout).unwrap_err().kind,
            PlaybackErrorKind::Unsupported
        );
        // Cut short anywhere: refused, or played as far as it goes.
        for cut in [0, 3, 4, 27, 40, 80, 91, 92, 93, 4_000, good.len() - 1] {
            if let Ok(mut d) = DsdDecoder::new(Box::new(Cursor::new(good[..cut].to_vec()))) {
                let mut chunk = Vec::new();
                while d.next(&mut chunk).unwrap_or(false) {
                    assert!(chunk.iter().all(|s| s.is_finite()));
                }
            }
        }
        // DST compression in DSDIFF.
        let mut dst = dff(DSD64, &[bits.clone(), bits.clone()], None);
        let at = dst.windows(4).position(|w| w == b"CMPR").unwrap() + 12;
        dst[at..at + 4].copy_from_slice(b"DST ");
        assert_eq!(open(dst).unwrap_err().kind, PlaybackErrorKind::Unsupported);
        // A sample count far beyond the data: only what is there plays.
        let mut long = good.clone();
        long[28 + 36..28 + 44].copy_from_slice(&u64::MAX.to_le_bytes());
        let (info, pcm) = decode_all(long);
        assert!(info.samples <= 2 * 4096 * 8 * 8 && !pcm.is_empty());
    }

    #[test]
    fn the_id3_tag_is_found_in_both_files() {
        let bits = sine_bits(DSD64, 1_000.0, 0.5, 8_000);
        let tag = b"ID3\x03\x00\x00\x00\x00\x00\x0aTIT2\x00\x00\x00\x00\x00".to_vec();
        for file in [
            dsf(DSD64, &[bits.clone(), bits.clone()], 8_000, Some(&tag)),
            dff(DSD64, &[bits.clone(), bits.clone()], Some(&tag)),
        ] {
            let mut cursor = Cursor::new(file);
            let info = read_info(&mut cursor).unwrap();
            assert_eq!(read_tag(&mut cursor, &info).as_deref(), Some(&tag[..]));
        }
        let mut cursor = Cursor::new(dsf(DSD64, &[bits.clone(), bits], 8_000, None));
        let info = read_info(&mut cursor).unwrap();
        assert_eq!(read_tag(&mut cursor, &info), None);
    }
}
