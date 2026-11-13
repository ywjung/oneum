//! Value types shared between modules.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Stable identifier of a track (SQLite rowid). Never reused for a different file while
/// the row exists; survives rescans and relinking of moved files.
pub type TrackId = i64;

/// ReplayGain values read from existing tags. Gains are in dB, peaks are linear (1.0 = full scale).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplayGainTags {
    pub track_gain_db: Option<f32>,
    pub track_peak: Option<f32>,
    pub album_gain_db: Option<f32>,
    pub album_peak: Option<f32>,
}

/// One audio file known to the library.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub id: TrackId,
    /// Original path exactly as found on disk (never normalised).
    pub path: PathBuf,
    /// Music folder this track was found under; `None` for individually opened files.
    pub root_id: Option<i64>,
    /// False when the file is missing or its drive is disconnected.
    pub available: bool,
    pub title: String,
    pub artist: String,
    pub album_artist: String,
    pub album: String,
    /// Identity of the album this track belongs to (see ARCHITECTURE.md, "앨범 식별").
    pub album_key: u64,
    pub disc_no: Option<u32>,
    pub track_no: Option<u32>,
    pub year: Option<i32>,
    pub genre: String,
    pub duration_ms: u64,
    /// Codec name for display and the support table: "MP3", "FLAC", "PCM", "AAC", "ALAC", "Vorbis".
    pub codec: String,
    /// Container name: "MP3", "FLAC", "WAV", "AIFF", "MP4", "Ogg".
    pub container: String,
    pub sample_rate: u32,
    pub channels: u8,
    pub bit_depth: Option<u8>,
    pub bitrate_kbps: Option<u32>,
    pub file_size: u64,
    pub mtime_ns: i64,
    pub replaygain: ReplayGainTags,
    /// Cached cover image id (hex), if the track has artwork.
    pub art_id: Option<String>,
    pub favorite: bool,
    pub play_count: u32,
    /// Unix seconds.
    pub last_played_at: Option<i64>,
    /// Unix seconds.
    pub added_at: i64,
}

/// A track with plausible values everywhere, for tests that only care about a few.
#[cfg(test)]
pub(crate) fn test_track(id: TrackId) -> Track {
    Track {
        id,
        path: PathBuf::from(format!("/music/{id:03}.flac")),
        root_id: Some(1),
        available: true,
        title: format!("Song {id}"),
        artist: "Artist".into(),
        album_artist: String::new(),
        album: "Album".into(),
        album_key: 1,
        disc_no: None,
        track_no: Some(id as u32),
        year: None,
        genre: String::new(),
        duration_ms: 180_000,
        codec: "FLAC".into(),
        container: "FLAC".into(),
        sample_rate: 44_100,
        channels: 2,
        bit_depth: Some(16),
        bitrate_kbps: Some(900),
        file_size: 1,
        mtime_ns: 1,
        replaygain: ReplayGainTags::default(),
        art_id: None,
        favorite: false,
        play_count: 0,
        last_played_at: None,
        added_at: 0,
    }
}
