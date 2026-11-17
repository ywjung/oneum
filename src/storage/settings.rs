//! Generic helpers for small JSON state files (e.g. `settings.json`).
//!
//! Loading never fails: a missing file yields `T::default()`, and a file that does not
//! parse (`<name>.corrupt`) or cannot be read even after a few tries (`<name>.unreadable`)
//! is renamed aside first, so the user's data is not silently overwritten by the next
//! save. Saving is atomic: the content is written to a temporary
//! file in the same directory, synced, and renamed over the destination.
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Largest state file that is read. The app writes a few kilobytes (at its limits, a
/// few hundred); something far larger under that name is not its file, and is treated
/// like one that does not parse rather than loaded into memory whole.
const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// How often a file that is there but cannot be read is tried, and how long apart: a
/// lock held for a moment by another program (an antivirus or backup scan on Windows)
/// passes.
const READ_ATTEMPTS: u32 = 3;
const READ_RETRY: std::time::Duration = std::time::Duration::from_millis(200);

/// Loads `path` as JSON. Missing or corrupt files yield `T::default()`; so does a file
/// that cannot be read, which is first moved aside (`<name>.unreadable`) so that saving
/// the defaults later does not put them over what it holds.
pub fn load_json<T: DeserializeOwned + Default>(path: &Path) -> T {
    let read = || super::files::read_bounded(path, MAX_BYTES);
    let mut attempt = 1;
    let bytes = loop {
        match read() {
            Ok(bytes) => break bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return T::default(),
            Err(_) if attempt < READ_ATTEMPTS => {
                attempt += 1;
                std::thread::sleep(READ_RETRY);
            }
            Err(err) => {
                let aside = free_aside_name(path, "unreadable");
                match fs::rename(path, &aside) {
                    Ok(()) => tracing::warn!(
                        error = %err,
                        aside = %aside.display(),
                        "could not read the settings file; moved it aside and using defaults"
                    ),
                    Err(rename_err) => tracing::warn!(
                        error = %err,
                        rename_error = %rename_err,
                        "could not read the settings file nor move it aside; using defaults"
                    ),
                }
                return T::default();
            }
        }
    };
    let parsed: Result<T, String> = match &bytes {
        Some(bytes) => serde_json::from_slice(bytes).map_err(|err| err.to_string()),
        None => Err(format!(
            "larger than {MAX_BYTES} bytes, or not a regular file"
        )),
    };
    match parsed {
        Ok(value) => value,
        Err(err) => {
            // A file that is what the app wrote but for a member or two (a word that
            // a later version knows, a value of another kind) is read without those:
            // one of them does not cost all the others. What the file held is kept
            // beside it, since the next save writes it without them.
            if let Some((value, left_out)) = bytes.as_deref().and_then(member_by_member::<T>) {
                let aside = free_aside_name(path, "partial");
                let copied = fs::copy(path, &aside).is_ok();
                tracing::warn!(
                    error = %err,
                    left_out = ?left_out,
                    copied,
                    "parts of the settings file could not be read; they have their defaults"
                );
                return value;
            }
            let aside = free_aside_name(path, "corrupt");
            match fs::rename(path, &aside) {
                Ok(()) => tracing::warn!(
                    error = %err,
                    "settings file is corrupt; moved it aside and using defaults"
                ),
                Err(rename_err) => tracing::warn!(
                    error = %err,
                    rename_error = %rename_err,
                    "settings file is corrupt and could not be moved aside; using defaults"
                ),
            }
            T::default()
        }
    }
}

/// Reads a JSON object one member at a time: every member that can be read by itself
/// is kept, and one that cannot is left out (it then has the default of `T`). Gives
/// what was read and the names of what was left out; `None` when the bytes are no
/// object, when nothing had to be left out, or when `T` cannot be read from a part
/// of its members at all.
fn member_by_member<T: DeserializeOwned>(bytes: &[u8]) -> Option<(T, Vec<String>)> {
    let serde_json::Value::Object(read) = serde_json::from_slice(bytes).ok()? else {
        return None;
    };
    let mut kept = serde_json::Map::new();
    let mut left_out = Vec::new();
    for (name, member) in read {
        let alone: serde_json::Map<String, serde_json::Value> =
            std::iter::once((name.clone(), member.clone())).collect();
        if serde_json::from_value::<T>(serde_json::Value::Object(alone)).is_ok() {
            kept.insert(name, member);
        } else {
            left_out.push(name);
        }
    }
    if left_out.is_empty() {
        return None;
    }
    let value = serde_json::from_value(serde_json::Value::Object(kept)).ok()?;
    Some((value, left_out))
}

/// Serialises `value` as pretty JSON and atomically replaces `path` with it. It is the
/// newest version there is: a write that was handed over earlier and is still on its
/// way does not replace it afterwards (see [`write_atomic_in_turn`]).
pub fn save_json_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    write_atomic_in_turn(path, &bytes, take_turn())
}

/// The next place in the order of versions of a state file. Taken at the moment a
/// version is decided on, by whoever will write it (or have it written).
pub(crate) fn take_turn() -> u64 {
    static TURN: AtomicU64 = AtomicU64::new(0);
    TURN.fetch_add(1, Ordering::Relaxed) + 1
}

/// [`write_atomic`] for a file that more than one thread writes: versions reach the
/// file in the order of their turns. One whose turn has been overtaken (a newer
/// version is in the file already) is not written, and that is no error: the file
/// holds what is newer. Without this an older version, handed to a writer thread
/// before a newer one was written elsewhere, could land after it and stay.
pub(crate) fn write_atomic_in_turn(path: &Path, bytes: &[u8], turn: u64) -> io::Result<()> {
    static WRITTEN: std::sync::Mutex<Vec<(PathBuf, u64)>> = std::sync::Mutex::new(Vec::new());
    // Held over the write: the versions of a file follow each other.
    let mut written = WRITTEN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let at = written.iter().position(|(file, _)| file == path);
    if at.is_some_and(|at| written[at].1 > turn) {
        return Ok(());
    }
    write_atomic(path, bytes)?;
    match at {
        Some(at) => written[at].1 = turn,
        None => written.push((path.to_path_buf(), turn)),
    }
    Ok(())
}

/// Writes `bytes` to a temporary sibling of `path`, syncs it and renames it into place.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => PathBuf::from("."),
    };
    fs::create_dir_all(&dir)?;
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let mut temp_name = OsString::from(".");
    temp_name.push(file_name);
    temp_name.push(format!(
        ".tmp-{}-{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let temp_path = dir.join(temp_name);

    let result = (|| {
        let mut file = File::create(&temp_path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp_path, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
        return result;
    }
    // Persist the rename itself; failure here is not fatal for the caller.
    if let Ok(dir_handle) = File::open(&dir) {
        let _ = dir_handle.sync_all();
    }
    Ok(())
}

/// Where a damaged (or unreadable) file is put aside: `<name>.corrupt`, or if that is
/// taken (by an earlier damaged file, kept for the same reason) `<name>.corrupt-2`,
/// `-3` … The earlier one is never overwritten; after 99 the last name is reused.
fn free_aside_name(path: &Path, why: &str) -> PathBuf {
    let first = with_suffix(path, &format!(".{why}"));
    if !first.exists() {
        return first;
    }
    (2..=99)
        .map(|n| with_suffix(path, &format!(".{why}-{n}")))
        .find(|candidate| !candidate.exists())
        .unwrap_or_else(|| with_suffix(path, &format!(".{why}-99")))
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    struct Sample {
        volume: f32,
        name: String,
    }

    #[test]
    fn missing_file_yields_default() {
        let dir = tempfile::tempdir().unwrap();
        let loaded: Sample = load_json(&dir.path().join("settings.json"));
        assert_eq!(loaded, Sample::default());
    }

    #[test]
    fn a_file_far_larger_than_the_app_writes_is_set_aside_unread() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        // Valid JSON, so that only its size can be the reason.
        let huge = format!(
            r#"{{"volume":0.5,"name":"{}"}}"#,
            "a".repeat(MAX_BYTES as usize)
        );
        fs::write(&path, huge).unwrap();
        assert_eq!(load_json::<Sample>(&path), Sample::default());
        assert!(!path.exists());
        assert!(with_suffix(&path, ".corrupt").exists());
        // Just under the limit is read.
        let large = format!(
            r#"{{"volume":0.5,"name":"{}"}}"#,
            "a".repeat(MAX_BYTES as usize - 100)
        );
        fs::write(&path, large).unwrap();
        assert_eq!(load_json::<Sample>(&path).volume, 0.5);
    }

    #[test]
    fn round_trip_and_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("settings.json");
        let first = Sample {
            volume: 0.5,
            name: "아이유 🎵".into(),
        };
        save_json_atomic(&path, &first).unwrap();
        assert_eq!(load_json::<Sample>(&path), first);

        let second = Sample {
            volume: 1.0,
            name: "b".into(),
        };
        save_json_atomic(&path, &second).unwrap();
        assert_eq!(load_json::<Sample>(&path), second);

        // No temporary files are left behind.
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name() != "settings.json")
            .collect();
        assert!(leftovers.is_empty(), "leftover files: {leftovers:?}");
    }

    #[test]
    fn corrupt_file_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, b"{ not json").unwrap();
        let loaded: Sample = load_json(&path);
        assert_eq!(loaded, Sample::default());
        assert!(!path.exists());
        let aside = dir.path().join("settings.json.corrupt");
        assert_eq!(fs::read(aside).unwrap(), b"{ not json");
    }

    /// A file of which one member cannot be read keeps all its other members; the
    /// one has its default, and the file as it was is kept beside it.
    #[test]
    fn one_member_that_cannot_be_read_does_not_cost_the_others() {
        #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
        #[serde(default)]
        struct Kinds {
            volume: f32,
            name: String,
            kind: Option<Kind>,
            list: Vec<u32>,
        }
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        enum Kind {
            Old,
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let written = br#"{"volume":0.5,"name":"mine","kind":"Newer","list":[1,2],"later":true}"#;
        fs::write(&path, written).unwrap();
        let loaded: Kinds = load_json(&path);
        assert_eq!(
            loaded,
            Kinds {
                volume: 0.5,
                name: "mine".into(),
                kind: None,
                list: vec![1, 2],
            }
        );
        // The file stays in its place, and what it held is beside it.
        assert_eq!(fs::read(&path).unwrap(), written);
        let aside = dir.path().join("settings.json.partial");
        assert_eq!(fs::read(aside).unwrap(), written);

        // A kind of file that cannot be read from a part of its members (it has no
        // defaults) is moved aside whole, as before.
        fs::write(&path, br#"{"volume":"loud","name":"mine"}"#).unwrap();
        let loaded: Sample = load_json(&path);
        assert_eq!(loaded, Sample::default());
        assert!(!path.exists());
        assert!(dir.path().join("settings.json.corrupt").exists());
    }

    /// A settings file that is there but cannot be read is moved aside before the
    /// defaults are used, so saving them later does not destroy what it holds.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_file_is_moved_aside_not_overwritten() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, br#"{"volume":0.25,"name":"mine"}"#).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read(&path).is_ok() {
            eprintln!("skipped: permissions do not apply to this user");
            return;
        }
        assert_eq!(load_json::<Sample>(&path), Sample::default());
        assert!(!path.exists(), "moved aside");
        save_json_atomic(&path, &Sample::default()).unwrap();
        let aside = with_suffix(&path, ".unreadable");
        fs::set_permissions(&aside, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            fs::read(&aside).unwrap(),
            br#"{"volume":0.25,"name":"mine"}"#
        );
    }

    /// A file damaged a second time does not take the place of the first one put aside.
    #[test]
    fn an_earlier_damaged_file_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, b"{ first").unwrap();
        assert_eq!(load_json::<Sample>(&path), Sample::default());
        fs::write(&path, b"{ second").unwrap();
        assert_eq!(load_json::<Sample>(&path), Sample::default());
        fs::write(&path, b"{ third").unwrap();
        assert_eq!(load_json::<Sample>(&path), Sample::default());
        let read = |suffix: &str| fs::read(with_suffix(&path, suffix)).unwrap();
        assert_eq!(read(".corrupt"), b"{ first");
        assert_eq!(read(".corrupt-2"), b"{ second");
        assert_eq!(read(".corrupt-3"), b"{ third");
    }

    /// Versions of a file reach it in the order of their turns: one that was decided
    /// on earlier and arrives later (a writer thread that was slow) does not replace
    /// the newer one. Each file has its own order.
    #[test]
    fn an_older_version_does_not_replace_a_newer_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let other = dir.path().join("other.json");
        let earlier = take_turn();
        let later = take_turn();
        assert!(later > earlier);
        write_atomic_in_turn(&path, b"newer", later).unwrap();
        write_atomic_in_turn(&path, b"older", earlier).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"newer");
        // Another file is not held back by that.
        write_atomic_in_turn(&other, b"its own", earlier).unwrap();
        assert_eq!(fs::read(&other).unwrap(), b"its own");
        // The newest of all is written.
        save_json_atomic(
            &path,
            &Sample {
                volume: 0.5,
                name: "x".into(),
            },
        )
        .unwrap();
        assert_eq!(
            load_json::<Sample>(&path),
            Sample {
                volume: 0.5,
                name: "x".into()
            }
        );
        // A write that failed leaves the order as it was: the same turn can be tried
        // again.
        let nowhere = dir.path().join("missing").join("deeper");
        fs::write(
            dir.path().join("missing"),
            b"a file where a folder is needed",
        )
        .unwrap();
        let turn = take_turn();
        assert!(write_atomic_in_turn(&nowhere, b"x", turn).is_err());
        assert!(write_atomic_in_turn(&nowhere, b"x", turn).is_err());
    }
}
