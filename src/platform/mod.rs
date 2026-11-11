//! Platform integration. Everything Apple-specific (and all `unsafe`) lives under `macos`.
#[cfg(target_os = "macos")]
pub mod macos;
