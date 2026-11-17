//! Where OnEum keeps its files (standard macOS locations).
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct AppPaths {
    /// `~/Library/Application Support/OnEum` — database and settings.
    pub data_dir: PathBuf,
    /// `~/Library/Caches/OnEum` — regenerable data (album art thumbnails).
    pub cache_dir: PathBuf,
    /// `~/Library/Logs/OnEum`.
    pub log_dir: PathBuf,
}

impl AppPaths {
    /// Resolves the directories and creates them. `ONEUM_DATA_DIR` redirects everything
    /// below one folder (used for testing against a throw-away profile).
    pub fn resolve() -> std::io::Result<Self> {
        let paths = match std::env::var_os("ONEUM_DATA_DIR").filter(|v| !v.is_empty()) {
            Some(root) => {
                let root = PathBuf::from(root);
                Self {
                    data_dir: root.join("data"),
                    cache_dir: root.join("cache"),
                    log_dir: root.join("logs"),
                }
            }
            #[cfg(not(windows))]
            None => {
                let home = std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::NotFound, "HOME is not set")
                })?;
                let library = home.join("Library");
                Self {
                    data_dir: library.join("Application Support").join("OnEum"),
                    cache_dir: library.join("Caches").join("OnEum"),
                    log_dir: library.join("Logs").join("OnEum"),
                }
            }
            // Windows: settings and the library roam with the user (%APPDATA%); what can
            // be rebuilt or thrown away stays on this machine (%LOCALAPPDATA%).
            #[cfg(windows)]
            None => {
                let folder = |name: &str| {
                    std::env::var_os(name)
                        .filter(|v| !v.is_empty())
                        .map(PathBuf::from)
                        .ok_or_else(|| {
                            std::io::Error::new(
                                std::io::ErrorKind::NotFound,
                                format!("{name} is not set"),
                            )
                        })
                };
                let local = folder("LOCALAPPDATA")?.join("OnEum");
                Self {
                    data_dir: folder("APPDATA")?.join("OnEum"),
                    cache_dir: local.join("Cache"),
                    log_dir: local.join("Logs"),
                }
            }
        };
        for dir in [&paths.data_dir, &paths.cache_dir, &paths.log_dir] {
            std::fs::create_dir_all(dir)?;
            keep_private(dir);
        }
        Ok(paths)
    }

    /// The folder the music is in on an iPhone: the Documents folder of the app's
    /// container, which the Files app shows as "On My iPhone › OnEum" and Finder shows
    /// when the phone is connected. `None` if the container cannot be told.
    ///
    /// On Android it is the phone's shared Music folder, which the activity tells
    /// through the environment (see `android/`).
    pub fn phone_music_dir() -> Option<PathBuf> {
        if cfg!(target_os = "android") {
            return std::env::var_os("ONEUM_MUSIC_DIR")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from);
        }
        let home = std::env::var_os("HOME").filter(|v| !v.is_empty())?;
        Some(PathBuf::from(home).join("Documents"))
    }

    /// Where a phone's app keeps its own copies of music it could not read in place
    /// (Android: a file chosen from a cloud drive or handed over by another app; the
    /// activity tells the folder through the environment). `None` where music is
    /// always used where it is.
    pub fn imported_copies_dir() -> Option<PathBuf> {
        if !cfg!(target_os = "android") {
            return None;
        }
        static DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
        DIR.get_or_init(|| {
            let dir =
                PathBuf::from(std::env::var_os("ONEUM_IMPORT_DIR").filter(|v| !v.is_empty())?);
            // Named as the library names the files in it: by the real name of the
            // app's folder, which the activity reaches through a link. (The folder
            // itself may not be there yet: the one above it is.)
            let real = match (dir.parent(), dir.file_name()) {
                (Some(above), Some(name)) => std::fs::canonicalize(above)
                    .map(|above| above.join(name))
                    .unwrap_or_else(|_| dir.clone()),
                _ => dir.clone(),
            };
            Some(real)
        })
        .clone()
    }

    /// The impulse response of the room correction, as the app keeps it.
    pub fn room_file(&self) -> PathBuf {
        self.data_dir.join("room.ir")
    }

    pub fn settings_file(&self) -> PathBuf {
        self.data_dir.join("settings.json")
    }
}

/// Closes a folder of the app's to other users of the machine: what is in it says what
/// the user owns and listens to (file paths, play counts, logs). The folders above it
/// are private on macOS already; this holds where they are not (a redirected profile,
/// another system). Best effort: a folder that cannot be changed is used as it is.
fn keep_private(dir: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let private = std::fs::Permissions::from_mode(0o700);
        if let Err(error) = std::fs::set_permissions(dir, private) {
            tracing::debug!(%error, "could not make {} private", dir.display());
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn the_folders_of_the_app_are_closed_to_other_users() {
        let dir = tempfile::tempdir().unwrap();
        let open = dir.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
        keep_private(&open);
        let mode = std::fs::metadata(&open).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        // A folder that is not there is nothing to stop for.
        keep_private(&dir.path().join("missing"));
    }
}
