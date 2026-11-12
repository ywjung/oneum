//! File watching and the periodic online/offline check of music folders.
//!
//! One thread owns the FSEvents watcher. File events are collected for a short while and
//! then turned into incremental scans of just the affected directories; if the system
//! reports that events were dropped, the whole folder is rescanned. Every few seconds the
//! thread also checks whether folders appeared or disappeared (external drives).
use super::{Inner, lock, tags};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

/// Events are collected for this long before a scan is requested.
const COALESCE_WINDOW: Duration = Duration::from_secs(2);
/// Interval of the folder online/offline check.
const ONLINE_CHECK_INTERVAL: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(250);

pub(crate) enum WatchMsg {
    Fs(notify::Result<notify::Event>),
    /// The set of folders changed; re-check now.
    Reconcile,
    Stop,
}

struct Watched {
    path: PathBuf,
    /// Resolved form of `path`; FSEvents reports resolved paths (e.g. `/private/var/…`).
    canonical: PathBuf,
}

#[derive(Default)]
struct Dirty {
    full: bool,
    dirs: BTreeSet<PathBuf>,
}

/// Body of the watcher thread.
pub(crate) fn run_watch_thread(inner: Arc<Inner>, rx: Receiver<WatchMsg>, tx: Sender<WatchMsg>) {
    let mut watcher = match notify::recommended_watcher(move |event| {
        let _ = tx.send(WatchMsg::Fs(event));
    }) {
        Ok(watcher) => Some(watcher),
        Err(err) => {
            tracing::warn!(%err, "file watching is unavailable; folders are only checked periodically");
            None
        }
    };
    let mut watched: HashMap<i64, Watched> = HashMap::new();
    // Folders whose watch could not be established (logged once, retried every check).
    let mut unwatchable: HashSet<i64> = HashSet::new();
    let mut dirty: HashMap<i64, Dirty> = HashMap::new();
    let mut dirty_since: Option<Instant> = None;
    let mut last_check: Option<Instant> = None;

    loop {
        let mut reconcile_now = false;
        match rx.recv_timeout(POLL_INTERVAL) {
            Ok(WatchMsg::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(WatchMsg::Reconcile) => reconcile_now = true,
            Ok(WatchMsg::Fs(Ok(event))) => {
                if note_event(&event, &watched, &mut dirty) && dirty_since.is_none() {
                    dirty_since = Some(Instant::now());
                }
            }
            Ok(WatchMsg::Fs(Err(err))) => {
                tracing::debug!(%err, "file watcher error");
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        if inner.shutdown.load(Ordering::Relaxed) {
            break;
        }

        if reconcile_now || last_check.is_none_or(|at| at.elapsed() >= ONLINE_CHECK_INTERVAL) {
            reconcile(&inner, watcher.as_mut(), &mut watched, &mut unwatchable);
            last_check = Some(Instant::now());
        }

        if dirty_since.is_some_and(|since| since.elapsed() >= COALESCE_WINDOW) {
            dirty_since = None;
            for (root_id, changes) in dirty.drain() {
                let dirs = (!changes.full).then_some(changes.dirs);
                inner.scan.request_root(&inner.counters, root_id, dirs);
            }
            inner.notify();
        }
    }
}

/// Brings the watched set in line with the library's folders and requests a scan for
/// every folder whose presence on disk changed.
fn reconcile(
    inner: &Inner,
    mut watcher: Option<&mut RecommendedWatcher>,
    watched: &mut HashMap<i64, Watched>,
    unwatchable: &mut HashSet<i64>,
) {
    let roots: Vec<(i64, PathBuf, bool)> = lock(&inner.state)
        .roots
        .iter()
        .map(|root| (root.id, root.path.clone(), root.online))
        .collect();

    // Folders that were removed from the library.
    let stale: Vec<i64> = watched
        .keys()
        .copied()
        .filter(|id| !roots.iter().any(|(root_id, _, _)| root_id == id))
        .collect();
    unwatchable.retain(|id| roots.iter().any(|(root_id, _, _)| root_id == id));
    for id in stale {
        if let Some(entry) = watched.remove(&id)
            && let Some(watcher) = watcher.as_deref_mut()
        {
            let _ = watcher.unwatch(&entry.path);
        }
    }

    for (id, path, was_online) in roots {
        let online = path.is_dir();
        if online && !watched.contains_key(&id) {
            // Only a watch that was really established is recorded; a failed one is
            // tried again at the next check.
            if let Some(watcher) = watcher.as_deref_mut() {
                match watcher.watch(&path, RecursiveMode::Recursive) {
                    Ok(()) => {
                        unwatchable.remove(&id);
                        let canonical =
                            std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                        watched.insert(id, Watched { path, canonical });
                    }
                    Err(err) => {
                        if unwatchable.insert(id) {
                            // The error text contains the folder path: debug level only.
                            tracing::warn!(kind = ?err.kind, "could not watch a music folder; will keep trying");
                        }
                        tracing::debug!(%err, "watch error detail");
                    }
                }
            }
        } else if !online
            && let Some(entry) = watched.remove(&id)
            && let Some(watcher) = watcher.as_deref_mut()
        {
            let _ = watcher.unwatch(&entry.path);
        }
        if online != was_online {
            tracing::info!(online, "music folder availability changed");
            inner.scan.request_root(&inner.counters, id, None);
        }
    }
}

/// Records which directories an event touches. Returns true if anything was recorded.
fn note_event(
    event: &notify::Event,
    watched: &HashMap<i64, Watched>,
    dirty: &mut HashMap<i64, Dirty>,
) -> bool {
    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }
    let mut noted = false;
    if event.need_rescan() {
        // Events were dropped: we no longer know what changed.
        for (id, root) in watched {
            let affected = event.paths.is_empty()
                || event
                    .paths
                    .iter()
                    .any(|p| p.starts_with(&root.canonical) || p.starts_with(&root.path));
            if affected {
                dirty.entry(*id).or_default().full = true;
                noted = true;
            }
        }
        return noted;
    }
    for path in &event.paths {
        for (id, root) in watched {
            let relative = path
                .strip_prefix(&root.canonical)
                .or_else(|_| path.strip_prefix(&root.path));
            let Ok(relative) = relative else { continue };
            if let Some(dir) = affected_dir(&root.path, relative) {
                dirty.entry(*id).or_default().dirs.insert(dir);
                noted = true;
            }
        }
    }
    noted
}

/// Directory to rescan for a change at `root/relative`, or `None` if the change cannot
/// matter (hidden files, files that are neither audio nor cover images).
fn affected_dir(root: &Path, relative: &Path) -> Option<PathBuf> {
    let hidden = relative
        .components()
        .any(|c| c.as_os_str().as_encoded_bytes().first() == Some(&b'.'));
    if hidden {
        return None;
    }
    let path = root.join(relative);
    if path.is_dir() {
        return Some(path);
    }
    if path.is_file() && !tags::has_audio_extension(&path) && !is_image(&path) {
        return None;
    }
    // A file, or something that no longer exists (possibly a removed directory): its
    // parent directory is where the difference shows.
    Some(path.parent().unwrap_or(root).to_path_buf())
}

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            ["jpg", "jpeg", "png"]
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, Flag};

    fn watched(root: &Path) -> HashMap<i64, Watched> {
        let mut map = HashMap::new();
        map.insert(
            7,
            Watched {
                path: root.to_path_buf(),
                canonical: std::fs::canonicalize(root).unwrap(),
            },
        );
        map
    }

    #[test]
    fn events_map_to_directories() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let canonical = std::fs::canonicalize(root).unwrap();
        std::fs::create_dir_all(root.join("album")).unwrap();
        std::fs::write(root.join("album/a.mp3"), b"x").unwrap();
        std::fs::write(root.join("album/notes.txt"), b"x").unwrap();
        std::fs::write(root.join("album/.hidden.mp3"), b"x").unwrap();
        let watched = watched(root);

        let event =
            |path: PathBuf| notify::Event::new(EventKind::Create(CreateKind::Any)).add_path(path);
        let dirs_for = |path: PathBuf| -> Vec<PathBuf> {
            let mut dirty = HashMap::new();
            note_event(&event(path), &watched, &mut dirty);
            dirty
                .remove(&7)
                .map(|d: Dirty| d.dirs.into_iter().collect())
                .unwrap_or_default()
        };

        // Events arrive with resolved paths and are mapped back under the folder path.
        assert_eq!(
            dirs_for(canonical.join("album/a.mp3")),
            [root.join("album")]
        );
        assert_eq!(dirs_for(canonical.join("album")), [root.join("album")]);
        // A deleted directory: its parent is rescanned.
        assert_eq!(dirs_for(canonical.join("gone/sub")), [root.join("gone")]);
        // Irrelevant files do not cause scans.
        assert!(dirs_for(canonical.join("album/notes.txt")).is_empty());
        assert!(dirs_for(canonical.join("album/.hidden.mp3")).is_empty());
        assert!(dirs_for(canonical.join(".Trashes/x.mp3")).is_empty());
        assert!(dirs_for(PathBuf::from("/somewhere/else/a.mp3")).is_empty());
    }

    #[test]
    fn rescan_flag_requests_full_scan() {
        let temp = tempfile::tempdir().unwrap();
        let watched = watched(temp.path());
        let event = notify::Event::new(EventKind::Other).set_flag(Flag::Rescan);
        let mut dirty = HashMap::new();
        assert!(note_event(&event, &watched, &mut dirty));
        assert!(dirty[&7].full);
    }

    #[test]
    fn a_watch_that_was_not_established_is_not_recorded() {
        use crate::library::{Library, LibraryPaths};
        let temp = tempfile::tempdir().unwrap();
        let music = temp.path().join("music");
        std::fs::create_dir_all(&music).unwrap();
        let (library, _) = Library::open(LibraryPaths {
            data_dir: temp.path().join("data"),
            cache_dir: temp.path().join("cache"),
        })
        .unwrap();
        let root = library.add_folder(&music).unwrap();
        assert!(library.wait_for_scan(Duration::from_secs(30)));

        // Without a working watcher nothing may be remembered as "watched": that would
        // stop every later attempt to establish the watch.
        let mut watched = HashMap::new();
        let mut unwatchable = HashSet::new();
        reconcile(&library.inner, None, &mut watched, &mut unwatchable);
        assert!(watched.is_empty());

        // With one, the folder is recorded once the watch stands.
        let mut watcher = notify::recommended_watcher(|_| {}).unwrap();
        reconcile(
            &library.inner,
            Some(&mut watcher),
            &mut watched,
            &mut unwatchable,
        );
        assert!(watched.contains_key(&root));
        assert!(unwatchable.is_empty());
        // When the folder disappears the record goes with it, so that the watch is set
        // up again when it returns.
        std::fs::remove_dir_all(&music).unwrap();
        reconcile(
            &library.inner,
            Some(&mut watcher),
            &mut watched,
            &mut unwatchable,
        );
        assert!(watched.is_empty());
        std::fs::create_dir_all(&music).unwrap();
        reconcile(
            &library.inner,
            Some(&mut watcher),
            &mut watched,
            &mut unwatchable,
        );
        assert!(watched.contains_key(&root));
        library.shutdown();
    }

    #[test]
    fn access_events_are_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let watched = watched(temp.path());
        let event = notify::Event::new(EventKind::Access(notify::event::AccessKind::Any))
            .add_path(temp.path().join("a.mp3"));
        let mut dirty = HashMap::new();
        assert!(!note_event(&event, &watched, &mut dirty));
        assert!(dirty.is_empty());
    }
}
