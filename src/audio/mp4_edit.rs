//! Which part of an MP4 audio track is the music.
//!
//! An AAC encoder puts a stretch of priming audio in front of the music and pads the end
//! to a whole packet. The file says how much to drop, in one of two ways: an edit list
//! (`edts/elst`, written by ffmpeg and most other tools), or Apple's `iTunSMPB` tag
//! (written by Music, iTunes and `afconvert`, which write no edit list). The MP4 reader
//! we decode with reads neither, so the gaps it would leave between the songs of an
//! album are closed here.
//!
//! The file is not trusted: every size is checked against what encloses it, and the
//! number of boxes looked at is bounded.
use std::io::{Read, Seek, SeekFrom};

/// The frames of the decoded stream that are the music: `valid` frames after the first
/// `delay`. In frames at the track's sample rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trim {
    pub delay: u64,
    pub valid: u64,
}

/// Boxes looked at before giving up on a file (a real one has a few dozen at the levels
/// that are walked).
const MAX_BOXES: u32 = 4_096;
/// Largest payload that is read into memory (the boxes of interest are tiny).
const MAX_PAYLOAD: u64 = 4_096;
/// Priming longer than this is not priming (AAC uses 1,024 to 2,112 frames).
const MAX_DELAY_SECONDS: u64 = 1;

struct Walker<R> {
    reader: R,
    budget: u32,
}

/// One box: its type and where its payload starts and ends.
#[derive(Clone, Copy)]
struct BoxAt {
    kind: [u8; 4],
    start: u64,
    end: u64,
}

impl<R: Read + Seek> Walker<R> {
    /// The boxes directly inside `start..end`.
    fn children(&mut self, start: u64, end: u64) -> Vec<BoxAt> {
        let mut found = Vec::new();
        let mut at = start;
        while at.saturating_add(8) <= end && self.budget > 0 {
            self.budget -= 1;
            let mut header = [0u8; 16];
            if self.reader.seek(SeekFrom::Start(at)).is_err()
                || self.reader.read_exact(&mut header[..8]).is_err()
            {
                break;
            }
            let short = u64::from(u32::from_be_bytes([
                header[0], header[1], header[2], header[3],
            ]));
            let kind = [header[4], header[5], header[6], header[7]];
            let (size, header_len) = match short {
                // The size follows as 64 bits.
                1 => {
                    if self.reader.read_exact(&mut header[8..16]).is_err() {
                        break;
                    }
                    let mut wide = [0u8; 8];
                    wide.copy_from_slice(&header[8..16]);
                    (u64::from_be_bytes(wide), 16)
                }
                // "To the end of what encloses it."
                0 => (end - at, 8),
                n => (n, 8),
            };
            if size < header_len || at.saturating_add(size) > end {
                break;
            }
            found.push(BoxAt {
                kind,
                start: at + header_len,
                end: at + size,
            });
            at += size;
        }
        found
    }

    fn child(&mut self, parent: BoxAt, kind: &[u8; 4]) -> Option<BoxAt> {
        self.children(parent.start, parent.end)
            .into_iter()
            .find(|b| &b.kind == kind)
    }

    /// The payload of a small box (`None` if it is larger than anything we look for).
    fn payload(&mut self, at: BoxAt) -> Option<Vec<u8>> {
        let len = at.end - at.start;
        if len > MAX_PAYLOAD {
            return None;
        }
        let mut bytes = vec![0u8; len as usize];
        self.reader.seek(SeekFrom::Start(at.start)).ok()?;
        self.reader.read_exact(&mut bytes).ok()?;
        Some(bytes)
    }
}

fn be32(bytes: &[u8], at: usize) -> Option<u64> {
    let b = bytes.get(at..at + 4)?;
    Some(u64::from(u32::from_be_bytes([b[0], b[1], b[2], b[3]])))
}

fn be64(bytes: &[u8], at: usize) -> Option<u64> {
    let b = bytes.get(at..at + 8)?;
    let mut wide = [0u8; 8];
    wide.copy_from_slice(b);
    Some(u64::from_be_bytes(wide))
}

/// Time scale and duration of an `mvhd` or `mdhd` box (they share their layout).
fn scale_and_duration(payload: &[u8]) -> Option<(u64, u64)> {
    match payload.first()? {
        0 => Some((be32(payload, 12)?, be32(payload, 16)?)),
        1 => Some((be32(payload, 20)?, be64(payload, 24)?)),
        _ => None,
    }
}

/// The first edit of an edit list that plays media: (duration in movie time, start in
/// media time). Leading "empty" edits (a start of −1: silence before the music) are
/// passed over.
fn first_edit(payload: &[u8]) -> Option<(u64, u64)> {
    let version = *payload.first()?;
    let count = be32(payload, 4)?.min(64) as usize;
    let entry = if version == 1 { 20 } else { 12 };
    for i in 0..count {
        let at = 8 + i * entry;
        let (duration, media_time) = match version {
            0 => (
                be32(payload, at)?,
                i64::from(be32(payload, at + 4)? as u32 as i32),
            ),
            1 => (be64(payload, at)?, be64(payload, at + 8)?.cast_signed()),
            _ => return None,
        };
        if media_time >= 0 {
            return Some((duration, media_time.cast_unsigned()));
        }
    }
    None
}

/// Apple's gapless note: " 00000000 00000840 00000317 000000000000D4A9 …" — a zero, the
/// priming, the padding and the number of frames of music, in hexadecimal.
fn itunsmpb(text: &str) -> Option<Trim> {
    let mut fields = text.split_whitespace();
    let _zero = fields.next()?;
    let delay = u64::from_str_radix(fields.next()?, 16).ok()?;
    let _padding = u64::from_str_radix(fields.next()?, 16).ok()?;
    let valid = u64::from_str_radix(fields.next()?, 16).ok()?;
    Some(Trim { delay, valid })
}

/// The `iTunSMPB` note among the tags (`moov/udta/meta/ilst/----`), if there is one.
fn apple_note<R: Read + Seek>(walker: &mut Walker<R>, moov: BoxAt) -> Option<Trim> {
    let udta = walker.child(moov, b"udta")?;
    let mut meta = walker.child(udta, b"meta")?;
    // `meta` has four bytes of version and flags in front of its boxes.
    meta.start = meta.start.checked_add(4).filter(|s| *s <= meta.end)?;
    let ilst = walker.child(meta, b"ilst")?;
    for item in walker.children(ilst.start, ilst.end) {
        if &item.kind != b"----" {
            continue;
        }
        let parts = walker.children(item.start, item.end);
        let named = parts.iter().any(|part| {
            &part.kind == b"name"
                && walker
                    .payload(*part)
                    .is_some_and(|bytes| bytes.get(4..) == Some(b"iTunSMPB".as_slice()))
        });
        if !named {
            continue;
        }
        let data = parts.iter().find(|part| &part.kind == b"data")?;
        // Eight bytes of type and locale, then the text.
        let bytes = walker.payload(*data)?;
        return itunsmpb(std::str::from_utf8(bytes.get(8..)?).ok()?);
    }
    None
}

/// What the first audio track's edit list says, in frames at `sample_rate`.
fn edit_list<R: Read + Seek>(
    walker: &mut Walker<R>,
    moov: BoxAt,
    sample_rate: u32,
) -> Option<Trim> {
    let rate = u64::from(sample_rate);
    let mvhd = walker.child(moov, b"mvhd")?;
    let (movie_scale, _) = scale_and_duration(&walker.payload(mvhd)?)?;
    for trak in walker.children(moov.start, moov.end) {
        if &trak.kind != b"trak" {
            continue;
        }
        let Some(mdia) = walker.child(trak, b"mdia") else {
            continue;
        };
        // The handler says what kind of track this is: bytes 8..12 of `hdlr`.
        let is_audio = walker
            .child(mdia, b"hdlr")
            .and_then(|hdlr| walker.payload(hdlr))
            .is_some_and(|bytes| bytes.get(8..12) == Some(b"soun".as_slice()));
        if !is_audio {
            continue;
        }
        let mdhd = walker.child(mdia, b"mdhd")?;
        let (media_scale, media_duration) = scale_and_duration(&walker.payload(mdhd)?)?;
        let edts = walker.child(trak, b"edts")?;
        let elst = walker.child(edts, b"elst")?;
        let (edit_duration, media_time) = first_edit(&walker.payload(elst)?)?;
        if movie_scale == 0 || media_scale == 0 {
            return None;
        }
        let frames = |ticks: u64, scale: u64| {
            u64::try_from(u128::from(ticks) * u128::from(rate) / u128::from(scale)).ok()
        };
        let delay = frames(media_time, media_scale)?;
        let total = frames(media_duration, media_scale)?;
        let by_edit = frames(edit_duration, movie_scale)?;
        // A movie time scale coarser than the media's (milliseconds, say) cannot name
        // the last frame. Writers that use one shorten the last packet instead, so the
        // media's own length ends where the music does: use it when the two agree to
        // within what the coarse scale can express.
        let by_length = total.checked_sub(delay);
        let coarse = rate / movie_scale + 2;
        let valid = match by_length {
            Some(by_length)
                if movie_scale != media_scale && by_length.abs_diff(by_edit) <= coarse =>
            {
                by_length
            }
            _ => by_edit,
        };
        return Some(Trim { delay, valid });
    }
    None
}

/// Reads what an MP4 file says about priming and padding of its (first) audio track.
/// `None` if it says nothing usable; the track is then played as it decodes.
pub fn read<R: Read + Seek>(mut reader: R, sample_rate: u32) -> Option<Trim> {
    let len = reader.seek(SeekFrom::End(0)).ok()?;
    let mut walker = Walker {
        reader,
        budget: MAX_BOXES,
    };
    let moov = walker
        .children(0, len)
        .into_iter()
        .find(|b| &b.kind == b"moov")?;
    // Apple's note names the frames exactly; the edit list is what everyone else writes.
    let trim =
        apple_note(&mut walker, moov).or_else(|| edit_list(&mut walker, moov, sample_rate))?;
    let sane = trim.valid > 0 && trim.delay <= u64::from(sample_rate) * MAX_DELAY_SECONDS;
    sane.then_some(trim)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out
    }

    fn header(scale: u32, duration: u32) -> Vec<u8> {
        let mut p = vec![0u8; 12];
        p.extend_from_slice(&scale.to_be_bytes());
        p.extend_from_slice(&duration.to_be_bytes());
        p
    }

    fn hdlr(kind: &[u8; 4]) -> Vec<u8> {
        let mut p = vec![0u8; 8];
        p.extend_from_slice(kind);
        p.extend_from_slice(&[0u8; 12]);
        boxed(b"hdlr", &p)
    }

    fn elst(entries: &[(u32, i32)]) -> Vec<u8> {
        let mut p = vec![0u8; 4];
        p.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for (duration, media_time) in entries {
            p.extend_from_slice(&duration.to_be_bytes());
            p.extend_from_slice(&media_time.to_be_bytes());
            p.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        }
        boxed(b"edts", &boxed(b"elst", &p))
    }

    fn track(kind: &[u8; 4], media: (u32, u32), edits: Option<&[(u32, i32)]>) -> Vec<u8> {
        let mut mdia = boxed(b"mdhd", &header(media.0, media.1));
        mdia.extend(hdlr(kind));
        let mut trak = boxed(b"tkhd", &[0u8; 20]);
        if let Some(edits) = edits {
            trak.extend(elst(edits));
        }
        trak.extend(boxed(b"mdia", &mdia));
        boxed(b"trak", &trak)
    }

    fn file(movie_scale: u32, tracks: &[Vec<u8>], tags: Option<Vec<u8>>) -> Cursor<Vec<u8>> {
        let mut moov = boxed(b"mvhd", &header(movie_scale, 0));
        for t in tracks {
            moov.extend(t);
        }
        if let Some(tags) = tags {
            moov.extend(tags);
        }
        let mut out = boxed(b"ftyp", b"M4A \0\0\0\0");
        // The audio data may come first; nothing in it is looked at.
        out.extend(boxed(b"mdat", &[0xAB; 100]));
        out.extend(boxed(b"moov", &moov));
        Cursor::new(out)
    }

    fn apple_tags(note: &str) -> Vec<u8> {
        let mut name = vec![0u8; 4];
        name.extend_from_slice(b"iTunSMPB");
        let mut data = vec![0, 0, 0, 1, 0, 0, 0, 0];
        data.extend_from_slice(note.as_bytes());
        let mut mean = vec![0u8; 4];
        mean.extend_from_slice(b"com.apple.iTunes");
        let mut item = boxed(b"mean", &mean);
        item.extend(boxed(b"name", &name));
        item.extend(boxed(b"data", &data));
        // Another tag in front of it, as in a real file.
        let mut ilst = boxed(b"\xa9nam", &boxed(b"data", b"\0\0\0\x01\0\0\0\0Title"));
        ilst.extend(boxed(b"----", &item));
        let mut meta = vec![0u8; 4];
        meta.extend(boxed(b"hdlr", &[0u8; 25]));
        meta.extend(boxed(b"ilst", &ilst));
        boxed(b"udta", &boxed(b"meta", &meta))
    }

    #[test]
    fn an_edit_list_names_the_music() {
        // ffmpeg: both time scales are the sample rate; 1,024 frames of priming.
        let f = file(
            44_100,
            &[track(b"soun", (44_100, 55_465), Some(&[(54_441, 1_024)]))],
            None,
        );
        assert_eq!(
            read(f, 44_100),
            Some(Trim {
                delay: 1_024,
                valid: 54_441
            })
        );
        // A video track first, and silence before the music (an empty edit): the audio
        // track's first real edit counts.
        let f = file(
            48_000,
            &[
                track(b"vide", (600, 900), Some(&[(900, 0)])),
                track(
                    b"soun",
                    (48_000, 61_504),
                    Some(&[(100, -1), (59_392, 2_112)]),
                ),
            ],
            None,
        );
        assert_eq!(
            read(f, 48_000),
            Some(Trim {
                delay: 2_112,
                valid: 59_392
            })
        );
    }

    #[test]
    fn a_coarse_movie_time_scale_does_not_cost_the_last_frames() {
        // The movie counts milliseconds: 1,234 ms is 54,419 frames, but the track itself
        // ends 1,024 + 54,441 frames in. The media's own length is the exact one.
        let f = file(
            1_000,
            &[track(b"soun", (44_100, 55_465), Some(&[(1_234, 1_024)]))],
            None,
        );
        assert_eq!(read(f, 44_100).map(|t| t.valid), Some(54_441));
        // When the two disagree by more than rounding, the edit is what was asked for
        // (the track goes on after the edit ends).
        let f = file(
            1_000,
            &[track(b"soun", (44_100, 99_999), Some(&[(1_000, 1_024)]))],
            None,
        );
        assert_eq!(read(f, 44_100).map(|t| t.valid), Some(44_100));
    }

    #[test]
    fn apples_note_names_the_music() {
        let note = " 00000000 00000840 00000317 000000000000D4A9 00000000 00000000";
        let f = file(
            44_100,
            &[track(b"soun", (44_100, 57_344), None)],
            Some(apple_tags(note)),
        );
        assert_eq!(
            read(f, 44_100),
            Some(Trim {
                delay: 2_112,
                valid: 54_441
            })
        );
        // It wins over an edit list in the same file.
        let f = file(
            44_100,
            &[track(b"soun", (44_100, 57_344), Some(&[(50_000, 1_024)]))],
            Some(apple_tags(note)),
        );
        assert_eq!(read(f, 44_100).map(|t| t.delay), Some(2_112));
    }

    #[test]
    fn files_that_say_nothing_or_nonsense_are_left_alone() {
        // No edit list, no note.
        let f = file(44_100, &[track(b"soun", (44_100, 57_344), None)], None);
        assert_eq!(read(f, 44_100), None);
        // Priming of many seconds is not priming; an empty note is no note.
        let f = file(
            44_100,
            &[track(b"soun", (44_100, 900_000), Some(&[(1_000, 300_000)]))],
            None,
        );
        assert_eq!(read(f, 44_100), None);
        let f = file(
            44_100,
            &[track(b"soun", (44_100, 57_344), None)],
            Some(apple_tags("garbage")),
        );
        assert_eq!(read(f, 44_100), None);
        // Not an MP4 at all, an empty file, and boxes whose sizes lie.
        assert_eq!(read(Cursor::new(b"RIFF....WAVE".to_vec()), 44_100), None);
        assert_eq!(read(Cursor::new(Vec::new()), 44_100), None);
        let mut lying = boxed(b"moov", &boxed(b"mvhd", &header(44_100, 0)));
        lying[3] = 0xFF; // claims to be longer than the file
        assert_eq!(read(Cursor::new(lying), 44_100), None);
        let mut tiny = vec![0, 0, 0, 4];
        tiny.extend_from_slice(b"moov"); // shorter than its own header
        assert_eq!(read(Cursor::new(tiny), 44_100), None);
    }
}
