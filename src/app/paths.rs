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
        };
        for dir in [&paths.data_dir, &paths.cache_dir, &paths.log_dir] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(paths)
    }

    pub fn settings_file(&self) -> PathBuf {
        self.data_dir.join("settings.json")
    }
}
