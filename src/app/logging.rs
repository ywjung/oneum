//! File logging. Logs never contain full music file paths at `info` level and above.
use std::fs::{File, OpenOptions};
use std::path::Path;
use std::sync::Mutex;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

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
