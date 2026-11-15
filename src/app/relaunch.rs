//! Starting anew when the app cannot go on in its process (Android: the graphics
//! device was lost, see `graphics_lost` in `ui`). What the run that ends itself leaves
//! for the one that follows: when it ended, whether the music was playing, and whether
//! the run that follows has taken note of it yet.
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// A start anew that ends the same way within this time is not followed by another:
/// what failed twice in a row fails a third time.
pub const NOT_AGAIN_WITHIN_SECS: u64 = 60;
const FILE: &str = "relaunch.txt";

/// What a run that ended itself to start anew wrote down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Note {
    /// When it ended, in seconds since 1970.
    pub at: u64,
    /// The music was playing then.
    pub playing: bool,
    /// The run that followed has read this.
    pub taken: bool,
    /// The interface to the graphics hardware the device was lost on ("vulkan", "gl"),
    /// where there is another one to go to. The runs that follow try that one first
    /// while the settings leave the choice to the app.
    pub lost_on: Option<&'static str>,
}

impl Note {
    /// Whether the run that wrote this ended only a moment before `now`.
    pub fn recent(&self, now: u64) -> bool {
        now.saturating_sub(self.at) < NOT_AGAIN_WITHIN_SECS
    }
}

/// Present while a run is trying to draw through an interface that is not the
/// system's first choice (see [`begin_trial`]).
const TRIAL_FILE: &str = "graphics-trying.txt";
/// Set when this run found the trial of the run before unfinished ([`begin_trial`]).
static GAVE_UP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Before a run draws through an interface that is not the system's first choice
/// (because the settings say so, or because a run lost its device on the first): the
/// trial is written down, and [`end_trial`] takes it back once the run has drawn for
/// a while. A run that finds the trial of the one before still there knows that that
/// one did not get far (it crashed, or showed nothing and was ended), and must not
/// try the same again: nobody could reach the settings to change it. Returns whether
/// the trial may be made.
pub fn begin_trial(data_dir: &Path, interface: &str) -> bool {
    let file = data_dir.join(TRIAL_FILE);
    if file.exists() {
        let _ = std::fs::remove_file(&file);
        GAVE_UP.store(true, std::sync::atomic::Ordering::Release);
        return false;
    }
    // A trial that cannot be written down is not made: it could not be given up.
    std::fs::write(&file, interface).is_ok()
}

/// Whether this run is trying an interface ([`begin_trial`] wrote it down).
pub fn on_trial(data_dir: &Path) -> bool {
    data_dir.join(TRIAL_FILE).exists()
}

/// The run has drawn for a while: its interface works well enough to reach the settings.
pub fn end_trial(data_dir: &Path) {
    let _ = std::fs::remove_file(data_dir.join(TRIAL_FILE));
}

/// Whether this run gave up the interface it was to try ([`begin_trial`]).
pub fn gave_up() -> bool {
    GAVE_UP.load(std::sync::atomic::Ordering::Acquire)
}

/// Forgets which interface a device was lost on: "automatic" is the system's order
/// again.
pub fn forget_lost_interface(data_dir: &Path) {
    if let Some(note) = read(data_dir)
        && note.lost_on.is_some()
    {
        let _ = write(
            data_dir,
            Note {
                lost_on: None,
                ..note
            },
        );
    }
}

/// Seconds since 1970.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

/// The note in `data_dir`, if a run left one that can be read.
pub fn read(data_dir: &Path) -> Option<Note> {
    parse(&std::fs::read_to_string(data_dir.join(FILE)).ok()?)
}

pub fn write(data_dir: &Path, note: Note) -> io::Result<()> {
    let text = format!(
        "{} {} {} {}\n",
        note.at,
        u8::from(note.playing),
        u8::from(note.taken),
        note.lost_on.unwrap_or("-")
    );
    std::fs::write(data_dir.join(FILE), text)
}

fn parse(text: &str) -> Option<Note> {
    let mut words = text.split_whitespace();
    let at = words.next()?.parse().ok()?;
    let mut flag = || match words.next()? {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    };
    let playing = flag()?;
    let taken = flag()?;
    // (Notes of the first build that wrote them end here.)
    let lost_on = match words.next() {
        Some("vulkan") => Some("vulkan"),
        Some("gl") => Some("gl"),
        _ => None,
    };
    Some(Note {
        at,
        playing,
        taken,
        lost_on,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_comes_back_as_it_was_written() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path()), None, "no note where none was left");
        for note in [
            Note {
                at: 1_800_000_000,
                playing: true,
                taken: false,
                lost_on: Some("vulkan"),
            },
            Note {
                at: 7,
                playing: false,
                taken: true,
                lost_on: None,
            },
            Note {
                at: 8,
                playing: false,
                taken: false,
                lost_on: Some("gl"),
            },
        ] {
            write(dir.path(), note).unwrap();
            assert_eq!(read(dir.path()), Some(note));
        }
    }

    #[test]
    fn what_is_not_a_note_is_not_read_as_one() {
        for text in [
            "",
            "soon 1 0",
            "12",
            "12 1",
            "12 2 0",
            "12 yes no",
            "-3 1 0",
        ] {
            assert_eq!(parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn a_trial_that_was_not_ended_is_not_made_again() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!on_trial(dir.path()));
        assert!(begin_trial(dir.path(), "gl"), "the first trial is made");
        assert!(on_trial(dir.path()));
        end_trial(dir.path());
        assert!(!on_trial(dir.path()));
        assert!(
            begin_trial(dir.path(), "gl"),
            "one that ended well may be repeated"
        );
        // That run ended without saying that it drew: the next one does not try again…
        assert!(!begin_trial(dir.path(), "gl"));
        assert!(gave_up());
        // …and the one after it may, with a clean slate.
        assert!(begin_trial(dir.path(), "gl"));
    }

    #[test]
    fn the_interface_a_device_was_lost_on_can_be_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        forget_lost_interface(dir.path()); // no note: nothing to forget
        assert_eq!(read(dir.path()), None);
        let note = Note {
            at: 9,
            playing: true,
            taken: true,
            lost_on: Some("vulkan"),
        };
        write(dir.path(), note).unwrap();
        forget_lost_interface(dir.path());
        assert_eq!(
            read(dir.path()),
            Some(Note {
                lost_on: None,
                ..note
            })
        );
    }

    #[test]
    fn a_note_of_the_first_build_is_still_read() {
        assert_eq!(
            parse("1794739841 1 1\n"),
            Some(Note {
                at: 1_794_739_841,
                playing: true,
                taken: true,
                lost_on: None,
            })
        );
        // A word that names no interface is no interface.
        assert_eq!(parse("5 0 0 metal").unwrap().lost_on, None);
    }

    #[test]
    fn a_note_is_recent_for_a_minute() {
        let note = Note {
            at: 1000,
            playing: false,
            taken: false,
            lost_on: None,
        };
        assert!(note.recent(1000));
        assert!(note.recent(1000 + NOT_AGAIN_WITHIN_SECS - 1));
        assert!(!note.recent(1000 + NOT_AGAIN_WITHIN_SECS));
        // A clock that was set back: the note is taken for recent, not for ancient.
        assert!(note.recent(990));
    }
}
