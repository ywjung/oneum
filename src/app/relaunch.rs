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
}

impl Note {
    /// Whether the run that wrote this ended only a moment before `now`.
    pub fn recent(&self, now: u64) -> bool {
        now.saturating_sub(self.at) < NOT_AGAIN_WITHIN_SECS
    }
}

/// What a run that cannot draw any more does next ([`note_loss`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loss {
    /// The first in a while: a new start may follow.
    StartAnew,
    /// A new start a moment ago ended the same way: no other follows.
    Again,
    /// The note could not be written: a new start would not know what it follows.
    Unwritten,
}

/// A run ends because it cannot draw any more: the note for the run that follows is
/// written, and the answer says whether a new start may follow.
pub fn note_loss(data_dir: &Path, playing: bool, now: u64) -> Loss {
    // A new start a moment ago that ends the same way is not followed by another.
    let again = read(data_dir).is_some_and(|note| note.recent(now));
    let note = Note {
        at: now,
        playing,
        taken: again,
    };
    let written = write(data_dir, note).is_ok();
    match (again, written) {
        (true, _) => Loss::Again,
        (false, true) => Loss::StartAnew,
        (false, false) => Loss::Unwritten,
    }
}

/// Present for a moment when a run ended because the system made the app's screen
/// anew while music played (see [`note_remade`]): the time it ended at.
const REMADE_FILE: &str = "remade.txt";

/// A run ends because the system makes the app's screen anew (Android, for a change
/// it does not leave to the app), which this app answers by ending its process. The
/// app is opened again right after (by its shell; the system does not do it once the
/// process has ended): music that was `playing` goes on there ([`take_remade`]).
/// Nothing went wrong, and nothing is said about it.
pub fn note_remade(data_dir: &Path, playing: bool, now: u64) {
    let file = data_dir.join(REMADE_FILE);
    if playing {
        let _ = std::fs::write(file, now.to_string());
    } else {
        let _ = std::fs::remove_file(file);
    }
}

/// Whether the run before this one ended as [`note_remade`] says, with music playing,
/// a moment before `now`. Asked once: the note is gone afterwards.
pub fn take_remade(data_dir: &Path, now: u64) -> bool {
    let file = data_dir.join(REMADE_FILE);
    let Ok(text) = std::fs::read_to_string(&file) else {
        return false;
    };
    let _ = std::fs::remove_file(&file);
    text.trim()
        .parse::<u64>()
        .is_ok_and(|at| now.saturating_sub(at) < NOT_AGAIN_WITHIN_SECS)
}

/// Kept while starts end before the app's window was there (see [`note_early_end`]):
/// when the first of them ended, and how many there were.
const EARLY_FILE: &str = "ended-early.txt";
/// How many starts in a row may be made again for having ended early.
const EARLY_ENDS_MAX: u32 = 3;

/// A start ended before the app's window was there, with nothing wrong (Android: the
/// UI library ends its loop when something about the window is told before the
/// window is made, as when the screen turns while the app starts). Starting once
/// more costs nothing, and this says whether to: not a fourth time within a minute,
/// where something else must be the matter.
pub fn note_early_end(data_dir: &Path, now: u64) -> bool {
    let file = data_dir.join(EARLY_FILE);
    let before = std::fs::read_to_string(&file).ok().and_then(|text| {
        let mut words = text.split_whitespace();
        let first: u64 = words.next()?.parse().ok()?;
        let count: u32 = words.next()?.parse().ok()?;
        Some((first, count))
    });
    let (first, count) = match before {
        Some((first, count)) if now.saturating_sub(first) < NOT_AGAIN_WITHIN_SECS => {
            (first, count.saturating_add(1))
        }
        _ => (now, 1),
    };
    if count > EARLY_ENDS_MAX {
        let _ = std::fs::remove_file(file);
        return false;
    }
    std::fs::write(file, format!("{first} {count}\n")).is_ok()
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
        "{} {} {}\n",
        note.at,
        u8::from(note.playing),
        u8::from(note.taken)
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
    // (Builds that could draw through more than one interface wrote a fourth word
    // here, the interface a device was lost on: it is passed over.)
    Some(Note { at, playing, taken })
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
            },
            Note {
                at: 7,
                playing: false,
                taken: true,
            },
            Note {
                at: 8,
                playing: false,
                taken: false,
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
    fn a_note_of_an_earlier_build_is_still_read() {
        assert_eq!(
            parse("1794739841 1 1\n"),
            Some(Note {
                at: 1_794_739_841,
                playing: true,
                taken: true,
            })
        );
        // The interface a device was lost on, which earlier builds wrote, is passed over.
        assert_eq!(
            parse("5 0 1 vulkan"),
            Some(Note {
                at: 5,
                playing: false,
                taken: true,
            })
        );
    }

    #[test]
    fn a_loss_is_followed_by_one_new_start_and_not_by_a_second_within_a_minute() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(note_loss(dir.path(), true, 1000), Loss::StartAnew);
        assert_eq!(
            read(dir.path()),
            Some(Note {
                at: 1000,
                playing: true,
                taken: false,
            })
        );
        // The run that followed lost its device too, a moment later: no third start,
        // and the note is not one a later run would take for news.
        assert_eq!(note_loss(dir.path(), false, 1030), Loss::Again);
        assert_eq!(
            read(dir.path()),
            Some(Note {
                at: 1030,
                playing: false,
                taken: true,
            })
        );
        // Long after, a loss is the first again.
        assert_eq!(
            note_loss(dir.path(), true, 1030 + NOT_AGAIN_WITHIN_SECS),
            Loss::StartAnew
        );
        // Where the note cannot be written, the run that followed would know nothing.
        let nowhere = dir.path().join("no-such-folder");
        assert_eq!(note_loss(&nowhere, true, 5000), Loss::Unwritten);
    }

    #[test]
    fn a_note_is_recent_for_a_minute() {
        let note = Note {
            at: 1000,
            playing: false,
            taken: false,
        };
        assert!(note.recent(1000));
        assert!(note.recent(1000 + NOT_AGAIN_WITHIN_SECS - 1));
        assert!(!note.recent(1000 + NOT_AGAIN_WITHIN_SECS));
        // A clock that was set back: the note is taken for recent, not for ancient.
        assert!(note.recent(990));
    }
    /// The note of a screen made anew is taken once, by a run that follows at once,
    /// and only where music was playing.
    #[test]
    fn music_goes_on_after_the_screen_was_made_anew() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(!take_remade(dir.path(), 1_000));
        note_remade(dir.path(), true, 1_000);
        assert!(take_remade(dir.path(), 1_003));
        assert!(!take_remade(dir.path(), 1_004), "taken once");
        // Nothing was playing: nothing to go on with (and an older note is gone).
        note_remade(dir.path(), true, 2_000);
        note_remade(dir.path(), false, 2_001);
        assert!(!take_remade(dir.path(), 2_002));
        // A run that follows much later is another matter.
        note_remade(dir.path(), true, 3_000);
        assert!(!take_remade(dir.path(), 3_000 + NOT_AGAIN_WITHIN_SECS));
        assert!(!take_remade(dir.path(), 3_001), "and the note is gone");
    }
    /// A start that ended before its window is made again, but not without end.
    #[test]
    fn a_start_that_ended_early_is_made_again_three_times() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(note_early_end(dir.path(), 1_000));
        assert!(note_early_end(dir.path(), 1_001));
        assert!(note_early_end(dir.path(), 1_002));
        assert!(!note_early_end(dir.path(), 1_003), "the fourth in a row");
        // The count begins anew after that, and after a while without one.
        assert!(note_early_end(dir.path(), 1_004));
        assert!(note_early_end(dir.path(), 1_005));
        assert!(note_early_end(dir.path(), 1_004 + NOT_AGAIN_WITHIN_SECS));
        assert!(note_early_end(dir.path(), 1_005 + NOT_AGAIN_WITHIN_SECS));
        assert!(note_early_end(dir.path(), 1_006 + NOT_AGAIN_WITHIN_SECS));
        assert!(!note_early_end(dir.path(), 1_007 + NOT_AGAIN_WITHIN_SECS));
        // What cannot be read counts for nothing.
        std::fs::write(dir.path().join(EARLY_FILE), "not a note").expect("write");
        assert!(note_early_end(dir.path(), 5_000));
    }
}
