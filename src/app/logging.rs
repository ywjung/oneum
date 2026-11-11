//! File logging. Logs never contain full music file paths at `info` level and above.
use std::fs::{File, OpenOptions};
use std::path::Path;
use std::sync::Mutex;

const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;

/// Sends `tracing` output to `<log_dir>/oneum.log` (and to stderr in debug builds).
/// The level comes from `ONEUM_LOG` (`error`…`trace`, default `info`).
pub fn init(log_dir: &Path) {
    let level = std::env::var("ONEUM_LOG")
        .ok()
        .and_then(|v| v.parse::<tracing::Level>().ok())
        .unwrap_or(tracing::Level::INFO);
    let path = log_dir.join("oneum.log");
    // Keep one previous log; start fresh when the file grows large.
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        let _ = std::fs::rename(&path, log_dir.join("oneum.previous.log"));
    }
    let file: Option<File> = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok();
    let builder = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_ansi(false)
        .with_target(false);
    let result = match file {
        Some(file) => builder.with_writer(Mutex::new(file)).try_init(),
        None => builder.with_writer(std::io::stderr).try_init(),
    };
    if result.is_err() {
        eprintln!("logging was already initialised");
    }
}
