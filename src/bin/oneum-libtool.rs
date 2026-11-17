//! Developer tool for the library module: search/sort benchmarks on synthetic data,
//! generating a large test database, and measuring start-up and scan times.
//!
//! ```text
//! oneum-libtool bench-search <N>            build N synthetic tracks, time build/search/sort
//! oneum-libtool gen-db <N> <data_dir>       write N synthetic tracks into <data_dir>/library.db
//! oneum-libtool open-bench <data_dir>       time Library::open on an existing database
//! oneum-libtool scan <folder> [--verbose]   scan a folder into a throw-away library
//! oneum-libtool audio-devices               list output devices: rates, hardware formats, exclusive owner
//! oneum-libtool audio-probe <id> <rate> [--exclusive]
//!                                           switch a device to a rate for a second, then put it back
//! oneum-libtool audio-selftest <id>         play silence on a device and check that the engine
//!                                           recovers from a late format notification
//! ```
//! Synthetic metadata is generated from a fixed seed; no audio files are involved.
use oneum::library::{Library, LibraryPaths, LibrarySnapshot, SortKey, album_key};
use oneum::model::{ReplayGainTags, Track};
use oneum::storage::db::{DB_FILE_NAME, Db, StoredTrack};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

const USAGE: &str = "usage:
  oneum-libtool bench-search <N>
  oneum-libtool gen-db <N> <data_dir>
  oneum-libtool open-bench <data_dir>
  oneum-libtool scan <folder> [--verbose]
  oneum-libtool loudness <file-or-folder>…
  oneum-libtool loudness-vs-tags <folder>
  oneum-libtool leveled <folder> [--count N] [--ab <out.wav>]
  oneum-libtool audio-devices
  oneum-libtool audio-probe <device-id> <rate> [--exclusive]
  oneum-libtool audio-selftest <device-id>";

/// Root of the synthetic library. It does not exist, so the app shows it as offline.
const FAKE_ROOT: &str = "/Volumes/OnEumBench/Music";

/// Measures the loudness of files the way the app does for songs without ReplayGain
/// tags, one line per file (tab-separated: LUFS, gain in dB, peak, seconds of audio,
/// seconds taken, path), so the numbers can be compared with another scanner's.
/// Folders are walked; files that cannot be decoded are reported and skipped.
fn loudness(paths: &[&str]) -> Result<(), String> {
    use oneum::audio::loudness::{gain_db, measure_file};
    use std::sync::atomic::AtomicBool;

    fn collect(path: &Path, out: &mut Vec<PathBuf>) {
        if path.is_dir() {
            let mut entries: Vec<PathBuf> = std::fs::read_dir(path)
                .map(|dir| dir.flatten().map(|e| e.path()).collect())
                .unwrap_or_default();
            entries.sort();
            for entry in entries {
                collect(&entry, out);
            }
        } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            let e = e.to_ascii_lowercase();
            [
                "flac", "mp3", "m4a", "wav", "aiff", "aif", "ogg", "mp2", "dsf", "dff", "mka",
                "webm", "opus", "wv", "ape",
            ]
            .contains(&e.as_str())
        }) {
            out.push(path.to_path_buf());
        }
    }

    let mut files = Vec::new();
    for path in paths {
        collect(Path::new(path), &mut files);
    }
    if files.is_empty() {
        return Err("no audio files found".to_string());
    }
    let never = AtomicBool::new(false);
    let started = Instant::now();
    let (mut audio_seconds, mut failed) = (0.0f64, 0usize);
    for file in &files {
        let one = Instant::now();
        match measure_file(file, &never) {
            Ok(Some(m)) => {
                // Blocks are 100 ms apart, and the first one ends after 400 ms.
                let seconds = if m.blocks.is_empty() {
                    0.0
                } else {
                    m.blocks.len() as f64 / 10.0 + 0.3
                };
                audio_seconds += seconds;
                let loudness = m.loudness();
                println!(
                    "{}\t{:+.2}\t{:.4}\t{:.1}\t{:.3}\t{}",
                    loudness.map_or("silent".to_string(), |l| format!("{l:.2}")),
                    gain_db(loudness),
                    m.peak,
                    seconds,
                    one.elapsed().as_secs_f64(),
                    file.display()
                );
            }
            Ok(None) => {}
            Err(err) => {
                failed += 1;
                println!("error\t\t\t\t\t{}\t{err}", file.display());
            }
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    eprintln!(
        "{} files ({failed} not decoded), {:.0} s of audio in {:.1} s on one thread: {:.0}x real time",
        files.len(),
        audio_seconds,
        elapsed,
        audio_seconds / elapsed.max(1e-9)
    );
    Ok(())
}

/// Scans a folder into a throw-away library and, for every track whose file carries a
/// ReplayGain track gain, measures the file and prints how far the app's own values
/// are from the tag's (tab-separated: gain difference in dB, measured gain, tagged
/// gain, measured peak, tagged peak, codec, channels, path). The files are only read.
fn loudness_vs_tags(folder: &Path) -> Result<(), String> {
    use oneum::audio::loudness::{gain_db, measure_file};
    use std::sync::atomic::AtomicBool;

    let work = std::env::temp_dir().join(format!("oneum-libtool-loudness-{}", std::process::id()));
    let paths = LibraryPaths {
        data_dir: work.join("data"),
        cache_dir: work.join("cache"),
    };
    let result = (|| {
        let (library, _) = Library::open(paths).map_err(|e| e.to_string())?;
        library.add_folder(folder).map_err(|e| e.to_string())?;
        library.wait_for_scan(Duration::from_secs(3600));
        let snapshot = library.snapshot();
        let never = AtomicBool::new(false);
        let mut differences: Vec<f32> = Vec::new();
        for track in &snapshot.tracks {
            let Some(tagged) = track.replaygain.track_gain_db else {
                continue;
            };
            let Ok(Some(m)) = measure_file(&track.path, &never) else {
                continue;
            };
            let measured = gain_db(m.loudness());
            differences.push(measured - tagged);
            println!(
                "{:+.2}\t{measured:+.2}\t{tagged:+.2}\t{:.4}\t{}\t{}\t{}\t{}",
                measured - tagged,
                m.peak,
                track
                    .replaygain
                    .track_peak
                    .map_or("-".to_string(), |p| format!("{p:.4}")),
                track.codec,
                track.channels,
                track.path.display()
            );
        }
        library.shutdown();
        differences.sort_by(f32::total_cmp);
        let within = |limit: f32| differences.iter().filter(|d| d.abs() <= limit).count();
        eprintln!(
            "{} tracks, {} with a tagged track gain; measured - tagged: median {:+.2} dB, within 0.1 dB: {}, within 0.5 dB: {}, within 1 dB: {}, within 2 dB: {}",
            snapshot.tracks.len(),
            differences.len(),
            differences
                .get(differences.len() / 2)
                .copied()
                .unwrap_or(0.0),
            within(0.1),
            within(0.5),
            within(1.0),
            within(2.0),
        );
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// Plays one file through the real pipeline (decode, leveling, peak protection,
/// renderer) without a device, at the file's own sample rate and full volume, and
/// returns the stereo output with the number of frames the peak limiter turned down.
fn render_offline(
    path: &Path,
    rate: u32,
    values: ReplayGainTags,
    settings: oneum::audio::ReplayGainSettings,
) -> Result<(Vec<f32>, u64), String> {
    use oneum::audio::{Engine, EngineEvent, NextSlot, NextTrack, TrackRequest};
    const BLOCK: usize = 512;

    let (mut engine, mut renderer) = Engine::new_offline(rate, 1.0, settings);
    let slot = NextSlot::new();
    slot.set(NextTrack::End);
    let request = TrackRequest {
        item_id: 1,
        track_id: 1,
        path: path.to_path_buf(),
        replaygain: values,
        crossfade_ms: 0,
    };
    engine
        .start_feed(request, 0, slot)
        .map_err(|e| e.to_string())?;
    engine.set_playing(true);
    let mut pcm = Vec::new();
    let mut out = vec![0f32; BLOCK * 2];
    let mut events = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        if Instant::now() > deadline {
            return Err("rendering did not finish".to_string());
        }
        events.clear();
        engine.poll(&mut events);
        let mut drained = false;
        for event in &events {
            match event {
                EngineEvent::Drained { .. } => drained = true,
                EngineEvent::TrackFailed { error, .. } => return Err(error.to_string()),
                _ => {}
            }
        }
        if drained {
            break;
        }
        match engine.active_buffered() {
            Some((available, finished)) if available >= BLOCK as u64 || finished => {
                let before = engine.shared().position().1;
                renderer.render(&mut out, 2);
                let after = engine.shared().position().1;
                let consumed = (after.saturating_sub(before) as usize).min(BLOCK);
                pcm.extend_from_slice(&out[..consumed * 2]);
            }
            _ => std::thread::sleep(Duration::from_millis(1)),
        }
    }
    Ok((pcm, engine.limited_frames()))
}

/// Writes interleaved stereo float audio as a 16-bit WAV (with dither).
fn write_wav16(path: &Path, rate: u32, stereo: &[f32]) -> Result<(), String> {
    let mut rng = fastrand::Rng::with_seed(1);
    let mut bytes = Vec::with_capacity(44 + stereo.len() * 2);
    let data_len = (stereo.len() * 2) as u32;
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 4).to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for &sample in stereo {
        let dither = rng.f32() - rng.f32();
        let value = (sample * 32_767.0 + dither)
            .round()
            .clamp(-32_768.0, 32_767.0);
        bytes.extend_from_slice(&(value as i16).to_le_bytes());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, bytes).map_err(|e| e.to_string())
}

/// What volume leveling does to real files, measured at the output: scans a folder
/// into a throw-away library, measures what has no ReplayGain tags (as the app does),
/// then plays a sample of the tracks through the real pipeline with leveling by track
/// and prints the loudness of each file and of what came out (tab-separated: source
/// of the values, LUFS of the file, gain applied in dB, LUFS of the output with and
/// without clipping prevention, peak of the output, frames the peak limiter touched,
/// path). `--ab` writes a listening file: ten-second excerpts of the three loudest
/// and the three softest tracks of the sample as they are, then the same leveled.
fn leveled(folder: &Path, options: &[&str]) -> Result<(), String> {
    use oneum::audio::loudness::{Meter, measure_file};
    use oneum::audio::{ReplayGainMode, ReplayGainSettings};
    use std::sync::atomic::AtomicBool;

    let (mut count, mut ab): (usize, Option<PathBuf>) = (80, None);
    let mut rest = options.iter();
    while let Some(option) = rest.next() {
        match (*option, rest.next()) {
            ("--count", Some(n)) => count = parse_count(n)?,
            ("--ab", Some(path)) => ab = Some(PathBuf::from(path)),
            _ => return Err(USAGE.to_string()),
        }
    }
    let work = std::env::temp_dir().join(format!("oneum-libtool-leveled-{}", std::process::id()));
    let paths = LibraryPaths {
        data_dir: work.join("data"),
        cache_dir: work.join("cache"),
    };
    let result = (|| {
        let (library, _) = Library::open(paths).map_err(|e| e.to_string())?;
        library.add_folder(folder).map_err(|e| e.to_string())?;
        library.wait_for_scan(Duration::from_secs(3600));
        library.measure_loudness();
        library.wait_for_loudness(Duration::from_secs(3600));
        let snapshot = library.snapshot();
        // Songs of ordinary length that have values to level with, evenly spread.
        let candidates: Vec<&std::sync::Arc<Track>> = snapshot
            .tracks
            .iter()
            .filter(|t| {
                t.leveling().track_gain_db.is_some() && (30_000..900_000).contains(&t.duration_ms)
            })
            .collect();
        let step = (candidates.len() / count.max(1)).max(1);
        let never = AtomicBool::new(false);
        let level = |pcm: &[f32], rate: u32| {
            let mut meter = Meter::new(rate);
            meter.push(pcm);
            let m = meter.finish();
            (m.loudness(), m.peak)
        };
        let with = |prevent_clipping| ReplayGainSettings {
            mode: ReplayGainMode::Track,
            preamp_db: 0.0,
            prevent_clipping,
        };
        // (LUFS of the file, LUFS of the leveled output, track)
        let mut rows: Vec<(f64, f64, std::sync::Arc<Track>)> = Vec::new();
        for track in candidates.into_iter().step_by(step).take(count) {
            let Ok(Some(file)) = measure_file(&track.path, &never) else {
                continue;
            };
            let Some(before) = file.loudness() else {
                continue;
            };
            let values = track.leveling();
            let (protected, _) =
                render_offline(&track.path, track.sample_rate, values, with(true))?;
            let (full, limited) =
                render_offline(&track.path, track.sample_rate, values, with(false))?;
            let (after, peak) = level(&protected, track.sample_rate);
            let (after_full, _) = level(&full, track.sample_rate);
            let (Some(after), Some(after_full)) = (after, after_full) else {
                continue;
            };
            println!(
                "{}\t{before:.2}\t{:+.2}\t{after:.2}\t{after_full:.2}\t{peak:.3}\t{limited}\t{}",
                if track.replaygain.track_gain_db.is_some() {
                    "tag"
                } else {
                    "measured"
                },
                after - before,
                track.path.display()
            );
            rows.push((before, after, std::sync::Arc::clone(track)));
        }
        let spread = |values: &mut Vec<f64>| -> String {
            values.sort_by(f64::total_cmp);
            let n = values.len().max(1) as f64;
            let mean = values.iter().sum::<f64>() / n;
            let deviation = (values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n).sqrt();
            format!(
                "{:.1} to {:.1} LUFS, mean {mean:.1}, standard deviation {deviation:.2} LU",
                values.first().copied().unwrap_or(0.0),
                values.last().copied().unwrap_or(0.0)
            )
        };
        eprintln!("{} tracks played", rows.len());
        eprintln!(
            "  as they are:  {}",
            spread(&mut rows.iter().map(|r| r.0).collect())
        );
        eprintln!(
            "  leveled:      {}",
            spread(&mut rows.iter().map(|r| r.1).collect())
        );

        if let Some(out) = ab {
            // The same rate throughout, so that the excerpts can share one file.
            let mut usable: Vec<&(f64, f64, std::sync::Arc<Track>)> = rows
                .iter()
                .filter(|r| r.2.sample_rate == 44_100 && r.2.duration_ms >= 90_000)
                .collect();
            usable.sort_by(|a, b| a.0.total_cmp(&b.0));
            if usable.len() < 6 {
                return Err("not enough 44.1 kHz tracks for the listening file".to_string());
            }
            // Soft and loud in turn: the jumps are what leveling is there to remove.
            let n = usable.len();
            let picks = [
                usable[0],
                usable[n - 1],
                usable[1],
                usable[n - 2],
                usable[2],
                usable[n - 3],
            ];
            const RATE: usize = 44_100;
            let excerpt = |pcm: &[f32]| -> Vec<f32> {
                let (from, len) = (60 * RATE * 2, 10 * RATE * 2);
                let mut part = pcm[from.min(pcm.len())..(from + len).min(pcm.len())].to_vec();
                // Short fades, so that the cuts do not click.
                let fade = RATE / 20;
                let frames = part.len() / 2;
                for i in 0..fade.min(frames / 2) {
                    let gain = i as f32 / fade as f32;
                    for ch in 0..2 {
                        part[i * 2 + ch] *= gain;
                        part[(frames - 1 - i) * 2 + ch] *= gain;
                    }
                }
                part.extend(std::iter::repeat_n(0.0, RATE)); // half a second of silence
                part
            };
            let mut file = Vec::new();
            let off = ReplayGainSettings::default();
            for settings in [off, with(true)] {
                for (before, after, track) in picks.iter().map(|r| (r.0, r.1, &r.2)) {
                    let (pcm, _) =
                        render_offline(&track.path, track.sample_rate, track.leveling(), settings)?;
                    file.extend(excerpt(&pcm));
                    if settings.mode == ReplayGainMode::Off {
                        eprintln!("  excerpt: {before:.1} LUFS as it is, {after:.1} LUFS leveled");
                    }
                }
                file.extend(std::iter::repeat_n(0.0, RATE * 2 * 2)); // two seconds between the halves
            }
            write_wav16(&out, RATE as u32, &file)?;
            eprintln!(
                "wrote {} ({:.0} s)",
                out.display(),
                file.len() as f64 / 2.0 / RATE as f64
            );
        }
        library.shutdown();
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["bench-search", n] => parse_count(n).map(bench_search),
        ["gen-db", n, dir] => parse_count(n).and_then(|n| gen_db(n, Path::new(dir))),
        ["open-bench", dir] => open_bench(Path::new(dir)),
        ["scan", folder] => scan(Path::new(folder), false),
        ["scan", folder, "--verbose"] | ["scan", "--verbose", folder] => {
            scan(Path::new(folder), true)
        }
        ["loudness", paths @ ..] if !paths.is_empty() => loudness(paths),
        ["loudness-vs-tags", folder] => loudness_vs_tags(Path::new(folder)),
        ["leveled", folder, options @ ..] => leveled(Path::new(folder), options),
        ["audio-devices"] => audio_devices(),
        ["audio-probe", id, rate] => audio_probe(id, rate, false),
        ["audio-probe", id, rate, "--exclusive"] => audio_probe(id, rate, true),
        ["audio-selftest", id] => audio_selftest(id),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

/// Prints what the system reports about every output device. Read-only.
fn audio_devices() -> Result<(), String> {
    use oneum::audio::output::list_output_devices;
    use oneum::platform::device::AudioDevice;
    let devices = list_output_devices();
    if devices.is_empty() {
        return Err("no output devices".to_string());
    }
    for info in devices {
        let default = if info.is_default { " (default)" } else { "" };
        println!("{}{default}\n  id: {}", info.name, info.id);
        let Some(device) = AudioDevice::from_cpal_id(&info.id) else {
            println!("  (not found through the audio HAL)");
            continue;
        };
        println!("  nominal rate: {:?}", device.nominal_rate());
        println!("  rates: {:?}", device.rate_ranges());
        println!("  exclusive owner (pid): {:?}", device.hog_owner());
        match device.physical_format() {
            Some(f) => println!(
                "  hardware format now: {} Hz, {} bit{}, {} ch",
                f.sample_rate,
                f.bits,
                if f.float { " float" } else { "" },
                f.channels
            ),
            None => println!("  hardware format now: unknown"),
        }
        for f in device.physical_formats() {
            println!(
                "    offers {}..={} Hz, {} bit{}, {} ch{}",
                f.min_rate,
                f.max_rate,
                f.bits,
                if f.float { " float" } else { "" },
                f.channels,
                if f.pcm { "" } else { " (not PCM)" }
            );
        }
    }
    Ok(())
}

/// Does to a device what rate matching does for one track, with timings: claims it,
/// switches it to `rate`, waits a second and puts everything back. No audio is played.
/// Use it to see whether (and how fast) a DAC follows.
fn audio_probe(id: &str, rate: &str, exclusive: bool) -> Result<(), String> {
    use oneum::audio::output::{DeviceHold, default_output_id};
    let rate: u32 = rate
        .parse()
        .map_err(|_| format!("invalid rate {rate:?}\n{USAGE}"))?;
    let report = |label: &str, hold: &DeviceHold| {
        println!(
            "{label}: rate {:?}, hardware (bits, float) {:?}, exclusive {}, system default {:?}",
            hold.nominal_rate(),
            hold.hardware_bits(),
            hold.is_exclusive(),
            default_output_id()
        );
    };
    println!("system default before: {:?}", default_output_id());
    let started = Instant::now();
    let mut hold =
        DeviceHold::acquire(id, exclusive).ok_or_else(|| format!("device {id:?} not found"))?;
    println!("claimed in {:?}", started.elapsed());
    report("before", &hold);
    let started = Instant::now();
    let ok = hold.apply_rate(rate);
    println!("switch to {rate} Hz: ok={ok} in {:?}", started.elapsed());
    report("during", &hold);
    std::thread::sleep(Duration::from_secs(1));
    let started = Instant::now();
    drop(hold);
    println!("put back in {:?}", started.elapsed());
    println!("system default after: {:?}", default_output_id());
    audio_devices()
}

/// Writes `seconds` of a quiet 440 Hz tone as a 16-bit stereo WAV at `rate`.
fn write_tone_wav(path: &Path, rate: u32, seconds: u32) -> std::io::Result<()> {
    let frames = rate * seconds;
    let data_len = frames * 4;
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
    for n in 0..frames {
        let phase = f64::from(n) * 440.0 * std::f64::consts::TAU / f64::from(rate);
        let v = (phase.sin() * 3000.0) as i16;
        bytes.extend_from_slice(&v.to_le_bytes());
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(path, bytes)
}

/// Plays a tone on `id` at volume 0 with rate matching on, then does to the stream what
/// the audio backend does when the device announces a format change (it stops the stream
/// and reports an invalidation) while the device is in fact still at the stream's rate.
/// That is the "our own rate change is announced late" case. The engine must restart
/// the stream and go on, without rebuilding it. Nothing is audible.
fn audio_selftest(id: &str) -> Result<(), String> {
    // Like in the app, the engine runs on a thread of its own while the main thread
    // serves the system's run loop.
    let (tx, rx) = std::sync::mpsc::channel();
    let id = id.to_string();
    std::thread::spawn(move || {
        let _ = tx.send(audio_selftest_run(&id));
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(result) = rx.try_recv() {
            return result;
        }
        if Instant::now() >= deadline {
            return Err("FAIL: the self-test did not finish within a minute".into());
        }
        oneum::platform::pump_main_loop(Duration::from_millis(50));
    }
}

fn audio_selftest_run(id: &str) -> Result<(), String> {
    use oneum::audio::output::device_nominal_rate;
    use oneum::audio::{
        Engine, EngineEvent, NextSlot, NextTrack, OutputSettings, ReplayGainSettings, TrackRequest,
    };
    let rate = device_nominal_rate(id).ok_or_else(|| format!("device {id:?} not found"))?;
    let dir = std::env::temp_dir().join(format!("oneum-selftest-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let wav = dir.join("tone.wav");
    write_tone_wav(&wav, rate, 8).map_err(|e| e.to_string())?;

    let mut engine = Engine::new(Some(id.to_string()), 0.0, ReplayGainSettings::default());
    engine.set_output_settings(OutputSettings {
        match_sample_rate: true,
        ..OutputSettings::default()
    });
    let result = (|| -> Result<bool, String> {
        engine
            .open_output_for(Some(rate))
            .map_err(|e| e.to_string())?;
        let slot = NextSlot::new();
        slot.set(NextTrack::End);
        let request = TrackRequest {
            item_id: 1,
            track_id: 1,
            path: wav.clone(),
            replaygain: ReplayGainTags::default(),
            crossfade_ms: 0,
        };
        engine
            .start_feed(request, 0, slot)
            .map_err(|e| e.to_string())?;
        engine.set_playing(true);
        let mut events = Vec::new();
        let run = |engine: &mut Engine, events: &mut Vec<EngineEvent>, ms: u64| {
            let until = Instant::now() + Duration::from_millis(ms);
            while Instant::now() < until {
                engine.poll(events);
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        run(&mut engine, &mut events, 800);
        let flowing = engine.played().1;
        println!("playing at {rate} Hz: {flowing} frames played");
        if flowing == 0 {
            return Err("no audio reached the device".into());
        }

        engine.simulate_format_notification();
        std::thread::sleep(Duration::from_millis(300));
        let stopped_at = engine.shared().callbacks();
        std::thread::sleep(Duration::from_millis(400));
        let still_stopped = engine.shared().callbacks() == stopped_at;
        let frames_before = engine.played().1;
        println!("stream stopped by the simulated backend: {still_stopped}");

        events.clear();
        run(&mut engine, &mut events, 1000);
        let callbacks_after = engine.shared().callbacks() - stopped_at;
        let frames_after = engine.played().1.saturating_sub(frames_before);
        let rebuilt = events
            .iter()
            .any(|e| matches!(e, EngineEvent::OutputChanged | EngineEvent::DeviceLost));
        println!(
            "after the engine noticed: {callbacks_after} callbacks, {frames_after} frames more, stream rebuilt: {rebuilt}"
        );
        Ok(still_stopped && callbacks_after > 10 && frames_after > u64::from(rate) / 4 && !rebuilt)
    })();
    engine.close_output();
    let _ = std::fs::remove_dir_all(&dir);
    match result {
        Ok(true) => {
            println!("PASS: the engine restarted the stream and went on playing");
            Ok(())
        }
        Ok(false) => Err("FAIL: the engine did not recover as expected".into()),
        Err(e) => Err(format!("FAIL: {e}")),
    }
}

fn parse_count(text: &str) -> Result<usize, String> {
    match text.replace('_', "").parse::<usize>() {
        Ok(n) if (1..=5_000_000).contains(&n) => Ok(n),
        _ => Err(format!(
            "invalid track count {text:?} (expected 1..=5000000)\n{USAGE}"
        )),
    }
}

// ---- synthetic data -------------------------------------------------------------------

const KO_SYLLABLES: &[&str] = &[
    "아", "이", "유", "방", "탄", "소", "년", "단", "블", "랙", "핑", "크", "태", "연", "지", "수",
    "민", "하", "늘", "별", "밤", "빛", "봄", "여", "름", "가", "을", "겨", "울", "바", "람", "꽃",
    "달", "해", "강", "산", "비", "눈", "길", "꿈", "숲", "새", "물", "불", "정", "윤", "서", "현",
];
const KO_WORDS: &[&str] = &[
    "밤편지",
    "좋은",
    "날",
    "사랑",
    "그대",
    "우리",
    "봄날",
    "너의",
    "의미",
    "하루",
    "끝",
    "기억",
    "바람",
    "별",
    "보러",
    "가자",
    "오늘",
    "취하는",
    "밤",
    "가을",
    "아침",
    "눈꽃",
    "첫눈",
    "처럼",
    "너에게",
    "가겠다",
    "안녕",
    "잘",
    "지내",
    "소나기",
    "여름",
    "밤의",
    "꿈",
    "이름",
    "에게",
    "시간",
    "바깥",
    "무릎",
    "마음",
    "푸르던",
    "비밀",
    "정원",
    "잠",
    "못",
    "드는",
    "서울",
    "거리",
    "한강",
    "노래",
    "걷고",
    "싶다",
    "그리움",
    "편지",
    "달빛",
    "파도",
    "약속",
];
const EN_WORDS: &[&str] = &[
    "love",
    "night",
    "blue",
    "summer",
    "dream",
    "light",
    "city",
    "river",
    "heart",
    "fire",
    "rain",
    "moon",
    "dance",
    "story",
    "golden",
    "wild",
    "silver",
    "ocean",
    "morning",
    "letter",
    "ghost",
    "paper",
    "velvet",
    "electric",
    "last",
    "first",
    "broken",
    "little",
    "midnight",
    "satellite",
    "garden",
    "holiday",
    "runaway",
    "yesterday",
    "forever",
    "under",
    "over",
    "again",
    "home",
    "strange",
    "beautiful",
    "winter",
    "echo",
    "shadow",
    "mirror",
    "thunder",
    "paradise",
    "road",
];
const EN_NAMES: &[&str] = &[
    "The Beatles",
    "Queen",
    "Radiohead",
    "Daft Punk",
    "Nirvana",
    "Miles Davis",
    "Bill Evans",
    "Fleetwood Mac",
    "Pink Floyd",
    "Led Zeppelin",
    "Portishead",
    "Massive Attack",
    "Björk",
    "Sigur Rós",
    "坂本龍一",
    "宇多田ヒカル",
    "Beyoncé",
    "The Smiths",
    "Joy Division",
    "Blur",
];
const GENRES: &[&str] = &[
    "K-Pop",
    "Rock",
    "Jazz",
    "Classical",
    "Electronic",
    "Ballad",
    "Indie",
    "OST",
];
const FORMATS: &[(&str, &str, &str, Option<u8>)] = &[
    ("FLAC", "FLAC", "flac", Some(16)),
    ("MP3", "MP3", "mp3", None),
    ("AAC", "MP4", "m4a", None),
    ("ALAC", "MP4", "m4a", Some(24)),
    ("Vorbis", "Ogg", "ogg", None),
    ("PCM", "WAV", "wav", Some(16)),
];

fn capitalise(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn pick<'a>(rng: &mut fastrand::Rng, pool: &[&'a str]) -> &'a str {
    pool[rng.usize(..pool.len())]
}

fn korean_name(rng: &mut fastrand::Rng) -> String {
    (0..rng.usize(2..=4))
        .map(|_| pick(rng, KO_SYLLABLES))
        .collect()
}

fn korean_phrase(rng: &mut fastrand::Rng, words: std::ops::RangeInclusive<usize>) -> String {
    let n = rng.usize(words);
    (0..n)
        .map(|_| pick(rng, KO_WORDS))
        .collect::<Vec<_>>()
        .join(" ")
}

fn english_phrase(rng: &mut fastrand::Rng, words: std::ops::RangeInclusive<usize>) -> String {
    let n = rng.usize(words);
    (0..n)
        .map(|_| capitalise(pick(rng, EN_WORDS)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn mixed_title(rng: &mut fastrand::Rng) -> String {
    let mut title = match rng.u8(..10) {
        0..=4 => korean_phrase(rng, 1..=4),
        5..=8 => english_phrase(rng, 1..=4),
        _ => format!(
            "{} ({})",
            korean_phrase(rng, 1..=3),
            english_phrase(rng, 1..=2)
        ),
    };
    match rng.u8(..40) {
        0 => title.push_str(" (Remastered)"),
        1 => title.push_str(" (Live)"),
        2 => title.push_str(&format!(" (feat. {})", korean_name(rng))),
        _ => {}
    }
    title
}

/// Builds `n` synthetic tracks: about n/60 artists and n/12 albums, mixed Korean and
/// English text, ids 1..=n.
fn synthetic_tracks(n: usize) -> Vec<Track> {
    let mut rng = fastrand::Rng::with_seed(0x0E_0E_0E);
    let artist_count = (n / 60).max(1);
    let artists: Vec<String> = (0..artist_count)
        .map(|i| match i % 5 {
            0..=2 => korean_name(&mut rng),
            3 => format!(
                "{} {}",
                capitalise(pick(&mut rng, EN_WORDS)),
                capitalise(pick(&mut rng, EN_WORDS))
            ),
            _ => {
                let base = EN_NAMES[(i / 5) % EN_NAMES.len()];
                if i / 5 < EN_NAMES.len() {
                    base.to_string()
                } else {
                    format!("{base} {}", i / 5)
                }
            }
        })
        .collect();

    let album_count = (n / 12).max(1);
    let mut tracks = Vec::with_capacity(n);
    let mut id = 0i64;
    for album_no in 0..album_count {
        let artist = &artists[rng.usize(..artists.len())];
        let album = if rng.bool() {
            korean_phrase(&mut rng, 1..=3)
        } else {
            english_phrase(&mut rng, 1..=3)
        };
        let compilation = rng.u8(..50) == 0;
        let album_artist = match rng.u8(..10) {
            _ if compilation => "Various Artists".to_string(),
            0..=5 => artist.clone(),
            _ => String::new(),
        };
        let year = Some(1965 + rng.i32(0..60));
        let genre = pick(&mut rng, GENRES);
        let (codec, container, ext, bit_depth) = FORMATS[rng.usize(..FORMATS.len())];
        let art_id = (rng.u8(..10) < 8).then(|| format!("{:016x}", rng.u64(..)));
        let folder = PathBuf::from(FAKE_ROOT)
            .join(artist)
            .join(format!("{album} [{album_no}]"));
        // Spread the remainder so that exactly n tracks are produced.
        let count = n / album_count + usize::from(album_no < n % album_count);
        for track_no in 1..=count {
            id += 1;
            let title = mixed_title(&mut rng);
            let track_artist = if compilation {
                artists[rng.usize(..artists.len())].clone()
            } else {
                artist.clone()
            };
            let path = folder.join(format!("{track_no:02} {title}.{ext}"));
            let duration_ms = rng.u64(90_000..420_000);
            tracks.push(Track {
                id,
                album_key: album_key(&album, &album_artist, compilation, &path),
                path,
                root_id: Some(1),
                available: false,
                title,
                artist: track_artist,
                album_artist: album_artist.clone(),
                album: album.clone(),
                disc_no: (rng.u8(..8) == 0).then_some(1),
                track_no: Some(track_no as u32),
                year,
                genre: genre.to_string(),
                duration_ms,
                codec: codec.to_string(),
                container: container.to_string(),
                sample_rate: if bit_depth == Some(24) {
                    96_000
                } else {
                    44_100
                },
                channels: 2,
                bit_depth,
                bitrate_kbps: Some(if bit_depth.is_some() {
                    rng.u32(700..1400)
                } else {
                    256
                }),
                file_size: duration_ms * 40,
                mtime_ns: 1_700_000_000_000_000_000 + id * 1_000_000_000,
                replaygain: ReplayGainTags {
                    track_gain_db: Some(-(rng.f32() * 12.0)),
                    track_peak: Some(0.9 + rng.f32() * 0.1),
                    album_gain_db: None,
                    album_peak: None,
                },
                measured: None,
                unmeasurable: false,
                check: None,
                art_id: art_id.clone(),
                favorite: rng.u8(..50) == 0,
                rating: 0,
                play_count: if rng.u8(..4) == 0 { rng.u32(..200) } else { 0 },
                last_played_at: (rng.u8(..4) == 0).then(|| 1_700_000_000 + rng.i64(0..30_000_000)),
                added_at: 1_690_000_000 + rng.i64(0..20_000_000),
            });
        }
    }
    tracks
}

// ---- bench-search ---------------------------------------------------------------------

struct Stats {
    p50: Duration,
    p95: Duration,
    max: Duration,
}

fn stats(mut samples: Vec<Duration>) -> Stats {
    samples.sort();
    let at = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize];
    Stats {
        p50: at(0.50),
        p95: at(0.95),
        max: at(1.0),
    }
}

fn ms(d: Duration) -> String {
    format!("{:8.3} ms", d.as_secs_f64() * 1000.0)
}

/// Fixed query list: (category, query).
const QUERIES: &[(&str, &str)] = &[
    ("english", "love"),
    ("english", "bea"),
    ("english", "the"),
    ("english", "a"),
    ("english", "MIDNIGHT"),
    ("english", "satell"),
    ("english", "ｂｌｕｅ"),
    ("english", "queen"),
    ("english", "remastered"),
    ("english", "feat"),
    ("korean", "이유"),
    ("korean", "아이유"),
    ("korean", "밤편지"),
    ("korean", "사랑"),
    ("korean", "그대"),
    ("korean", "봄날"),
    ("korean", "편지"),
    ("korean", "소년단"),
    ("korean", "가을"),
    ("korean", "별"),
    ("choseong", "ㅇㅇㅇ"),
    ("choseong", "ㅂㅍㅈ"),
    ("choseong", "ㅅㄹ"),
    ("choseong", "ㄱㄷ"),
    ("choseong", "ㅂㅌㅅㄴㄷ"),
    ("choseong", "ㅇ"),
    ("choseong", "ㅊㄴ"),
    ("choseong", "ㄲ"),
    ("choseong", "ㅎㄱ ㄴㄹ"),
    ("choseong", "ㄱㅇ ㅇㅊ"),
    ("multi-term", "love night"),
    ("multi-term", "the beatles blue"),
    ("multi-term", "사랑 그대"),
    ("multi-term", "밤 love"),
    ("multi-term", "ㅇㅇㅇ love"),
    ("multi-term", "a e i o"),
    ("multi-term", "queen 사랑 ㅂ"),
    ("multi-term", "remastered 봄날"),
    ("multi-term", "live dream city"),
    ("multi-term", "별 보러 가자"),
    ("no-match", "zzzzqqq"),
    ("no-match", "xylophone quartz"),
    ("no-match", "뷁뷁"),
    ("no-match", "ㅃㅃㅃㅃㅃㅃ"),
    ("no-match", "love zzzzqqq"),
    ("no-match", "0123456789abcdef"),
];
const RUNS_PER_QUERY: usize = 25;

fn bench_search(n: usize) {
    println!("OnEum library benchmark — {n} synthetic tracks");
    println!(
        "build profile: {}",
        if cfg!(debug_assertions) {
            "debug (numbers are not representative)"
        } else {
            "release"
        }
    );

    let started = Instant::now();
    let tracks = synthetic_tracks(n);
    println!("generate tracks        {}", ms(started.elapsed()));

    // Cold build: normalise every track's text, then index.
    let started = Instant::now();
    let snapshot = LibrarySnapshot::from_tracks(tracks);
    let cold = started.elapsed();
    // Rebuild with cached normalised text: what the library does when tracks change.
    let mut rebuilds = Vec::new();
    for _ in 0..7 {
        let started = Instant::now();
        let rebuilt = snapshot.rebuilt();
        rebuilds.push(started.elapsed());
        assert_eq!(rebuilt.len(), snapshot.len());
    }
    let rebuild = stats(rebuilds);
    println!("snapshot build (cold)  {}", ms(cold));
    println!(
        "snapshot rebuild       {}  (p50 of 7; max {})",
        ms(rebuild.p50),
        ms(rebuild.max).trim()
    );
    println!(
        "snapshot memory        {:8.1} MB (approx.)   {} albums, {} artists",
        snapshot.approx_memory_bytes() as f64 / (1024.0 * 1024.0),
        snapshot.albums().len(),
        snapshot.artists().len()
    );

    println!(
        "\nsearch — {} queries x {RUNS_PER_QUERY} runs",
        QUERIES.len()
    );
    let mut all = Vec::new();
    let mut by_category: BTreeMap<&str, Vec<Duration>> = BTreeMap::new();
    let mut slowest: (Duration, &str, usize) = (Duration::ZERO, "", 0);
    for (category, query) in QUERIES {
        let mut hits = 0;
        for _ in 0..RUNS_PER_QUERY {
            let started = Instant::now();
            let result = snapshot.search(query);
            let elapsed = started.elapsed();
            hits = result.len();
            std::hint::black_box(&result);
            all.push(elapsed);
            by_category.entry(category).or_default().push(elapsed);
            if elapsed > slowest.0 {
                slowest = (elapsed, query, hits);
            }
        }
        std::hint::black_box(hits);
    }
    println!(
        "  {:<12} {:>11} {:>11} {:>11}",
        "category", "p50", "p95", "max"
    );
    for (category, samples) in by_category {
        let s = stats(samples);
        println!(
            "  {:<12} {} {} {}",
            category,
            ms(s.p50),
            ms(s.p95),
            ms(s.max)
        );
    }
    let s = stats(all);
    println!("  {:<12} {} {} {}", "ALL", ms(s.p50), ms(s.p95), ms(s.max));
    println!(
        "  slowest single run: {:?} ({} hits) {}",
        slowest.1,
        slowest.2,
        ms(slowest.0).trim()
    );
    let started = Instant::now();
    let everything = snapshot.search("");
    println!(
        "  empty query (all {} tracks) {}",
        everything.len(),
        ms(started.elapsed()).trim()
    );

    // Typing "아이유 love" one keystroke at a time.
    let typed = [
        "ㅇ",
        "아",
        "아ㅇ",
        "아이",
        "아이ㅇ",
        "아이유",
        "아이유 l",
        "아이유 lo",
        "아이유 lov",
        "아이유 love",
    ];
    let started = Instant::now();
    for query in typed {
        std::hint::black_box(snapshot.search(query));
    }
    println!(
        "  typing 10 keystrokes: {} total",
        ms(started.elapsed()).trim()
    );

    println!("\nsort — all {n} tracks, ascending / descending");
    for key in [
        SortKey::Default,
        SortKey::Title,
        SortKey::Artist,
        SortKey::Album,
        SortKey::Duration,
        SortKey::TrackNo,
        SortKey::Year,
        SortKey::DateAdded,
        SortKey::PlayCount,
        SortKey::LastPlayed,
        SortKey::Format,
    ] {
        let mut timings = [Duration::ZERO; 2];
        for (slot, ascending) in [true, false].into_iter().enumerate() {
            // Start from a shuffled order so the sort has real work to do.
            let mut indices: Vec<u32> = (0..n as u32).collect();
            fastrand::Rng::with_seed(7).shuffle(&mut indices);
            let started = Instant::now();
            snapshot.sort(&mut indices, key, ascending);
            timings[slot] = started.elapsed();
            std::hint::black_box(&indices);
        }
        println!(
            "  {:<12} {} / {}",
            format!("{key:?}"),
            ms(timings[0]),
            ms(timings[1]).trim()
        );
    }
}

// ---- gen-db ---------------------------------------------------------------------------

fn gen_db(n: usize, data_dir: &Path) -> Result<(), String> {
    let db_path = data_dir.join(DB_FILE_NAME);
    if db_path.exists() {
        return Err(format!(
            "{} already exists; refusing to add synthetic tracks to an existing library",
            db_path.display()
        ));
    }
    let started = Instant::now();
    let opened = Db::open(&db_path).map_err(|e| e.to_string())?;
    let mut db = opened.db;
    let root_id = db
        .insert_root(Path::new(FAKE_ROOT))
        .map_err(|e| e.to_string())?;
    let rows: Vec<StoredTrack> = synthetic_tracks(n)
        .into_iter()
        .map(|mut track| {
            track.root_id = Some(root_id);
            let compilation = track.album_artist == "Various Artists";
            StoredTrack {
                track,
                compilation,
                art_from_folder: false,
            }
        })
        .collect();
    let mut ids = Vec::with_capacity(n);
    for chunk in rows.chunks(1000) {
        ids.extend(db.upsert_tracks(chunk).map_err(|e| e.to_string())?);
    }
    // A few playlists so the playlist views have content too.
    for (name, step) in [
        ("즐겨 듣는 곡", 97usize),
        ("Road trip", 211),
        ("새벽 감성", 499),
    ] {
        let playlist = db
            .create_playlist(name, 1_700_000_000)
            .map_err(|e| e.to_string())?;
        let picks: Vec<i64> = ids.iter().copied().step_by(step).take(300).collect();
        db.append_playlist_items(playlist, &picks)
            .map_err(|e| e.to_string())?;
    }
    db.checkpoint().map_err(|e| e.to_string())?;
    let size = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
    println!(
        "wrote {n} synthetic tracks under the offline folder {FAKE_ROOT}\n  database: {} ({:.1} MB) in {}",
        db_path.display(),
        size as f64 / (1024.0 * 1024.0),
        ms(started.elapsed()).trim()
    );
    Ok(())
}

// ---- open-bench -----------------------------------------------------------------------

fn open_bench(data_dir: &Path) -> Result<(), String> {
    if !data_dir.join(DB_FILE_NAME).is_file() {
        return Err(format!("no {DB_FILE_NAME} in {}", data_dir.display()));
    }
    let cache_dir =
        std::env::temp_dir().join(format!("oneum-libtool-cache-{}", std::process::id()));
    println!("Library::open + first snapshot ({})", data_dir.display());
    for run in 1..=5 {
        let paths = LibraryPaths {
            data_dir: data_dir.to_path_buf(),
            cache_dir: cache_dir.clone(),
        };
        let started = Instant::now();
        let (library, report) = Library::open(paths).map_err(|e| e.to_string())?;
        let snapshot = library.snapshot();
        let opened = started.elapsed();
        let started = Instant::now();
        let hits = snapshot.search("사랑").len();
        let searched = started.elapsed();
        println!(
            "  run {run}: open {}   {} tracks, {} albums, {} playlists; first search {} ({hits} hits){}",
            ms(opened).trim(),
            snapshot.len(),
            snapshot.albums().len(),
            library.playlists().len(),
            ms(searched).trim(),
            if report.recovered_corrupt_db.is_some() {
                "  [database was recovered!]"
            } else {
                ""
            }
        );
        library.shutdown();
    }
    let _ = std::fs::remove_dir_all(&cache_dir);
    Ok(())
}

// ---- scan -----------------------------------------------------------------------------

fn scan(folder: &Path, verbose: bool) -> Result<(), String> {
    if verbose {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(std::io::stderr)
            .init();
    }
    let work = std::env::temp_dir().join(format!("oneum-libtool-scan-{}", std::process::id()));
    let paths = LibraryPaths {
        data_dir: work.join("data"),
        cache_dir: work.join("cache"),
    };
    let result = (|| {
        let (library, _) = Library::open(paths).map_err(|e| e.to_string())?;
        let started = Instant::now();
        library.add_folder(folder).map_err(|e| e.to_string())?;
        while !library.wait_for_scan(Duration::from_secs(1)) {
            let status = library.scan_status();
            eprintln!("  … {} / {} files", status.files_done, status.files_found);
        }
        let elapsed = started.elapsed();
        let status = library.scan_status();
        let snapshot = library.snapshot();
        println!(
            "scanned in {}: {} files found, {} tracks added, {} unsupported, {} errors",
            ms(elapsed).trim(),
            status.files_found,
            status.tracks_added,
            status.unsupported,
            status.errors
        );
        let mut by_format: BTreeMap<String, usize> = BTreeMap::new();
        for track in &snapshot.tracks {
            *by_format
                .entry(format!("{}/{}", track.codec, track.container))
                .or_default() += 1;
        }
        for (format, count) in by_format {
            println!("  {format:<12} {count}");
        }
        println!(
            "  {} albums, {} artists, {} tracks with artwork",
            snapshot.albums().len(),
            snapshot.artists().len(),
            snapshot
                .tracks
                .iter()
                .filter(|t| t.art_id.is_some())
                .count()
        );
        let started = Instant::now();
        library.rescan();
        library.wait_for_scan(Duration::from_secs(3600));
        println!(
            "incremental rescan (nothing changed): {}",
            ms(started.elapsed()).trim()
        );
        library.shutdown();
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}
