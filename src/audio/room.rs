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
use std::sync::Arc;

/// The longest response that is used, in seconds, and in frames at any rate.
const MAX_SECONDS: u64 = 6;
const MAX_FRAMES: usize = 1 << 21;
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
        })
    }

    /// From a sound file, as the app's decoder reads it (WAV, FLAC and the others; a
    /// file with one channel is both channels).
    pub fn load(path: &Path) -> Result<Self, Unusable> {
        let unreadable = |e: super::PlaybackError| Unusable::Unreadable(e.detail);
        let mut decoder = super::decoder::TrackDecoder::open(path).map_err(unreadable)?;
        let rate = decoder.info().sample_rate;
        let most = (u64::from(rate) * MAX_SECONDS).min(MAX_FRAMES as u64) as usize * 2;
        let (mut all, mut piece) = (Vec::new(), Vec::new());
        while decoder.next(&mut piece).map_err(unreadable)? {
            all.extend_from_slice(&piece);
            // (Silence at the end may still be cut; twice the most is surely too long.)
            if all.len() > most * 2 {
                return Err(Unusable::TooLong);
            }
        }
        let name = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        Impulse::from_frames(&name, rate, &all)
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
                    left.truncate(MAX_FRAMES);
                    right.truncate(MAX_FRAMES);
                }
            }
        }
        let peak = loudest(&left).max(loudest(&right));
        let trim = if peak > 1.0 { 1.0 / peak } else { 1.0 };
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
    /// at all), at the rate it was made at.
    pub fn turned_down(&self) -> f32 {
        let peak = loudest(&self.left).max(loudest(&self.right));
        if peak > 1.0 { 1.0 / peak } else { 1.0 }
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

/// One size of block: the taps from `block` on (up to where the next size begins),
/// in pieces of `block` taps.
struct Stage {
    block: usize,
    forward: Arc<dyn Fft<f32>>,
    inverse: Arc<dyn Fft<f32>>,
    /// The spectrum of each piece of the response (padded to two blocks).
    pieces: Vec<Vec<Complex<f32>>>,
    /// The spectra of the last blocks of input, newest first (as many as pieces).
    heard: std::collections::VecDeque<Vec<Complex<f32>>>,
    /// The block before the one being filled, and the one being filled.
    input: Vec<f32>,
    filled: usize,
    /// This stage's part of the output for the block being filled.
    ready: Vec<f32>,
    work: Vec<Complex<f32>>,
}

impl Stage {
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
        let silence = vec![Complex::new(0.0, 0.0); size];
        Stage {
            block,
            forward,
            inverse,
            heard: std::iter::repeat_n(silence.clone(), pieces.len()).collect(),
            pieces,
            input: vec![0.0; size],
            filled: 0,
            ready: vec![0.0; block],
            work: silence,
        }
    }

    /// Takes `input` in and adds this stage's part to `out` (of the same length).
    fn run(&mut self, input: &[f32], out: &mut [f32]) {
        let block = self.block;
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
        let block = self.block;
        for (bin, sample) in self.work.iter_mut().zip(&self.input) {
            *bin = Complex::new(*sample, 0.0);
        }
        self.forward.process(&mut self.work);
        // (The oldest spectrum's place is taken by the newest.)
        if let Some(mut oldest) = self.heard.pop_back() {
            oldest.copy_from_slice(&self.work);
            self.heard.push_front(oldest);
        }
        self.work.fill(Complex::new(0.0, 0.0));
        for (piece, heard) in self.pieces.iter().zip(&self.heard) {
            for ((sum, a), b) in self.work.iter_mut().zip(piece).zip(heard) {
                *sum += a * b;
            }
        }
        self.inverse.process(&mut self.work);
        let scale = 1.0 / (block * 2) as f32;
        for (ready, bin) in self.ready.iter_mut().zip(&self.work[block..]) {
            *ready = bin.re * scale;
        }
        self.input.copy_within(block.., 0);
        self.filled = 0;
    }
}

/// One channel's filter.
struct Channel {
    /// The first taps, newest sample last (so they meet the samples in the order
    /// those are held in).
    head: Vec<f32>,
    /// The last samples that went by (one fewer than the head has taps), then those
    /// being worked on.
    held: Vec<f32>,
    stages: Vec<Stage>,
    out: Vec<f32>,
}

impl Channel {
    fn new(taps: &[f32], planner: &mut FftPlanner<f32>) -> Self {
        let first = taps.len().min(BLOCKS[0]);
        let head: Vec<f32> = taps[..first].iter().rev().copied().collect();
        let mut stages = Vec::new();
        for (n, block) in BLOCKS.iter().enumerate() {
            let until = BLOCKS
                .get(n + 1)
                .copied()
                .unwrap_or(usize::MAX)
                .min(taps.len());
            if taps.len() > *block {
                stages.push(Stage::new(*block, &taps[*block..until], planner));
            }
        }
        Channel {
            held: vec![0.0; head.len().saturating_sub(1)],
            head,
            stages,
            out: Vec::new(),
        }
    }

    /// Filters `samples` (one channel) in place.
    fn process(&mut self, samples: &mut [f32]) {
        let taps = self.head.len();
        if taps == 0 {
            samples.fill(0.0);
            return;
        }
        self.out.clear();
        self.out.resize(samples.len(), 0.0);
        // The first taps, one sample at a time.
        self.held.extend_from_slice(samples);
        for (n, out) in self.out.iter_mut().enumerate() {
            let window = &self.held[n..n + taps];
            *out = window.iter().zip(&self.head).map(|(s, tap)| s * tap).sum();
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
    /// For audio at `rate`.
    pub fn new(impulse: &Impulse, rate: u32) -> Self {
        let (left, right, trim) = impulse.at_rate(rate);
        let mut planner = FftPlanner::<f32>::new();
        Convolver {
            left: Channel::new(&left, &mut planner),
            right: Channel::new(&right, &mut planner),
            trim,
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
