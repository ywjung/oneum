//! Value types shared between modules.
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::path::{Path, PathBuf};

/// Stable identifier of a track (SQLite rowid). Never reused for a different file while
/// the row exists; survives rescans and relinking of moved files.
pub type TrackId = i64;

/// The address a track's "path" stands for when the track is on a media server of the
/// network rather than in a file: such a track's `http://` address travels where a
/// file's path does (see `library::remote`).
pub fn http_address(path: &std::path::Path) -> Option<&str> {
    let text = path.to_str()?;
    text.get(..7)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("http://"))
        .then_some(text)
}

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
    /// Leveling values the app measured itself for a file without such tags (see
    /// `library::loudness`); kept in the app's database, never written to the file.
    pub measured: Option<ReplayGainTags>,
    /// The app tried to measure the file as it is and could not read it: not tried
    /// again until the file changes (a rescan replaces the track) or the app restarts.
    pub unmeasurable: bool,
    /// What the file check found for the file as it is (see `library::check`); kept
    /// in the app's database.
    pub check: Option<FileCheck>,
    /// Cached cover image id (hex), if the track has artwork.
    pub art_id: Option<String>,
    pub favorite: bool,
    /// The listener's rating: 1 to 5 stars, 0 for none. Kept in the app's database,
    /// never written to the file.
    pub rating: u8,
    pub play_count: u32,
    /// Unix seconds.
    pub last_played_at: Option<i64>,
    /// Unix seconds.
    pub added_at: i64,
}

/// What the file check found (see `library::check`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FileCheck {
    pub verdict: CheckVerdict,
    /// Dynamic range (the "DR" value), when the file has enough audio to tell.
    pub dr: Option<f32>,
    /// True peak (1.0 = full scale), measured on the audio oversampled.
    pub true_peak: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckVerdict {
    /// Decoded to the end without an error, and the checksum the file carries for its
    /// audio (FLAC's MD5) matches.
    Verified,
    /// Decoded to the end without an error; the file carries no checksum to compare.
    Decoded,
    /// Undecodable parts, a checksum that does not match, or audio missing at the end.
    Damaged,
}

impl CheckVerdict {
    pub fn code(self) -> i64 {
        match self {
            CheckVerdict::Verified => 0,
            CheckVerdict::Decoded => 1,
            CheckVerdict::Damaged => 2,
        }
    }

    pub fn from_code(code: i64) -> Option<Self> {
        match code {
            0 => Some(CheckVerdict::Verified),
            1 => Some(CheckVerdict::Decoded),
            2 => Some(CheckVerdict::Damaged),
            _ => None,
        }
    }
}

impl Track {
    /// The file that stands for the track on disk, without reading anything: its own
    /// file, or for a track of a CUE sheet the sheet (whose folder is the album's).
    pub fn file(&self) -> &Path {
        crate::library::cue::backing_file(&self.path)
    }

    /// The audio file the track is read from: its own file, or for a track of a CUE
    /// sheet the sheet's audio file (the sheet is read to find it).
    pub fn audio_file(&self) -> Cow<'_, Path> {
        crate::library::cue::real_file(&self.path)
    }

    /// The sample rate playback runs at: the file's own, except for DSD, which plays as
    /// the PCM it is converted to.
    pub fn playback_rate(&self) -> u32 {
        if self.codec.starts_with("DSD") {
            crate::audio::dsd::pcm_rate(self.sample_rate).unwrap_or(self.sample_rate)
        } else {
            self.sample_rate
        }
    }

    /// True for formats that keep the recording as it was made: FLAC, ALAC, PCM, DSD.
    pub fn is_lossless(&self) -> bool {
        matches!(self.codec.as_str(), "FLAC" | "ALAC" | "PCM") || self.codec.starts_with("DSD")
    }

    /// The values volume leveling works with: the file's tags, and where a gain is
    /// missing there, what the app measured. A gain always comes with the peak from
    /// the same source.
    pub fn leveling(&self) -> ReplayGainTags {
        let mut values = self.replaygain;
        if let Some(measured) = self.measured {
            if values.track_gain_db.is_none() {
                values.track_gain_db = measured.track_gain_db;
                values.track_peak = measured.track_peak;
            }
            if values.album_gain_db.is_none() {
                values.album_gain_db = measured.album_gain_db;
                values.album_peak = measured.album_peak;
            }
        }
        values
    }

    /// True when the file's tags give no track gain, so that measuring it would help.
    pub fn wants_measuring(&self) -> bool {
        self.replaygain.track_gain_db.is_none() && self.measured.is_none() && !self.unmeasurable
    }
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
        measured: None,
        unmeasurable: false,
        check: None,
        art_id: None,
        favorite: false,
        rating: 0,
        play_count: 0,
        last_played_at: None,
        added_at: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_come_before_what_was_measured() {
        let mut track = test_track(1);
        assert!(track.wants_measuring());
        assert_eq!(track.leveling(), ReplayGainTags::default());
        track.measured = Some(ReplayGainTags {
            track_gain_db: Some(-5.0),
            track_peak: Some(0.9),
            album_gain_db: Some(-4.0),
            album_peak: Some(0.95),
        });
        assert!(!track.wants_measuring());
        assert_eq!(track.leveling(), track.measured.expect("set above"));
        // A tagged track gain keeps its own peak (even an unknown one); the album
        // values, which the tags do not have, come from the measurement.
        track.replaygain.track_gain_db = Some(-7.5);
        let values = track.leveling();
        assert_eq!(values.track_gain_db, Some(-7.5));
        assert_eq!(values.track_peak, None);
        assert_eq!(values.album_gain_db, Some(-4.0));
        assert_eq!(values.album_peak, Some(0.95));
        assert!(!track.wants_measuring());
    }
}
