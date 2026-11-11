//! Generic helpers for small JSON state files (e.g. `settings.json`).
//!
//! Loading never fails: a missing or unreadable file yields `T::default()`, and a file that
//! does not parse is renamed aside (`<name>.corrupt`) so the user's data is not silently
//! overwritten by the next save. Saving is atomic: the content is written to a temporary
//! file in the same directory, synced, and renamed over the destination.
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Loads `path` as JSON. Missing or corrupt files yield `T::default()`.
pub fn load_json<T: DeserializeOwned + Default>(path: &Path) -> T {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => {
            if err.kind() != io::ErrorKind::NotFound {
                tracing::warn!(error = %err, "could not read settings file; using defaults");
            }
            return T::default();
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(err) => {
            let aside = with_suffix(path, ".corrupt");
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

/// Serialises `value` as pretty JSON and atomically replaces `path` with it.
pub fn save_json_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    write_atomic(path, &bytes)
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
}
