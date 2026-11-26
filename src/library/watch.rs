//! File watching and the periodic online/offline check of music folders.
//!
//! One thread owns the FSEvents watcher. File events are collected for a short while and
//! then turned into incremental scans of just the affected directories; if the system
//! reports that events were dropped, the whole folder is rescanned. Every few seconds the
//! thread also checks whether folders appeared or disappeared (external drives).
//!
//! Where file events cannot be had, folders are rescanned now and then instead: when
//! the watcher itself could not be created (it is tried again), when the system refused
//! to watch a folder, and for folders on network shares, where the system does not see
//! changes made from other computers.
//!
//! On a phone there are no file events worth having. The only kind there (kqueue) takes
//! an open file for every file and folder it watches, and an app may hold only a few
//! hundred: a library of some hundred songs would leave none for playing them. So no
//! watcher is made there, and the folders are looked through again every half minute
//! while the app is in front (a look that finds nothing changed only lists them).
use super::{Inner, lock, tags};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// The intervals of the watcher thread (shorter ones in tests).
#[derive(Clone, Copy)]
struct Timing {
    /// Events are collected for this long before a scan is requested.
    coalesce: Duration,
    /// Interval of the folder online/offline check.
    online_check: Duration,
    /// How often a watcher that could not be created is tried again.
    watcher_retry: Duration,
    /// How often a folder without file events (the system could not watch it) is
    /// rescanned. A scan of a folder whose files did not change only lists it and
    /// compares sizes and dates.
    unwatched_rescan: Duration,
    /// How often a watched folder on a network share is rescanned: changes made there
    /// from other computers are not reported. Rarely, since every scan walks the whole
    /// share and wakes the disks of the computer it is on.
    remote_rescan: Duration,
    /// How long after a folder began to be watched it is scanned once more: the
    /// system's event stream takes a moment to start, and what changed between the
    /// scan that came with the folder and that moment is reported by nobody.
    watch_settle: Duration,
    /// Whether file events are asked for at all (see the top of this file).
    file_events: bool,
}

/// True on a phone: folders are not watched but looked through again now and then.
const POLLED_ONLY: bool = cfg!(any(target_os = "ios", target_os = "android"));

const TIMING: Timing = Timing {
    coalesce: Duration::from_secs(2),
    online_check: Duration::from_secs(5),
    watcher_retry: Duration::from_secs(30),
    unwatched_rescan: if POLLED_ONLY {
        Duration::from_secs(30)
    } else {
        Duration::from_secs(5 * 60)
    },
    remote_rescan: Duration::from_secs(30 * 60),
    watch_settle: Duration::from_secs(3),
    file_events: !POLLED_ONLY,
};

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

fn new_watcher(tx: &Sender<WatchMsg>) -> notify::Result<RecommendedWatcher> {
    let tx = tx.clone();
    notify::recommended_watcher(move |event| {
        let _ = tx.send(WatchMsg::Fs(event));
    })
}

/// Body of the watcher thread.
pub(crate) fn run_watch_thread(inner: Arc<Inner>, rx: Receiver<WatchMsg>, tx: Sender<WatchMsg>) {
    run(&inner, &rx, &tx, TIMING, new_watcher);
}

fn run(
    inner: &Inner,
    rx: &Receiver<WatchMsg>,
    tx: &Sender<WatchMsg>,
    timing: Timing,
    mut make_watcher: impl FnMut(&Sender<WatchMsg>) -> notify::Result<RecommendedWatcher>,
) {
    let mut watcher: Option<RecommendedWatcher> = None;
    let mut watcher_tried: Option<Instant> = None;
    let mut watcher_failed = false;
    let mut watched: HashMap<i64, Watched> = HashMap::new();
    // Folders whose watch could not be established (logged once, retried every check).
    let mut unwatchable: HashSet<i64> = HashSet::new();
    let mut polled = Polled::default();
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

        if reconcile_now || last_check.is_none_or(|at| at.elapsed() >= timing.online_check) {
            if timing.file_events
                && watcher.is_none()
                && watcher_tried.is_none_or(|at| at.elapsed() >= timing.watcher_retry)
            {
                watcher_tried = Some(Instant::now());
                match make_watcher(tx) {
                    Ok(new) => {
                        if watcher_failed {
                            tracing::info!("file watching is available again");
                        }
                        watcher = Some(new);
                        watched.clear();
                    }
                    Err(err) => {
                        if !watcher_failed {
                            tracing::warn!(%err, "file watching is unavailable; folders are rescanned now and then, and it is tried again");
                        }
                        watcher_failed = true;
                    }
                }
            }
            let newly = reconcile(inner, watcher.as_mut(), &mut watched, &mut unwatchable);
            polled.watch_began(&newly);
            polled.check(inner, &watched, &timing);
            last_check = Some(Instant::now());
        }

        if dirty_since.is_some_and(|since| since.elapsed() >= timing.coalesce) {
            dirty_since = None;
            for (root_id, changes) in dirty.drain() {
                let dirs = (!changes.full).then_some(changes.dirs);
                inner.scan.request_root(&inner.counters, root_id, dirs);
            }
            inner.notify();
        }
    }
}

/// How often a music folder is scanned again without file events telling of changes:
/// `None` for a watched folder on this computer.
fn rescan_interval(remote: bool, watched: bool, timing: &Timing) -> Option<Duration> {
    if !watched {
        Some(timing.unwatched_rescan)
    } else if remote {
        Some(timing.remote_rescan)
    } else {
        None
    }
}

/// Folders that file events do not cover, and when each was last rescanned for that.
#[derive(Default)]
struct Polled {
    since: HashMap<i64, Instant>,
    /// Whether a folder is on a network share, as found when it was last seen online.
    remote: HashMap<i64, bool>,
    /// Folders that began to be watched, and when: each is scanned once more a little
    /// later (see `Timing::watch_settle`).
    settling: HashMap<i64, Instant>,
}

impl Polled {
    /// Requests a rescan of every online folder without file events (not watched, or on
    /// a network share) whose last one is `interval` ago. A folder that is watched again
    /// gets one more, for what changed since its last.
    /// Notes that these folders have just begun to be watched.
    fn watch_began(&mut self, roots: &[i64]) {
        let now = Instant::now();
        for id in roots {
            self.settling.insert(*id, now);
        }
    }

    fn check(&mut self, inner: &Inner, watched: &HashMap<i64, Watched>, timing: &Timing) {
        let online: Vec<(i64, PathBuf)> = lock(&inner.state)
            .roots
            .iter()
            .filter(|root| root.online)
            .map(|root| (root.id, root.path.clone()))
            .collect();
        let is_online = |id: &i64| online.iter().any(|(online_id, _)| online_id == id);
        self.remote.retain(|id, _| is_online(id));
        let now = Instant::now();
        let mut due = Vec::new();
        for (id, path) in &online {
            let remote = *self.remote.entry(*id).or_insert_with(|| {
                crate::platform::finder::is_on_local_volume(path) == Some(false)
            });
            if let Some(interval) = rescan_interval(remote, watched.contains_key(id), timing) {
                // Not while the app is out of sight (a phone that plays with its screen
                // off): what changed meanwhile is seen when it comes back in front.
                if !crate::platform::lifecycle::in_foreground() {
                    continue;
                }
                match self.since.entry(*id) {
                    // Just seen: the scan that comes with a folder (added, back online)
                    // covers the present.
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(now);
                    }
                    std::collections::hash_map::Entry::Occupied(mut entry) => {
                        if now.duration_since(*entry.get()) >= interval {
                            entry.insert(now);
                            due.push(*id);
                        }
                    }
                }
            } else if self.since.remove(id).is_some() {
                due.push(*id);
            }
        }
        self.since.retain(|id, _| is_online(id));
        // A folder whose watch has had time to start: what changed before it did.
        self.settling.retain(|id, began| {
            if !is_online(id) || !watched.contains_key(id) {
                return false;
            }
            let settled = now.duration_since(*began) >= timing.watch_settle;
            if settled && !due.contains(id) {
                due.push(*id);
            }
            !settled
        });
        for id in due {
            inner.scan.request_root(&inner.counters, id, None);
        }
    }
}

/// Brings the watched set in line with the library's folders and requests a scan for
/// every folder whose presence on disk changed. Returns the folders that have just
/// begun to be watched.
fn reconcile(
    inner: &Inner,
    mut watcher: Option<&mut RecommendedWatcher>,
    watched: &mut HashMap<i64, Watched>,
    unwatchable: &mut HashSet<i64>,
) -> Vec<i64> {
    let mut newly_watched = Vec::new();
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
                        newly_watched.push(id);
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
    newly_watched
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
    // A CUE sheet changes what its audio file is taken in as.
    let sheet = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cue"));
    if path.is_file() && !tags::has_audio_extension(&path) && !is_image(&path) && !sheet {
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
    fn folders_without_reported_changes_are_rescanned_at_their_pace() {
        assert_eq!(rescan_interval(false, true, &TIMING), None);
        assert_eq!(
            rescan_interval(true, true, &TIMING),
            Some(Duration::from_secs(30 * 60)),
            "a watched network share: rarely"
        );
        assert_eq!(
            rescan_interval(true, false, &TIMING),
            Some(Duration::from_secs(5 * 60)),
            "not watched at all: more often"
        );
        assert_eq!(
            rescan_interval(false, false, &TIMING),
            Some(Duration::from_secs(5 * 60))
        );
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
        std::fs::write(root.join("album/image.CUE"), b"x").unwrap();
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
        // A CUE sheet decides what its audio file is taken in as.
        assert_eq!(
            dirs_for(canonical.join("album/image.CUE")),
            [root.join("album")]
        );
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

    /// Without a watcher, changes still show up (by rescans now and then), the watcher is
    /// tried again, and once it stands the folder is watched as usual.
    #[test]
    fn a_watcher_that_could_not_be_made_is_made_later_and_rescans_fill_in() {
        use crate::library::{Library, LibraryPaths};
        use std::sync::atomic::AtomicUsize;
        let temp = tempfile::tempdir().unwrap();
        let music = temp.path().join("music");
        std::fs::create_dir_all(&music).unwrap();
        let (library, _) = Library::open(LibraryPaths {
            data_dir: temp.path().join("data"),
            cache_dir: temp.path().join("cache"),
        })
        .unwrap();
        library.add_folder(&music).unwrap();
        assert!(library.wait_for_scan(Duration::from_secs(30)));

        let timing = Timing {
            coalesce: Duration::from_millis(50),
            online_check: Duration::from_millis(50),
            // (How soon the rescan below finds its file against how soon the watcher
            // is tried again is not left to the clock: on a computer busy with the
            // other tests the second attempt came first, with three seconds and with
            // eight. A watcher can be made only once the test allows it.)
            watcher_retry: Duration::from_millis(300),
            unwatched_rescan: Duration::from_millis(200),
            remote_rescan: Duration::from_millis(200),
            watch_settle: Duration::from_millis(200),
            file_events: true,
        };
        // How often a watcher was asked for, how often one was made, and whether one
        // may be made yet (until then the system refuses, as it might).
        let attempts = Arc::new(AtomicUsize::new(0));
        let made = Arc::new(AtomicUsize::new(0));
        let allowed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = {
            let (inner, tx) = (library.inner.clone(), tx.clone());
            let (attempts, made, allowed) = (attempts.clone(), made.clone(), allowed.clone());
            std::thread::spawn(move || {
                run(&inner, &rx, &tx, timing, |tx| {
                    attempts.fetch_add(1, Ordering::Relaxed);
                    if !allowed.load(Ordering::Relaxed) {
                        return Err(notify::Error::generic("refused, as a system might"));
                    }
                    made.fetch_add(1, Ordering::Relaxed);
                    new_watcher(tx)
                })
            })
        };
        let has = |name: &str| {
            library
                .snapshot()
                .tracks
                .iter()
                .any(|t| t.path.ends_with(name))
        };
        let wait_for = |what: &str, cond: &dyn Fn() -> bool| {
            let deadline = Instant::now() + Duration::from_secs(20);
            while !cond() {
                assert!(Instant::now() < deadline, "timed out waiting for {what}");
                std::thread::sleep(Duration::from_millis(20));
            }
        };

        // No watcher, however often one is asked for: a file added now is found by
        // a rescan.
        std::thread::sleep(Duration::from_millis(100));
        crate::library::tests::write_wav(&music.join("one.wav"));
        wait_for("the rescan to find the new file", &|| has("one.wav"));
        assert!(
            attempts.load(Ordering::Relaxed) >= 1,
            "a watcher was asked for"
        );
        assert_eq!(made.load(Ordering::Relaxed), 0, "and none was made");
        // The watcher is tried again, made, and then not tried any more.
        let refused = attempts.load(Ordering::Relaxed);
        allowed.store(true, Ordering::Relaxed);
        wait_for("an attempt that makes the watcher", &|| {
            made.load(Ordering::Relaxed) == 1
        });
        let tried = attempts.load(Ordering::Relaxed);
        assert!(
            tried > refused,
            "it was tried again: {tried} after {refused}"
        );
        crate::library::tests::write_wav(&music.join("two.wav"));
        wait_for("the watched folder to report the new file", &|| {
            has("two.wav")
        });
        // (Several times the wait between attempts.)
        std::thread::sleep(Duration::from_millis(1600));
        assert_eq!(
            attempts.load(Ordering::Relaxed),
            tried,
            "not tried any more"
        );
        assert_eq!(made.load(Ordering::Relaxed), 1);

        let _ = tx.send(WatchMsg::Stop);
        thread.join().unwrap();
        library.shutdown();
    }

    #[test]
    fn a_local_folder_is_known_to_be_local() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(
            crate::platform::finder::is_on_local_volume(temp.path()),
            Some(true)
        );
        assert_eq!(
            crate::platform::finder::is_on_local_volume(&temp.path().join("missing")),
            if cfg!(windows) { Some(true) } else { None }
        );
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
