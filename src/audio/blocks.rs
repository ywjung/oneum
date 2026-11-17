//! WavPack and Monkey's Audio: two lossless formats the decoding library does not
//! read. Each is decoded by a crate of its own (`wavicle`, `ape-decoder`), a block of
//! the file at a time; what is here reads the blocks, finds the one a position is in,
//! and turns the samples into the floats the rest of the path works with.
//!
//! What is not played is refused when the file is opened, with a reason: WavPack's
//! lossy ("hybrid") mode, DSD in WavPack, more than two channels in WavPack, and
//! Monkey's Audio with floating point samples.
use super::decoder::SourceInfo;
use super::dsd::ReadSeek;
use super::{PlaybackError, PlaybackErrorKind};
use std::io::{Read, Seek, SeekFrom};

/// Damaged blocks of a WavPack file in a row after which it is given up (each is
/// played as silence of its length until then; a block is at most three seconds).
const MAX_DAMAGED_IN_A_ROW: u32 = 8;
/// The longest stretch of a WavPack file that may be missing between two blocks and
/// is played as silence: beyond it the file's own account of itself is not believed.
const MAX_MISSING_FRAMES: u64 = 1 << 20;

/// A decoder of this module, as [`super::decoder::TrackDecoder`] uses it.
pub(super) trait Blocks: Send + Sync {
    fn info(&self) -> &SourceInfo;
    /// The next block as interleaved floats into `out` (cleared first); `false` at the
    /// end of the file.
    fn next(&mut self, out: &mut Vec<f32>) -> Result<bool, PlaybackError>;
    /// So that the next frame given is `frame` of the file (its end, if beyond it).
    fn seek(&mut self, frame: u64) -> Result<(), PlaybackError>;
    /// Blocks that were damaged and played as silence.
    fn damaged(&self) -> u32;
}

/// Which of the two formats a file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// WavPack, with the place of its first block.
    WavPack(u64),
    Ape,
}

/// The format of what `reader` holds, by its first bytes (after the ID3v2 tags some
/// programs put in front); `None`: neither. Leaves the reader at its start.
pub(super) fn kind_of<R: Read + Seek>(reader: &mut R) -> Result<Option<Kind>, PlaybackError> {
    let start = audio_start(reader);
    let mut magic = [0u8; 4];
    let found = reader
        .seek(SeekFrom::Start(start))
        .and_then(|_| reader.read_exact(&mut magic))
        .is_ok();
    reader.rewind().map_err(|e| unreadable(&e))?;
    Ok(match &magic {
        b"wvpk" if found => Some(Kind::WavPack(start)),
        b"MAC " | b"MACF" if found => Some(Kind::Ape),
        _ => None,
    })
}

/// The decoder for a file of `kind`.
pub(super) fn open(
    kind: Kind,
    reader: Box<dyn ReadSeek>,
    in_memory: bool,
) -> Result<Box<dyn Blocks>, PlaybackError> {
    Ok(match kind {
        Kind::WavPack(start) => Box::new(WavPack::new(reader, start, in_memory)?),
        Kind::Ape => Box::new(Ape::new(reader, in_memory)?),
    })
}

/// Where the audio begins: after any ID3v2 tags at the start of the file.
fn audio_start<R: Read + Seek>(reader: &mut R) -> u64 {
    let mut start = 0u64;
    for _ in 0..4 {
        let mut header = [0u8; 10];
        if reader.seek(SeekFrom::Start(start)).is_err() || reader.read_exact(&mut header).is_err() {
            break;
        }
        let size = &header[6..10];
        if &header[..3] != b"ID3" || size.iter().any(|b| b & 0x80 != 0) {
            break;
        }
        let len = size.iter().fold(0u64, |n, b| n << 7 | u64::from(*b));
        let footer = if header[5] & 0x10 != 0 { 10 } else { 0 };
        start += 10 + len + footer;
    }
    start
}

fn refuse(detail: impl Into<String>) -> PlaybackError {
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

fn unreadable(e: &std::io::Error) -> PlaybackError {
    PlaybackError {
        kind: PlaybackErrorKind::FileUnreadable,
        detail: format!("read failed: {}", e.kind()),
    }
}

/// Reads as much of `buf` as the file has from `at` on: the number of bytes read.
fn read_at(reader: &mut Box<dyn ReadSeek>, at: u64, buf: &mut [u8]) -> std::io::Result<usize> {
    reader.seek(SeekFrom::Start(at))?;
    let mut read = 0;
    while read < buf.len() {
        match reader.read(&mut buf[read..]) {
            Ok(0) => break,
            Ok(n) => read += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(read)
}

// ---------------------------------------------------------------------------------
// WavPack

use wavicle::block::HEADER_LEN;
use wavicle::{BlockHeader, Error as WvError, Scope};

/// How far ahead a lost place in the file is searched for the next block: a block is
/// at most a megabyte, so one begins within that.
const RESYNC_WINDOW: u64 = (1 << 20) + HEADER_LEN as u64;
/// Bytes read at a time while searching.
const RESYNC_STEP: usize = 64 * 1024;
/// Below this distance a position is found by walking from block to block.
const WALK_WITHIN: u64 = 2 << 20;

/// What every audio block of one file has in common.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WvFormat {
    channels: usize,
    rate: u32,
    bits: u32,
    float: bool,
}

pub(super) struct WavPack {
    reader: Box<dyn ReadSeek>,
    info: SourceInfo,
    format: WvFormat,
    /// Where the first block is, and the length of the file.
    first_block: u64,
    file_len: u64,
    /// The block read next: where it is and the frame it begins with.
    next_block: u64,
    next_frame: u64,
    /// Frames of the next block that come before the frame sought.
    skip: u64,
    /// Blocks already met: (first frame, place in the file), in order.
    met: Vec<(u64, u64)>,
    block: Vec<u8>,
    damaged: u32,
    damaged_in_a_row: u32,
    ended: bool,
}

fn wv_refusal(e: &WvError) -> PlaybackError {
    match e {
        WvError::OutOfScope(Scope::Dsd) => refuse("WavPack holding DSD"),
        WvError::OutOfScope(Scope::Hybrid | Scope::CorrectionFile) => {
            refuse("WavPack in its lossy (hybrid) mode")
        }
        WvError::OutOfScope(Scope::MoreThanTwoChannels | Scope::MultichannelSpanning) => {
            refuse("WavPack with more than two channels")
        }
        WvError::UnsupportedVersion(v) => refuse(format!("WavPack stream version {v:#x}")),
        WvError::NotYetImplemented(what) => refuse(format!("WavPack: {what}")),
        other => corrupt(format!("WavPack: {other}")),
    }
}

impl WavPack {
    fn new(
        mut reader: Box<dyn ReadSeek>,
        first_block: u64,
        in_memory: bool,
    ) -> Result<Self, PlaybackError> {
        let file_len = reader.seek(SeekFrom::End(0)).map_err(|e| unreadable(&e))?;
        // The first block that holds audio says what the file is.
        let mut at = first_block;
        let mut block = Vec::new();
        let (header, total) = loop {
            let header = read_block(&mut reader, at, &mut block)
                .map_err(|e| unreadable(&e))?
                .ok_or_else(|| corrupt("WavPack: no audio block"))?
                .map_err(|e| wv_refusal(&e))?;
            if header.block_samples > 0 {
                break (header, header.total_samples);
            }
            at += header.block_len() as u64;
        };
        if !(header.flags.initial_block() && header.flags.final_block()) {
            return Err(refuse("WavPack with more than two channels"));
        }
        // The crate's own look at the block: its scope (lossy, DSD) and the sample
        // rate, which a block may carry apart from its header.
        let scanned = wavicle::StreamInfo::scan(&block).map_err(|e| wv_refusal(&e))?;
        if !scanned.lossless {
            return Err(refuse("WavPack in its lossy (hybrid) mode"));
        }
        let format = WvFormat {
            channels: scanned.channels as usize,
            rate: scanned.sample_rate,
            bits: scanned.bits_per_sample,
            float: scanned.is_float,
        };
        if !(1..=2).contains(&format.channels) || !(4_000..=768_000).contains(&format.rate) {
            return Err(refuse("WavPack with an unusable format"));
        }
        let info = SourceInfo {
            sample_rate: format.rate,
            channels: format.channels,
            total_frames: total,
            codec: "wavpack".into(),
            has_gapless_info: true,
            in_memory,
            dsd_rate: None,
        };
        Ok(WavPack {
            reader,
            info,
            format,
            first_block: at,
            file_len,
            next_block: at,
            next_frame: header.block_index,
            skip: 0,
            met: vec![(header.block_index, at)],
            block,
            damaged: 0,
            damaged_in_a_row: 0,
            ended: false,
        })
    }

    /// The format of a block, as far as its header tells (the rate may be elsewhere).
    fn agrees(&self, header: &BlockHeader) -> bool {
        header.flags.output_channels() as usize == self.format.channels
            && header.flags.bytes_per_sample() * 8 == self.format.bits
            && header.flags.is_float() == self.format.float
            && header
                .flags
                .sample_rate()
                .is_none_or(|r| r == self.format.rate)
            && header.flags.initial_block()
            && header.flags.final_block()
            && self
                .info
                .total_frames
                .is_none_or(|total| header.block_index <= total)
    }

    /// The first block that begins at or after `from` and before `until`: its place
    /// and header. For a place in the file that is not the beginning of a block.
    fn sync(&mut self, from: u64, until: u64) -> std::io::Result<Option<(u64, BlockHeader)>> {
        let until = until
            .min(self.file_len)
            .min(from.saturating_add(RESYNC_WINDOW));
        let mut at = from;
        let mut piece = vec![0u8; RESYNC_STEP + HEADER_LEN];
        while at < until {
            let read = read_at(&mut self.reader, at, &mut piece)?;
            if read < HEADER_LEN {
                return Ok(None);
            }
            let mut i = 0;
            while i + HEADER_LEN <= read && at + (i as u64) < until {
                if &piece[i..i + 4] == b"wvpk"
                    && let Ok(header) = BlockHeader::parse(&piece[i..i + HEADER_LEN])
                    && self.agrees(&header)
                {
                    let place = at + i as u64;
                    // A block is followed by another, or by the end of the audio.
                    let after = place + header.block_len() as u64;
                    let mut next = [0u8; 4];
                    let followed = match read_at(&mut self.reader, after, &mut next)? {
                        4 => &next == b"wvpk" || &next == b"APET" || &next[..3] == b"TAG",
                        _ => after >= self.file_len,
                    };
                    if followed {
                        return Ok(Some((place, header)));
                    }
                }
                i += 1;
            }
            at += (read - HEADER_LEN + 1) as u64;
        }
        Ok(None)
    }

    /// The header of the block at `at`, if one begins there.
    fn header_at(&mut self, at: u64) -> std::io::Result<Option<BlockHeader>> {
        let mut bytes = [0u8; HEADER_LEN];
        if read_at(&mut self.reader, at, &mut bytes)? < HEADER_LEN {
            return Ok(None);
        }
        Ok(BlockHeader::parse(&bytes).ok())
    }

    /// The block `frame` is in: its place and first frame. `None`: the file ends
    /// before it.
    fn find(&mut self, frame: u64) -> std::io::Result<Option<(u64, u64)>> {
        // Between the nearest blocks already met…
        let after = self.met.partition_point(|(first, _)| *first <= frame);
        let (mut lo_frame, mut lo) = self.met[after.saturating_sub(1)];
        if lo_frame > frame {
            (lo_frame, lo) = (0, self.first_block);
        }
        let (mut hi_frame, mut hi) = match self.met.get(after) {
            Some((first, place)) => (*first, *place),
            None => (self.info.total_frames.unwrap_or(u64::MAX), self.file_len),
        };
        // …the place is guessed from the position, and the block found there narrows
        // the stretch, until it is short enough to walk.
        for _ in 0..48 {
            if hi - lo <= WALK_WITHIN || hi_frame == u64::MAX || hi_frame <= lo_frame {
                break;
            }
            let share = (frame - lo_frame) as f64 / (hi_frame - lo_frame) as f64;
            let guess = lo + ((hi - lo) as f64 * share) as u64;
            // A little before the guess: the block wanted begins before its frame.
            // (A guess that close to the block already known: the rest is walked.)
            let from = guess.saturating_sub(WALK_WITHIN / 4);
            if from <= lo {
                break;
            }
            match self.sync(from, hi)? {
                Some((place, header)) if header.block_index <= frame => {
                    (lo_frame, lo) = (header.block_index, place);
                }
                Some((place, header)) if place < hi => (hi_frame, hi) = (header.block_index, place),
                _ => (hi_frame, hi) = (hi_frame, from),
            }
        }
        // From block to block.
        let mut at = lo;
        loop {
            let Some(header) = self.header_at(at)? else {
                return Ok(None);
            };
            let frames = u64::from(header.block_samples);
            if frames > 0 && header.block_index + frames > frame {
                return Ok(Some((at, header.block_index)));
            }
            at += header.block_len() as u64;
        }
    }

    fn remember(&mut self, first_frame: u64, place: u64) {
        if self
            .met
            .last()
            .is_none_or(|(first, _)| *first < first_frame)
        {
            self.met.push((first_frame, place));
        }
    }

    /// Silence in place of `frames` frames that could not be decoded.
    fn silence(&mut self, frames: u64, out: &mut Vec<f32>) -> Result<(), PlaybackError> {
        self.damaged += 1;
        self.damaged_in_a_row += 1;
        if self.damaged_in_a_row > MAX_DAMAGED_IN_A_ROW {
            return Err(corrupt("WavPack: too many damaged blocks"));
        }
        let frames = frames.saturating_sub(std::mem::take(&mut self.skip));
        out.resize(frames as usize * self.format.channels, 0.0);
        Ok(())
    }
}

/// Reads the block at `at` into `block`: its header, or why it is none. `None`: the
/// file has no block there (its end, or the tags after the audio).
fn read_block(
    reader: &mut Box<dyn ReadSeek>,
    at: u64,
    block: &mut Vec<u8>,
) -> std::io::Result<Option<Result<BlockHeader, WvError>>> {
    block.resize(HEADER_LEN, 0);
    if read_at(reader, at, block)? < HEADER_LEN || &block[..4] != b"wvpk" {
        return Ok(None);
    }
    let header = match BlockHeader::parse(block) {
        Ok(header) => header,
        Err(e) => return Ok(Some(Err(e))),
    };
    let len = header.block_len();
    block.resize(len, 0);
    let read = read_at(reader, at + HEADER_LEN as u64, &mut block[HEADER_LEN..])?;
    if read < len - HEADER_LEN {
        return Ok(Some(Err(WvError::Truncated {
            need: len,
            have: HEADER_LEN + read,
        })));
    }
    Ok(Some(Ok(header)))
}

impl Blocks for WavPack {
    fn info(&self) -> &SourceInfo {
        &self.info
    }

    fn next(&mut self, out: &mut Vec<f32>) -> Result<bool, PlaybackError> {
        out.clear();
        loop {
            if self.ended
                || self
                    .info
                    .total_frames
                    .is_some_and(|total| self.next_frame >= total)
            {
                return Ok(false);
            }
            let at = self.next_block;
            let header = match read_block(&mut self.reader, at, &mut self.block)
                .map_err(|e| unreadable(&e))?
            {
                Some(Ok(header)) if self.agrees(&header) || header.block_samples == 0 => header,
                Some(Ok(_)) => return Err(corrupt("WavPack: the format changes within the file")),
                // The file ends here, or the place is not a block: the next one that is.
                found => {
                    let lost = found.is_some();
                    match self.sync(at + 1, u64::MAX).map_err(|e| unreadable(&e))? {
                        Some((place, header)) if lost && header.block_index >= self.next_frame => {
                            let gap = header.block_index - self.next_frame;
                            if gap > MAX_MISSING_FRAMES {
                                return Err(corrupt("WavPack: a long stretch is missing"));
                            }
                            self.next_block = place;
                            self.next_frame = header.block_index;
                            self.silence(gap, out)?;
                            if out.is_empty() {
                                continue;
                            }
                            return Ok(true);
                        }
                        _ => {
                            self.ended = true;
                            return Ok(false);
                        }
                    }
                }
            };
            self.next_block = at + header.block_len() as u64;
            let frames = u64::from(header.block_samples);
            if frames == 0 {
                continue;
            }
            self.remember(header.block_index, at);
            self.next_frame = header.block_index + frames;
            match wavicle::decode_stream(&self.block) {
                Ok(decoded)
                    if decoded.samples.len() as u64 == frames * self.format.channels as u64 =>
                {
                    self.damaged_in_a_row = 0;
                    let skip = std::mem::take(&mut self.skip).min(frames) as usize;
                    let samples = &decoded.samples[skip * self.format.channels..];
                    if self.format.float {
                        out.extend(samples.iter().map(|s| f32::from_bits(*s as u32)));
                    } else {
                        let scale = 1.0 / (1u64 << (self.format.bits - 1)) as f64;
                        out.extend(samples.iter().map(|s| (f64::from(*s) * scale) as f32));
                    }
                }
                Ok(_) => self.silence(frames, out)?,
                Err(e @ (WvError::OutOfScope(_) | WvError::NotYetImplemented(_))) => {
                    return Err(wv_refusal(&e));
                }
                Err(_) => self.silence(frames, out)?,
            }
            // Not beyond the length the file states.
            if let Some(total) = self.info.total_frames
                && self.next_frame > total
            {
                let over = (self.next_frame - total) as usize * self.format.channels;
                out.truncate(out.len().saturating_sub(over));
            }
            if !out.is_empty() {
                return Ok(true);
            }
        }
    }

    fn seek(&mut self, frame: u64) -> Result<(), PlaybackError> {
        self.ended = false;
        self.damaged_in_a_row = 0;
        if self.info.total_frames.is_some_and(|total| frame >= total) {
            self.next_frame = frame;
            self.skip = 0;
            return Ok(());
        }
        match self.find(frame).map_err(|e| unreadable(&e))? {
            Some((place, first_frame)) => {
                self.next_block = place;
                self.next_frame = first_frame;
                self.skip = frame - first_frame;
            }
            None => self.ended = true,
        }
        Ok(())
    }

    fn damaged(&self) -> u32 {
        self.damaged
    }
}

// ---------------------------------------------------------------------------------
// Monkey's Audio

use ape_decoder::{ApeDecoder, ApeError};

pub(super) struct Ape {
    decoder: ApeDecoder<Box<dyn ReadSeek>>,
    info: SourceInfo,
    bytes_per_sample: usize,
    /// The frame of the file decoded next (a frame of this format is seconds long).
    frame: u32,
    /// Samples of that frame that come before the position sought.
    skip: usize,
}

fn ape_refusal(e: ApeError) -> PlaybackError {
    match e {
        ApeError::Io(e) => unreadable(&e),
        ApeError::UnsupportedVersion(v) => refuse(format!("Monkey's Audio version {v}")),
        other => corrupt(format!("Monkey's Audio: {other}")),
    }
}

impl Ape {
    fn new(reader: Box<dyn ReadSeek>, in_memory: bool) -> Result<Self, PlaybackError> {
        let decoder = ApeDecoder::new(reader).map_err(ape_refusal)?;
        let ape = decoder.info();
        if ape.is_floating_point {
            return Err(refuse("Monkey's Audio with floating point samples"));
        }
        let bytes_per_sample = usize::from(ape.bits_per_sample / 8);
        let channels = usize::from(ape.channels);
        if !(1..=4).contains(&bytes_per_sample)
            || ape.bits_per_sample % 8 != 0
            || !(1..=32).contains(&channels)
            || usize::from(ape.block_align) != bytes_per_sample * channels
            || !(4_000..=768_000).contains(&ape.sample_rate)
        {
            return Err(refuse("Monkey's Audio with an unusable format"));
        }
        let info = SourceInfo {
            sample_rate: ape.sample_rate,
            channels,
            total_frames: Some(ape.total_samples),
            codec: "ape".into(),
            has_gapless_info: true,
            in_memory,
            dsd_rate: None,
        };
        Ok(Ape {
            decoder,
            info,
            bytes_per_sample,
            frame: 0,
            skip: 0,
        })
    }
}

/// Samples as the decoder gives them (little-endian integers of `bytes` bytes, those of
/// one byte unsigned) as floats.
fn pcm_to_floats(pcm: &[u8], bytes: usize, out: &mut Vec<f32>) {
    match bytes {
        1 => out.extend(pcm.iter().map(|b| (f32::from(*b) - 128.0) / 128.0)),
        2 => out.extend(
            pcm.as_chunks::<2>()
                .0
                .iter()
                .map(|b| f32::from(i16::from_le_bytes(*b)) / 32_768.0),
        ),
        3 => out.extend(pcm.as_chunks::<3>().0.iter().map(|b| {
            let value = i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8;
            value as f32 / 8_388_608.0
        })),
        _ => out.extend(
            pcm.as_chunks::<4>()
                .0
                .iter()
                .map(|b| (f64::from(i32::from_le_bytes(*b)) / 2_147_483_648.0) as f32),
        ),
    }
}

impl Blocks for Ape {
    fn info(&self) -> &SourceInfo {
        &self.info
    }

    fn next(&mut self, out: &mut Vec<f32>) -> Result<bool, PlaybackError> {
        out.clear();
        while self.frame < self.decoder.total_frames() {
            let frame = self.frame;
            self.frame += 1;
            let samples = self.decoder.info().frame_samples(frame) as usize;
            let skip = std::mem::take(&mut self.skip).min(samples);
            let align = self.bytes_per_sample * self.info.channels;
            // A frame that does not decode to what its checksum says ends the song
            // there: a frame is seconds long (up to 27 of them), too long to pass
            // over in silence.
            match self.decoder.decode_frame(frame) {
                Ok(pcm) if pcm.len() == samples * align => {
                    pcm_to_floats(&pcm[skip * align..], self.bytes_per_sample, out);
                }
                Ok(_) => return Err(corrupt("Monkey's Audio: a frame of the wrong length")),
                Err(e) => return Err(ape_refusal(e)),
            }
            if !out.is_empty() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn seek(&mut self, frame: u64) -> Result<(), PlaybackError> {
        let total = self.info.total_frames.unwrap_or(0);
        if frame >= total {
            self.frame = self.decoder.total_frames();
            self.skip = 0;
            return Ok(());
        }
        let found = self.decoder.seek(frame).map_err(ape_refusal)?;
        self.frame = found.frame_index;
        self.skip = found.skip_samples as usize;
        Ok(())
    }

    fn damaged(&self) -> u32 {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The audio blocks of the one-second WavPack fixture, `times` over, as one file:
    /// the blocks renumbered and the length in the first. With the frames of one
    /// round, as the fixture decodes.
    fn long_wavpack(times: u64) -> (Vec<u8>, Vec<f32>) {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/gap_a.wv");
        let fixture = std::fs::read(path).expect("fixture");
        let mut blocks = Vec::new();
        let mut at = 0;
        while fixture[at..].starts_with(b"wvpk") {
            let header = BlockHeader::parse(&fixture[at..]).expect("a block");
            let len = header.block_len();
            if header.block_samples > 0 {
                blocks.push((
                    fixture[at..at + len].to_vec(),
                    u64::from(header.block_samples),
                ));
            }
            at += len;
        }
        let per_round: u64 = blocks.iter().map(|(_, frames)| frames).sum();
        assert_eq!(per_round, 44_100);
        let mut file = Vec::new();
        let mut frame = 0u64;
        for _ in 0..times {
            for (block, frames) in &blocks {
                let mut block = block.clone();
                block[10] = (frame >> 32) as u8;
                block[16..20].copy_from_slice(&(frame as u32).to_le_bytes());
                block[11] = 0;
                block[12..16].copy_from_slice(&((per_round * times) as u32).to_le_bytes());
                file.extend_from_slice(&block);
                frame += frames;
            }
        }
        // What comes after the audio in real files: a tag.
        file.extend_from_slice(b"APETAGEX and so on");
        let mut one = WavPack::new(Box::new(std::io::Cursor::new(fixture)), 0, true).unwrap();
        let (mut round, mut out) = (Vec::new(), Vec::new());
        while one.next(&mut out).unwrap() {
            round.extend_from_slice(&out);
        }
        (file, round)
    }

    /// A place in a long WavPack file is found without reading the file up to it:
    /// from a guess at where the frame lies, and the blocks found around the guess.
    #[test]
    fn a_frame_of_a_long_wavpack_file_is_found_exactly_and_cheaply() {
        /// Counts what is read of the file.
        struct Counted(
            std::io::Cursor<Vec<u8>>,
            std::sync::Arc<std::sync::atomic::AtomicU64>,
        );
        impl Read for Counted {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let n = self.0.read(buf)?;
                self.1
                    .fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
                Ok(n)
            }
        }
        impl Seek for Counted {
            fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
                self.0.seek(to)
            }
        }
        let times = 400;
        let (file, round) = long_wavpack(times);
        let len = file.len() as u64;
        assert!(
            len > 20 << 20,
            "long enough to need the search: {len} bytes"
        );
        let read = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let counted = Counted(std::io::Cursor::new(file), std::sync::Arc::clone(&read));
        let mut decoder = WavPack::new(Box::new(counted), 0, false).unwrap();
        assert_eq!(decoder.info().total_frames, Some(44_100 * times));
        let mut out = Vec::new();
        for frame in [
            44_100 * 250 + 12_345,
            17,
            44_100 * times - 1,
            44_100 * 399,
            44_100 * 100 + 22_049,
            44_100 * 100 + 22_050,
            0,
        ] {
            let before = read.load(std::sync::atomic::Ordering::Relaxed);
            decoder.seek(frame).unwrap();
            assert!(decoder.next(&mut out).unwrap(), "frame {frame}");
            let cost = read.load(std::sync::atomic::Ordering::Relaxed) - before;
            assert!(
                cost < 1 << 20,
                "frame {frame}: {cost} bytes read to find it in a file of {len}"
            );
            // The audio repeats every second: what is there is known.
            let at = (frame % 44_100) as usize * 2;
            let n = out.len().min(round.len() - at);
            assert!(n > 0);
            assert_eq!(out[..n], round[at..at + n], "from frame {frame}");
        }
        // Past the end: nothing, and a position before it plays again.
        decoder.seek(44_100 * times).unwrap();
        assert!(!decoder.next(&mut out).unwrap());
        decoder.seek(44_100 * times - 10).unwrap();
        assert!(decoder.next(&mut out).unwrap());
        assert_eq!(out.len(), 20);
        assert!(!decoder.next(&mut out).unwrap());
    }

    /// A block that does not decode is a stretch of silence as long as it was, and
    /// the file goes on after it; a file cut short ends where it was cut.
    #[test]
    fn a_damaged_wavpack_block_is_silence_and_the_song_goes_on() {
        let (mut file, round) = long_wavpack(3);
        let whole = 44_100 * 3 * 2;
        // Damage inside the second block of the file (after its header).
        let first = BlockHeader::parse(&file).unwrap();
        let second_at = first.block_len();
        let second = BlockHeader::parse(&file[second_at..]).unwrap();
        for byte in &mut file[second_at + 200..second_at + 260] {
            *byte ^= 0x5A;
        }
        let mut decoder =
            WavPack::new(Box::new(std::io::Cursor::new(file.clone())), 0, true).unwrap();
        let (mut all, mut out) = (Vec::new(), Vec::new());
        while decoder.next(&mut out).unwrap() {
            all.extend_from_slice(&out);
        }
        assert_eq!(all.len(), whole, "no frame more or less");
        assert_eq!(decoder.damaged(), 1);
        let from = first.block_samples as usize * 2;
        let to = from + second.block_samples as usize * 2;
        assert!(
            all[from..to].iter().all(|s| *s == 0.0),
            "silence for the block"
        );
        assert_eq!(all[..from], round[..from], "before it");
        assert_eq!(all[to..44_100 * 2], round[to..], "after it");

        // Cut in the middle of a block: what is whole plays, and the song ends.
        file.truncate(second_at + 100);
        let mut decoder = WavPack::new(Box::new(std::io::Cursor::new(file)), 0, true).unwrap();
        let mut frames = 0;
        while decoder.next(&mut out).unwrap() {
            frames += out.len() / 2;
        }
        assert_eq!(frames, first.block_samples as usize);
    }

    #[test]
    fn integers_of_every_width_become_floats() {
        let mut out = Vec::new();
        pcm_to_floats(&[0, 128, 255], 1, &mut out);
        assert_eq!(out, [-1.0, 0.0, 127.0 / 128.0]);
        out.clear();
        pcm_to_floats(&[0x00, 0x80, 0xFF, 0x7F], 2, &mut out);
        assert_eq!(out, [-1.0, 32_767.0 / 32_768.0]);
        out.clear();
        pcm_to_floats(
            &[0x00, 0x00, 0x80, 0xFF, 0xFF, 0x7F, 0xFF, 0xFF, 0xFF],
            3,
            &mut out,
        );
        assert_eq!(out, [-1.0, 8_388_607.0 / 8_388_608.0, -1.0 / 8_388_608.0]);
        out.clear();
        pcm_to_floats(&[0, 0, 0, 0x80, 0, 0, 0, 0x40], 4, &mut out);
        assert_eq!(out, [-1.0, 0.5]);
    }
}
