//! Lyrics from the internet, for a song whose file brings none with times: asked of
//! the LRCLIB service (lrclib.net), which answers by a song's title, artist, album and
//! length and needs no account.
//!
//! This is the one thing in the app that talks to the internet, and it does so only
//! when the listener has switched it on (it is off unless they do) and only for a
//! song whose lyrics are being looked at. What leaves the device is what identifies
//! the song to the service: its title, its artist, its album and its length. Nothing
//! else does: no name of a file or a folder, nothing about the library or the device.
use super::NetError;
use super::http::{self, Access, Limits};
use std::fmt::Write as _;

/// Where the service is.
pub const SERVICE: &str = "https://lrclib.net";
/// An address at which nothing answers, for where no service is to be asked.
pub const NOWHERE: &str = "http://127.0.0.1:9";
/// The largest answer that is read (lyrics are a few kilobytes).
const MAX_ANSWER_BYTES: u64 = 1 << 20;

/// What a song is asked for by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Song<'a> {
    pub title: &'a str,
    pub artist: &'a str,
    pub album: &'a str,
    pub duration_ms: u64,
}

impl Song<'_> {
    /// Whether the song can be asked for: the service wants a title and an artist.
    pub fn can_be_asked_for(&self) -> bool {
        !self.title.trim().is_empty() && !self.artist.trim().is_empty()
    }
}

/// What the service has for a song.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Answer {
    /// The lyrics with time stamps (LRC text).
    pub synced: Option<String>,
    /// The words without times.
    pub plain: Option<String>,
    /// The service says the song has no words.
    pub instrumental: bool,
}

/// A value of the query: everything but letters, digits and `-._~` as `%XX`.
fn encoded(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.trim().bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// The address the song is asked for at, on the service at `base`.
pub fn address(base: &str, song: &Song<'_>) -> String {
    let mut address = format!(
        "{}/api/get?track_name={}&artist_name={}",
        base.trim_end_matches('/'),
        encoded(song.title),
        encoded(song.artist)
    );
    if !song.album.trim().is_empty() {
        let _ = write!(address, "&album_name={}", encoded(song.album));
    }
    if song.duration_ms > 0 {
        let _ = write!(address, "&duration={}", (song.duration_ms + 500) / 1000);
    }
    address
}

/// The lyrics in an answer of the service, as far as it holds any.
fn read_answer(bytes: &[u8]) -> Option<Answer> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let object = value.as_object()?;
    let text = |name: &str| {
        object
            .get(name)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };
    Some(Answer {
        synced: text("syncedLyrics"),
        plain: text("plainLyrics"),
        instrumental: object
            .get("instrumental")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    })
}

/// Asks the service at `base` for a song's lyrics. `Ok(None)`: it knows no such song.
pub fn ask(base: &str, song: &Song<'_>) -> Result<Option<Answer>, NetError> {
    let limits = Limits::new(MAX_ANSWER_BYTES);
    match http::get_web(&address(base, song), &limits, &Access::default()) {
        Ok(body) => read_answer(&body)
            .map(Some)
            .ok_or(NetError::BadResponse("not an answer of the lyrics service")),
        Err(NetError::Status(404)) => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_song_is_asked_for_by_what_it_is_and_nothing_else() {
        let song = Song {
            title: " 밤편지 ",
            artist: "아이유 (IU)",
            album: "Palette & more",
            duration_ms: 253_600,
        };
        assert_eq!(
            address("https://lrclib.net/", &song),
            "https://lrclib.net/api/get?track_name=%EB%B0%A4%ED%8E%B8%EC%A7%80\
             &artist_name=%EC%95%84%EC%9D%B4%EC%9C%A0%20%28IU%29\
             &album_name=Palette%20%26%20more&duration=254"
        );
        let bare = Song {
            title: "A-b_c.d~e",
            artist: "X",
            album: "",
            duration_ms: 0,
        };
        assert_eq!(
            address("http://127.0.0.1:9", &bare),
            "http://127.0.0.1:9/api/get?track_name=A-b_c.d~e&artist_name=X"
        );
        assert!(song.can_be_asked_for());
        assert!(
            !Song {
                artist: " ",
                ..song
            }
            .can_be_asked_for()
        );
        assert!(!Song { title: "", ..song }.can_be_asked_for());
    }

    #[test]
    fn an_answer_is_read_for_what_it_holds() {
        let full = br#"{"id":1,"trackName":"x","instrumental":false,
            "plainLyrics":"one\ntwo","syncedLyrics":"[00:01.00] one\n[00:02.50] two"}"#;
        let answer = read_answer(full).unwrap();
        assert_eq!(
            answer.synced.as_deref(),
            Some("[00:01.00] one\n[00:02.50] two")
        );
        assert_eq!(answer.plain.as_deref(), Some("one\ntwo"));
        assert!(!answer.instrumental);
        let none =
            read_answer(br#"{"instrumental":true,"plainLyrics":null,"syncedLyrics":""}"#).unwrap();
        assert_eq!(
            none,
            Answer {
                synced: None,
                plain: None,
                instrumental: true
            }
        );
        for bytes in [
            &b""[..],
            b"[]",
            b"not json",
            b"\"text\"",
            b"{\"syncedLyrics\":7}",
        ] {
            let read = read_answer(bytes);
            assert!(read.is_none_or(|a| a.synced.is_none() && a.plain.is_none()));
        }
    }
}
