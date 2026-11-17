//! Audio pipeline: file → decode → stereo map → ReplayGain → resample → peak limiter → PCM ring
//! → output callback.
//!
//! See `docs/ARCHITECTURE.md` ("오디오 경로") for the design and threading rules.
mod blocks;
pub mod check;
pub mod crossfeed;
pub mod decoder;
pub mod dsd;
pub mod dsp;
pub mod engine;
pub mod eq;
pub mod feed;
#[cfg(test)]
mod hostile;
pub mod limiter;
pub mod loudness;
pub mod mp4_edit;
pub mod output;
pub mod pace;
pub mod peq;
pub mod remote;
pub mod render;
pub mod resample;
pub mod room;
pub mod source;
pub mod upsample;

pub use crossfeed::Crossfeed;
pub use dsp::{ReplayGainMode, ReplayGainSettings};
pub use engine::{Engine, EngineEvent, OutputStatus};
pub use eq::{EqPreset, EqSettings};
pub use feed::{NextSlot, NextTrack, TrackRequest};
pub use output::{OutputDeviceInfo, OutputError, OutputSettings};
pub use peq::{PeqError, PeqFilter, PeqKind, PeqProfile};
pub use render::{LevelMeter, Levels, SCOPE_LEN, Scope};

/// Why a track could not be played. The UI turns this into a localized message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaybackErrorKind {
    /// The file does not exist (deleted, moved, or its drive is disconnected).
    FileMissing,
    /// The file exists but could not be read (permissions, I/O error).
    FileUnreadable,
    /// Container or codec is not supported (includes DRM-protected files).
    Unsupported,
    /// The stream is damaged beyond recovery.
    Corrupt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackError {
    pub kind: PlaybackErrorKind,
    /// Technical detail for the log; never contains the full path.
    pub detail: String,
}

impl std::fmt::Display for PlaybackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.detail)
    }
}

impl std::error::Error for PlaybackError {}
