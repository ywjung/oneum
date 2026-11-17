//! File logging. Logs never contain full music file paths at `info` level and above.
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;

/// The log file, begun anew when it has grown large: what was written until then is
/// kept under another name (one such file, the one before it goes).
struct Turning {
    path: PathBuf,
    previous: PathBuf,
    file: Option<File>,
    written: u64,
    limit: u64,
}

impl Turning {
    fn open(path: PathBuf, previous: PathBuf) -> Option<Self> {
        Self::open_within(path, previous, MAX_LOG_BYTES)
    }

    fn open_within(path: PathBuf, previous: PathBuf, limit: u64) -> Option<Self> {
        let mut log = Turning {
            path,
            previous,
            file: None,
            written: 0,
            limit,
        };
        log.written = std::fs::metadata(&log.path).map_or(0, |meta| meta.len());
        if log.written > limit {
            log.turn();
        } else {
            log.file = log.append();
        }
        log.file.is_some().then_some(log)
    }

    fn append(&self) -> Option<File> {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .ok()
    }

    /// Puts the file aside and begins a new one. (The file is closed first: Windows
    /// does not rename one that is open.) Where that cannot be done, the same file
    /// is written on, and the next try is a whole file's length away.
    fn turn(&mut self) {
        self.file = None;
        let _ = std::fs::rename(&self.path, &self.previous);
        self.file = self.append();
        self.written = 0;
    }
}

impl Write for Turning {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.written > self.limit {
            self.turn();
        }
        let Some(file) = self.file.as_mut() else {
            // (No file to be had: what is logged is lost, the app goes on.)
            return Ok(buf.len());
        };
        let written = file.write(buf)?;
        self.written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self.file.as_mut() {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }
}

/// Sends `tracing` output to `<log_dir>/oneum.log` (and to stderr in debug builds).
/// The level comes from `ONEUM_LOG` (`error`…`trace`, default `info`).
pub fn init(log_dir: &Path) {
    let level = std::env::var("ONEUM_LOG")
        .ok()
        .and_then(|v| v.parse::<tracing::Level>().ok())
        .unwrap_or(tracing::Level::INFO);
    let path = log_dir.join("oneum.log");
    // Keep one previous log; start fresh when the file grows large (at the start,
    // and while the app runs: it may run for weeks).
    let file = Turning::open(path, log_dir.join("oneum.previous.log"));
    // Third-party crates are chatty at `info` (decoder and tag-reader details that may
    // include file names): keep them at `warn` unless tracing is requested explicitly.
    let third_party = if level >= tracing::Level::DEBUG {
        level
    } else {
        tracing::Level::WARN
    };
    let mut filter = Targets::new()
        .with_default(third_party)
        .with_target("oneum", level)
        .with_target("symphonia", third_party.min(tracing::Level::WARN))
        .with_target("lofty", third_party.min(tracing::Level::WARN));
    // On iOS winit warns in every frame that a window there has no title, cannot be
    // minimised and so on (eframe asks each time): megabytes of it in an hour.
    if crate::platform::PHONE && level < tracing::Level::DEBUG {
        filter = filter.with_target("winit", tracing::Level::ERROR);
    }
    let layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(false);
    let result = match file {
        Some(file) => tracing_subscriber::registry()
            .with(layer.with_writer(Mutex::new(file)).with_filter(filter))
            .try_init(),
        None => tracing_subscriber::registry()
            .with(layer.with_writer(std::io::stderr).with_filter(filter))
            .try_init(),
    };
    if result.is_err() {
        eprintln!("logging was already initialised");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A log that has grown large is put aside and begun anew, at the start and in
    /// the middle of a run; one earlier file is kept.
    #[test]
    fn a_log_that_has_grown_large_is_begun_anew() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (path, previous) = (
            dir.path().join("oneum.log"),
            dir.path().join("oneum.previous.log"),
        );
        let mut log = Turning::open_within(path.clone(), previous.clone(), 100).expect("opens");
        log.write_all(&[b'a'; 80]).unwrap();
        log.write_all(&[b'b'; 80]).unwrap();
        assert!(
            !previous.exists(),
            "one write past the limit is still this file's"
        );
        log.write_all(&[b'c'; 10]).unwrap();
        log.flush().unwrap();
        assert_eq!(std::fs::read(&previous).unwrap().len(), 160);
        assert_eq!(std::fs::read(&path).unwrap(), [b'c'; 10]);
        // Opened again: written on while it is small, put aside when it is large.
        drop(log);
        let mut log = Turning::open_within(path.clone(), previous.clone(), 100).expect("opens");
        log.write_all(&[b'd'; 200]).unwrap();
        drop(log);
        assert_eq!(std::fs::read(&path).unwrap().len(), 210);
        let mut log = Turning::open_within(path.clone(), previous.clone(), 100).expect("opens");
        log.write_all(b"new").unwrap();
        assert_eq!(std::fs::read(&previous).unwrap().len(), 210);
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
    }
}
