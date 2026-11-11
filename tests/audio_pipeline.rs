//! Offline verification of the decode → feed → renderer path (no audio device involved).
//! Fixtures are synthesized by `scripts/gen-fixtures.sh`.
use oneum::audio::decoder::TrackDecoder;
use oneum::audio::render::Renderer;
use oneum::audio::{
    Engine, EngineEvent, NextSlot, NextTrack, PlaybackErrorKind, ReplayGainMode,
    ReplayGainSettings, TrackRequest,
};
use oneum::model::ReplayGainTags;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const BLOCK: usize = 512;
/// Frames affected by the fade-in at the start of every feed.
const FADE_GUARD: usize = 400;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures")).join(name)
}

fn request(item_id: u64, name: &str) -> TrackRequest {
    TrackRequest {
        item_id,
        track_id: item_id as i64,
        path: fixture(name),
        replaygain: ReplayGainTags::default(),
    }
}

/// Decodes a whole file directly (reference data).
fn decode_all(name: &str) -> (u32, usize, Vec<f32>) {
    let mut dec = TrackDecoder::open(&fixture(name)).expect("fixture opens");
    let info = dec.info().clone();
    let mut all = Vec::new();
    let mut buf = Vec::new();
    while dec.next(&mut buf).expect("fixture decodes") {
        all.extend_from_slice(&buf);
    }
    (info.sample_rate, info.channels, all)
}

struct Played {
    /// Interleaved stereo frames that the renderer actually consumed from feeds.
    pcm: Vec<f32>,
    events: Vec<EngineEvent>,
    underruns: u64,
}

impl Played {
    fn frames(&self) -> usize {
        self.pcm.len() / 2
    }
    fn left(&self) -> Vec<f32> {
        self.pcm.as_chunks::<2>().0.iter().map(|f| f[0]).collect()
    }
    fn started(&self) -> Vec<(u64, u64)> {
        self.events
            .iter()
            .filter_map(|e| match e {
                EngineEvent::TrackStarted {
                    item_id, at_frame, ..
                } => Some((*item_id, *at_frame)),
                _ => None,
            })
            .collect()
    }
    fn failures(&self) -> Vec<(u64, PlaybackErrorKind)> {
        self.events
            .iter()
            .filter_map(|e| match e {
                EngineEvent::TrackFailed { item_id, error, .. } => {
                    Some((*item_id, error.kind.clone()))
                }
                _ => None,
            })
            .collect()
    }
}

/// Pulls audio like an output callback would, but never faster than the decoder
/// produces it, so the result is deterministic. Stops when the feed has drained.
fn pump(
    engine: &mut Engine,
    renderer: &mut Renderer,
    queue: &[TrackRequest],
    slot: &NextSlot,
) -> Played {
    let mut pcm = Vec::new();
    let mut events = Vec::new();
    let mut out = vec![0f32; BLOCK * 2];
    let deadline = Instant::now() + Duration::from_secs(600);
    let mut drained = false;
    while !drained {
        assert!(
            Instant::now() < deadline,
            "playback did not finish; events so far: {events:?}"
        );
        let mut new_events = Vec::new();
        engine.poll(&mut new_events);
        for e in &new_events {
            match e {
                EngineEvent::TrackStarted { item_id, .. } => {
                    // Announce what follows this item, as the player does.
                    let pos = queue.iter().position(|r| r.item_id == *item_id);
                    let next = pos.and_then(|p| queue.get(p + 1)).cloned();
                    slot.set(next.map_or(NextTrack::End, NextTrack::Track));
                }
                EngineEvent::Drained { .. } => drained = true,
                _ => {}
            }
        }
        events.extend(new_events);
        match engine.active_buffered() {
            Some((avail, finished)) if avail >= BLOCK as u64 || finished => {
                let (epoch_before, before) = engine.shared().position();
                renderer.render(&mut out, 2);
                let (epoch_after, after) = engine.shared().position();
                if epoch_after == epoch_before {
                    let consumed = (after.saturating_sub(before) as usize).min(BLOCK);
                    pcm.extend_from_slice(&out[..consumed * 2]);
                } else {
                    // The renderer switched feeds inside this block: keep only the new feed's frames.
                    pcm.clear();
                    let consumed = (after as usize).min(BLOCK);
                    pcm.extend_from_slice(&out[(BLOCK - consumed) * 2..]);
                }
            }
            _ => std::thread::sleep(Duration::from_millis(1)),
        }
    }
    Played {
        pcm,
        events,
        underruns: engine.underruns(),
    }
}

fn play_at(
    names: &[&str],
    rate: u32,
    start_ms: u64,
    rg: ReplayGainSettings,
    tags: ReplayGainTags,
    volume: f32,
) -> Played {
    let (mut engine, mut renderer) = Engine::new_offline(rate, volume, rg);
    let queue: Vec<TrackRequest> = names
        .iter()
        .enumerate()
        .map(|(i, n)| TrackRequest {
            replaygain: tags,
            ..request(i as u64 + 1, n)
        })
        .collect();
    let slot = NextSlot::new();
    slot.set(
        queue
            .get(1)
            .cloned()
            .map_or(NextTrack::End, NextTrack::Track),
    );
    engine
        .start_feed(queue[0].clone(), start_ms, slot.clone())
        .expect("feed starts");
    engine.set_playing(true);
    pump(&mut engine, &mut renderer, &queue, &slot)
}

fn play(names: &[&str], rate: u32) -> Played {
    play_at(
        names,
        rate,
        0,
        ReplayGainSettings::default(),
        ReplayGainTags::default(),
        1.0,
    )
}

fn max_abs_diff(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).fold(0f32, |m, (x, y)| m.max((x - y).abs()))
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

/// Peak amplitude of the fixture tones (ffmpeg's mono→stereo up-mix applies −3 dB to 0.5).
const AMP: f32 = std::f32::consts::FRAC_1_SQRT_2 * 0.5;
/// RMS of a sine with peak `AMP`.
const TONE_RMS: f32 = 0.25;

/// The fixture tone: 441 Hz at 44.1 kHz (period of exactly 100 samples).
fn reference_tone(frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|n| AMP * (std::f32::consts::TAU * 441.0 * n as f32 / 44_100.0).sin())
        .collect()
}

#[test]
fn lossless_formats_decode_to_identical_samples() {
    let (rate, ch, wav) = decode_all("gap_a.wav");
    assert_eq!((rate, ch, wav.len() / 2), (44_100, 2, 44_100));
    for name in ["gap_a.flac", "gap_a.alac.m4a", "gap_a.aiff"] {
        let (r, c, pcm) = decode_all(name);
        assert_eq!((r, c), (44_100, 2), "{name}");
        assert_eq!(pcm.len(), wav.len(), "{name} frame count");
        assert_eq!(
            max_abs_diff(&pcm, &wav),
            0.0,
            "{name} must be sample-identical to the WAV"
        );
    }
}

#[test]
fn gapless_lossless_pairs_reproduce_the_continuous_signal() {
    let (_, _, a) = decode_all("gap_a.wav");
    let (_, _, b) = decode_all("gap_b.wav");
    let expected: Vec<f32> = a.iter().chain(b.iter()).copied().collect();
    for (first, second) in [
        ("gap_a.wav", "gap_b.wav"),
        ("gap_a.flac", "gap_b.flac"),
        ("gap_a.alac.m4a", "gap_b.alac.m4a"),
        ("gap_a.aiff", "gap_b.aiff"),
        ("gap_a.flac", "gap_b.wav"),
    ] {
        let played = play(&[first, second], 44_100);
        assert_eq!(played.underruns, 0, "{first}+{second}");
        assert_eq!(
            played.frames(),
            88_200,
            "{first}+{second}: no frames added or lost"
        );
        assert_eq!(
            played.started(),
            vec![(1, 0), (2, 44_100)],
            "{first}+{second}: boundary position"
        );
        // Identical to the uncut signal everywhere after the initial fade-in,
        // including across the track boundary.
        let diff = max_abs_diff(&played.pcm[FADE_GUARD * 2..], &expected[FADE_GUARD * 2..]);
        assert_eq!(
            diff, 0.0,
            "{first}+{second}: output differs from the continuous signal"
        );
    }
}

#[test]
fn gapless_mp3_has_no_added_silence_or_discontinuity() {
    let played = play(&["gap_a.mp3", "gap_b.mp3"], 44_100);
    assert_eq!(played.underruns, 0);
    // Encoder delay and padding are removed: exactly the original number of frames.
    assert_eq!(played.frames(), 88_200);
    assert_eq!(played.started(), vec![(1, 0), (2, 44_100)]);
    let left = played.left();
    let reference = reference_tone(88_200);
    // Around the boundary the lossy signal still follows the continuous tone closely…
    let window = 43_100..45_100;
    let err = max_abs_diff(&left[window.clone()], &reference[window.clone()]);
    assert!(
        err < 0.05,
        "max deviation from the continuous tone near the boundary: {err}"
    );
    // …with no step larger than the tone's own slope allows (AMP·2π·441/44100 ≈ 0.022)…
    let max_step = left[window.clone()]
        .windows(2)
        .fold(0f32, |m, w| m.max((w[1] - w[0]).abs()));
    assert!(
        max_step < 0.03,
        "largest sample-to-sample step near the boundary: {max_step}"
    );
    // …and no dip in level (inserted silence would lower the RMS of a short window).
    let boundary_rms = rms(&left[44_000..44_200]);
    assert!(
        (boundary_rms - TONE_RMS).abs() < 0.01,
        "RMS across the boundary: {boundary_rms}"
    );
    println!(
        "REPORT gap_a.mp3+gap_b.mp3: total_frames=88200, max_error_vs_tone={err:.4}, max_step={max_step:.4}, boundary_rms={boundary_rms:.4}"
    );
}

/// Not a guarantee, just a measurement that is recorded in docs/TESTING.md.
#[test]
fn report_boundary_behaviour_of_other_lossy_formats() {
    for (first, second) in [
        ("gap_a.aac.m4a", "gap_b.aac.m4a"),
        ("gap_a.ogg", "gap_b.ogg"),
    ] {
        let played = play(&[first, second], 44_100);
        assert_eq!(played.underruns, 0);
        assert!(
            played.failures().is_empty(),
            "{first}: {:?}",
            played.failures()
        );
        let frames = played.frames();
        assert!(
            (86_000..=93_000).contains(&frames),
            "{first}: {frames} frames"
        );
        let left = played.left();
        let boundary = played.started().get(1).map_or(0, |s| s.1 as usize);
        let lo = boundary.saturating_sub(1000);
        let hi = (boundary + 1000).min(left.len());
        let max_step = left[lo..hi]
            .windows(2)
            .fold(0f32, |m, w| m.max((w[1] - w[0]).abs()));
        let min_rms = left[lo..hi].chunks(100).map(rms).fold(f32::MAX, f32::min);
        println!(
            "REPORT {first}+{second}: total_frames={frames} (source 88200), boundary_at={boundary}, \
             max_step_near_boundary={max_step:.4}, min_100frame_rms_near_boundary={min_rms:.4}"
        );
    }
}

#[test]
fn resampling_keeps_duration_pitch_and_level() {
    for (name, src_rate, out_rate) in [
        ("tone_48k.wav", 48_000u32, 44_100u32),
        ("tone_96k_24bit.flac", 96_000, 44_100),
        ("gap_a.wav", 44_100, 48_000),
    ] {
        let played = play(&[name], out_rate);
        assert_eq!(played.underruns, 0);
        let (_, _, src) = decode_all(name);
        let expected =
            ((src.len() / 2) as u64 * u64::from(out_rate)).div_ceil(u64::from(src_rate)) as usize;
        assert_eq!(played.frames(), expected, "{name}: resampled length");
        let left = played.left();
        let mid = &left[4000..left.len() - 4000];
        let peak = mid.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!((peak - AMP).abs() < 0.01, "{name}: peak {peak}");
        let crossings = mid.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count() as f32;
        let freq = crossings / (mid.len() as f32 / out_rate as f32);
        let want = if name == "gap_a.wav" { 441.0 } else { 1000.0 };
        assert!((freq - want).abs() < 2.0, "{name}: frequency {freq}");
    }
}

#[test]
fn gapless_also_holds_through_the_resampler() {
    // Two halves of one tone, resampled 44.1 → 48 kHz with one continuous resampler.
    let played = play(&["gap_a.flac", "gap_b.flac"], 48_000);
    assert_eq!(played.frames(), 96_000);
    let left = played.left();
    let boundary = played.started()[1].1 as usize;
    assert!(
        (boundary as i64 - 48_000).abs() <= 1,
        "boundary reported at {boundary}"
    );
    let max_step = left[47_000..49_000]
        .windows(2)
        .fold(0f32, |m, w| m.max((w[1] - w[0]).abs()));
    assert!(
        max_step < 0.022,
        "largest step near the boundary: {max_step}"
    );
    // 441 Hz at 48 kHz has a period of 108.84 samples: use a window of ten periods.
    let boundary_rms = rms(&left[47_456..48_544]);
    assert!(
        (boundary_rms - TONE_RMS).abs() < 0.003,
        "RMS across the boundary: {boundary_rms}"
    );
}

#[test]
fn mono_is_played_on_both_channels() {
    let played = play(&["tone_mono.wav"], 44_100);
    assert_eq!(played.frames(), 44_100);
    let frame = &played.pcm[20_000..20_002];
    assert_eq!(frame[0], frame[1]);
    assert!(
        played.left()[1000..]
            .iter()
            .fold(0f32, |m, s| m.max(s.abs()))
            > 0.49,
        "mono source is not attenuated"
    );
}

#[test]
fn seek_starts_at_the_requested_frame() {
    let (_, _, wav) = decode_all("gap_a.wav");
    for name in ["gap_a.wav", "gap_a.flac", "gap_a.alac.m4a", "gap_a.aiff"] {
        let played = play_at(
            &[name],
            44_100,
            500,
            ReplayGainSettings::default(),
            ReplayGainTags::default(),
            1.0,
        );
        assert_eq!(
            played.frames(),
            22_050,
            "{name}: frames after seeking to 500 ms"
        );
        let expected = &wav[22_050 * 2..];
        let diff = max_abs_diff(&played.pcm[FADE_GUARD * 2..], &expected[FADE_GUARD * 2..]);
        assert_eq!(
            diff, 0.0,
            "{name}: audio after seek must match the source exactly"
        );
    }
    // Lossy formats with trimmed encoder delay: same position within codec tolerance.
    let reference = reference_tone(44_100);
    for (name, total) in [("gap_a.mp3", 44_100i64), ("gap_a.ogg", 44_160)] {
        let played = play_at(
            &[name],
            44_100,
            500,
            ReplayGainSettings::default(),
            ReplayGainTags::default(),
            1.0,
        );
        let frames = played.frames() as i64;
        assert_eq!(
            frames,
            total - 22_050,
            "{name}: frames after seeking to 500 ms"
        );
        let left = played.left();
        let err = max_abs_diff(
            &left[2000..12_000],
            &reference[22_050 + 2000..22_050 + 12_000],
        );
        println!("REPORT seek {name}: frames={frames}, max_error_vs_tone={err:.4}");
        assert!(
            err < 0.03,
            "{name}: audio after seek is misaligned (error {err})"
        );
    }
    // AAC keeps its encoder delay (not trimmed by the decoder), so only the length is checked.
    let aac = play_at(
        &["gap_a.aac.m4a"],
        44_100,
        500,
        ReplayGainSettings::default(),
        ReplayGainTags::default(),
        1.0,
    );
    println!(
        "REPORT seek gap_a.aac.m4a: frames={} (stream is 46080 frames long incl. encoder delay)",
        aac.frames()
    );
    assert_eq!(aac.frames(), 46_080 - 22_050);
}

#[test]
fn seeking_past_the_end_finishes_cleanly() {
    let played = play_at(
        &["gap_a.wav"],
        44_100,
        60_000,
        ReplayGainSettings::default(),
        ReplayGainTags::default(),
        1.0,
    );
    assert_eq!(played.frames(), 0);
    assert!(played.failures().is_empty());
}

#[test]
fn replaygain_and_volume_scale_the_output() {
    let tags = ReplayGainTags {
        track_gain_db: Some(-6.0206),
        track_peak: Some(AMP),
        ..Default::default()
    };
    let rg = ReplayGainSettings {
        mode: ReplayGainMode::Track,
        preamp_db: 0.0,
        prevent_clipping: true,
    };
    let played = play_at(&["gap_a.wav"], 44_100, 0, rg, tags, 1.0);
    let peak = played.left()[1000..]
        .iter()
        .fold(0f32, |m, s| m.max(s.abs()));
    assert!(
        (peak - AMP * 0.5).abs() < 0.001,
        "ReplayGain -6 dB: peak {peak}"
    );

    let off = play_at(
        &["gap_a.wav"],
        44_100,
        0,
        ReplayGainSettings::default(),
        tags,
        1.0,
    );
    let peak = off.left()[1000..].iter().fold(0f32, |m, s| m.max(s.abs()));
    assert!((peak - AMP).abs() < 0.001, "ReplayGain off: peak {peak}");

    // Volume 0.5 on the cubic curve is a gain of 0.125.
    let quiet = play_at(
        &["gap_a.wav"],
        44_100,
        0,
        ReplayGainSettings::default(),
        ReplayGainTags::default(),
        0.5,
    );
    let peak = quiet.left()[1000..]
        .iter()
        .fold(0f32, |m, s| m.max(s.abs()));
    assert!((peak - AMP * 0.125).abs() < 0.001, "volume: peak {peak}");
}

#[test]
fn replaygain_never_clips_with_prevention() {
    let tags = ReplayGainTags {
        track_gain_db: Some(12.0),
        track_peak: Some(AMP),
        ..Default::default()
    };
    let rg = ReplayGainSettings {
        mode: ReplayGainMode::Track,
        preamp_db: 6.0,
        prevent_clipping: true,
    };
    let played = play_at(&["gap_a.wav"], 44_100, 0, rg, tags, 1.0);
    let peak = played.pcm.iter().fold(0f32, |m, s| m.max(s.abs()));
    assert!(
        peak <= 1.0 && peak > 0.99,
        "gain is capped at 1/peak: {peak}"
    );
}

#[test]
fn broken_files_fail_without_stopping_the_engine() {
    let missing = play(&["does_not_exist.flac"], 44_100);
    assert_eq!(
        missing.failures(),
        vec![(1, PlaybackErrorKind::FileMissing)]
    );
    assert_eq!(missing.frames(), 0);

    for name in ["garbage.mp3", "empty.flac"] {
        let played = play(&[name], 44_100);
        let failures = played.failures();
        assert_eq!(failures.len(), 1, "{name}: {failures:?}");
        assert!(
            matches!(
                failures[0].1,
                PlaybackErrorKind::Unsupported | PlaybackErrorKind::Corrupt
            ),
            "{name}: {failures:?}"
        );
    }

    // A truncated file plays what is there and ends.
    let truncated = play(&["truncated.mp3"], 44_100);
    assert!(
        truncated.frames() > 5_000 && truncated.frames() < 44_100,
        "{}",
        truncated.frames()
    );

    // Damage in the middle of a file is skipped and the next track still plays gaplessly after it.
    let damaged = play(&["damaged_middle.flac", "gap_b.wav"], 44_100);
    let started = damaged.started();
    assert_eq!(
        started.len(),
        2,
        "next track must start: {:?}",
        damaged.events
    );
    assert!(damaged.frames() > 44_100, "{}", damaged.frames());
    let (_, _, b) = decode_all("gap_b.wav");
    let tail = &damaged.pcm[damaged.pcm.len() - 20_000..];
    assert_eq!(max_abs_diff(tail, &b[b.len() - 20_000..]), 0.0);
}

#[test]
fn rapid_restarts_never_mix_in_stale_audio() {
    let (mut engine, mut renderer) =
        Engine::new_offline(44_100, 1.0, ReplayGainSettings::default());
    let slot = NextSlot::new();
    slot.set(NextTrack::End);
    let mut last_epoch = 0;
    // A burst of seeks/track changes; only the last one may ever be heard.
    for i in 0..25u64 {
        let name = if i % 2 == 0 {
            "tone_mono.wav"
        } else {
            "gap_a.wav"
        };
        last_epoch = engine
            .start_feed(request(100 + i, name), i * 10, slot.clone())
            .expect("feed");
        if i % 5 == 0 {
            std::thread::sleep(Duration::from_millis(3));
            engine.poll(&mut Vec::new());
        }
    }
    last_epoch = {
        let e = engine
            .start_feed(request(999, "gap_a.wav"), 250, slot.clone())
            .expect("feed");
        assert!(e > last_epoch);
        e
    };
    engine.set_playing(true);
    let queue = [request(999, "gap_a.wav")];
    let played = pump(&mut engine, &mut renderer, &queue, &slot);
    for e in &played.events {
        let epoch = match e {
            EngineEvent::TrackStarted { epoch, .. }
            | EngineEvent::TrackFailed { epoch, .. }
            | EngineEvent::FeedFinished { epoch, .. }
            | EngineEvent::Drained { epoch } => *epoch,
            _ => last_epoch,
        };
        assert_eq!(
            epoch, last_epoch,
            "event from a stale feed leaked through: {e:?}"
        );
    }
    let (_, _, wav) = decode_all("gap_a.wav");
    let expected = &wav[11_025 * 2..];
    assert_eq!(played.frames(), 44_100 - 11_025);
    assert_eq!(
        max_abs_diff(&played.pcm[FADE_GUARD * 2..], &expected[FADE_GUARD * 2..]),
        0.0
    );
}

/// Verification against real music files (not part of the normal run):
///
///   ONEUM_REAL_DIR=/path/to/flacs cargo test --release --test audio_pipeline real_flac -- --ignored --nocapture
///
/// Every `.flac` in the folder is decoded completely and compared sample by sample with
/// what `ffmpeg` decodes; then the two shortest files are played back to back through
/// the engine and compared with their plain concatenation. Files are only read.
#[test]
#[ignore = "needs ONEUM_REAL_DIR and ffmpeg"]
fn real_flac_files_decode_and_join_exactly() {
    let Some(dir) = std::env::var_os("ONEUM_REAL_DIR").map(PathBuf::from) else {
        println!("ONEUM_REAL_DIR not set; skipping");
        return;
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("folder is readable")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("flac"))
        })
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no .flac files in {}", dir.display());

    let decode = |path: &PathBuf| -> (u32, usize, Vec<f32>) {
        let mut dec = TrackDecoder::open(path).expect("file opens");
        let info = dec.info().clone();
        let (mut all, mut buf) = (Vec::new(), Vec::new());
        while dec.next(&mut buf).expect("file decodes") {
            all.extend_from_slice(&buf);
        }
        assert_eq!(dec.skipped_packets, 0, "no damaged packets expected");
        (info.sample_rate, info.channels, all)
    };

    let mut decoded: Vec<(PathBuf, u32, Vec<f32>)> = Vec::new();
    let mut total_seconds = 0.0;
    for path in &files {
        let (rate, channels, pcm) = decode(path);
        assert_eq!(channels, 2, "stereo expected");
        let reference = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(path)
            .args(["-f", "s16le", "-acodec", "pcm_s16le", "-"])
            .output()
            .expect("ffmpeg runs");
        assert!(reference.status.success(), "ffmpeg failed on a file");
        let reference: Vec<i16> = reference
            .stdout
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect();
        assert_eq!(
            pcm.len(),
            reference.len(),
            "sample count differs from ffmpeg"
        );
        let mismatches = pcm
            .iter()
            .zip(&reference)
            .filter(|(a, b)| (**a * 32768.0).round() as i32 != i32::from(**b))
            .count();
        assert_eq!(mismatches, 0, "decoded samples differ from ffmpeg");
        let seconds = pcm.len() as f64 / 2.0 / f64::from(rate);
        total_seconds += seconds;
        println!(
            "REPORT real file #{}: {rate} Hz, {:.1} s, {} samples identical to ffmpeg",
            decoded.len() + 1,
            seconds,
            pcm.len()
        );
        decoded.push((path.clone(), rate, pcm));
    }
    println!(
        "REPORT {} files, {:.1} minutes decoded, all identical to ffmpeg",
        decoded.len(),
        total_seconds / 60.0
    );

    if decoded.len() < 2 {
        return;
    }
    // Gapless join of the two shortest files through the real feed/renderer path.
    decoded.sort_by_key(|(_, _, pcm)| pcm.len());
    let (first, second) = (&decoded[0], &decoded[1]);
    assert_eq!(
        first.1, second.1,
        "same sample rate expected for the join test"
    );
    let (mut engine, mut renderer) =
        Engine::new_offline(first.1, 1.0, ReplayGainSettings::default());
    let queue: Vec<TrackRequest> = [&first.0, &second.0]
        .iter()
        .enumerate()
        .map(|(i, path)| TrackRequest {
            item_id: i as u64 + 1,
            track_id: i as i64 + 1,
            path: (*path).clone(),
            replaygain: ReplayGainTags::default(),
        })
        .collect();
    let slot = NextSlot::new();
    slot.set(NextTrack::Track(queue[1].clone()));
    engine
        .start_feed(queue[0].clone(), 0, slot.clone())
        .expect("feed starts");
    engine.set_playing(true);
    let played = pump(&mut engine, &mut renderer, &queue, &slot);
    let expected: Vec<f32> = first.2.iter().chain(second.2.iter()).copied().collect();
    assert_eq!(played.underruns, 0);
    assert_eq!(
        played.pcm.len(),
        expected.len(),
        "no frames added or lost across the boundary"
    );
    assert_eq!(
        played.started()[1].1 as usize,
        first.2.len() / 2,
        "boundary at the end of the first file"
    );
    let diff = max_abs_diff(&played.pcm[FADE_GUARD * 2..], &expected[FADE_GUARD * 2..]);
    assert_eq!(
        diff, 0.0,
        "joined playback differs from the two files back to back"
    );
    println!(
        "REPORT gapless join of two real files: {} frames, boundary at frame {}, identical to plain concatenation",
        expected.len() / 2,
        first.2.len() / 2
    );
}
