//! Lyrics found on the internet (see `net::lyrics`), kept on this device: a song is
//! asked for once, and what came back, or that nothing did, is remembered in the
//! cache folder, under a name that is a hash of what the song was asked for by.
//!
//! Only called for a listener who has switched online lyrics on.
use super::lyrics::Lyrics;
use crate::net::lyrics::{self as service, Answer, Song};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

/// A song the service did not know is asked for again after this long.
const ASK_AGAIN_AFTER: Duration = Duration::from_secs(7 * 24 * 3600);
/// After the service could not be reached it is left alone for this long: without the
/// internet, every song looked at would otherwise wait for an answer that cannot come.
const LEFT_ALONE_FOR: Duration = Duration::from_secs(60);
/// And for this long after it said that it is asked too much, where it does not say
/// for how long. Where it does: for that long and up to a third more (not everyone
/// who was told the same should come back in the same second), a day at most.
const LEFT_ALONE_WHEN_ASKED_TOO_MUCH: Duration = Duration::from_secs(15 * 60);
const LEFT_ALONE_AT_MOST: Duration = Duration::from_secs(24 * 3600);

/// For how long the service is left alone after `error`.
fn pause_after(error: &crate::net::NetError) -> Duration {
    match error {
        crate::net::NetError::TooMany {
            wait_secs: Some(secs),
        } => {
            let told = Duration::from_secs(*secs).min(LEFT_ALONE_AT_MOST);
            let more = told.mul_f32(fastrand::f32() * 0.3);
            (told + more).clamp(LEFT_ALONE_FOR, LEFT_ALONE_AT_MOST)
        }
        crate::net::NetError::TooMany { wait_secs: None } => LEFT_ALONE_WHEN_ASKED_TOO_MUCH,
        _ => LEFT_ALONE_FOR,
    }
}
/// The largest file of the cache that is read.
const MAX_KEPT_BYTES: u64 = 1 << 20;

/// Where what is known of a song is kept.
fn kept_at(cache_dir: &Path, song: &Song<'_>) -> PathBuf {
    let key = format!(
        "{}\0{}\0{}\0{}",
        song.artist.trim().to_lowercase(),
        song.title.trim().to_lowercase(),
        song.album.trim().to_lowercase(),
        (song.duration_ms + 500) / 1000
    );
    let name: String = ring::digest::digest(&ring::digest::SHA256, key.as_bytes())
        .as_ref()
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    cache_dir.join("lyrics").join(format!("{name}.json"))
}

fn lyrics_of(answer: &Answer) -> Option<Lyrics> {
    answer
        .synced
        .as_deref()
        .and_then(Lyrics::from_text)
        .or_else(|| answer.plain.as_deref().and_then(Lyrics::from_text))
}

/// What is kept for a song: `None` when nothing is, or when what is kept says
/// "nothing was found" and is old enough to ask again.
fn kept(file: &Path) -> Option<Answer> {
    let meta = std::fs::metadata(file).ok()?;
    if !meta.is_file() || meta.len() > MAX_KEPT_BYTES {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(file).ok()?).ok()?;
    let text = |name: &str| value.get(name).and_then(|v| v.as_str()).map(str::to_string);
    let answer = Answer {
        synced: text("synced"),
        plain: text("plain"),
        instrumental: value
            .get("instrumental")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    };
    let empty = answer.synced.is_none() && answer.plain.is_none() && !answer.instrumental;
    let old = meta
        .modified()
        .ok()
        .and_then(|at| SystemTime::now().duration_since(at).ok())
        .is_some_and(|age| age > ASK_AGAIN_AFTER);
    (!(empty && old)).then_some(answer)
}

fn keep(file: &Path, answer: &Answer) {
    let value = serde_json::json!({
        "synced": answer.synced,
        "plain": answer.plain,
        "instrumental": answer.instrumental,
    });
    let written = file
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(file, value.to_string()));
    if let Err(error) = written {
        tracing::debug!("lyrics from the internet could not be kept: {error}");
    }
}

/// Where lyrics are asked for and where the answers are kept.
#[derive(Debug)]
pub struct Source {
    cache_dir: PathBuf,
    /// The address of the service.
    base: String,
    /// Until when the service is left alone, after it could not be reached.
    out_of_reach: Mutex<Option<Instant>>,
}

impl Source {
    /// The service at `base`, with its answers kept under `cache_dir`.
    pub fn new(cache_dir: &Path, base: &str) -> Self {
        Source {
            cache_dir: cache_dir.to_path_buf(),
            base: base.to_string(),
            out_of_reach: Mutex::new(None),
        }
    }

    /// The lyrics of `song`: what is kept of an earlier answer, or what the service
    /// answers now (which is then kept). `None`: there are none, or the service could
    /// not be reached (it is asked again later).
    pub fn find(&self, song: &Song<'_>) -> Option<Lyrics> {
        if !song.can_be_asked_for() {
            return None;
        }
        let file = kept_at(&self.cache_dir, song);
        if let Some(answer) = kept(&file) {
            return lyrics_of(&answer);
        }
        let left_alone = |until: &Option<Instant>| until.is_some_and(|t| Instant::now() < t);
        if left_alone(&self.out_of_reach.lock().unwrap_or_else(|p| p.into_inner())) {
            return None;
        }
        match service::ask(&self.base, song) {
            Ok(answer) => {
                let answer = answer.unwrap_or_default();
                keep(&file, &answer);
                lyrics_of(&answer)
            }
            Err(error) => {
                // (The error, never what was asked for: a log is not where the songs
                // a person listens to belong.)
                tracing::info!(%error, "the lyrics service did not answer");
                *self.out_of_reach.lock().unwrap_or_else(|p| p.into_inner()) =
                    Instant::now().checked_add(pause_after(&error));
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song() -> Song<'static> {
        Song {
            title: "Title",
            artist: "Artist",
            album: "Album",
            duration_ms: 180_000,
        }
    }

    #[test]
    fn what_was_found_is_kept_and_not_asked_for_again() {
        let dir = tempfile::tempdir().unwrap();
        let file = kept_at(dir.path(), &song());
        assert!(file.starts_with(dir.path().join("lyrics")));
        // The name tells nothing of the song.
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        assert!(!name.to_lowercase().contains("title") && name.len() == 37);
        assert_eq!(kept(&file), None);
        let answer = Answer {
            synced: Some("[00:01.00] one\n[00:03.00] two".into()),
            plain: Some("one\ntwo".into()),
            instrumental: false,
        };
        keep(&file, &answer);
        assert_eq!(kept(&file), Some(answer.clone()));
        // A service that cannot be reached is not asked: what is kept answers.
        let source = Source::new(dir.path(), "http://127.0.0.1:9");
        let lyrics = source.find(&song()).expect("lyrics");
        assert!(source.out_of_reach.lock().unwrap().is_none());
        assert_eq!(lyrics.timed.as_ref().map(Vec::len), Some(2));
        assert_eq!(lyrics.plain, "one\ntwo");
        // The same song written otherwise is the same entry; another song is not.
        let same = Song {
            title: " title ",
            artist: "ARTIST",
            ..song()
        };
        assert_eq!(kept_at(dir.path(), &same), file);
        let other = Song {
            duration_ms: 240_000,
            ..song()
        };
        assert_ne!(kept_at(dir.path(), &other), file);
    }

    #[test]
    fn nothing_found_is_remembered_too_and_plain_words_do_when_there_are_no_times() {
        let dir = tempfile::tempdir().unwrap();
        let file = kept_at(dir.path(), &song());
        keep(&file, &Answer::default());
        assert_eq!(kept(&file), Some(Answer::default()));
        let source = Source::new(dir.path(), "http://127.0.0.1:9");
        assert!(source.find(&song()).is_none());
        let plain = Answer {
            plain: Some("only words".into()),
            ..Answer::default()
        };
        let lyrics = lyrics_of(&plain).unwrap();
        assert!(lyrics.timed.is_none());
        assert_eq!(lyrics.plain, "only words");
        // A song without an artist is never asked for.
        let unnamed = Song {
            artist: "",
            ..song()
        };
        assert!(source.find(&unnamed).is_none());
        assert!(!kept_at(dir.path(), &unnamed).exists());
        assert!(source.out_of_reach.lock().unwrap().is_none());
        // A service that cannot be reached is left alone for a while, and nothing is
        // kept of the song: it is asked for again when the service is back.
        let other = Song {
            title: "Another",
            ..song()
        };
        assert!(source.find(&other).is_none());
        assert!(source.out_of_reach.lock().unwrap().is_some());
        assert!(!kept_at(dir.path(), &other).exists());
    }

    /// A service that says for how long it wants to be left alone is left alone for
    /// that long and a little more; one that does not say, for a quarter of an hour;
    /// one that could not be reached, for a minute.
    #[test]
    fn the_service_is_left_alone_for_as_long_as_it_says() {
        use crate::net::NetError;
        let told = |secs| NetError::TooMany {
            wait_secs: Some(secs),
        };
        for _ in 0..50 {
            let pause = pause_after(&told(3_600)).as_secs_f32();
            assert!((3_600.0..=4_680.5).contains(&pause), "{pause}");
        }
        // Never less than after a failure of the connection, never more than a day.
        assert_eq!(pause_after(&told(0)), LEFT_ALONE_FOR);
        assert_eq!(pause_after(&told(5)), LEFT_ALONE_FOR);
        assert_eq!(pause_after(&told(u64::MAX)), LEFT_ALONE_AT_MOST);
        assert_eq!(
            pause_after(&NetError::TooMany { wait_secs: None }),
            LEFT_ALONE_WHEN_ASKED_TOO_MUCH
        );
        assert_eq!(pause_after(&NetError::Timeout), LEFT_ALONE_FOR);
        assert_eq!(pause_after(&NetError::Status(500)), LEFT_ALONE_FOR);
    }
}
