//! Offline verification of the decode → feed → renderer path (no audio device involved).
//! Fixtures are synthesized by `scripts/gen-fixtures.sh`.
use oneum::audio::decoder::TrackDecoder;
use oneum::audio::render::Renderer;
use oneum::audio::{
    Engine, EngineEvent, NextSlot, NextTrack, OutputSettings, PlaybackErrorKind, ReplayGainMode,
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
        crossfade_ms: 0,
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
    /// Frames the peak limiter turned down.
    limited: u64,
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
        limited: engine.limited_frames(),
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
            crossfade_ms: 0,
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

/// Like [`play_at`] with defaults, but with memory playback switched on.
fn play_from_memory(names: &[&str], rate: u32, start_ms: u64) -> Played {
    let (mut engine, mut renderer) = Engine::new_offline(rate, 1.0, ReplayGainSettings::default());
    engine.set_output_settings(OutputSettings {
        memory_playback: true,
        ..OutputSettings::default()
    });
    let queue: Vec<TrackRequest> = names
        .iter()
        .enumerate()
        .map(|(i, n)| request(i as u64 + 1, n))
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

/// WavPack is lossless: what it decodes to is what was packed, sample for sample
/// (16 bits in stereo and in mono, 24 bits at 96 kHz).
#[test]
fn wavpack_decodes_to_the_samples_that_were_packed() {
    for (packed, original) in [
        ("gap_a.wv", "gap_a.wav"),
        ("gap_b.wv", "gap_b.wav"),
        ("tone_mono.wv", "tone_mono.wav"),
        ("tone_96k_24bit.wv", "tone_96k_24bit.flac"),
    ] {
        let (rate, channels, pcm) = decode_all(packed);
        let (r, c, expected) = decode_all(original);
        assert_eq!((rate, channels), (r, c), "{packed}");
        assert_eq!(pcm.len(), expected.len(), "{packed} frame count");
        assert_eq!(
            max_abs_diff(&pcm, &expected),
            0.0,
            "{packed} must be sample-identical to {original}"
        );
        let dec = TrackDecoder::open(&fixture(packed)).expect("opens");
        assert_eq!(dec.info().codec, "wavpack");
        assert_eq!(
            dec.info().total_frames,
            Some((expected.len() / channels) as u64),
            "{packed} states its length"
        );
    }
}

/// A position in a WavPack file is found exactly: what comes after a seek is the
/// file from that frame on.
#[test]
fn wavpack_seeks_to_the_frame() {
    let (_, _, whole) = decode_all("gap_a.wv");
    let mut dec = TrackDecoder::open(&fixture("gap_a.wv")).expect("opens");
    let mut buf = Vec::new();
    for frame in [30_000u64, 0, 44_099, 22_050, 1] {
        dec.seek(frame).expect("seeks");
        let mut rest = Vec::new();
        while dec.next(&mut buf).expect("decodes") {
            rest.extend_from_slice(&buf);
        }
        assert_eq!(rest, whole[frame as usize * 2..], "from frame {frame}");
    }
    // Past the end there is nothing.
    dec.seek(50_000).expect("seeks");
    assert!(!dec.next(&mut buf).expect("decodes"));
}

/// Opus is always 48 kHz. What the file says about its start (the pre-skip) and its
/// end is taken out: a second of tone is a second of tone, close to the original.
#[test]
fn opus_decodes_to_the_length_that_was_encoded() {
    let (rate, channels, pcm) = decode_all("gap_a.opus");
    assert_eq!((rate, channels), (48_000, 2));
    assert_eq!(
        pcm.len() / 2,
        48_000,
        "exactly the frames that were encoded"
    );
    let dec = TrackDecoder::open(&fixture("gap_a.opus")).expect("opens");
    assert_eq!(dec.info().codec, "opus");
    assert_eq!(dec.info().total_frames, Some(48_000));
    let left: Vec<f32> = pcm.as_chunks::<2>().0.iter().map(|f| f[0]).collect();
    let reference: Vec<f32> = (0..48_000)
        .map(|n| AMP * (std::f32::consts::TAU * 480.0 * n as f32 / 48_000.0).sin())
        .collect();
    // After the first milliseconds (the codec starts from silence).
    let err = max_abs_diff(&left[2_400..], &reference[2_400..]);
    assert!(
        err < 0.02,
        "deviation from the tone that was encoded: {err}"
    );
    println!("REPORT gap_a.opus: frames=48000, max_error_vs_tone={err:.4}");
}

/// Two halves of a continuous tone as Opus files, played one after the other: no
/// frame added or lost, no step and no dip where they meet.
#[test]
fn gapless_opus_has_no_added_silence_or_discontinuity() {
    let played = play(&["gap_a.opus", "gap_b.opus"], 48_000);
    assert_eq!(played.underruns, 0);
    assert_eq!(
        played.frames(),
        96_000,
        "exactly the original number of frames"
    );
    assert_eq!(played.started(), vec![(1, 0), (2, 48_000)]);
    let left = played.left();
    let reference: Vec<f32> = (0..96_000)
        .map(|n| AMP * (std::f32::consts::TAU * 480.0 * n as f32 / 48_000.0).sin())
        .collect();
    let window = 47_000..49_000;
    let err = max_abs_diff(&left[window.clone()], &reference[window.clone()]);
    assert!(
        err < 0.05,
        "deviation from the continuous tone near the boundary: {err}"
    );
    // The tone's own slope allows AMP·2π·480/48000 ≈ 0.022 from sample to sample.
    let max_step = left[window]
        .windows(2)
        .fold(0f32, |m, w| m.max((w[1] - w[0]).abs()));
    assert!(
        max_step < 0.03,
        "largest step near the boundary: {max_step}"
    );
    let boundary_rms = rms(&left[47_900..48_100]);
    assert!(
        (boundary_rms - TONE_RMS).abs() < 0.01,
        "RMS across the boundary: {boundary_rms}"
    );
    println!(
        "REPORT gap_a.opus+gap_b.opus: total_frames=96000, max_error_vs_tone={err:.4}, max_step={max_step:.4}, boundary_rms={boundary_rms:.4}"
    );
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
        ("gap_a.wv", "gap_b.wv"),
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
/// AAC: the priming in front of every track and the padding at its end are taken out,
/// as the file's edit list says, so that two halves of a continuous tone join the way
/// they were cut.
#[test]
fn gapless_aac_has_no_added_silence_or_discontinuity() {
    let played = play(&["gap_a.aac.m4a", "gap_b.aac.m4a"], 44_100);
    assert_eq!(played.underruns, 0);
    assert_eq!(
        played.frames(),
        88_200,
        "exactly the original number of frames"
    );
    assert_eq!(played.started(), vec![(1, 0), (2, 44_100)]);
    let left = played.left();
    let reference = reference_tone(88_200);
    let window = 43_100..45_100;
    let err = max_abs_diff(&left[window.clone()], &reference[window.clone()]);
    assert!(
        err < 0.05,
        "max deviation from the continuous tone near the boundary: {err}"
    );
    let max_step = left[window.clone()]
        .windows(2)
        .fold(0f32, |m, w| m.max((w[1] - w[0]).abs()));
    assert!(
        max_step < 0.03,
        "largest sample-to-sample step near the boundary: {max_step}"
    );
    let boundary_rms = rms(&left[44_000..44_200]);
    assert!(
        (boundary_rms - TONE_RMS).abs() < 0.01,
        "RMS across the boundary: {boundary_rms}"
    );
    // The very start of the first track is the tone, not priming silence.
    let start_err = max_abs_diff(&left[FADE_GUARD..2_000], &reference[FADE_GUARD..2_000]);
    assert!(start_err < 0.05, "deviation at the start: {start_err}");
    println!(
        "REPORT gap_a.aac.m4a+gap_b.aac.m4a: total_frames=88200, max_error_vs_tone={err:.4}, max_step={max_step:.4}, boundary_rms={boundary_rms:.4}"
    );
}

/// Seeking in an AAC file lands where the position says, counted in the music (the
/// priming is not part of the count), and the track's length is the music's.
#[test]
fn aac_positions_are_counted_in_the_music() {
    let mut decoder = TrackDecoder::open(&fixture("gap_a.aac.m4a")).expect("opens");
    assert_eq!(decoder.info().total_frames, Some(44_100));
    assert!(decoder.info().has_gapless_info);
    let (mut whole, mut buf) = (Vec::new(), Vec::new());
    while decoder.next(&mut buf).expect("decodes") {
        whole.extend_from_slice(&buf);
    }
    assert_eq!(whole.len() / 2, 44_100);
    // From the middle: the same samples as in the straight run (after the few
    // packets the decoder needs to settle after a jump).
    for target in [0u64, 1_000, 22_050, 43_000] {
        // A fresh decoder per jump, as playback has (a feed opens its file and seeks).
        let mut decoder = TrackDecoder::open(&fixture("gap_a.aac.m4a")).expect("opens");
        decoder.seek(target).expect("seeks");
        let mut part = Vec::new();
        while decoder.next(&mut buf).expect("decodes") {
            part.extend_from_slice(&buf);
        }
        assert_eq!(part.len() / 2, 44_100 - target as usize, "from {target}");
        let settle = 2 * 2_048.min(part.len() / 2);
        let expected = &whole[target as usize * 2 + settle..];
        let difference = max_abs_diff(&part[settle..], expected);
        assert!(difference < 1e-3, "from {target}: {difference}");
    }
    // Past the end: nothing.
    let mut decoder = TrackDecoder::open(&fixture("gap_a.aac.m4a")).expect("opens");
    decoder.seek(50_000).expect("seeks");
    assert!(!decoder.next(&mut buf).expect("decodes") || buf.is_empty());
}

/// AAC from the two kinds of encoder there are: ffmpeg's (which writes an edit list) and
/// Apple's own (`afconvert`, as Music and iTunes: no edit list, an `iTunSMPB` note). Of
/// a tone of an odd length, exactly the frames that went in come out, in the same place.
#[test]
fn aac_is_trimmed_to_the_music_whoever_encoded_it() {
    const FRAMES: usize = 54_441;
    let tone: Vec<i16> = (0..FRAMES)
        .map(|n| {
            let phase = 2.0 * std::f64::consts::PI * 441.0 * n as f64 / 44_100.0;
            (0.35 * 32_767.0 * phase.sin()) as i16
        })
        .collect();
    let dir = tempfile::tempdir().expect("temp dir");
    let source = dir.path().join("tone.wav");
    write_wav(&source, 44_100, &tone);
    // (file name, program, arguments before the input, arguments after it)
    let encoders: [(&str, &str, &[&str], &[&str]); 2] = [
        (
            "ffmpeg.m4a",
            "ffmpeg",
            &["-v", "error", "-y", "-i"],
            &["-c:a", "aac", "-b:a", "160k"],
        ),
        (
            "apple.m4a",
            "afconvert",
            &["-f", "m4af", "-d", "aac", "-b", "160000"],
            &[],
        ),
    ];
    let mut checked = 0;
    for (name, program, before, after) in encoders {
        let out = dir.path().join(name);
        let made = std::process::Command::new(program)
            .args(before)
            .arg(&source)
            .args(after)
            .arg(&out)
            .stdin(std::process::Stdio::null())
            .output()
            .is_ok_and(|o| o.status.success());
        if !made {
            eprintln!("note: {program} is not available; {name} not checked");
            continue;
        }
        let (rate, channels, audio) = decode_file(&out);
        assert_eq!((rate, channels), (44_100, 2), "{name}");
        assert_eq!(audio.len() / 2, FRAMES, "{name}: frames of music");
        // In place: sample for sample against what was encoded (a lossy copy of it).
        let worst = (1_000..FRAMES - 1_000).fold(0f32, |worst, n| {
            worst.max((audio[n * 2] - f32::from(tone[n]) / 32_768.0).abs())
        });
        assert!(worst < 0.02, "{name}: {worst} away from the source");
        let info = TrackDecoder::open(&out).expect("opens").info().clone();
        assert_eq!(info.total_frames, Some(FRAMES as u64), "{name}");
        checked += 1;
    }
    if checked == 0 {
        eprintln!("note: no AAC encoder available; nothing checked");
    }
}

#[test]
fn report_boundary_behaviour_of_other_lossy_formats() {
    for (first, second) in [("gap_a.ogg", "gap_b.ogg"), ("gap_a.mp2", "gap_b.mp2")] {
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
    for (name, total) in [
        ("gap_a.mp3", 44_100i64),
        ("gap_a.aac.m4a", 44_100),
        ("gap_a.ogg", 44_160),
    ] {
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
        assert_eq!(dec.skipped_packets(), 0, "no damaged packets expected");
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
            crossfade_ms: 0,
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

/// Memory playback changes where the bytes come from and nothing else: every format
/// renders the very same samples as when it is streamed from the file, across a track
/// boundary and after a seek.
#[test]
fn memory_playback_renders_the_same_samples_as_streaming() {
    for ext in [
        "wav", "flac", "aiff", "alac.m4a", "mp3", "mp2", "aac.m4a", "ogg",
    ] {
        let (a, b) = (format!("gap_a.{ext}"), format!("gap_b.{ext}"));
        let names = [a.as_str(), b.as_str()];
        for (rate, start_ms) in [(44_100, 0), (44_100, 400), (48_000, 0)] {
            let streamed = play_at(
                &names,
                rate,
                start_ms,
                ReplayGainSettings::default(),
                ReplayGainTags::default(),
                1.0,
            );
            let memory = play_from_memory(&names, rate, start_ms);
            assert_eq!(
                memory.pcm.len(),
                streamed.pcm.len(),
                "{ext} at {rate} Hz from {start_ms} ms: length"
            );
            assert_eq!(
                max_abs_diff(&memory.pcm, &streamed.pcm),
                0.0,
                "{ext} at {rate} Hz from {start_ms} ms: samples"
            );
            assert_eq!(memory.underruns, 0);
            let in_memory: Vec<bool> = memory
                .events
                .iter()
                .filter_map(|e| match e {
                    EngineEvent::TrackStarted { info, .. } => Some(info.in_memory),
                    _ => None,
                })
                .collect();
            assert_eq!(
                in_memory,
                vec![true, true],
                "{ext}: both tracks from memory"
            );
            assert!(streamed.events.iter().all(|e| !matches!(
                e,
                EngineEvent::TrackStarted { info, .. } if info.in_memory
            )));
        }
    }
}

/// MPEG Layer II decodes to the tone that was encoded: right length (give or take the
/// codec's padding), right pitch, right level.
#[test]
fn mpeg_layer_2_plays() {
    let (rate, channels, pcm) = decode_all("gap_a.mp2");
    assert_eq!((rate, channels), (44_100, 2));
    let frames = pcm.len() / 2;
    assert!((44_100..=47_000).contains(&frames), "{frames} frames");
    let left: Vec<f32> = pcm.as_chunks::<2>().0.iter().map(|f| f[0]).collect();
    // Away from the codec's start-up: the 441 Hz tone at its level.
    let middle = &left[8_820..39_690];
    assert!((rms(middle) - TONE_RMS).abs() < 0.01, "rms {}", rms(middle));
    let crossings = middle
        .windows(2)
        .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
        .count();
    let hz = crossings as f32 / (middle.len() as f32 / 44_100.0);
    assert!((hz - 441.0).abs() < 3.0, "{hz} Hz");
    let played = play(&["gap_a.mp2", "gap_b.mp2"], 48_000);
    assert_eq!(played.underruns, 0);
    assert!(played.failures().is_empty(), "{:?}", played.failures());
}

/// Writes a 16-bit stereo WAV with the same samples on both channels.
fn write_wav(path: &std::path::Path, rate: u32, samples: &[i16]) {
    let data_len = (samples.len() * 4) as u32;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 4).to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        bytes.extend_from_slice(&s.to_le_bytes());
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, bytes).expect("wav written");
}

/// Plays one file given by path at `rate`.
fn play_file(path: &std::path::Path, rate: u32) -> Played {
    let (mut engine, mut renderer) = Engine::new_offline(rate, 1.0, ReplayGainSettings::default());
    let queue = [TrackRequest {
        item_id: 1,
        track_id: 1,
        path: path.to_path_buf(),
        replaygain: ReplayGainTags::default(),
        crossfade_ms: 0,
    }];
    let slot = NextSlot::new();
    slot.set(NextTrack::End);
    engine
        .start_feed(queue[0].clone(), 0, slot.clone())
        .expect("feed starts");
    engine.set_playing(true);
    pump(&mut engine, &mut renderer, &queue, &slot)
}

/// Everything in `signal` that is not the sine of `freq` Hz, relative to that sine, in dB.
fn distortion_db(signal: &[f32], rate: u32, freq: f64) -> f64 {
    let w = freq * std::f64::consts::TAU / f64::from(rate);
    let (mut ss, mut cc, mut sc, mut xs, mut xc) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (k, &v) in signal.iter().enumerate() {
        let (s, c) = (k as f64 * w).sin_cos();
        ss += s * s;
        cc += c * c;
        sc += s * c;
        xs += f64::from(v) * s;
        xc += f64::from(v) * c;
    }
    let det = ss * cc - sc * sc;
    let a = (xs * cc - xc * sc) / det;
    let b = (xc * ss - xs * sc) / det;
    let rest: f64 = signal
        .iter()
        .enumerate()
        .map(|(k, &v)| {
            let (s, c) = (k as f64 * w).sin_cos();
            (f64::from(v) - a * s - b * c).powi(2)
        })
        .sum();
    10.0 * (rest / signal.len() as f64 / ((a * a + b * b) / 2.0)).log10()
}

#[test]
fn peaks_between_the_samples_are_not_clipped_when_the_rate_is_converted() {
    // The textbook inter-sample over: a tone at a quarter of the sample rate whose
    // samples all sit at full scale, 45° away from its crests. The file never exceeds
    // full scale; the wave it describes does, by 3 dB.
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("intersample.wav");
    let pattern = [i16::MAX, i16::MAX, -i16::MAX, -i16::MAX];
    let samples: Vec<i16> = pattern.iter().copied().cycle().take(2 * 44_100).collect();
    write_wav(&path, 44_100, &samples);

    // At its own rate the file goes out untouched: nothing to protect, bit for bit.
    let own = play_file(&path, 44_100);
    assert_eq!(own.limited, 0);
    assert_eq!(own.frames(), samples.len());
    let expected: Vec<f32> = samples.iter().map(|s| f32::from(*s) / 32_768.0).collect();
    assert_eq!(
        max_abs_diff(&own.left()[FADE_GUARD..], &expected[FADE_GUARD..]),
        0.0
    );

    // Converted to 48 kHz the crests come out. They are brought down to full scale
    // instead of being cut off: what is heard is still the tone.
    let converted = play_file(&path, 48_000);
    assert_eq!(converted.frames(), 2 * 48_000);
    assert_eq!(converted.underruns, 0);
    assert!(converted.limited > 48_000, "limited {}", converted.limited);
    let left = converted.left();
    let peak = left.iter().fold(0f32, |m, s| m.max(s.abs()));
    assert!(peak <= 1.0 && peak > 0.99, "peak {peak}");
    let settled = &left[48_000..2 * 48_000 - 4_800];
    let limited_db = distortion_db(settled, 48_000, 11_025.0);
    assert!(limited_db < -50.0, "distortion {limited_db:.1} dB");
    // What a device does with the same samples when nothing protects them.
    let unprotected: Vec<f32> = settled
        .iter()
        .map(|s| (s * std::f32::consts::SQRT_2).clamp(-1.0, 1.0))
        .collect();
    let clipped_db = distortion_db(&unprotected, 48_000, 11_025.0);
    assert!(
        clipped_db > limited_db + 25.0,
        "clipped {clipped_db:.1} dB, limited {limited_db:.1} dB"
    );
}

#[test]
fn gain_above_full_scale_is_limited_not_clipped() {
    // ReplayGain asks for +12 dB and clipping prevention is switched off: the tone
    // would peak at 1.41.
    let tags = ReplayGainTags {
        track_gain_db: Some(12.0),
        track_peak: Some(AMP),
        ..Default::default()
    };
    let rg = ReplayGainSettings {
        mode: ReplayGainMode::Track,
        preamp_db: 0.0,
        prevent_clipping: false,
    };
    let played = play_at(&["gap_a.wav"], 44_100, 0, rg, tags, 1.0);
    assert!(played.limited > 0);
    let left = played.left();
    let peak = left.iter().fold(0f32, |m, s| m.max(s.abs()));
    assert!(peak <= 1.0 && peak > 0.99, "peak {peak}");
    let settled = &left[left.len() / 2..left.len() - 4_410];
    let db = distortion_db(settled, 44_100, 441.0);
    assert!(db < -50.0, "distortion {db:.1} dB");

    // With gain that stays within full scale the limiter has nothing to do.
    let quiet = play_at(
        &["gap_a.wav"],
        44_100,
        0,
        ReplayGainSettings {
            prevent_clipping: true,
            ..rg
        },
        tags,
        1.0,
    );
    assert_eq!(quiet.limited, 0);
}

#[test]
fn a_track_played_from_its_beginning_is_the_file_from_the_very_first_sample() {
    // No fade-in over the start of a track: every sample, the first one included, is
    // the file's own.
    for name in ["gap_b.wav", "gap_b.flac", "gap_a.aiff", "gap_a.alac.m4a"] {
        let (rate, channels, expected) = decode_all(name);
        assert_eq!((rate, channels), (44_100, 2), "{name}");
        let played = play(&[name], 44_100);
        assert_eq!(played.limited, 0, "{name}");
        assert_eq!(played.pcm.len(), expected.len(), "{name}");
        assert!(
            played
                .pcm
                .iter()
                .zip(&expected)
                .all(|(a, b)| a.to_bits() == b.to_bits()),
            "{name}: not bit for bit"
        );
    }
    assert!(
        decode_all("gap_b.wav").2[..200]
            .iter()
            .any(|s| s.abs() > 0.3),
        "the tone is at its crest within a millisecond: a fade would have shown"
    );

    // Starting in the middle of a track (a seek) is still faded in: no click.
    let seeked = play_at(
        &["gap_b.wav"],
        44_100,
        501,
        ReplayGainSettings::default(),
        ReplayGainTags::default(),
        1.0,
    );
    let (_, _, whole) = decode_all("gap_b.wav");
    let from = 22_094 * 2; // 501 ms, where the tone is well away from zero
    assert!(whole[from].abs() > 0.1, "{}", whole[from]);
    assert!(seeked.pcm[0].abs() < whole[from].abs() * 0.1);
    assert_eq!(
        max_abs_diff(
            &seeked.pcm[FADE_GUARD * 2..],
            &whole[from + FADE_GUARD * 2..]
        ),
        0.0
    );
}

/// Plays `name` at 44.1 kHz with the equalizer set to `eq`.
/// A parametric correction profile reaches the audio as its response says, and
/// without one (or with one that changes nothing) the file goes out untouched.
#[test]
fn a_correction_profile_changes_the_level_by_what_its_response_says() {
    use oneum::audio::PeqProfile;
    let play_corrected = |profile: Option<PeqProfile>| -> Played {
        let (mut engine, mut renderer) =
            Engine::new_offline(44_100, 1.0, ReplayGainSettings::default());
        engine.set_correction(profile);
        let queue = [request(1, "gap_a.wav")];
        let slot = NextSlot::new();
        slot.set(NextTrack::End);
        engine
            .start_feed(queue[0].clone(), 0, slot.clone())
            .expect("feed starts");
        engine.set_playing(true);
        pump(&mut engine, &mut renderer, &queue, &slot)
    };
    // The fixture is a 441 Hz tone. A profile as AutoEq writes them.
    let profile = PeqProfile::parse(
        "Test",
        "Preamp: -4.0 dB\n\
         Filter 1: ON LSC Fc 105 Hz Gain 4.0 dB Q 0.70\n\
         Filter 2: ON PK Fc 441 Hz Gain -5.0 dB Q 1.20\n\
         Filter 3: ON HSC Fc 8000 Hz Gain 3.0 dB Q 0.70\n",
    )
    .expect("parses");
    let (_, _, plain) = decode_all("gap_a.wav");
    let played = play_corrected(Some(profile.clone()));
    assert_eq!(played.pcm.len(), plain.len());
    assert_eq!(played.limited, 0);
    let settled = plain.len() / 2..plain.len() - 2000;
    let heard_db = 20.0 * (rms(&played.pcm[settled.clone()]) / rms(&plain[settled])).log10();
    let says_db = profile.response_db(44_100, 441.0);
    assert!((-10.0..-8.0).contains(&says_db), "{says_db}");
    assert!(
        (heard_db - says_db).abs() < 0.05,
        "heard {heard_db:.3} dB, the response says {says_db:.3} dB"
    );
    for nothing in [None, Some(PeqProfile::default())] {
        let played = play_corrected(nothing.clone());
        assert!(
            played
                .pcm
                .iter()
                .zip(&plain)
                .all(|(a, b)| a.to_bits() == b.to_bits()),
            "{nothing:?}"
        );
    }
}

fn play_equalized(name: &str, eq: oneum::audio::EqSettings) -> Played {
    let (mut engine, mut renderer) =
        Engine::new_offline(44_100, 1.0, ReplayGainSettings::default());
    engine.set_eq(eq);
    let queue = [request(1, name)];
    let slot = NextSlot::new();
    slot.set(NextTrack::End);
    engine
        .start_feed(queue[0].clone(), 0, slot.clone())
        .expect("feed starts");
    engine.set_playing(true);
    pump(&mut engine, &mut renderer, &queue, &slot)
}

#[test]
fn the_equalizer_changes_the_level_by_what_its_curve_says() {
    use oneum::audio::EqSettings;
    use oneum::audio::eq::response_db;
    // The fixture is a 441 Hz tone; the 500 Hz band up by 6 dB, the pre-amp down by 2.
    let mut gains = [0.0; 10];
    gains[4] = 6.0;
    let eq = EqSettings {
        enabled: true,
        preamp_db: -2.0,
        gains_db: gains,
    };
    let (_, _, plain) = decode_all("gap_a.wav");
    let played = play_equalized("gap_a.wav", eq);
    assert_eq!(played.pcm.len(), plain.len());
    assert_eq!(played.limited, 0);
    let settled = plain.len() / 2..plain.len() - 2000;
    let heard_db = 20.0 * (rms(&played.pcm[settled.clone()]) / rms(&plain[settled])).log10();
    let curve_db = response_db(&eq, 44_100, 441.0);
    assert!((2.0..4.0).contains(&curve_db), "{curve_db}");
    assert!(
        (heard_db - curve_db).abs() < 0.05,
        "heard {heard_db:.3} dB, the curve says {curve_db:.3} dB"
    );

    // Switched on but with everything at 0 dB, and switched off with values set: the
    // file goes out untouched, from the first sample to the last.
    for nothing in [
        EqSettings {
            enabled: true,
            ..EqSettings::default()
        },
        EqSettings {
            enabled: false,
            ..eq
        },
    ] {
        let played = play_equalized("gap_a.wav", nothing);
        assert!(
            played
                .pcm
                .iter()
                .zip(&plain)
                .all(|(a, b)| a.to_bits() == b.to_bits()),
            "{nothing:?}"
        );
    }
}

#[test]
fn a_boost_beyond_full_scale_is_limited_not_clipped() {
    // Everything around the tone up by 12 dB and the pre-amp up as well: the tone
    // (peak 0.35) would come out at more than four times full scale.
    let mut gains = [0.0; 10];
    gains[3] = 12.0;
    gains[4] = 12.0;
    let eq = oneum::audio::EqSettings {
        enabled: true,
        preamp_db: 12.0,
        gains_db: gains,
    };
    let played = play_equalized("gap_a.wav", eq);
    assert!(played.limited > 10_000, "limited {}", played.limited);
    let left = played.left();
    let peak = left.iter().fold(0f32, |m, s| m.max(s.abs()));
    assert!(peak <= 1.0 && peak > 0.99, "peak {peak}");
    let settled = &left[left.len() / 2..left.len() - 4_410];
    let db = distortion_db(settled, 44_100, 441.0);
    assert!(db < -50.0, "distortion {db:.1} dB");
}

#[test]
fn crossfeed_reaches_the_audio_and_leaves_the_centre_almost_alone() {
    use oneum::audio::Crossfeed;
    let (_, _, plain) = decode_all("gap_a.wav");
    let (mut engine, mut renderer) =
        Engine::new_offline(44_100, 1.0, ReplayGainSettings::default());
    engine.set_crossfeed(Crossfeed::Strong);
    let queue = [request(1, "gap_a.wav")];
    let slot = NextSlot::new();
    slot.set(NextTrack::End);
    engine
        .start_feed(queue[0].clone(), 0, slot.clone())
        .expect("feed starts");
    engine.set_playing(true);
    let played = pump(&mut engine, &mut renderer, &queue, &slot);
    assert_eq!(played.pcm.len(), plain.len());
    assert_eq!(played.limited, 0, "crossfeed never makes anything louder");
    // The fixture has the same tone on both channels (a sound in the middle): both
    // channels stay alike, and at 441 Hz the level drops by less than 2 dB.
    assert!(
        played
            .pcm
            .as_chunks::<2>()
            .0
            .iter()
            .all(|f| (f[0] - f[1]).abs() < 1e-6)
    );
    let settled = plain.len() / 2..plain.len() - 2000;
    let change_db = 20.0 * (rms(&played.pcm[settled.clone()]) / rms(&plain[settled])).log10();
    assert!((-2.0..-0.05).contains(&change_db), "{change_db:.3} dB");
}

/// Writes a 32-bit float stereo WAV (the same samples on both channels).
fn write_float_wav(path: &std::path::Path, rate: u32, samples: &[f32]) {
    let data_len = (samples.len() * 8) as u32;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 8).to_le_bytes());
    bytes.extend_from_slice(&8u16.to_le_bytes());
    bytes.extend_from_slice(&32u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        bytes.extend_from_slice(&s.to_le_bytes());
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, bytes).expect("wav written");
}

/// A float file can hold anything: numbers far outside the range, infinities, NaN.
/// None of it may reach the device, and none of it may stay behind in the equalizer
/// or the crossfeed and spoil the music that follows.
#[test]
fn garbage_in_a_float_file_does_not_poison_what_follows() {
    use oneum::audio::{Crossfeed, EqSettings};
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("garbage.wav");
    let rate = 44_100u32;
    let tone = |n: usize| 0.1 * (std::f32::consts::TAU * 1000.0 * n as f32 / rate as f32).sin();
    let mut samples: Vec<f32> = (0..rate as usize * 2).map(tone).collect();
    // A burst of garbage a quarter of a second in.
    let garbage = [
        3.0e38,
        -3.0e38,
        f32::INFINITY,
        f32::NAN,
        f32::NEG_INFINITY,
        1.0e30,
    ];
    for (k, bad) in garbage.iter().cycle().take(600).enumerate() {
        samples[11_025 + k] = *bad;
    }
    write_float_wav(&path, rate, &samples);

    let mut gains = [0.0; 10];
    gains[5] = 12.0; // 1 kHz up as far as it goes
    let eq = EqSettings {
        enabled: true,
        preamp_db: 0.0,
        gains_db: gains,
    };
    // At the file's rate and through the rate conversion.
    for out_rate in [44_100, 48_000] {
        let (mut engine, mut renderer) =
            Engine::new_offline(out_rate, 1.0, ReplayGainSettings::default());
        engine.set_eq(eq);
        engine.set_crossfeed(Crossfeed::Strong);
        let queue = [TrackRequest {
            item_id: 1,
            track_id: 1,
            path: path.clone(),
            replaygain: ReplayGainTags::default(),
            crossfade_ms: 0,
        }];
        let slot = NextSlot::new();
        slot.set(NextTrack::End);
        engine
            .start_feed(queue[0].clone(), 0, slot.clone())
            .expect("feed starts");
        engine.set_playing(true);
        let played = pump(&mut engine, &mut renderer, &queue, &slot);
        assert!(
            played.pcm.iter().all(|s| s.is_finite() && s.abs() <= 1.0),
            "{out_rate} Hz: something that is not audio came out"
        );
        // The last half second is the tone again, at the level the equalizer and the
        // crossfeed give it: +12 dB at 1 kHz, a little off for the crossfeed.
        let left = played.left();
        let tail = &left[left.len() - out_rate as usize / 2..];
        let level_db = 20.0 * (rms(tail) / (0.1 / std::f32::consts::SQRT_2)).log10();
        assert!(
            (10.0..12.5).contains(&level_db),
            "{out_rate} Hz: the tone after the garbage is at {level_db:.2} dB"
        );
    }
}

// ----- multichannel files ------------------------------------------------------------------

/// Tone of each channel in the multichannel test files, in the standard channel order
/// (FL FR FC LFE, then the surrounds).
const CHANNEL_TONES: [f64; 8] = [300.0, 500.0, 700.0, 60.0, 1100.0, 1300.0, 1700.0, 1900.0];

fn have_ffmpeg() -> bool {
    std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-version"])
        .stdin(std::process::Stdio::null())
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Writes a file whose channels each carry their own tone at −14 dBFS.
fn write_multichannel(path: &std::path::Path, layout: &str, channels: usize, codec: &[&str]) {
    let tones: Vec<String> = CHANNEL_TONES[..channels]
        .iter()
        .map(|f| format!("0.2*sin(2*PI*{f}*t)"))
        .collect();
    let source = format!(
        "aevalsrc={}:channel_layout={layout}:sample_rate=48000:duration=4",
        tones.join("|")
    );
    let output = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", &source])
        .args(codec)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(
        output.status.success(),
        "ffmpeg could not write {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Amplitude of the sine of `freq` in one channel of interleaved audio.
fn tone_amplitude(samples: &[f32], channels: usize, channel: usize, rate: u32, freq: f64) -> f64 {
    let (mut re, mut im, mut n) = (0.0f64, 0.0f64, 0usize);
    // Skip the first and last half second: codec delay and filter ringing.
    let frames = samples.len() / channels;
    for i in rate as usize / 2..frames.saturating_sub(rate as usize / 2) {
        let phase = 2.0 * std::f64::consts::PI * freq * i as f64 / f64::from(rate);
        let s = f64::from(samples[i * channels + channel]);
        re += s * phase.cos();
        im += s * phase.sin();
        n += 1;
    }
    2.0 * (re * re + im * im).sqrt() / n.max(1) as f64
}

fn decode_file(path: &std::path::Path) -> (u32, usize, Vec<f32>) {
    let mut decoder = TrackDecoder::open(path).expect("opens");
    let (rate, channels) = (decoder.info().sample_rate, decoder.info().channels);
    let (mut all, mut buf) = (Vec::new(), Vec::new());
    while decoder.next(&mut buf).expect("decodes") {
        all.extend_from_slice(&buf);
    }
    (rate, channels, all)
}

/// The formats that can hold more than two channels and that the decoder plays.
const MULTICHANNEL_FORMATS: [(&str, &str, usize, &[&str]); 7] = [
    ("51.flac", "5.1", 6, &["-c:a", "flac"]),
    ("51.wav", "5.1", 6, &["-c:a", "pcm_s16le"]),
    ("51side.wav", "5.1(side)", 6, &["-c:a", "pcm_s24le"]),
    ("51.aiff", "5.1", 6, &["-c:a", "pcm_s16be"]),
    ("51.alac.m4a", "5.1", 6, &["-c:a", "alac"]),
    ("71.flac", "7.1", 8, &["-c:a", "flac"]),
    ("71.wav", "7.1", 8, &["-c:a", "pcm_s16le"]),
];

/// 5.1 and 7.1 files come out of the decoder with their channels in the standard
/// order (FL FR FC LFE, surrounds), whatever order the format stores them in — the
/// down-mix depends on it.
#[test]
fn multichannel_files_are_decoded_in_the_standard_channel_order() {
    if !have_ffmpeg() {
        eprintln!("note: skipped, ffmpeg is not installed");
        return;
    }
    let dir = tempfile::tempdir().expect("temp dir");
    for (name, layout, channels, codec) in MULTICHANNEL_FORMATS {
        let path = dir.path().join(name);
        write_multichannel(&path, layout, channels, codec);
        let (rate, decoded_channels, audio) = decode_file(&path);
        assert_eq!((rate, decoded_channels), (48_000, channels), "{name}");
        assert_eq!(audio.len() / channels, 4 * 48_000, "{name}");
        for channel in 0..channels {
            for (tone_of, tone) in CHANNEL_TONES[..channels].iter().enumerate() {
                let amplitude = tone_amplitude(&audio, channels, channel, rate, *tone);
                if tone_of == channel {
                    assert!(
                        (amplitude - 0.2).abs() < 0.005,
                        "{name}: channel {channel} holds its own tone at {amplitude:.3}"
                    );
                } else {
                    assert!(
                        amplitude < 0.002,
                        "{name}: channel {channel} holds the tone of channel {tone_of} ({amplitude:.4})"
                    );
                }
            }
        }
    }
}

/// What is played of a multichannel file: front left and right, the centre and the
/// surrounds mixed in 3 dB down, the LFE left out, and the sum scaled so that it cannot
/// exceed full scale. The loudness the app measures for the file is that of this mix,
/// and an independent scanner given the same mix reads the same.
#[test]
fn multichannel_files_are_mixed_down_as_documented_and_measured_as_played() {
    use oneum::audio::loudness::{Meter, measure_file};
    use std::sync::atomic::AtomicBool;

    if !have_ffmpeg() {
        eprintln!("note: skipped, ffmpeg is not installed");
        return;
    }
    let dir = tempfile::tempdir().expect("temp dir");
    let c = std::f64::consts::FRAC_1_SQRT_2;
    for (name, layout, channels, codec) in MULTICHANNEL_FORMATS {
        let path = dir.path().join(name);
        write_multichannel(&path, layout, channels, codec);
        let played = play_file(&path, 48_000);
        assert_eq!(played.underruns, 0, "{name}");
        assert_eq!(played.limited, 0, "{name}: the mix stays within full scale");
        assert_eq!(played.frames(), 4 * 48_000, "{name}");
        let surrounds = (channels - 4) / 2;
        let norm = 1.0 / (1.0 + c + c * surrounds as f64);
        // Expected amplitude of each channel's tone in the left and the right output.
        for (index, tone) in CHANNEL_TONES[..channels].iter().enumerate() {
            let weight = match index {
                0 | 1 => 1.0,
                3 => 0.0,
                _ => c,
            };
            let (in_left, in_right) = match index {
                2 => (true, true),
                3 => (false, false),
                i => (i % 2 == 0, i % 2 == 1),
            };
            for (side, present) in [(0, in_left), (1, in_right)] {
                let expected = if present { 0.2 * weight * norm } else { 0.0 };
                let amplitude = tone_amplitude(&played.pcm, 2, side, 48_000, *tone);
                assert!(
                    (amplitude - expected).abs() < 0.002,
                    "{name}: tone of channel {index} on output {side}: {amplitude:.4}, expected {expected:.4}"
                );
            }
        }

        // Measured as played.
        let never = AtomicBool::new(false);
        let measured = measure_file(&path, &never)
            .expect("decodes")
            .expect("not cancelled");
        let mut meter = Meter::new(48_000);
        meter.push(&played.pcm);
        let of_output = meter.finish();
        let (file, output) = (
            measured.loudness().expect("not silent"),
            of_output.loudness().expect("not silent"),
        );
        assert!(
            (file - output).abs() < 0.05,
            "{name}: {file:.2} vs {output:.2}"
        );
        assert!((measured.peak - of_output.peak).abs() < 0.002, "{name}");

        // ffmpeg, told to make the same mix, reads the same loudness.
        let side = |own: usize, extra: &[usize]| {
            let mut terms = vec![format!("{norm:.6}*c{own}"), format!("{:.6}*c2", c * norm)];
            terms.extend(extra.iter().map(|i| format!("{:.6}*c{i}", c * norm)));
            terms.join("+")
        };
        let (left_extra, right_extra): (&[usize], &[usize]) = if channels == 6 {
            (&[4], &[5])
        } else {
            (&[4, 6], &[5, 7])
        };
        let filter = format!(
            "pan=stereo|c0={}|c1={},ebur128=framelog=quiet",
            side(0, left_extra),
            side(1, right_extra)
        );
        let output = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-hide_banner", "-i"])
            .arg(&path)
            .args(["-filter:a", &filter, "-f", "null", "-"])
            .output()
            .expect("run ffmpeg");
        let log = String::from_utf8_lossy(&output.stderr);
        let theirs: f64 = log
            .rsplit("Integrated loudness:")
            .next()
            .and_then(|summary| summary.split("I:").nth(1))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|value| value.parse().ok())
            .unwrap_or_else(|| panic!("{name}: no loudness in ffmpeg's output: {log}"));
        assert!(
            (file - theirs).abs() <= 0.11,
            "{name}: {file:.2} LUFS here, {theirs:.1} LUFS by ffmpeg"
        );
    }
}

/// DSD files from one continuous stream, cut in two (`a` ends on a whole PCM frame),
/// and the stream whole: written to a temporary folder.
fn dsd_pair(dir: &std::path::Path, hz: f64) -> (PathBuf, PathBuf, PathBuf, u64) {
    use oneum::audio::dsd::encode::{Modulator, dff, dsf};
    let rate = 2_822_400u32;
    let samples_a: u64 = 32 * 30_000; // 30,000 PCM frames
    let samples_b: u64 = 1_200_000;
    let mut m = Modulator::new();
    for n in 0..samples_a + samples_b {
        m.push(0.45 * (2.0 * std::f64::consts::PI * hz * n as f64 / f64::from(rate)).sin());
    }
    let bits = m.finish();
    let cut = (samples_a / 8) as usize;
    let (a, b) = (bits[..cut].to_vec(), bits[cut..].to_vec());
    let paths = (dir.join("a.dsf"), dir.join("b.dff"), dir.join("whole.dsf"));
    std::fs::write(&paths.0, dsf(rate, &[a.clone(), a], samples_a, None)).unwrap();
    std::fs::write(&paths.1, dff(rate, &[b.clone(), b], None)).unwrap();
    std::fs::write(
        &paths.2,
        dsf(rate, &[bits.clone(), bits], samples_a + samples_b, None),
    )
    .unwrap();
    (paths.0, paths.1, paths.2, samples_a / 32)
}

/// Two DSD songs in a row play as the one stream they were cut from: the decode thread
/// gives the first song's last frames the second song's first bits, and the second
/// song starts from the first one's history. At the device's rate (88.2 kHz), through
/// the resampler (48 kHz) and from memory.
#[test]
fn dsd_songs_in_a_row_play_as_one_stream() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b, whole, frames_a) = dsd_pair(dir.path(), 1_000.0);
    let names = [a.to_str().unwrap(), b.to_str().unwrap()];
    let one = [whole.to_str().unwrap()];
    for rate in [88_200, 48_000] {
        let joined = play(&names, rate);
        let reference = play(&one, rate);
        assert_eq!(joined.underruns, 0);
        assert_eq!(
            joined.frames(),
            reference.frames(),
            "{rate}: no frames added or lost"
        );
        let boundary = if rate == 88_200 {
            frames_a
        } else {
            // The resampler counts the boundary in output frames.
            joined.started()[1].1
        };
        assert_eq!(joined.started()[0], (1, 0));
        assert_eq!(joined.started()[1].0, 2);
        if rate == 88_200 {
            assert_eq!(joined.started()[1].1, boundary);
        }
        let diff = max_abs_diff(
            &joined.pcm[FADE_GUARD * 2..],
            &reference.pcm[FADE_GUARD * 2..],
        );
        assert!(diff < 1e-6, "{rate}: {diff} off the continuous stream");
        // The level is the stream's, across the boundary too.
        let at = boundary as usize * 2;
        let around = &joined.pcm[at - 2_000..at + 2_000];
        let peak = around.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.45).abs() < 0.01, "{rate}: {peak}");
    }
    let from_memory = play_from_memory(&names, 88_200, 0);
    let reference = play(&one, 88_200);
    let diff = max_abs_diff(
        &from_memory.pcm[FADE_GUARD * 2..],
        &reference.pcm[FADE_GUARD * 2..],
    );
    assert!(diff < 1e-6, "memory playback: {diff}");
}

/// A DSD song followed by a PCM song, or alone, ends as decoding it alone does (its
/// last frames completed with silence), and seeking into a DSD song lands on the frame.
#[test]
fn dsd_ends_cleanly_before_other_music_and_seeks_to_the_frame() {
    let dir = tempfile::tempdir().unwrap();
    let (a, _, _, frames_a) = dsd_pair(dir.path(), 2_000.0);
    let mut alone = TrackDecoder::open(&a).unwrap();
    let mut expected = Vec::new();
    let mut chunk = Vec::new();
    while alone.next(&mut chunk).unwrap() {
        expected.extend_from_slice(&chunk);
    }
    assert_eq!(expected.len() as u64, 2 * frames_a);
    let wav = fixture("tone_48k.wav");
    let played = play(&[a.to_str().unwrap(), wav.to_str().unwrap()], 88_200);
    assert_eq!(played.started()[1], (2, frames_a));
    let diff = max_abs_diff(
        &played.pcm[FADE_GUARD * 2..frames_a as usize * 2],
        &expected[FADE_GUARD * 2..],
    );
    assert!(diff < 1e-6, "{diff}");
    // From 0.2 s in.
    let seeked = play_at(
        &[a.to_str().unwrap()],
        88_200,
        200,
        ReplayGainSettings::default(),
        ReplayGainTags::default(),
        1.0,
    );
    let from = 88_200 / 5;
    assert_eq!(seeked.frames() as u64, frames_a - from as u64);
    // A seek fades in; at 88.2 kHz the fade covers twice the frames it does at 44.1.
    let guard = 2 * FADE_GUARD;
    let diff = max_abs_diff(&seeked.pcm[guard * 2..], &expected[(from + guard) * 2..]);
    assert!(diff < 1e-6, "seek: {diff}");
}

/// Against an independent decoder: ffmpeg's DSD decoder (another filter design) reads
/// the same DSF and DSDIFF files to the same tones, at the same level, at the same time
/// and in the same channels. Skipped without ffmpeg.
#[test]
fn dsd_agrees_with_ffmpegs_decoder() {
    use oneum::audio::dsd::encode::{Modulator, dff, dsf};
    use std::process::Command;
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("note: dsd_agrees_with_ffmpegs_decoder skipped: no ffmpeg");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let rate = 2_822_400u32;
    let samples = u64::from(rate); // one second
    let tone = |hz: f64, level: f64| {
        let mut m = Modulator::new();
        for n in 0..samples {
            m.push(level * (2.0 * std::f64::consts::PI * hz * n as f64 / f64::from(rate)).sin());
        }
        m.finish()
    };
    let (left, right) = (tone(1_000.0, 0.45), tone(5_000.0, 0.2));
    for (name, bytes) in [
        (
            "x.dsf",
            dsf(rate, &[left.clone(), right.clone()], samples, None),
        ),
        ("x.dff", dff(rate, &[left.clone(), right.clone()], None)),
    ] {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        let out = dir.path().join(format!("{name}.f32"));
        let status = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-i"])
            .arg(&path)
            .args(["-ar", "88200", "-f", "f32le", "-c:a", "pcm_f32le"])
            .arg(&out)
            .status()
            .unwrap();
        assert!(status.success(), "ffmpeg reads {name}");
        let theirs: Vec<f32> = std::fs::read(&out)
            .unwrap()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        let mut ours = Vec::new();
        let mut dec = TrackDecoder::open(&path).unwrap();
        let mut chunk = Vec::new();
        while dec.next(&mut chunk).unwrap() {
            ours.extend_from_slice(&chunk);
        }
        // Amplitude and phase of a tone in one channel, over the middle half second.
        let measure = |pcm: &[f32], channel: usize, hz: f64| {
            let w = 2.0 * std::f64::consts::PI * hz / 88_200.0;
            let (mut s, mut c) = (0.0, 0.0);
            let range = 22_050..66_150;
            for n in range.clone() {
                let y = f64::from(pcm[n * 2 + channel]);
                s += y * (w * n as f64).sin();
                c += y * (w * n as f64).cos();
            }
            let len = range.len() as f64;
            let (a, b) = (2.0 * s / len, 2.0 * c / len);
            (a.hypot(b), b.atan2(a))
        };
        let mut delays = Vec::new();
        for (channel, hz, level) in [(0, 1_000.0, 0.45), (1, 5_000.0, 0.2)] {
            let (ours_amp, ours_phase) = measure(&ours, channel, hz);
            let (their_amp, their_phase) = measure(&theirs, channel, hz);
            let db = 20.0 * (ours_amp / their_amp).log10();
            let us = (ours_phase - their_phase) / (2.0 * std::f64::consts::PI * hz) * 1e6;
            println!(
                "REPORT {name} channel {channel}: ours {ours_amp:.4}, ffmpeg {their_amp:.4} \
                 ({db:+.3} dB), time {us:+.2} µs"
            );
            assert!(
                (ours_amp - level).abs() < 0.005,
                "{name} {channel}: {ours_amp}"
            );
            assert!(db.abs() < 0.1, "{name} {channel}: {db} dB from ffmpeg");
            // Ours is in time with the signal that was encoded (a sine starting at
            // phase 0); ffmpeg's decoder leaves its filter's delay in (about 14 µs),
            // the same for every frequency, and the same polarity.
            let ours_us = ours_phase / (2.0 * std::f64::consts::PI * hz) * 1e6;
            assert!(
                ours_us.abs() < 1.0,
                "{name} {channel}: {ours_us} µs from the signal"
            );
            assert!(
                (5.0..25.0).contains(&us),
                "{name} {channel}: {us} µs from ffmpeg"
            );
            delays.push(us);
            // The other channel's tone is not in this one.
            let (leak, _) = measure(&theirs, 1 - channel, hz);
            assert!(
                leak < 1e-3,
                "{name}: ffmpeg sees channel {channel} elsewhere"
            );
        }
        // A plain delay: the same at 1 and at 5 kHz.
        assert!((delays[0] - delays[1]).abs() < 0.5, "{name}: {delays:?}");
    }
}

/// Writes DSD files for trying the app by hand: `ONEUM_DSD_OUT=<folder>`.
#[test]
#[ignore = "writes files for a manual check"]
fn write_dsd_files_for_a_manual_check() {
    use oneum::audio::dsd::encode::{Modulator, dff, dsf};
    let Some(dir) = std::env::var_os("ONEUM_DSD_OUT").map(PathBuf::from) else {
        return;
    };
    let rate = 2_822_400u32;
    let samples = u64::from(rate) * 20;
    let mut m = Modulator::new();
    for n in 0..samples {
        let t = n as f64 / f64::from(rate);
        m.push(0.3 * (2.0 * std::f64::consts::PI * 440.0 * t).sin());
    }
    let bits = m.finish();
    let half = (samples / 2 / 8) as usize;
    std::fs::write(
        dir.join("01 First.dsf"),
        dsf(
            rate,
            &[bits[..half].to_vec(), bits[..half].to_vec()],
            samples / 2,
            None,
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("02 Second.dff"),
        dff(rate, &[bits[half..].to_vec(), bits[half..].to_vec()], None),
    )
    .unwrap();
}

/// Playback hands the demuxer the stream after the ID3v2 tags in front (tags come from
/// the library): a file with a huge tag of tiny frames, several tags in a row, or a tag
/// with a footer plays the same samples and seeks to the same frames as without them.
#[test]
fn the_tags_in_front_of_an_mp3_are_looked_past() {
    let dir = tempfile::tempdir().unwrap();
    let plain = std::fs::read(fixture("gap_a.mp3")).unwrap();
    let syncsafe = |n: usize| {
        [
            (n >> 21) as u8 & 0x7F,
            (n >> 14) as u8 & 0x7F,
            (n >> 7) as u8 & 0x7F,
            n as u8 & 0x7F,
        ]
    };
    let tag = |frames: usize, footer: bool| {
        let body: Vec<u8> = (0..frames)
            .flat_map(|i| {
                let mut f = b"TXXX".to_vec();
                f.extend(syncsafe(6));
                f.extend([0, 0, 0, b'k', 0, b'0' + (i % 10) as u8, b'v', 0]);
                f
            })
            .collect();
        let mut t = b"ID3\x04\x00".to_vec();
        t.push(if footer { 0x10 } else { 0 });
        t.extend(syncsafe(body.len()));
        t.extend(&body);
        if footer {
            t.extend(b"3DI\x04\x00\x10");
            t.extend(syncsafe(body.len()));
        }
        t
    };
    let mut tagged = tag(200_000, false);
    tagged.extend(tag(3, true));
    tagged.extend(tag(1, false));
    tagged.extend(&plain);
    let path = dir.path().join("tagged.mp3");
    std::fs::write(&path, &tagged).unwrap();
    let decode = |p: &std::path::Path, seek: Option<u64>| {
        let mut d = TrackDecoder::open(p).unwrap();
        if let Some(frame) = seek {
            d.seek(frame).unwrap();
        }
        let mut all = Vec::new();
        let mut chunk = Vec::new();
        while d.next(&mut chunk).unwrap() {
            all.extend_from_slice(&chunk);
        }
        (d.info().total_frames, all)
    };
    let (_, _, reference) = decode_all("gap_a.mp3");
    let (frames, samples) = decode(&path, None);
    assert_eq!(samples, reference);
    assert_eq!(frames, decode(&fixture("gap_a.mp3"), None).0);
    assert_eq!(
        decode(&path, Some(20_000)).1,
        decode(&fixture("gap_a.mp3"), Some(20_000)).1
    );
}

/// Large real files, made by hand (`ONEUM_LARGE_DIR=<folder>`, see docs/TESTING.md):
/// they open, play, and a seek to near their end lands there, although the demuxer reads
/// far more than 128 MB without audio while it seeks.
#[test]
#[ignore = "needs large files made by hand"]
fn large_files_open_and_seek_to_their_end() {
    let Some(dir) = std::env::var_os("ONEUM_LARGE_DIR").map(PathBuf::from) else {
        return;
    };
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    names.sort();
    for path in names {
        let started = Instant::now();
        let mut d = TrackDecoder::open(&path).expect("opens");
        let info = d.info().clone();
        let total = info.total_frames.expect("length known");
        let mut chunk = Vec::new();
        assert!(d.next(&mut chunk).unwrap(), "{path:?}: plays");
        let target = total - u64::from(info.sample_rate) * 5;
        d.seek(target).unwrap();
        let mut after = 0u64;
        while d.next(&mut chunk).unwrap() {
            after += (chunk.len() / info.channels) as u64;
        }
        let size = std::fs::metadata(&path).unwrap().len() / 1_048_576;
        println!(
            "REPORT {}: {size} MB, {total} frames, {after} frames after the seek, {:?}",
            path.file_name().unwrap().to_string_lossy(),
            started.elapsed()
        );
        assert!(
            after.abs_diff(total - target) < u64::from(info.sample_rate) / 10,
            "{path:?}"
        );
    }
}

/// A file whose audio is followed by gigabytes of zeros (a sparse file here; a damaged
/// disk or copy in life): the music plays and the track ends where the audio does, in
/// well under a second, instead of the demuxer searching the zeros to the end (a 10 GB
/// file took 21 s before).
#[test]
fn audio_followed_by_gigabytes_of_zeros_ends_where_the_audio_does() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("zeros.mp3");
    let file = std::fs::File::create(&path).unwrap();
    std::io::Write::write_all(&mut &file, &std::fs::read(fixture("gap_a.mp3")).unwrap()).unwrap();
    file.set_len(3 * 1024 * 1024 * 1024).unwrap();
    let started = Instant::now();
    let mut d = TrackDecoder::open(&path).unwrap();
    let mut all = Vec::new();
    let mut chunk = Vec::new();
    while d.next(&mut chunk).unwrap() {
        all.extend_from_slice(&chunk);
    }
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    let (_, _, reference) = decode_all("gap_a.mp3");
    assert_eq!(all, reference);
}

/// A CD image and its CUE sheet, written into `dir`: 44.1 kHz stereo 16-bit, five
/// seconds of a sweep with noise (every frame different), three tracks starting at 0,
/// 1 s + 37 CD frames and 3 s + 5 CD frames. Returns (image, sheet, track starts in
/// sample frames).
fn write_image_and_sheet(dir: &std::path::Path, image_name: &str) -> (PathBuf, PathBuf, [u64; 3]) {
    let rate = 44_100u32;
    let frames = rate * 5;
    let mut data = Vec::with_capacity(frames as usize * 4);
    let mut x: u32 = 7;
    for n in 0..frames {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let t = f64::from(n) / f64::from(rate);
        let sweep = (std::f64::consts::TAU * (200.0 + 400.0 * t) * t).sin() * 0.4;
        let noise = (f64::from(x >> 16) / 65_536.0 - 0.5) * 0.1;
        let l = ((sweep + noise) * 32_000.0) as i16;
        let r = ((sweep * 0.5 - noise) * 32_000.0) as i16;
        data.extend_from_slice(&l.to_le_bytes());
        data.extend_from_slice(&r.to_le_bytes());
    }
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 4).to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&data);
    let wav = dir.join("image.wav");
    std::fs::write(&wav, bytes).expect("write image");
    let image = if image_name == "image.wav" {
        wav
    } else {
        let flac = dir.join(image_name);
        let made = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-i"])
            .arg(&wav)
            .args(["-c:a", "flac"])
            .arg(&flac)
            .status()
            .is_ok_and(|s| s.success());
        assert!(made, "ffmpeg could not make the FLAC image");
        std::fs::remove_file(&wav).expect("remove wav");
        flac
    };
    // The sheet names the WAV it was ripped to, as sheets often do after conversion.
    let sheet = dir.join("image.cue");
    std::fs::write(
        &sheet,
        "PERFORMER \"Tester\"\nTITLE \"Image\"\nFILE \"image.wav\" WAVE\n\
         TRACK 01 AUDIO\n TITLE \"One\"\n INDEX 01 00:00:00\n\
         TRACK 02 AUDIO\n TITLE \"Two\"\n INDEX 00 00:01:20\n INDEX 01 00:01:37\n\
         TRACK 03 AUDIO\n TITLE \"Three\"\n INDEX 01 00:03:05\n",
    )
    .expect("write sheet");
    (image, sheet, [0, 44_100 + 37 * 588, 3 * 44_100 + 5 * 588])
}

fn decode_path(path: &std::path::Path) -> Vec<f32> {
    let mut dec = TrackDecoder::open(path).expect("opens");
    let (mut all, mut buf) = (Vec::new(), Vec::new());
    while dec.next(&mut buf).expect("decodes") {
        all.extend_from_slice(&buf);
    }
    all
}

/// The tracks of a CUE sheet are exactly their stretches of the image: decoded one
/// after another they are the image sample for sample, each its own length, and seeking
/// inside a track lands on the right frame. Through the player's feed (from the file and
/// from memory) they follow each other without a gap. For a WAV and a FLAC image.
#[test]
fn cue_sheet_tracks_are_their_stretches_of_the_image() {
    let has_ffmpeg = std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success());
    let mut images = vec!["image.wav"];
    if has_ffmpeg {
        images.push("image.flac");
    } else {
        eprintln!("note: the FLAC image is skipped: ffmpeg is not installed");
    }
    for image_name in images {
        let dir = tempfile::tempdir().expect("tempdir");
        let (image, sheet, starts) = write_image_and_sheet(dir.path(), image_name);
        let whole = decode_path(&image);
        let total = (whole.len() / 2) as u64;
        let tracks: Vec<PathBuf> = (1..=3)
            .map(|n| oneum::library::cue::track_path(&sheet, n))
            .collect();
        let mut joined = Vec::new();
        for (i, track) in tracks.iter().enumerate() {
            let pcm = decode_path(track);
            let end = starts.get(i + 1).copied().unwrap_or(total);
            assert_eq!(
                (pcm.len() / 2) as u64,
                end - starts[i],
                "{image_name} track {}: length",
                i + 1
            );
            let dec = TrackDecoder::open(track).expect("opens");
            assert_eq!(dec.info().total_frames, Some(end - starts[i]));
            joined.extend(pcm);
        }
        assert_eq!(joined, whole, "{image_name}: the tracks are the image");

        // A seek inside track 2 lands on its frame.
        let mut dec = TrackDecoder::open(&tracks[1]).expect("opens");
        dec.seek(10_000).expect("seeks");
        let mut buf = Vec::new();
        assert!(dec.next(&mut buf).expect("decodes"));
        let at = ((starts[1] + 10_000) * 2) as usize;
        assert_eq!(&buf[..8], &whole[at..at + 8], "{image_name}: seek");

        // Played as three tracks in a row: one continuous signal.
        let names: Vec<String> = tracks.iter().map(|t| t.display().to_string()).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        for played in [play(&names, 44_100), play_from_memory(&names, 44_100, 0)] {
            assert_eq!(played.frames() as u64, total, "{image_name}: played length");
            assert_eq!(played.underruns, 0);
            let starts_seen: Vec<u64> = played.started().iter().map(|s| s.0).collect();
            assert_eq!(starts_seen, [1, 2, 3], "{image_name}: every track started");
            // Past the fade-in at the very start, what was heard is the image.
            let heard = &played.pcm[FADE_GUARD * 2..];
            let image_pcm = &whole[FADE_GUARD * 2..];
            assert!(
                max_abs_diff(heard, image_pcm) < 1e-6,
                "{image_name}: the boundaries are seamless"
            );
        }
    }
}

/// A sheet can be for a file of any format the app plays. Cut at random places, the
/// tracks are always their stretches of the file: each exactly as long as the sheet
/// says, together as long as the file, and (the cut made by seeking) the same audio as
/// the file decoded in one go — sample for sample for the lossless formats, and within
/// what a decoder that starts in the middle differs by for the lossy ones.
#[test]
fn sheet_tracks_of_every_format_are_their_stretches_of_the_file() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut noise = 0x2545_F491_4F6C_DD1Du64;
    let mut roll = move |n: u64| {
        noise ^= noise << 13;
        noise ^= noise >> 7;
        noise ^= noise << 17;
        (noise >> 11) % n
    };
    for (name, lossless) in [
        ("gap_a.wav", true),
        ("gap_a.flac", true),
        ("gap_a.aiff", true),
        ("gap_a.alac.m4a", true),
        ("gap_a.mp3", false),
        ("gap_a.mp2", false),
        ("gap_a.ogg", false),
        ("gap_a.aac.m4a", false),
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let extension = name.split_once('.').expect("extension").1;
        let image = dir.path().join(format!("image.{extension}"));
        std::fs::copy(fixtures.join(name), &image).expect("copy fixture");
        let whole = decode_path(&image);
        let (rate, channels) = {
            let dec = TrackDecoder::open(&image).expect("opens");
            (dec.info().sample_rate, dec.info().channels)
        };
        let total = (whole.len() / channels) as u64;
        // The file's length in CD frames (75 to the second), rounded down.
        let cd_frames = total * 75 / u64::from(rate);
        assert!(cd_frames > 8, "{name}: too short to cut");
        for round in 0..12 {
            // Two to four tracks, starting at random CD frames (the first not always at 0).
            let mut starts: Vec<u64> = (0..2 + roll(3)).map(|_| roll(cd_frames)).collect();
            starts.sort_unstable();
            starts.dedup();
            let sheet = dir.path().join("image.cue");
            let mut text = format!("FILE \"image.{extension}\" WAVE\n");
            for (i, start) in starts.iter().enumerate() {
                text.push_str(&format!(
                    "TRACK {:02} AUDIO\nINDEX 01 {:02}:{:02}:{:02}\n",
                    i + 1,
                    start / 75 / 60,
                    start / 75 % 60,
                    start % 75
                ));
            }
            std::fs::write(&sheet, text).expect("write sheet");
            let frame = |cd: u64| cd * u64::from(rate) / 75;
            for (i, start) in starts.iter().enumerate() {
                let track = oneum::library::cue::track_path(&sheet, i as u32 + 1);
                let (from, to) = (
                    frame(*start),
                    starts.get(i + 1).map_or(total, |next| frame(*next)),
                );
                let pcm = decode_path(&track);
                let context = format!("{name} round {round} track {} of {starts:?}", i + 1);
                assert_eq!(
                    (pcm.len() / channels) as u64,
                    to - from,
                    "{context}: length"
                );
                let dec = TrackDecoder::open(&track).expect("opens");
                assert_eq!(dec.info().total_frames, Some(to - from), "{context}");
                let expected = &whole[from as usize * channels..to as usize * channels];
                if lossless {
                    assert!(pcm == expected, "{context}: not the file's audio");
                } else {
                    // A lossy decoder that starts in the middle needs a moment.
                    let skip = (4_608 * channels).min(pcm.len());
                    let diff = max_abs_diff(&pcm[skip..], &expected[skip..]);
                    assert!(diff < 1e-3, "{context}: differs by {diff}");
                }
                // A seek to a random place in the track: the rest of the track from
                // there, and nothing of the next one.
                if to > from {
                    let at = roll(to - from);
                    let mut dec = TrackDecoder::open(&track).expect("opens");
                    dec.seek(at).expect("seeks");
                    let (mut rest, mut buf) = (Vec::new(), Vec::new());
                    while dec.next(&mut buf).expect("decodes") {
                        rest.extend_from_slice(&buf);
                    }
                    let expected = &expected[at as usize * channels..];
                    assert_eq!(
                        rest.len(),
                        expected.len(),
                        "{context}: length after a seek to {at}"
                    );
                    if lossless {
                        assert!(rest == expected, "{context}: after a seek to {at}");
                    } else {
                        let skip = (4_608 * channels).min(rest.len());
                        let diff = max_abs_diff(&rest[skip..], &expected[skip..]);
                        assert!(diff < 1e-3, "{context}: seek to {at} differs by {diff}");
                    }
                }
            }
        }
    }
}

/// The tracks of a sheet and the image itself, queued in any order (a track twice, the
/// image between its own tracks): what is heard is each of them in turn, whole and
/// without a gap, read from the file or from memory (where tracks of one image share
/// one copy of it).
#[test]
fn sheet_tracks_in_any_order_play_as_queued() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (image, sheet, _) = write_image_and_sheet(dir.path(), "image.wav");
    let mut paths: Vec<PathBuf> = (1..=3)
        .map(|n| oneum::library::cue::track_path(&sheet, n))
        .collect();
    paths.push(image);
    let audio: Vec<Vec<f32>> = paths.iter().map(|p| decode_path(p)).collect();
    let mut noise = 0x9E37_79B9_7F4A_7C15u64;
    let mut roll = move |n: u64| {
        noise ^= noise << 13;
        noise ^= noise >> 7;
        noise ^= noise << 17;
        (noise >> 11) % n
    };
    for round in 0..10 {
        let order: Vec<usize> = (0..2 + roll(5)).map(|_| roll(4) as usize).collect();
        let names: Vec<String> = order
            .iter()
            .map(|&i| paths[i].display().to_string())
            .collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let expected: Vec<f32> = order
            .iter()
            .flat_map(|&i| audio[i].iter().copied())
            .collect();
        for (how, played) in [
            ("file", play(&names, 44_100)),
            ("memory", play_from_memory(&names, 44_100, 0)),
        ] {
            let context = format!("round {round} ({how}): {order:?}");
            assert_eq!(played.underruns, 0, "{context}");
            assert_eq!(played.frames() * 2, expected.len(), "{context}: length");
            assert_eq!(played.started().len(), order.len(), "{context}: starts");
            let heard = &played.pcm[FADE_GUARD * 2..];
            assert!(
                max_abs_diff(heard, &expected[FADE_GUARD * 2..]) < 1e-6,
                "{context}: not what was queued"
            );
        }
    }
}

/// Damage inside a sheet track is passed over like anywhere else (the FLAC reader drops
/// the broken frame and finds the next), and the track still ends exactly where the
/// next one starts: the tracks joined are the image as it decodes, hole included
/// (counting what was decoded would run on into the next track by the hole's length).
#[test]
fn a_damaged_packet_does_not_move_where_a_sheet_track_ends() {
    let has_ffmpeg = std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !has_ffmpeg {
        eprintln!("skipped: ffmpeg is not installed");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let (image, sheet, starts) = write_image_and_sheet(dir.path(), "image.flac");
    // Damage a stretch in the middle of the first track (about 0.7 s into a 5 s image;
    // the first track runs to 1.4 s).
    let mut bytes = std::fs::read(&image).expect("read");
    let at = bytes.len() * 14 / 100;
    for b in &mut bytes[at..at + 64] {
        *b ^= 0x5A;
    }
    std::fs::write(&image, &bytes).expect("write");

    let mut whole_dec = TrackDecoder::open(&image).expect("opens");
    let (mut whole, mut buf) = (Vec::new(), Vec::new());
    while whole_dec.next(&mut buf).expect("decodes") {
        whole.extend_from_slice(&buf);
    }
    assert!(
        whole.len() < 5 * 44_100 * 2,
        "the damage must leave a hole ({} samples)",
        whole.len()
    );
    let mut joined = Vec::new();
    for n in 1..=3 {
        joined.extend(decode_path(&oneum::library::cue::track_path(&sheet, n)));
    }
    assert_eq!(joined.len(), whole.len(), "nothing twice, nothing left out");
    assert!(
        joined == whole,
        "the tracks joined are the image as it decodes"
    );
    // The second track starts at its own start, untouched by the hole before it.
    let second = decode_path(&oneum::library::cue::track_path(&sheet, 2));
    let clean_len = (starts[2] - starts[1]) as usize * 2;
    assert_eq!(second.len(), clean_len);
}

/// Like [`play`], with every track after the first mixed into the end of the one
/// before it over `crossfade_ms` (what the player asks for between songs that are not
/// of one album, with crossfade switched on).
fn play_crossfaded(names: &[&str], rate: u32, crossfade_ms: u32) -> Played {
    let (mut engine, mut renderer) = Engine::new_offline(rate, 1.0, ReplayGainSettings::default());
    let queue: Vec<TrackRequest> = names
        .iter()
        .enumerate()
        .map(|(i, name)| TrackRequest {
            crossfade_ms: if i == 0 { 0 } else { crossfade_ms },
            ..request(i as u64 + 1, name)
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
        .start_feed(queue[0].clone(), 0, slot.clone())
        .expect("feed starts");
    engine.set_playing(true);
    pump(&mut engine, &mut renderer, &queue, &slot)
}

/// What a crossfade of `overlap` frames makes of the end of `a` and the beginning of
/// `b` (both stereo): the gains of equal power, as the feed has them.
fn crossfaded(a: &[f32], b: &[f32], overlap: usize) -> Vec<f32> {
    let head = a.len() - overlap * 2;
    let mut out = a[..head].to_vec();
    for i in 0..overlap {
        let turn = (i as f32 + 0.5) / overlap as f32 * std::f32::consts::FRAC_PI_2;
        for channel in 0..2 {
            out.push(b[i * 2 + channel] * turn.sin() + a[head + i * 2 + channel] * turn.cos());
        }
    }
    out.extend_from_slice(&b[overlap * 2..]);
    out
}

/// Crossfade: the end of a track is mixed with the beginning of the next one, sample
/// for sample what the two give, and the next track counts as begun where the mix
/// begins. Everything outside the mix is the files' own audio.
#[test]
fn a_crossfade_mixes_the_end_of_a_track_with_the_beginning_of_the_next() {
    let (_, _, a) = decode_all("gap_a.wav");
    let (_, _, b) = decode_all("gap_b.wav");
    // 200 ms at 44.1 kHz.
    let overlap = 8_820;
    let expected = crossfaded(&a, &b, overlap);
    let played = play_crossfaded(&["gap_a.wav", "gap_b.wav"], 44_100, 200);
    assert_eq!(played.underruns, 0);
    assert_eq!(
        played.frames(),
        88_200 - overlap,
        "shorter than the two by what they share"
    );
    assert_eq!(
        played.started(),
        vec![(1, 0), (2, (44_100 - overlap) as u64)],
        "the second track begins where the mix begins"
    );
    // (The mix of two full-scale signals may be turned down by the peak limiter: then
    // the comparison holds for all that lies outside its reach only.)
    let diff = max_abs_diff(&played.pcm[FADE_GUARD * 2..], &expected[FADE_GUARD * 2..]);
    if played.limited == 0 {
        assert!(diff < 1e-6, "differs from the mix of the two by {diff}");
    } else {
        let before = (44_100 - overlap - 4_410) * 2;
        assert_eq!(
            max_abs_diff(
                &played.pcm[FADE_GUARD * 2..before],
                &expected[FADE_GUARD * 2..before]
            ),
            0.0
        );
    }
    // Without a crossfade the same two are joined as they are (the request says 0).
    assert_eq!(play(&["gap_a.wav", "gap_b.wav"], 44_100).frames(), 88_200);
}

/// Three tracks, each mixed into the one before it; and through the rate converter,
/// where each track gets a converter of its own at the mix.
#[test]
fn crossfades_follow_each_other_and_pass_the_rate_converter() {
    let played = play_crossfaded(&["gap_a.wav", "gap_b.wav", "gap_a.wav"], 44_100, 100);
    assert_eq!(played.underruns, 0);
    assert_eq!(played.frames(), 3 * 44_100 - 2 * 4_410);
    assert_eq!(
        played.started(),
        vec![(1, 0), (2, 44_100 - 4_410), (3, 2 * (44_100 - 4_410))]
    );

    let played = play_crossfaded(&["gap_a.wav", "gap_b.wav"], 48_000, 200);
    assert_eq!(played.underruns, 0);
    // Two seconds less a fifth, at 48 kHz, give or take what the converters round.
    let frames = played.frames() as i64;
    assert!((frames - 86_400).abs() < 64, "{frames} frames");
    let second = played.started()[1].1 as i64;
    assert!((second - 38_400).abs() < 64, "second track at {second}");
    assert!(rms(&played.left()) > 0.01, "there is sound");
}

/// The track that was to be mixed in cannot be opened: the track before it ends as it
/// does, all of it, not faded into nothing.
#[test]
fn a_track_whose_follower_cannot_be_opened_ends_as_it_does() {
    let (_, _, a) = decode_all("gap_a.wav");
    let played = play_crossfaded(&["gap_a.wav", "no_such_file.wav"], 44_100, 300);
    assert_eq!(played.frames(), 44_100);
    assert_eq!(
        max_abs_diff(&played.pcm[FADE_GUARD * 2..], &a[FADE_GUARD * 2..]),
        0.0
    );
    assert!(
        played
            .events
            .iter()
            .any(|event| matches!(event, EngineEvent::TrackFailed { .. })),
        "and the failure is told"
    );
}

/// Opus and WavPack against another decoder: files made here with ffmpeg from noise, a
/// sweep and speech-like bursts (so that every mode of Opus is used: the music coder,
/// the speech coder and both together) are decoded by this app and by ffmpeg. WavPack
/// must be the same sample for sample; Opus, which a decoder may round differently,
/// within a small part of full scale.
///
/// Run with: `cargo test --test audio_pipeline -- against_ffmpeg --ignored --nocapture`.
#[test]
#[ignore = "needs ffmpeg with libopus"]
fn opus_and_wavpack_decode_as_ffmpeg_decodes_them() {
    let dir = tempfile::tempdir().expect("temp dir");
    let at = |name: &str| dir.path().join(name);
    let ffmpeg = |args: &[&str]| {
        let out = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y"])
            .args(args)
            .current_dir(dir.path())
            .output()
            .expect("ffmpeg runs");
        assert!(
            out.status.success(),
            "ffmpeg {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    };
    // Twenty seconds: pink noise under a sweep, loud and quiet stretches.
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "anoisesrc=color=pink:sample_rate=48000:duration=20:amplitude=0.3:seed=7",
        "-f",
        "lavfi",
        "-i",
        "aevalsrc=0.4*sin(2*PI*(200+900*t)*t)*(0.55+0.45*sin(2*PI*0.7*t)):s=48000:d=20",
        "-filter_complex",
        "[0][1]amix=inputs=2:normalize=0,aformat=channel_layouts=stereo",
        "-c:a",
        "pcm_s16le",
        "source48.wav",
    ]);
    ffmpeg(&[
        "-i",
        "source48.wav",
        "-ar",
        "44100",
        "-c:a",
        "pcm_s24le",
        "source44_24.wav",
    ]);
    let ours = |name: &str| -> (u32, usize, Vec<f32>) {
        let mut dec = TrackDecoder::open(&at(name)).expect("file opens");
        let info = dec.info().clone();
        let (mut all, mut buf) = (Vec::new(), Vec::new());
        while dec.next(&mut buf).expect("file decodes") {
            all.extend_from_slice(&buf);
        }
        assert_eq!(dec.skipped_packets(), 0, "{name}: nothing damaged");
        (info.sample_rate, info.channels, all)
    };
    let theirs = |args: &[&str]| -> Vec<f32> {
        let mut all = vec!["-i"];
        all.extend_from_slice(args);
        all.extend_from_slice(&["-f", "f32le", "-c:a", "pcm_f32le", "-"]);
        ffmpeg(&all)
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect()
    };

    for (name, encode) in [
        ("music_128k.opus", vec!["-c:a", "libopus", "-b:a", "128k"]),
        ("music_48k.opus", vec!["-c:a", "libopus", "-b:a", "48k"]),
        (
            "voice_16k.opus",
            vec![
                "-c:a",
                "libopus",
                "-b:a",
                "16k",
                "-application",
                "voip",
                "-ac",
                "1",
            ],
        ),
        (
            "voice_32k.opus",
            vec!["-c:a", "libopus", "-b:a", "32k", "-application", "voip"],
        ),
        (
            "short_frames.opus",
            vec!["-c:a", "libopus", "-b:a", "96k", "-frame_duration", "5"],
        ),
        (
            "long_frames.opus",
            vec!["-c:a", "libopus", "-b:a", "64k", "-frame_duration", "60"],
        ),
    ] {
        let mut args = vec!["-i", "source48.wav"];
        args.extend(encode);
        args.push(name);
        ffmpeg(&args);
        let (rate, channels, pcm) = ours(name);
        // The other decoder: libopus itself, as ffmpeg runs it.
        let out = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-c:a", "libopus", "-i", name])
            .args(["-f", "f32le", "-c:a", "pcm_f32le", "-"])
            .current_dir(dir.path())
            .output()
            .expect("ffmpeg runs");
        assert!(out.status.success());
        let reference: Vec<f32> = out
            .stdout
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        assert_eq!(rate, 48_000, "{name}");
        assert_eq!(
            pcm.len(),
            reference.len(),
            "{name}: as many samples as ffmpeg gives"
        );
        assert_eq!(
            pcm.len() / channels,
            48_000 * 20,
            "{name}: twenty seconds exactly"
        );
        let worst = max_abs_diff(&pcm, &reference);
        let error = rms(&pcm
            .iter()
            .zip(&reference)
            .map(|(a, b)| a - b)
            .collect::<Vec<_>>());
        println!(
            "REPORT {name}: {channels} ch, {} frames, largest difference from libopus {worst:.6}, RMS of the difference {error:.7}",
            pcm.len() / channels
        );
        assert!(worst < 2e-3, "{name}: differs from libopus by {worst}");
        assert!(error < 1e-4, "{name}: RMS difference from libopus {error}");
    }

    for (name, source, encode) in [
        ("s16.wv", "source48.wav", vec!["-c:a", "wavpack"]),
        ("s24.wv", "source44_24.wav", vec!["-c:a", "wavpack"]),
        (
            "s16_high.wv",
            "source48.wav",
            vec!["-c:a", "wavpack", "-compression_level", "8"],
        ),
        (
            "mono.wv",
            "source48.wav",
            vec!["-ac", "1", "-c:a", "wavpack"],
        ),
        (
            "float.wv",
            "source48.wav",
            vec![
                "-af",
                "volume=0.5",
                "-sample_fmt",
                "fltp",
                "-c:a",
                "wavpack",
            ],
        ),
    ] {
        let mut args = vec!["-i", source];
        args.extend(encode);
        args.push(name);
        ffmpeg(&args);
        let (_, channels, pcm) = ours(name);
        let reference = theirs(&[name]);
        assert_eq!(
            pcm.len(),
            reference.len(),
            "{name}: as many samples as ffmpeg gives"
        );
        let worst = max_abs_diff(&pcm, &reference);
        println!(
            "REPORT {name}: {channels} ch, {} frames, largest difference from ffmpeg {worst}",
            pcm.len() / channels
        );
        assert_eq!(
            worst, 0.0,
            "{name} must be what ffmpeg decodes, sample for sample"
        );
        // And a place in the middle is found exactly.
        let mut dec = TrackDecoder::open(&at(name)).expect("opens");
        let frame = (pcm.len() / channels) as u64 * 2 / 3 + 12_345;
        dec.seek(frame).expect("seeks");
        let mut buf = Vec::new();
        assert!(dec.next(&mut buf).expect("decodes"));
        let from = frame as usize * channels;
        assert_eq!(buf[..], pcm[from..from + buf.len()], "{name}: after a seek");
    }
}

/// Monkey's Audio, with a file put together by hand of frames marked silent (see
/// scripts/gen-fixtures.sh: there is no encoder to make one of music with): the file
/// is read, its length is known, and a position is found in it.
#[test]
fn monkeys_audio_is_read_and_sought_in() {
    let mut dec = TrackDecoder::open(&fixture("silence.ape")).expect("opens");
    let info = dec.info().clone();
    assert_eq!(
        (info.codec.as_str(), info.sample_rate, info.channels),
        ("ape", 44_100, 2)
    );
    assert_eq!(info.total_frames, Some(183_456));
    let mut buf = Vec::new();
    let mut count = |dec: &mut TrackDecoder| {
        let mut frames = 0;
        while dec.next(&mut buf).expect("decodes") {
            assert!(buf.iter().all(|s| *s == 0.0), "silence");
            frames += buf.len() / 2;
        }
        frames
    };
    assert_eq!(count(&mut dec), 183_456, "every frame of the file");
    assert_eq!(dec.skipped_packets(), 0);
    for (frame, left) in [
        (100_000u64, 83_456),
        (0, 183_456),
        (183_455, 1),
        (73_728, 109_728),
    ] {
        dec.seek(frame).expect("seeks");
        assert_eq!(count(&mut dec), left, "from frame {frame}");
    }
    dec.seek(200_000).expect("seeks");
    assert_eq!(count(&mut dec), 0, "nothing past the end");
}

/// A frame of Monkey's Audio is longer than the buffer between the decoder and the
/// output: the file plays through all the same (it is given in pieces), at the rate
/// of the file and through the resampler.
#[test]
fn a_file_of_long_blocks_plays_through_the_feed() {
    for rate in [44_100u32, 48_000] {
        let played = play(&["silence.ape"], rate);
        assert_eq!(played.underruns, 0, "at {rate} Hz");
        let expected = 183_456u64 * u64::from(rate) / 44_100;
        assert!(
            (played.frames() as u64).abs_diff(expected) <= 2,
            "at {rate} Hz: {} frames for {expected}",
            played.frames()
        );
        assert_eq!(played.started(), vec![(1, 0)]);
    }
    // FLAC with the largest blocks the format has (65,535 frames, a second and a
    // half): through to the end, sample for sample.
    let played = play(&["tone_big_blocks.flac"], 44_100);
    assert_eq!(played.frames(), 132_300);
    let (_, _, whole) = decode_all("tone_big_blocks.flac");
    assert_eq!(
        max_abs_diff(&played.pcm[FADE_GUARD * 2..], &whole[FADE_GUARD * 2..]),
        0.0
    );
    // And after a WavPack song without a gap between them.
    let played = play(&["gap_a.wv", "silence.ape"], 44_100);
    assert_eq!(played.frames(), 44_100 + 183_456);
    assert_eq!(played.started(), vec![(1, 0), (2, 44_100)]);
}

/// How strongly `hz` is in a signal sampled at `rate` (the amplitude of that one
/// frequency).
fn level_at(signal: &[f32], hz: f32, rate: u32) -> f32 {
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (n, s) in signal.iter().enumerate() {
        let turn = std::f64::consts::TAU * f64::from(hz) * n as f64 / f64::from(rate);
        re += f64::from(*s) * turn.cos();
        im += f64::from(*s) * turn.sin();
    }
    (2.0 * (re * re + im * im).sqrt() / signal.len().max(1) as f64) as f32
}

/// At another speed two songs take that much less or more time, the second begins
/// where the first ends, and the tone they hold stays at its pitch. So it is with a
/// device that runs above the rate the pacer works at (the sound is brought up to
/// the device's rate after it).
#[test]
fn another_speed_changes_the_length_and_keeps_the_pitch() {
    for (rate, speed) in [
        (44_100u32, 1.5f32),
        (44_100, 0.5),
        (48_000, 2.0),
        (96_000, 1.25),
    ] {
        let (mut engine, mut renderer) =
            Engine::new_offline(rate, 1.0, ReplayGainSettings::default());
        engine.set_speed(speed);
        let queue: Vec<TrackRequest> = ["gap_a.wav", "gap_b.wav"]
            .iter()
            .enumerate()
            .map(|(i, n)| request(i as u64 + 1, n))
            .collect();
        let slot = NextSlot::new();
        slot.set(NextTrack::Track(queue[1].clone()));
        engine
            .start_feed(queue[0].clone(), 0, slot.clone())
            .expect("feed starts");
        engine.set_playing(true);
        let played = pump(&mut engine, &mut renderer, &queue, &slot);
        let what = format!("{rate} Hz at {speed}");
        assert_eq!(played.underruns, 0, "{what}");
        // A second of song is this many frames of output.
        let second = (f64::from(rate) / f64::from(speed)) as usize;
        let slack = rate as usize / 20;
        let started = played.started();
        assert_eq!(started.len(), 2, "{what}");
        assert_eq!(started[0], (1, 0), "{what}");
        assert!(
            (started[1].1 as usize).abs_diff(second) < slack,
            "{what}: the second song begins at {} of {second}",
            started[1].1
        );
        assert!(
            played.frames().abs_diff(2 * second) < 2 * slack,
            "{what}: {} frames for {}",
            played.frames(),
            2 * second
        );
        // Each song by itself (where one ends and the next begins the tone may
        // jump in phase: nothing is carried from song to song at another speed).
        let left = played.left();
        let boundary = started[1].1 as usize;
        for (song, part) in [&left[..boundary], &left[boundary..]].iter().enumerate() {
            let middle = &part[part.len() / 8..part.len() * 7 / 8];
            let there = level_at(middle, 441.0, rate);
            assert!(
                (there - AMP).abs() < 0.05,
                "{what}, song {song}: the tone's level is {there}"
            );
            let moved = level_at(middle, 441.0 * speed, rate);
            assert!(
                moved < 0.05,
                "{what}, song {song}: {moved} of the tone moved in pitch"
            );
        }
        assert!(
            left.iter().all(|s| s.is_finite() && s.abs() <= 1.0),
            "{what}"
        );
    }
}

/// Plays `names` at `rate` with the upsampling filter the listener chose.
fn play_filtered(names: &[&str], rate: u32, filter: oneum::audio::upsample::Filter) -> Played {
    let (mut engine, mut renderer) = Engine::new_offline(rate, 1.0, ReplayGainSettings::default());
    engine.set_output_settings(OutputSettings {
        filter,
        ..OutputSettings::default()
    });
    let queue: Vec<TrackRequest> = names
        .iter()
        .enumerate()
        .map(|(i, n)| request(i as u64 + 1, n))
        .collect();
    let slot = NextSlot::new();
    slot.set(
        queue
            .get(1)
            .cloned()
            .map_or(NextTrack::End, NextTrack::Track),
    );
    engine
        .start_feed(queue[0].clone(), 0, slot.clone())
        .expect("feed starts");
    engine.set_playing(true);
    pump(&mut engine, &mut renderer, &queue, &slot)
}

/// With another upsampling filter, two songs that are one tone cut in two come out
/// at a whole multiple of their rate exactly that many times as long, the second
/// where the first ends, and as one tone still. Where the rate is not a whole multiple
/// the filter does not come into it.
#[test]
fn a_chosen_upsampling_filter_raises_songs_without_a_gap() {
    use oneum::audio::upsample::Filter;
    let songs = ["gap_a.flac", "gap_b.flac"];
    for factor in [2u32, 4, 8] {
        let rate = 44_100 * factor;
        let usual = play_filtered(&songs, rate, Filter::Sharp);
        assert_eq!(usual.frames(), 88_200 * factor as usize);
        for filter in [Filter::Slow, Filter::Minimum] {
            let played = play_filtered(&songs, rate, filter);
            let what = format!("{filter:?} at {rate} Hz");
            assert_eq!(played.underruns, 0, "{what}");
            assert_eq!(played.frames(), 88_200 * factor as usize, "{what}");
            assert_eq!(
                played.started(),
                vec![(1, 0), (2, 44_100 * u64::from(factor))],
                "{what}"
            );
            // The tone is there at its level, and nothing happens where the two
            // songs meet: around that place the signal is as smooth as anywhere.
            let left = played.left();
            let usual_left = usual.left();
            let level = level_at(&left[rate as usize / 2..rate as usize * 3 / 2], 441.0, rate);
            let usual_level = level_at(
                &usual_left[rate as usize / 2..rate as usize * 3 / 2],
                441.0,
                rate,
            );
            assert!(
                (level / usual_level - 1.0).abs() < 1e-3,
                "{what}: {level} for {usual_level}"
            );
            let step = |from: usize, to: usize| {
                left[from..to]
                    .windows(2)
                    .map(|w| (w[1] - w[0]).abs())
                    .fold(0.0f32, f32::max)
            };
            let seam = 44_100 * factor as usize;
            let reach = 200 * factor as usize;
            let (at_seam, elsewhere) = (
                step(seam - reach, seam + reach),
                step(seam / 2 - reach, seam / 2 + reach),
            );
            assert!(
                at_seam <= elsewhere * 1.02,
                "{what}: {at_seam} at the seam, {elsewhere} elsewhere"
            );
            // The symmetric filter is in step with the usual conversion, sample for
            // sample (the tone lies far below where the two differ).
            if filter == Filter::Slow {
                let guard = FADE_GUARD * factor as usize * 2;
                let diff = max_abs_diff(
                    &played.pcm[guard..played.pcm.len() - guard],
                    &usual.pcm[guard..usual.pcm.len() - guard],
                );
                assert!(diff < 1e-4, "{what}: {diff}");
            }
        }
    }
    // 44.1 → 48 kHz is no whole multiple: converted as ever, whatever the filter.
    let usual = play_filtered(&songs, 48_000, Filter::Sharp);
    for filter in [Filter::Slow, Filter::Minimum] {
        assert_eq!(play_filtered(&songs, 48_000, filter).pcm, usual.pcm);
    }
    // And a song at the output's own rate is not touched by any of them.
    let (_, _, whole) = decode_all("gap_a.flac");
    let played = play_filtered(&["gap_a.flac"], 44_100, Filter::Minimum);
    assert_eq!(
        max_abs_diff(&played.pcm[FADE_GUARD * 2..], &whole[FADE_GUARD * 2..]),
        0.0
    );
}

/// Upsampling asks the device for the highest of eight, four and two times a song's
/// rate that it offers; a device that offers none is left as it is.
#[test]
fn upsampling_runs_the_device_at_the_highest_whole_multiple_it_offers() {
    let (mut engine, _renderer) = Engine::new_offline(44_100, 1.0, ReplayGainSettings::default());
    engine.set_offline_rates(&[44_100, 48_000, 88_200, 96_000, 176_400, 192_000]);
    assert_eq!(
        engine.preferred_rate(Some(44_100)),
        None,
        "the device is left alone"
    );
    engine.set_output_settings(OutputSettings {
        upsample: true,
        ..OutputSettings::default()
    });
    assert_eq!(engine.preferred_rate(Some(44_100)), Some(176_400));
    assert_eq!(engine.preferred_rate(Some(48_000)), Some(192_000));
    assert_eq!(engine.preferred_rate(Some(88_200)), Some(176_400));
    assert_eq!(engine.preferred_rate(Some(96_000)), Some(192_000));
    assert_eq!(engine.preferred_rate(Some(192_000)), None);
    assert_eq!(engine.preferred_rate(None), None);
    assert_eq!(engine.preferred_rate(Some(22_050)), Some(176_400));
    // A device without a whole multiple is left alone, unless the song's own rate is
    // asked for as well.
    engine.set_offline_rates(&[44_100, 48_000]);
    assert_eq!(engine.preferred_rate(Some(44_100)), None);
    assert_eq!(engine.preferred_rate(Some(22_050)), Some(44_100));
    engine.set_output_settings(OutputSettings {
        upsample: true,
        match_sample_rate: true,
        ..OutputSettings::default()
    });
    assert_eq!(engine.preferred_rate(Some(44_100)), Some(44_100));
    assert_eq!(engine.preferred_rate(Some(96_000)), Some(48_000));
    // Following the song's rate alone asks for that rate, as before.
    engine.set_offline_rates(&[44_100, 48_000, 88_200, 176_400]);
    engine.set_output_settings(OutputSettings {
        match_sample_rate: true,
        ..OutputSettings::default()
    });
    assert_eq!(engine.preferred_rate(Some(44_100)), Some(44_100));
}

/// Room correction: everything that plays is filtered with the impulse response,
/// each channel with its own, nothing comes out late, and two songs that are one tone
/// cut in two are still one tone. At another output rate the response is converted.
#[test]
fn room_correction_filters_what_plays_and_adds_no_delay() {
    use oneum::audio::room::Impulse;
    // Left: half as loud. Right: three frames late, and the other way up.
    let impulse = Impulse::from_frames(
        "test room",
        44_100,
        &[0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -1.0],
    )
    .expect("an impulse response");
    let play_with_room = |names: &[&str], rate: u32| {
        let (mut engine, mut renderer) =
            Engine::new_offline(rate, 1.0, ReplayGainSettings::default());
        engine.set_room(Some(std::sync::Arc::new(impulse.clone())));
        let queue: Vec<TrackRequest> = names
            .iter()
            .enumerate()
            .map(|(i, n)| request(i as u64 + 1, n))
            .collect();
        let slot = NextSlot::new();
        slot.set(
            queue
                .get(1)
                .cloned()
                .map_or(NextTrack::End, NextTrack::Track),
        );
        engine
            .start_feed(queue[0].clone(), 0, slot.clone())
            .expect("feed starts");
        engine.set_playing(true);
        pump(&mut engine, &mut renderer, &queue, &slot)
    };
    let songs = ["gap_a.flac", "gap_b.flac"];
    let plain = play(&songs, 44_100);
    let filtered = play_with_room(&songs, 44_100);
    assert_eq!(filtered.underruns, 0);
    assert_eq!(filtered.frames(), plain.frames(), "no frame more or less");
    assert_eq!(filtered.started(), plain.started());
    let frames = plain.frames();
    for n in FADE_GUARD..frames - FADE_GUARD {
        let (left, right) = (filtered.pcm[n * 2], filtered.pcm[n * 2 + 1]);
        assert!(
            (left - plain.pcm[n * 2] * 0.5).abs() < 1e-6,
            "left at {n}: {left}"
        );
        assert!(
            (right + plain.pcm[(n - 3) * 2 + 1]).abs() < 1e-6,
            "right at {n}: {right}"
        );
    }
    // At twice the rate the response is converted: the same tone, half as loud on
    // the left as without the room.
    let plain = play(&songs, 88_200);
    let filtered = play_with_room(&songs, 88_200);
    assert_eq!(filtered.frames(), plain.frames());
    let middle = 44_100..132_300;
    let level = |played: &Played| level_at(&played.left()[middle.clone()], 441.0, 88_200);
    let ratio = level(&filtered) / level(&plain);
    assert!((ratio - 0.5).abs() < 0.005, "{ratio}");
}
