//! File logging. Logs never contain paths of files or folders at `info` level and
//! above: the log is what a person saves and passes on with a report (settings →
//! about), and a path tells their name and what they listen to. A line that has to
//! tell files apart names them with `storage::files::for_log`. A test below reads the
//! sources for lines that would write a path.
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;

/// The log file, begun anew when it has grown large: what was written until then is
/// kept under another name (one such file, the one before it goes).
struct Turning {
    path: PathBuf,
    previous: PathBuf,
    file: Option<File>,
    written: u64,
    limit: u64,
}

impl Turning {
    fn open(path: PathBuf, previous: PathBuf) -> Option<Self> {
        Self::open_within(path, previous, MAX_LOG_BYTES)
    }

    fn open_within(path: PathBuf, previous: PathBuf, limit: u64) -> Option<Self> {
        let mut log = Turning {
            path,
            previous,
            file: None,
            written: 0,
            limit,
        };
        log.written = std::fs::metadata(&log.path).map_or(0, |meta| meta.len());
        if log.written > limit {
            log.turn();
        } else {
            log.file = log.append();
        }
        log.file.is_some().then_some(log)
    }

    fn append(&self) -> Option<File> {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .ok()
    }

    /// Puts the file aside and begins a new one. (The file is closed first: Windows
    /// does not rename one that is open.) Where that cannot be done, the same file
    /// is written on, and the next try is a whole file's length away.
    fn turn(&mut self) {
        self.file = None;
        let _ = std::fs::rename(&self.path, &self.previous);
        self.file = self.append();
        self.written = 0;
    }
}

impl Write for Turning {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.written > self.limit {
            self.turn();
        }
        let Some(file) = self.file.as_mut() else {
            // (No file to be had: what is logged is lost, the app goes on.)
            return Ok(buf.len());
        };
        let written = file.write(buf)?;
        self.written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self.file.as_mut() {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }
}

/// Where a path of a person's files begins, on the systems the app runs on.
const PATH_STARTS: [&str; 11] = [
    "/Users/",
    "/home/",
    "/Volumes/",
    "/private/var/",
    "/var/mobile/",
    "/var/folders/",
    "/storage/",
    "/sdcard/",
    "/mnt/",
    "/data/user/",
    "/data/data/",
];

/// What stands for a path in a log that is handed out.
const PATH_LEFT_OUT: &str = "<path left out>";

/// The log as it is handed out (saved as a file, put into a report): where a path
/// begins in a line, the rest of that line is left out. New lines are written
/// without paths (see the module); this is for the lines earlier builds wrote, which
/// stay in the file until it is begun anew, and for what an error of the system or of
/// a library may say. A path may hold spaces and anything else, so where it ends
/// cannot be told: the line ends there.
pub fn without_paths(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let (body, end) = match line.strip_suffix('\n') {
            Some(body) => (body, "\n"),
            None => (line, ""),
        };
        let unix = PATH_STARTS
            .iter()
            .filter_map(|start| body.find(start))
            .min();
        // Windows: a drive ("C:\Users\…"), a share ("\\nas\music\…").
        let bytes = body.as_bytes();
        let drive = (0..bytes.len().saturating_sub(2)).find(|&at| {
            bytes[at].is_ascii_alphabetic()
                && bytes[at + 1] == b':'
                && bytes[at + 2] == b'\\'
                && (at == 0 || !bytes[at - 1].is_ascii_alphanumeric())
        });
        let share = body.find("\\\\");
        match [unix, drive, share].into_iter().flatten().min() {
            Some(at) => {
                out.push_str(&body[..at]);
                out.push_str(PATH_LEFT_OUT);
            }
            None => out.push_str(body),
        }
        out.push_str(end);
    }
    out
}

/// Sends `tracing` output to `<log_dir>/oneum.log` (and to stderr in debug builds).
/// The level comes from `ONEUM_LOG` (`error`…`trace`, default `info`).
pub fn init(log_dir: &Path) {
    let level = std::env::var("ONEUM_LOG")
        .ok()
        .and_then(|v| v.parse::<tracing::Level>().ok())
        .unwrap_or(tracing::Level::INFO);
    let path = log_dir.join("oneum.log");
    // Keep one previous log; start fresh when the file grows large (at the start,
    // and while the app runs: it may run for weeks).
    let file = Turning::open(path, log_dir.join("oneum.previous.log"));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_that_is_handed_out_holds_no_paths() {
        let log = "2026-11-20T01:02:03Z  WARN left out /Users/someone/Music/아이유/01 팔레트 (feat. G-DRAGON).flac: Permission denied (os error 13)\n\
                   2026-11-20T01:02:04Z  INFO library opened tracks=1200 roots=2\n\
                   2026-11-20T01:02:05Z  WARN cannot bring in /private/var/mobile/Containers/Shared/x/My Song.mp3: No space left\n\
                   2026-11-20T01:02:06Z  INFO sample music added folder=/var/mobile/Containers/Data/Application/1234/Documents/Samples\n\
                   2026-11-20T01:02:07Z  WARN left out C:\\Users\\someone\\Music\\a b.flac: denied\n\
                   2026-11-20T01:02:08Z  WARN could not read \\\\nas\\music\\x.flac\n\
                   2026-11-20T01:02:09Z  WARN the system did not let /storage/emulated/0/Music/앨범 be read";
        let out = without_paths(log);
        for told in [
            "someone",
            "Music",
            "아이유",
            "팔레트",
            "My Song",
            "Containers",
            "nas",
            "앨범",
            "a b.flac",
        ] {
            assert!(!out.contains(told), "{told} is told:\n{out}");
        }
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 7);
        assert_eq!(
            lines[0],
            "2026-11-20T01:02:03Z  WARN left out <path left out>"
        );
        // A line without a path is as it was; so is a time of day ("01:02:04").
        assert_eq!(
            lines[1],
            "2026-11-20T01:02:04Z  INFO library opened tracks=1200 roots=2"
        );
        assert_eq!(
            lines[3],
            "2026-11-20T01:02:06Z  INFO sample music added folder=<path left out>"
        );
        assert!(
            lines[4].ends_with("left out <path left out>"),
            "{}",
            lines[4]
        );
        assert!(lines[5].ends_with("could not read <path left out>"));
        assert!(!out.ends_with('\n'), "no line end is added");
        assert_eq!(without_paths("a\nb\n"), "a\nb\n");
    }

    /// What a `tracing` call is given, for every call of the levels that reach the log
    /// by default: `(file, line, arguments without their string literals)`.
    fn logged(dir: &Path, found: &mut Vec<(String, usize, String)>) {
        for entry in std::fs::read_dir(dir).expect("reads the sources").flatten() {
            let path = entry.path();
            if path.is_dir() {
                // (The command line tool writes to the terminal, not to the log.)
                if path.file_name().is_some_and(|name| name != "bin") {
                    logged(&path, found);
                }
                continue;
            }
            if path.extension().is_none_or(|kind| kind != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("reads a source file");
            for level in ["warn", "info", "error"] {
                let call = format!("tracing::{level}!(");
                let mut from = 0;
                while let Some(at) = text[from..].find(&call) {
                    let start = from + at + call.len();
                    let (mut depth, mut in_text, mut escaped) = (1usize, false, false);
                    let mut arguments = String::new();
                    let mut end = start;
                    for (offset, c) in text[start..].char_indices() {
                        end = start + offset;
                        if in_text {
                            match c {
                                _ if escaped => escaped = false,
                                '\\' => escaped = true,
                                '"' => in_text = false,
                                _ => {}
                            }
                            continue;
                        }
                        match c {
                            '"' => in_text = true,
                            '(' => depth += 1,
                            ')' => depth -= 1,
                            _ => {}
                        }
                        if depth == 0 {
                            break;
                        }
                        if c != '"' {
                            arguments.push(c);
                        }
                    }
                    let line = text[..start].matches('\n').count() + 1;
                    found.push((path.display().to_string(), line, arguments));
                    from = end;
                }
            }
        }
    }

    /// No line of the log is given a path: neither one that is written out
    /// (`path.display()`) nor one that is text already (`link.path`). What names a
    /// file there goes through `for_log`.
    #[test]
    fn no_log_line_of_the_default_level_is_given_a_path() {
        let mut calls = Vec::new();
        logged(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut calls,
        );
        assert!(calls.len() > 100, "the sources were read: {}", calls.len());
        let word = |text: &str, word: &str| {
            text.match_indices(word).any(|(at, _)| {
                let before = text[..at].chars().next_back();
                let after = text[at + word.len()..].chars().next();
                let part = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
                !part(before) && !part(after)
            })
        };
        let telling: Vec<String> = calls
            .iter()
            .filter(|(_, _, arguments)| {
                // What `for_log` is given is not written.
                let mut rest = String::new();
                let mut left = arguments.as_str();
                while let Some(at) = left.find("for_log(") {
                    rest.push_str(&left[..at]);
                    let inside = &left[at + "for_log(".len()..];
                    let mut depth = 1usize;
                    let close = inside
                        .char_indices()
                        .find(|(_, c)| {
                            match c {
                                '(' => depth += 1,
                                ')' => depth -= 1,
                                _ => {}
                            }
                            depth == 0
                        })
                        .map_or(inside.len(), |(at, _)| at + 1);
                    left = &inside[close..];
                }
                rest.push_str(left);
                rest.contains(".display()") || word(&rest, "path")
            })
            .map(|(file, line, _)| format!("{file}:{line}"))
            .collect();
        assert!(telling.is_empty(), "a path goes to the log: {telling:#?}");
    }

    /// A log that has grown large is put aside and begun anew, at the start and in
    /// the middle of a run; one earlier file is kept.
    #[test]
    fn a_log_that_has_grown_large_is_begun_anew() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (path, previous) = (
            dir.path().join("oneum.log"),
            dir.path().join("oneum.previous.log"),
        );
        let mut log = Turning::open_within(path.clone(), previous.clone(), 100).expect("opens");
        log.write_all(&[b'a'; 80]).unwrap();
        log.write_all(&[b'b'; 80]).unwrap();
        assert!(
            !previous.exists(),
            "one write past the limit is still this file's"
        );
        log.write_all(&[b'c'; 10]).unwrap();
        log.flush().unwrap();
        assert_eq!(std::fs::read(&previous).unwrap().len(), 160);
        assert_eq!(std::fs::read(&path).unwrap(), [b'c'; 10]);
        // Opened again: written on while it is small, put aside when it is large.
        drop(log);
        let mut log = Turning::open_within(path.clone(), previous.clone(), 100).expect("opens");
        log.write_all(&[b'd'; 200]).unwrap();
        drop(log);
        assert_eq!(std::fs::read(&path).unwrap().len(), 210);
        let mut log = Turning::open_within(path.clone(), previous.clone(), 100).expect("opens");
        log.write_all(b"new").unwrap();
        assert_eq!(std::fs::read(&previous).unwrap().len(), 210);
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
    }
}
