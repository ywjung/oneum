//! Shared helpers for the `library_*` integration tests (included with `#[path]`).
//!
//! Audio fixtures are generated at test time: WAV files are written directly, other
//! formats are produced with `ffmpeg` when it is installed. Tests that need ffmpeg call
//! [`ffmpeg_or_skip`] and return early (with a note on stderr) when it is absent.
#![allow(dead_code)]

use oneum::library::{Library, LibraryPaths};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Generous upper bound for anything that waits on background work.
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// A temporary directory with a music folder and the library's data/cache directories.
pub struct TestEnv {
    pub dir: tempfile::TempDir,
}

impl Default for TestEnv {
    fn default() -> Self {
        Self::new()
    }
}

impl TestEnv {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(dir.path().join("music")).unwrap();
        TestEnv { dir }
    }

    pub fn music(&self) -> PathBuf {
        self.dir.path().join("music")
    }

    pub fn paths(&self) -> LibraryPaths {
        LibraryPaths {
            data_dir: self.dir.path().join("data"),
            cache_dir: self.dir.path().join("cache"),
        }
    }

    pub fn open(&self) -> Arc<Library> {
        Library::open(self.paths()).expect("open library").0
    }

    /// Opens the library, adds the music folder and waits for the first scan.
    pub fn open_scanned(&self) -> (Arc<Library>, i64) {
        let library = self.open();
        let root = library.add_folder(&self.music()).expect("add folder");
        wait_scan(&library);
        (library, root)
    }
}

pub fn wait_scan(library: &Library) {
    assert!(
        library.wait_for_scan(TIMEOUT),
        "scan did not finish in time"
    );
}

pub fn rescan(library: &Library) {
    library.rescan();
    wait_scan(library);
}

/// Polls `condition` until it holds; panics with `what` after `timeout`.
pub fn wait_until(what: &str, timeout: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Writes a minimal 16-bit mono PCM WAV file of the given length. `info` entries are
/// RIFF INFO tags by chunk id, e.g. `("INAM", "title")`, `("IART", "artist")`,
/// `("IPRD", "album")`.
pub fn write_wav(path: &Path, millis: u32, info: &[(&str, &str)]) {
    const SAMPLE_RATE: u32 = 8000;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let samples = SAMPLE_RATE * millis / 1000;
    let mut data = Vec::with_capacity(samples as usize * 2);
    for i in 0..samples {
        // A quiet triangle wave; the content only has to be valid PCM.
        let phase = (i % 40) as i32;
        let value = ((phase - 20).abs() - 10) * 800;
        data.extend_from_slice(&(value as i16).to_le_bytes());
    }

    let mut list = Vec::new();
    if !info.is_empty() {
        list.extend_from_slice(b"INFO");
        for (id, text) in info {
            assert_eq!(id.len(), 4, "INFO chunk ids have four characters");
            let mut value = text.as_bytes().to_vec();
            value.push(0);
            list.extend_from_slice(id.as_bytes());
            list.extend_from_slice(&(value.len() as u32).to_le_bytes());
            list.extend_from_slice(&value);
            if value.len() % 2 == 1 {
                list.push(0);
            }
        }
    }

    let mut body = Vec::new();
    body.extend_from_slice(b"WAVE");
    body.extend_from_slice(b"fmt ");
    body.extend_from_slice(&16u32.to_le_bytes());
    body.extend_from_slice(&1u16.to_le_bytes()); // PCM
    body.extend_from_slice(&1u16.to_le_bytes()); // mono
    body.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    body.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // byte rate
    body.extend_from_slice(&2u16.to_le_bytes()); // block align
    body.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    if !list.is_empty() {
        body.extend_from_slice(b"LIST");
        body.extend_from_slice(&(list.len() as u32).to_le_bytes());
        body.extend_from_slice(&list);
    }
    body.extend_from_slice(b"data");
    body.extend_from_slice(&(data.len() as u32).to_le_bytes());
    body.extend_from_slice(&data);

    let mut file = Vec::with_capacity(body.len() + 8);
    file.extend_from_slice(b"RIFF");
    file.extend_from_slice(&(body.len() as u32).to_le_bytes());
    file.extend_from_slice(&body);
    std::fs::write(path, file).unwrap();
}

/// A tagged WAV: title, artist and album as RIFF INFO.
pub fn write_tagged_wav(path: &Path, millis: u32, title: &str, artist: &str, album: &str) {
    write_wav(
        path,
        millis,
        &[("INAM", title), ("IART", artist), ("IPRD", album)],
    );
}

/// Deterministic pseudo-random bytes (no dependency on a RNG crate or on the clock).
pub fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

fn ffmpeg_encoders() -> Option<String> {
    let output = Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn has_encoder(name: &str) -> bool {
    ffmpeg_encoders().is_some_and(|list| {
        list.lines()
            .any(|line| line.split_whitespace().nth(1) == Some(name))
    })
}

/// True if ffmpeg is installed; otherwise prints why the calling test is skipped.
pub fn ffmpeg_or_skip(test: &str) -> bool {
    if ffmpeg_encoders().is_some() {
        return true;
    }
    eprintln!("note: `{test}` skipped: ffmpeg is not installed");
    false
}

/// Arguments selecting a Vorbis encoder: libvorbis if built in, else ffmpeg's native
/// (experimental, stereo-only) encoder. `None` if neither exists.
pub fn vorbis_args() -> Option<Vec<&'static str>> {
    if has_encoder("libvorbis") {
        Some(vec!["-c:a", "libvorbis"])
    } else if has_encoder("vorbis") {
        Some(vec![
            "-c:a",
            "vorbis",
            "-strict",
            "experimental",
            "-ac",
            "2",
        ])
    } else {
        None
    }
}

/// Encodes one second of a sine tone to `dest` with the given codec arguments, metadata
/// and (optionally) an attached cover picture.
pub fn ffmpeg_audio(
    dest: &Path,
    codec_args: &[&str],
    metadata: &[(&str, &str)],
    cover: Option<&Path>,
) {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("sine=frequency=440:duration=1:sample_rate=44100");
    if let Some(cover) = cover {
        cmd.arg("-i").arg(cover);
        cmd.args(["-map", "0:a", "-map", "1:v", "-c:v", "copy"]);
        cmd.args(["-disposition:v", "attached_pic"]);
        cmd.args(["-metadata:s:v", "comment=Cover (front)"]);
    }
    cmd.args(codec_args);
    for (key, value) in metadata {
        cmd.arg("-metadata").arg(format!("{key}={value}"));
    }
    cmd.arg(dest).stdin(Stdio::null());
    let output = cmd.output().expect("run ffmpeg");
    assert!(
        output.status.success(),
        "ffmpeg failed for {}: {}",
        dest.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Writes a PNG test image.
pub fn write_png(path: &Path, width: u32, height: u32) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let image = image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([(x * 255 / width) as u8, (y * 255 / height) as u8, 90])
    });
    image.save(path).unwrap();
}
