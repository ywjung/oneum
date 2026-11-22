//! Room correction: every sample of the music is replaced by the music as a filter
//! hears it, the filter being an impulse response that a measuring program made for
//! the room and the speakers (REW, DRC and their kind write one as a sound file).
//!
//! The work is a convolution, done so that nothing comes out late: the first taps of
//! the response are applied one by one as the samples pass, and the rest in blocks by
//! way of the Fourier transform. A block's part of the answer depends only on samples
//! that have already gone by when the block begins (its taps lie at least a block
//! back), so it is ready in time. Blocks grow with the distance: 64 samples for the
//! taps from 64 on, 512 from 512 on, and so on, which keeps a response of seconds
//! affordable at any sample rate. What the method does is pinned down by the tests at
//! the end of this file, against the plain sum it stands for.
use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

/// The longest response that is taken in, in seconds, and in frames at any rate.
const MAX_SECONDS: u64 = 6;
const MAX_FRAMES: usize = 1 << 21;
/// The most taps a filter is made of. A response that is longer than this at the
/// rate it is used at (seconds of it at 352.8 or 384 kHz) is used up to here: what
/// is kept of a filter while it plays grows with its length (one spectrum per 32,768
/// taps, for the response and for the music), and a phone has only so much.
const MAX_TAPS: usize = 1 << 20;
/// Frames of silence put in front of and behind a response that is converted to
/// another rate (see `Impulse::at_rate`).
const LEAD_FRAMES: usize = 256;
/// The sizes of the blocks the response is worked in, each for the taps from its own
/// size up to the next one's.
const BLOCKS: [usize; 4] = [64, 512, 4096, 32_768];

/// Why a file is no impulse response that can be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unusable {
    /// It could not be read as sound.
    Unreadable(String),
    /// It holds no sound, or only silence.
    Silent,
    /// It is longer than a response may be.
    TooLong,
}

impl std::fmt::Display for Unusable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unusable::Unreadable(why) => write!(f, "not a sound file that can be read: {why}"),
            Unusable::Silent => write!(f, "the file holds no sound"),
            Unusable::TooLong => write!(f, "longer than {MAX_SECONDS} seconds"),
        }
    }
}

/// An impulse response: what the left and the right channel are filtered with.
#[derive(Debug, Clone, PartialEq)]
pub struct Impulse {
    /// What it is called (the file's name).
    pub name: String,
    /// The sample rate it was made at.
    pub rate: u32,
    /// Left and right, of one length. (A response with one channel is both.)
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    /// What was worked out of it (see [`Kept`]).
    kept: Kept,
}

/// What was worked out of a response, kept with it: how far it is turned down at its
/// own rate, and the filter as it was last made ready for a rate. Every feed needs
/// the filter (each seek begins one), and making it ready is a rate conversion and
/// Fourier transforms over the whole response: done once for a rate, not once a feed.
/// It is no part of what a response is: two that hold the same samples are the same.
#[derive(Default)]
struct Kept {
    trim: OnceLock<f32>,
    ready: Mutex<Option<Arc<Ready>>>,
}

impl Clone for Kept {
    fn clone(&self) -> Self {
        Kept::default()
    }
}

impl PartialEq for Kept {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl std::fmt::Debug for Kept {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Kept")
    }
}

/// A response made ready for audio at one rate.
struct Ready {
    rate: u32,
    /// How far it is turned down (1.0: not at all).
    trim: f32,
    left: Arc<Taps>,
    /// The same as `left` (not a second one like it) where both channels have one
    /// response.
    right: Arc<Taps>,
}

/// One channel's filter, as every convolver for it uses it.
struct Taps {
    /// The first taps, newest sample last (so they meet the samples in the order
    /// those are held in).
    head: Vec<f32>,
    /// The rest, by the sizes of the blocks they are worked in.
    plans: Vec<Arc<Plan>>,
}

/// One size of block: the taps from `block` on (up to where the next size begins),
/// in pieces of `block` taps, each as its spectrum (padded to two blocks).
struct Plan {
    block: usize,
    forward: Arc<dyn Fft<f32>>,
    inverse: Arc<dyn Fft<f32>>,
    pieces: Vec<Vec<Complex<f32>>>,
}

impl Impulse {
    /// From interleaved stereo frames at `rate`. What is no number counts as nothing,
    /// silence at the end is cut off.
    pub fn from_frames(name: &str, rate: u32, frames: &[f32]) -> Result<Self, Unusable> {
        if rate == 0 {
            return Err(Unusable::Silent);
        }
        let clean = |s: &f32| if s.is_finite() { *s } else { 0.0 };
        let pairs = frames.as_chunks::<2>().0;
        let last = pairs
            .iter()
            .rposition(|pair| clean(&pair[0]) != 0.0 || clean(&pair[1]) != 0.0)
            .ok_or(Unusable::Silent)?;
        let length = last + 1;
        if length > MAX_FRAMES || length as u64 > u64::from(rate) * MAX_SECONDS {
            return Err(Unusable::TooLong);
        }
        Ok(Impulse {
            name: name.chars().filter(|c| !c.is_control()).take(120).collect(),
            rate,
            left: pairs[..length].iter().map(|pair| clean(&pair[0])).collect(),
            right: pairs[..length].iter().map(|pair| clean(&pair[1])).collect(),
            kept: Kept::default(),
        })
    }

    /// From a sound file, as the app's decoder reads it (WAV, FLAC and the others; a
    /// file with one channel is both channels).
    pub fn load(path: &Path) -> Result<Self, Unusable> {
        let unreadable = |e: super::PlaybackError| Unusable::Unreadable(e.detail);
        let mut decoder = super::decoder::TrackDecoder::open(path).map_err(unreadable)?;
        let (rate, channels) = (decoder.info().sample_rate, decoder.info().channels);
        // One channel for both sides, or one for each. (A response of more channels
        // mixes the sides into each other, which is not done here.)
        if !(1..=2).contains(&channels) {
            return Err(Unusable::Unreadable(format!(
                "{channels} channels (a response has one or two)"
            )));
        }
        let most = (u64::from(rate) * MAX_SECONDS).min(MAX_FRAMES as u64) as usize * channels;
        // (The decoder gives the samples of the file's own channels, in turn.)
        let (mut all, mut piece) = (Vec::new(), Vec::new());
        while decoder.next(&mut piece).map_err(unreadable)? {
            all.extend_from_slice(&piece);
            // (Silence at the end may still be cut; twice the most is surely too long.)
            if all.len() > most * 2 {
                return Err(Unusable::TooLong);
            }
        }
        let frames: Vec<f32> = match channels {
            1 => all.iter().flat_map(|sample| [*sample, *sample]).collect(),
            _ => all,
        };
        let name = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        Impulse::from_frames(&name, rate, &frames)
    }

    /// The response as it is used at `rate`: converted where it was made at another
    /// rate (a filter keeps what it does when its samples are scaled by the ratio of
    /// the rates), and turned down as far as it would make any frequency louder than
    /// it is. Left, right, and how far it was turned down (1.0: not at all).
    fn at_rate(&self, rate: u32) -> (Vec<f32>, Vec<f32>, f32) {
        let (mut left, mut right) = (self.left.clone(), self.right.clone());
        if rate != self.rate && rate != 0 {
            // A response often begins with its largest sample. At another rate that
            // sample lies between two samples, and what stands for it there begins
            // before it and ends after it: room is left in front and behind for that
            // (the music then comes out that much later, six milliseconds, at such
            // a rate).
            let room = || std::iter::repeat_n(0.0, LEAD_FRAMES * 2);
            let frames: Vec<f32> = room()
                .chain(left.iter().zip(&right).flat_map(|(l, r)| [*l, *r]))
                .chain(room())
                .collect();
            if let Ok(mut converter) = super::resample::StereoResampler::new(self.rate, rate) {
                let mut out = Vec::new();
                if converter.process(&frames, &mut out).is_ok() && converter.flush(&mut out).is_ok()
                {
                    let scale = self.rate as f32 / rate as f32;
                    let pairs = out.as_chunks::<2>().0;
                    left = pairs.iter().map(|pair| pair[0] * scale).collect();
                    right = pairs.iter().map(|pair| pair[1] * scale).collect();
                }
            }
        }
        left.truncate(MAX_TAPS);
        right.truncate(MAX_TAPS);
        // (At its own rate, and whole, how far it is turned down is known already.)
        let whole = rate == self.rate && self.left.len() <= MAX_TAPS;
        let trim = match whole {
            true => self.turned_down(),
            false => trim_of(&left, &right),
        };
        if trim != 1.0 {
            left.iter_mut().for_each(|tap| *tap *= trim);
            right.iter_mut().for_each(|tap| *tap *= trim);
        }
        (left, right, trim)
    }
}

/// What a kept response begins with.
const MAGIC: &[u8; 8] = b"ONEUMIR1";

impl Impulse {
    /// How far the response is turned down where it is used, as a factor (1.0: not
    /// at all), at the rate it was made at. Worked out once.
    pub fn turned_down(&self) -> f32 {
        *self
            .kept
            .trim
            .get_or_init(|| trim_of(&self.left, &self.right))
    }

    /// The response made ready for audio at `rate`: what was made ready last, where
    /// that was for this rate.
    fn ready(&self, rate: u32) -> Arc<Ready> {
        let mut kept = self.kept.ready.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(ready) = kept.as_ref().filter(|ready| ready.rate == rate) {
            return Arc::clone(ready);
        }
        let (left, right, trim) = self.at_rate(rate);
        let mut planner = FftPlanner::<f32>::new();
        let left_taps = Arc::new(Taps::new(&left, &mut planner));
        let right_taps = match left == right {
            true => Arc::clone(&left_taps),
            false => Arc::new(Taps::new(&right, &mut planner)),
        };
        let ready = Arc::new(Ready {
            rate,
            trim,
            left: left_taps,
            right: right_taps,
        });
        *kept = Some(Arc::clone(&ready));
        ready
    }

    /// Writes the response to a file of the app's own (beside it first, then in its
    /// place: a file half written is never the one that is read).
    pub fn save(&self, file: &Path) -> std::io::Result<()> {
        let name = self.name.as_bytes();
        let name = &name[..name.len().min(usize::from(u16::MAX))];
        let mut bytes = Vec::with_capacity(32 + name.len() + self.left.len() * 8);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&self.rate.to_le_bytes());
        bytes.extend_from_slice(&(self.left.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(name.len() as u16).to_le_bytes());
        bytes.extend_from_slice(name);
        for tap in self.left.iter().chain(&self.right) {
            bytes.extend_from_slice(&tap.to_le_bytes());
        }
        let beside = file.with_extension("writing");
        std::fs::write(&beside, bytes)?;
        std::fs::rename(&beside, file)
    }

    /// Reads a response that [`Impulse::save`] wrote. `None`: there is none, or the
    /// file is not what was written.
    pub fn open(file: &Path) -> Option<Self> {
        let meta = std::fs::metadata(file).ok()?;
        if !meta.is_file() || meta.len() > (MAX_FRAMES as u64 * 8) + (1 << 17) {
            return None;
        }
        let bytes = std::fs::read(file).ok()?;
        let rest = bytes.strip_prefix(MAGIC.as_slice())?;
        let number = |at: usize| Some(u32::from_le_bytes(rest.get(at..at + 4)?.try_into().ok()?));
        let (rate, frames) = (number(0)?, number(4)? as usize);
        let name_len = usize::from(u16::from_le_bytes(rest.get(8..10)?.try_into().ok()?));
        let name = String::from_utf8_lossy(rest.get(10..10 + name_len)?).into_owned();
        let taps = rest.get(10 + name_len..)?;
        if frames == 0 || frames > MAX_FRAMES || taps.len() != frames * 8 {
            return None;
        }
        let read = |part: &[u8]| -> Vec<f32> {
            part.as_chunks::<4>()
                .0
                .iter()
                .map(|four| f32::from_le_bytes(*four))
                .collect()
        };
        let frames_of_both: Vec<f32> = read(&taps[..frames * 4])
            .into_iter()
            .zip(read(&taps[frames * 4..]))
            .flat_map(|(l, r)| [l, r])
            .collect();
        Impulse::from_frames(&name, rate, &frames_of_both).ok()
    }
}

/// How far a filter of these taps is turned down so that it makes no frequency louder
/// than it was, as a factor (1.0: not at all).
fn trim_of(left: &[f32], right: &[f32]) -> f32 {
    // (One response for both sides is looked at once.)
    let peak = match left == right {
        true => loudest(left),
        false => loudest(left).max(loudest(right)),
    };
    // (What the transform's rounding adds to a response that raises nothing is not
    // turned down for: such a peak came out as 1.0000001 on one processor.)
    if peak > 1.0 + ROUNDING {
        1.0 / peak
    } else {
        1.0
    }
}

/// How far above 1.0 the loudest frequency of a filter may be found and still be the
/// rounding of the transform that looked for it (a ten-thousandth of a decibel).
const ROUNDING: f32 = 1e-5;

/// How much the filter with these taps raises the frequency it raises most (1.0:
/// nothing is louder than it was).
fn loudest(taps: &[f32]) -> f32 {
    if taps.is_empty() {
        return 0.0;
    }
    let size = (taps.len() * 2).next_power_of_two().max(4096);
    let mut bins: Vec<Complex<f32>> = taps
        .iter()
        .map(|tap| Complex::new(*tap, 0.0))
        .chain(std::iter::repeat(Complex::new(0.0, 0.0)))
        .take(size)
        .collect();
    FftPlanner::<f32>::new()
        .plan_fft_forward(size)
        .process(&mut bins);
    bins[..=size / 2]
        .iter()
        .map(|bin| bin.norm())
        .fold(0.0, f32::max)
}

impl Plan {
    /// For `taps`: those of the response from `block` on (to the next size, or to
    /// the end).
    fn new(block: usize, taps: &[f32], planner: &mut FftPlanner<f32>) -> Self {
        let size = block * 2;
        let forward = planner.plan_fft_forward(size);
        let inverse = planner.plan_fft_inverse(size);
        let pieces: Vec<Vec<Complex<f32>>> = taps
            .chunks(block)
            .map(|piece| {
                let mut bins: Vec<Complex<f32>> = piece
                    .iter()
                    .map(|tap| Complex::new(*tap, 0.0))
                    .chain(std::iter::repeat(Complex::new(0.0, 0.0)))
                    .take(size)
                    .collect();
                forward.process(&mut bins);
                bins
            })
            .collect();
        Plan {
            block,
            forward,
            inverse,
            pieces,
        }
    }
}

impl Taps {
    fn new(taps: &[f32], planner: &mut FftPlanner<f32>) -> Self {
        let first = taps.len().min(BLOCKS[0]);
        let head: Vec<f32> = taps[..first].iter().rev().copied().collect();
        let mut plans = Vec::new();
        for (n, block) in BLOCKS.iter().enumerate() {
            let until = BLOCKS
                .get(n + 1)
                .copied()
                .unwrap_or(usize::MAX)
                .min(taps.len());
            if taps.len() > *block {
                plans.push(Arc::new(Plan::new(*block, &taps[*block..until], planner)));
            }
        }
        Taps { head, plans }
    }
}

/// One size of block at work: its part of the filter, and what it holds of the
/// music that goes by.
struct Stage {
    plan: Arc<Plan>,
    /// The spectra of the last blocks of input, newest first (as many as the plan
    /// has pieces).
    heard: std::collections::VecDeque<Vec<Complex<f32>>>,
    /// The block before the one being filled, and the one being filled.
    input: Vec<f32>,
    filled: usize,
    /// This stage's part of the output for the block being filled.
    ready: Vec<f32>,
    work: Vec<Complex<f32>>,
}

impl Stage {
    fn new(plan: &Arc<Plan>) -> Self {
        let size = plan.block * 2;
        let silence = vec![Complex::new(0.0, 0.0); size];
        Stage {
            plan: Arc::clone(plan),
            heard: std::iter::repeat_n(silence.clone(), plan.pieces.len()).collect(),
            input: vec![0.0; size],
            filled: 0,
            ready: vec![0.0; plan.block],
            work: silence,
        }
    }

    /// Takes `input` in and adds this stage's part to `out` (of the same length).
    fn run(&mut self, input: &[f32], out: &mut [f32]) {
        let block = self.plan.block;
        let mut at = 0;
        while at < input.len() {
            let take = (block - self.filled).min(input.len() - at);
            for (o, r) in out[at..at + take]
                .iter_mut()
                .zip(&self.ready[self.filled..self.filled + take])
            {
                *o += r;
            }
            self.input[block + self.filled..block + self.filled + take]
                .copy_from_slice(&input[at..at + take]);
            self.filled += take;
            at += take;
            if self.filled == block {
                self.turn();
            }
        }
    }

    /// A block is full: its spectrum joins the ones that were heard, and what the
    /// pieces of the response make of them is the part for the block that begins.
    fn turn(&mut self) {
        let block = self.plan.block;
        for (bin, sample) in self.work.iter_mut().zip(&self.input) {
            *bin = Complex::new(*sample, 0.0);
        }
        self.plan.forward.process(&mut self.work);
        // (The oldest spectrum's place is taken by the newest.)
        if let Some(mut oldest) = self.heard.pop_back() {
            oldest.copy_from_slice(&self.work);
            self.heard.push_front(oldest);
        }
        self.work.fill(Complex::new(0.0, 0.0));
        for (piece, heard) in self.plan.pieces.iter().zip(&self.heard) {
            for ((sum, a), b) in self.work.iter_mut().zip(piece).zip(heard) {
                *sum += a * b;
            }
        }
        self.plan.inverse.process(&mut self.work);
        let scale = 1.0 / (block * 2) as f32;
        for (ready, bin) in self.ready.iter_mut().zip(&self.work[block..]) {
            *ready = bin.re * scale;
        }
        self.input.copy_within(block.., 0);
        self.filled = 0;
    }
}

/// One channel's filter at work.
struct Channel {
    taps: Arc<Taps>,
    /// The last samples that went by (one fewer than the head has taps), then those
    /// being worked on.
    held: Vec<f32>,
    stages: Vec<Stage>,
    out: Vec<f32>,
}

impl Channel {
    fn new(taps: &Arc<Taps>) -> Self {
        Channel {
            held: vec![0.0; taps.head.len().saturating_sub(1)],
            stages: taps.plans.iter().map(Stage::new).collect(),
            taps: Arc::clone(taps),
            out: Vec::new(),
        }
    }

    /// Filters `samples` (one channel) in place.
    fn process(&mut self, samples: &mut [f32]) {
        let head = &self.taps.head;
        if head.is_empty() {
            samples.fill(0.0);
            return;
        }
        self.out.clear();
        self.out.resize(samples.len(), 0.0);
        // The first taps, one sample at a time.
        self.held.extend_from_slice(samples);
        for (n, out) in self.out.iter_mut().enumerate() {
            let window = &self.held[n..n + head.len()];
            *out = window.iter().zip(head).map(|(s, tap)| s * tap).sum();
        }
        self.held.drain(..samples.len());
        // The rest, block by block.
        for stage in &mut self.stages {
            stage.run(samples, &mut self.out);
        }
        samples.copy_from_slice(&self.out);
    }
}

/// Applies an impulse response to interleaved stereo audio.
pub struct Convolver {
    left: Channel,
    right: Channel,
    trim: f32,
    one: Vec<f32>,
    other: Vec<f32>,
}

impl Convolver {
    /// For audio at `rate`. (What the response has to be turned into for that rate
    /// is made once and kept with it: the next convolver for the rate begins at once.)
    pub fn new(impulse: &Impulse, rate: u32) -> Self {
        let ready = impulse.ready(rate);
        Convolver {
            left: Channel::new(&ready.left),
            right: Channel::new(&ready.right),
            trim: ready.trim,
            one: Vec::new(),
            other: Vec::new(),
        }
    }

    /// How far the response was turned down so that it makes nothing louder than it
    /// is, as a factor (1.0: not at all).
    pub fn trim(&self) -> f32 {
        self.trim
    }

    /// Filters `block` (interleaved stereo) in place. Nothing comes out late: a
    /// sample is in the output where it was in the input.
    pub fn process(&mut self, block: &mut [f32]) {
        self.one.clear();
        self.other.clear();
        for frame in block.as_chunks::<2>().0 {
            self.one.push(frame[0]);
            self.other.push(frame[1]);
        }
        self.left.process(&mut self.one);
        self.right.process(&mut self.other);
        for ((frame, l), r) in block
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip(&self.one)
            .zip(&self.other)
        {
            *frame = [*l, *r];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Numbers that look like noise, the same every time.
    fn noise(count: usize, seed: u64) -> Vec<f32> {
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..count)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 40) as f32 / (1u64 << 23) as f32 - 1.0
            })
            .collect()
    }

    /// What the convolution stands for: every output sample as the plain sum.
    fn plain(input: &[f32], taps: &[f32]) -> Vec<f32> {
        (0..input.len())
            .map(|n| {
                taps.iter()
                    .take(n + 1)
                    .enumerate()
                    .map(|(k, tap)| f64::from(*tap) * f64::from(input[n - k]))
                    .sum::<f64>() as f32
            })
            .collect()
    }

    fn impulse(left: &[f32], right: &[f32]) -> Impulse {
        Impulse {
            name: "test".into(),
            rate: 44_100,
            left: left.to_vec(),
            right: right.to_vec(),
            kept: Kept::default(),
        }
    }

    /// How large a tone is, by the power of its samples (the largest sample tells
    /// it only where one falls on a crest).
    fn amplitude(tone: &[f32]) -> f32 {
        let power: f64 = tone.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
        ((power / tone.len().max(1) as f64) * 2.0).sqrt() as f32
    }

    fn largest_difference(a: &[f32], b: &[f32]) -> f32 {
        assert_eq!(a.len(), b.len());
        a.iter()
            .zip(b)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0, f32::max)
    }

    #[test]
    fn the_output_is_the_plain_sum_whatever_the_length_and_however_it_is_fed() {
        // Lengths at and around where the ways of working change hands.
        for taps in [
            1usize, 2, 63, 64, 65, 100, 511, 512, 513, 700, 4_096, 4_500, 9_000,
        ] {
            let scale = 0.2 / (taps as f32).sqrt();
            let left: Vec<f32> = noise(taps, 7).iter().map(|t| t * scale).collect();
            let right: Vec<f32> = noise(taps, 8).iter().map(|t| t * scale).collect();
            let response = impulse(&left, &right);
            let frames = 12_000.min(3 * taps + 3_000);
            let (in_l, in_r) = (noise(frames, 1), noise(frames, 2));
            let input: Vec<f32> = in_l.iter().zip(&in_r).flat_map(|(l, r)| [*l, *r]).collect();
            let trim = Convolver::new(&response, 44_100).trim();
            let expect_l: Vec<f32> = plain(&in_l, &left).iter().map(|s| s * trim).collect();
            let expect_r: Vec<f32> = plain(&in_r, &right).iter().map(|s| s * trim).collect();
            for piece in [frames, 1, 7, 64, 333, 4_096] {
                let mut convolver = Convolver::new(&response, 44_100);
                let mut output = input.clone();
                for chunk in output.chunks_mut(piece * 2) {
                    convolver.process(chunk);
                }
                let out_l: Vec<f32> = output.as_chunks::<2>().0.iter().map(|f| f[0]).collect();
                let out_r: Vec<f32> = output.as_chunks::<2>().0.iter().map(|f| f[1]).collect();
                let what = format!("{taps} taps in pieces of {piece}");
                assert!(largest_difference(&out_l, &expect_l) < 2e-5, "{what}, left");
                assert!(
                    largest_difference(&out_r, &expect_r) < 2e-5,
                    "{what}, right"
                );
            }
        }
    }

    #[test]
    fn a_long_response_is_right_too() {
        // 40,000 taps: all four sizes of block at work.
        let taps = 40_000;
        let left: Vec<f32> = noise(taps, 3).iter().map(|t| t * 0.001).collect();
        let response = impulse(&left, &left);
        let frames = 45_000;
        let signal = noise(frames, 5);
        let mut output: Vec<f32> = signal.iter().flat_map(|s| [*s, *s]).collect();
        let mut convolver = Convolver::new(&response, 44_100);
        let trim = convolver.trim();
        for chunk in output.chunks_mut(2 * 1_111) {
            convolver.process(chunk);
        }
        // (The plain sum for every sample would take minutes: every 97th is looked at.)
        for n in (0..frames).step_by(97) {
            let expected: f64 = left
                .iter()
                .take(n + 1)
                .enumerate()
                .map(|(k, tap)| f64::from(*tap) * f64::from(signal[n - k]))
                .sum();
            let got = f64::from(output[n * 2]);
            assert!((got - expected * f64::from(trim)).abs() < 5e-5, "frame {n}");
            assert_eq!(output[n * 2], output[n * 2 + 1]);
        }
    }

    #[test]
    fn a_response_that_changes_nothing_changes_nothing_and_each_channel_has_its_own() {
        let signal: Vec<f32> = noise(4_000, 9).iter().map(|s| s * 0.5).collect();
        let mut same = signal.clone();
        Convolver::new(&impulse(&[1.0], &[1.0]), 44_100).process(&mut same);
        assert_eq!(same, signal);
        // Left: half as loud. Right: three frames late and the other way up.
        let response = impulse(&[0.5, 0.0, 0.0, 0.0], &[0.0, 0.0, 0.0, -1.0]);
        let mut output = signal.clone();
        let mut convolver = Convolver::new(&response, 44_100);
        assert_eq!(convolver.trim(), 1.0);
        convolver.process(&mut output);
        for n in 3..signal.len() / 2 {
            assert_eq!(output[n * 2], signal[n * 2] * 0.5);
            assert_eq!(output[n * 2 + 1], -signal[(n - 3) * 2 + 1]);
        }
        assert_eq!(output[1], 0.0);
    }

    #[test]
    fn a_response_that_would_make_something_louder_is_turned_down() {
        // Twice as loud at every frequency: turned down to as loud as it was.
        let convolver = Convolver::new(&impulse(&[2.0], &[1.0]), 44_100);
        assert!((convolver.trim() - 0.5).abs() < 1e-6);
        // A boost of the bass (the sum of the taps is what 0 Hz is multiplied by).
        let taps = vec![0.02f32; 100];
        let convolver = Convolver::new(&impulse(&taps, &taps), 44_100);
        assert!(
            (convolver.trim() - 0.5).abs() < 1e-3,
            "{}",
            convolver.trim()
        );
        // One that only takes away is left as it is.
        let convolver = Convolver::new(&impulse(&[0.5, 0.25], &[0.5, 0.25]), 44_100);
        assert_eq!(convolver.trim(), 1.0);
    }

    #[test]
    fn a_response_made_at_another_rate_does_the_same_at_this_one() {
        // A gentle low-pass made at 44.1 kHz, used at 88.2 and at 48 kHz: a tone of
        // 1 kHz and one of 8 kHz come out as loud as at the rate it was made at.
        let taps: Vec<f32> = (0..31)
            .map(|n| {
                let t = (n as f32 - 15.0) / 1.5;
                (-t * t).exp() * 0.35
            })
            .collect();
        let response = impulse(&taps, &taps);
        let level = |rate: u32, hz: f32| {
            let mut tone: Vec<f32> = (0..rate as usize)
                .flat_map(|n| {
                    let s = (n as f32 * hz * std::f32::consts::TAU / rate as f32).sin() * 0.5;
                    [s, s]
                })
                .collect();
            let mut convolver = Convolver::new(&response, rate);
            convolver.process(&mut tone);
            amplitude(&tone[tone.len() / 2..]) / convolver.trim()
        };
        for hz in [1_000.0, 8_000.0] {
            let made_at = level(44_100, hz);
            for rate in [88_200, 48_000, 96_000] {
                let used_at = level(rate, hz);
                assert!(
                    (used_at / made_at - 1.0).abs() < 0.02,
                    "{hz} Hz at {rate}: {used_at} for {made_at}"
                );
            }
        }
    }

    /// A response that is all in its first sample (as a response often nearly is)
    /// does at another rate what it does at its own: as loud at every frequency.
    #[test]
    fn a_response_that_begins_with_its_peak_is_converted_whole() {
        let response = impulse(&[0.5], &[0.5]);
        for rate in [48_000u32, 88_200, 96_000, 22_050] {
            for hz in [100.0f32, 1_000.0, 9_000.0] {
                let mut tone: Vec<f32> = (0..rate as usize)
                    .flat_map(|n| {
                        let s = (n as f32 * hz * std::f32::consts::TAU / rate as f32).sin() * 0.8;
                        [s, s]
                    })
                    .collect();
                let mut convolver = Convolver::new(&response, rate);
                assert_eq!(convolver.trim(), 1.0);
                convolver.process(&mut tone);
                let level = amplitude(&tone[tone.len() / 2..]);
                assert!(
                    (level / 0.4 - 1.0).abs() < 0.01,
                    "{hz} Hz at {rate}: {level}"
                );
            }
        }
    }

    /// What a response has to be turned into for a rate is made once: the convolvers
    /// of one rate share it, another rate has it made anew, and one response for
    /// both sides is one filter, not two.
    #[test]
    fn a_response_is_made_ready_once_for_a_rate() {
        let taps: Vec<f32> = noise(5_000, 4).iter().map(|t| t * 0.01).collect();
        let both = impulse(&taps, &taps);
        let first = both.ready(44_100);
        assert!(Arc::ptr_eq(&first, &both.ready(44_100)));
        assert!(Arc::ptr_eq(&first.left, &first.right));
        let other = both.ready(48_000);
        assert!(!Arc::ptr_eq(&first, &other));
        assert!(Arc::ptr_eq(&other, &both.ready(48_000)));
        // Two convolvers from it do not hear each other's music.
        let signal = noise(3_000, 6);
        let mut once: Vec<f32> = signal.iter().flat_map(|s| [*s, *s]).collect();
        let mut again = once.clone();
        let (mut a, mut b) = (Convolver::new(&both, 44_100), Convolver::new(&both, 44_100));
        a.process(&mut once);
        b.process(&mut vec![0.5; 2_000]);
        b = Convolver::new(&both, 44_100);
        b.process(&mut again);
        assert_eq!(once, again);
        // Each side its own response: two filters.
        let sides = impulse(&taps, &noise(5_000, 5));
        let ready = sides.ready(44_100);
        assert!(!Arc::ptr_eq(&ready.left, &ready.right));
        // A copy of a response is the same response, and has nothing made ready yet.
        let copy = both.clone();
        assert_eq!(copy, both);
        assert!(copy.kept.ready.lock().unwrap().is_none());
        // A response longer than a filter may be is used up to there.
        let long = impulse(&vec![1e-6; MAX_TAPS + 10], &vec![1e-6; MAX_TAPS + 10]);
        let (left, right, _) = long.at_rate(44_100);
        assert_eq!((left.len(), right.len()), (MAX_TAPS, MAX_TAPS));
    }

    #[test]
    fn what_is_no_response_is_refused() {
        assert_eq!(
            Impulse::from_frames("x", 44_100, &[]),
            Err(Unusable::Silent)
        );
        assert_eq!(
            Impulse::from_frames("x", 44_100, &[0.0; 64]),
            Err(Unusable::Silent)
        );
        assert_eq!(
            Impulse::from_frames("x", 0, &[1.0, 1.0]),
            Err(Unusable::Silent)
        );
        let long = vec![0.1f32; 2 * (44_100 * MAX_SECONDS as usize + 1)];
        assert_eq!(
            Impulse::from_frames("x", 44_100, &long),
            Err(Unusable::TooLong)
        );
        // Silence at the end is cut off, what is no number is nothing, and a name
        // is one line.
        let frames = [
            0.0,
            0.0,
            1.0,
            f32::NAN,
            0.5,
            f32::INFINITY,
            0.0,
            0.0,
            0.0,
            0.0,
        ];
        let read = Impulse::from_frames("a\nb", 48_000, &frames).unwrap();
        assert_eq!(read.left, [0.0, 1.0, 0.5]);
        assert_eq!(read.right, [0.0, 0.0, 0.0]);
        assert_eq!(read.name, "ab");
        // What is kept in a file of the app's own comes back as it was, and a file
        // that is not such a one is none.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("room.ir");
        assert_eq!(Impulse::open(&file), None);
        read.save(&file).unwrap();
        assert_eq!(Impulse::open(&file), Some(read.clone()));
        assert!(!dir.path().join("room.writing").exists());
        let mut bytes = std::fs::read(&file).unwrap();
        bytes.truncate(bytes.len() - 3);
        std::fs::write(&file, &bytes).unwrap();
        assert_eq!(Impulse::open(&file), None);
        std::fs::write(&file, b"something else").unwrap();
        assert_eq!(Impulse::open(&file), None);
        // (Its left channel makes what is lowest half as loud again: turned down
        // by that much.)
        assert!((read.turned_down() - 1.0 / 1.5).abs() < 1e-3);
        // A file that is no sound.
        let path = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
        assert!(matches!(Impulse::load(path), Err(Unusable::Unreadable(_))));
        // A file with one channel (what measuring programs mostly write) is the
        // response of both sides, sample for sample: not every other sample each.
        let mono = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/tone_mono.wav"
        ));
        let one = Impulse::load(mono).unwrap();
        assert_eq!(one.left, one.right);
        assert_eq!((one.rate, one.left.len()), (44_100, 44_100));
        // A sound file is read with its rate and its name.
        let sound = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/tone_48k.wav"
        ));
        let read = Impulse::load(sound).unwrap();
        assert_eq!((read.rate, read.name.as_str()), (48_000, "tone_48k"));
        assert_eq!(read.left.len(), read.right.len());
        assert!(!read.left.is_empty());
    }
}
