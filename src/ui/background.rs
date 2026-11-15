//! Work the window hands to threads of its own, so that no frame waits for the disk:
//! library edits (favourites, playlists, removals) and writing the settings file.
//!
//! Edits run one at a time, in the order they were made, on one thread: an undo can
//! never overtake the removal it undoes. What the window has to show about them (a
//! toast, the playlist to open) comes back as an `AppEvent`; the lists themselves
//! follow from the library's own notifications, as after every other change.
//!
//! Settings are written by another thread, newest first: a version that was replaced
//! before it reached the disk is not written at all.
use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

type Job = Box<dyn FnOnce() + Send>;

/// The thread that applies library edits.
pub(super) struct Edits {
    tx: Option<Sender<Job>>,
    thread: Option<JoinHandle<()>>,
}

impl Edits {
    pub(super) fn start() -> Self {
        let (tx, rx) = channel::<Job>();
        let spawned = std::thread::Builder::new()
            .name("oneum-edits".into())
            .spawn(move || {
                for job in rx {
                    // One edit that goes wrong must not take the later ones with it.
                    if crate::app::panics::catch(job).is_err() {
                        tracing::error!("a library edit panicked");
                    }
                }
            });
        match spawned {
            Ok(thread) => Self {
                tx: Some(tx),
                thread: Some(thread),
            },
            Err(err) => {
                tracing::warn!(%err, "no thread for library edits; they run on the UI thread");
                Self {
                    tx: None,
                    thread: None,
                }
            }
        }
    }

    /// Runs `job` after the edits handed over before it. If there is no edit thread (it
    /// could not be started, or quitting has begun) it runs at once, here.
    pub(super) fn run(&self, job: impl FnOnce() + Send + 'static) {
        let job: Job = Box::new(job);
        match &self.tx {
            Some(tx) => {
                if let Err(unsent) = tx.send(job) {
                    (unsent.0)();
                }
            }
            None => job(),
        }
    }

    /// Lets the edits handed over so far finish, waiting at most `patience`.
    pub(super) fn finish(&mut self, patience: Duration) {
        self.tx = None;
        if let Some(thread) = self.thread.take() {
            join_within(thread, patience, "library edits");
        }
    }
}

#[derive(Default)]
struct Slot {
    /// The newest settings not yet written: (generation, file contents).
    next: Option<(u64, Vec<u8>)>,
    stop: bool,
}

type Written = Box<dyn Fn(u64, io::Result<()>) + Send>;

/// The thread that writes the settings file.
pub(super) struct SettingsWriter {
    shared: Arc<(Mutex<Slot>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}

fn lock(slot: &Mutex<Slot>) -> MutexGuard<'_, Slot> {
    slot.lock().unwrap_or_else(|e| e.into_inner())
}

impl SettingsWriter {
    /// `written` is called on the writer thread after every write, with the
    /// generation that was written and how it went.
    pub(super) fn start(
        path: PathBuf,
        written: impl Fn(u64, io::Result<()>) + Send + 'static,
    ) -> Self {
        let shared = Arc::new((Mutex::new(Slot::default()), Condvar::new()));
        let theirs = shared.clone();
        let written: Written = Box::new(written);
        let spawned = std::thread::Builder::new()
            .name("oneum-settings".into())
            .spawn(move || {
                let (slot, wake) = &*theirs;
                loop {
                    let (generation, bytes) = {
                        let mut slot = lock(slot);
                        loop {
                            if let Some(next) = slot.next.take() {
                                break next;
                            }
                            if slot.stop {
                                return;
                            }
                            slot = wake.wait(slot).unwrap_or_else(|e| e.into_inner());
                        }
                    };
                    // The generation is the version's turn (see `persist_settings`):
                    // a version written elsewhere meanwhile, as a phone's app leaves
                    // the screen, is not replaced by this older one.
                    let result =
                        crate::storage::settings::write_atomic_in_turn(&path, &bytes, generation);
                    written(generation, result);
                }
            });
        match spawned {
            Ok(thread) => Self {
                shared,
                thread: Some(thread),
            },
            Err(err) => {
                tracing::warn!(%err, "no thread for saving settings; they are saved on the UI thread");
                Self {
                    shared,
                    thread: None,
                }
            }
        }
    }

    /// Hands over the newest settings, replacing any that were not written yet. Returns
    /// false if there is no writer thread: then the caller has to write them itself.
    pub(super) fn write(&self, generation: u64, bytes: Vec<u8>) -> bool {
        if self.thread.is_none() {
            return false;
        }
        let (slot, wake) = &*self.shared;
        lock(slot).next = Some((generation, bytes));
        wake.notify_one();
        true
    }

    /// Writes what was handed over and ends the thread, waiting at most `patience`.
    pub(super) fn finish(&mut self, patience: Duration) {
        let (slot, wake) = &*self.shared;
        lock(slot).stop = true;
        wake.notify_one();
        if let Some(thread) = self.thread.take() {
            join_within(thread, patience, "saving settings");
        }
    }
}

/// Joins `thread` unless it is still busy after `patience` (a write stuck on a dead
/// disk must not keep the app from quitting).
fn join_within(thread: JoinHandle<()>, patience: Duration, what: &str) {
    let deadline = Instant::now() + patience;
    while !thread.is_finished() {
        if Instant::now() >= deadline {
            tracing::warn!(what, "still busy when quitting; not waiting any longer");
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = thread.join();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn edits_run_in_order_and_survive_a_panic() {
        let mut edits = Edits::start();
        let (tx, rx) = mpsc::channel();
        for i in 0..50 {
            let tx = tx.clone();
            edits.run(move || {
                assert!(i != 7, "a broken edit");
                tx.send(i).unwrap();
            });
        }
        edits.finish(Duration::from_secs(10));
        drop(tx);
        let order: Vec<i32> = rx.iter().collect();
        let expected: Vec<i32> = (0..50).filter(|&i| i != 7).collect();
        assert_eq!(order, expected);
        // After quitting has begun, an edit still runs (here, at once).
        let (tx, rx) = mpsc::channel();
        edits.run(move || tx.send(1).unwrap());
        assert_eq!(rx.try_recv(), Ok(1));
    }

    #[test]
    fn settings_are_written_newest_first_and_completely() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let (tx, rx) = mpsc::channel();
        let mut writer = SettingsWriter::start(path.clone(), move |generation, result| {
            tx.send((generation, result.is_ok())).unwrap();
        });
        for generation in 1..=200u64 {
            assert!(writer.write(generation, format!("{{\"v\":{generation}}}").into_bytes()));
        }
        writer.finish(Duration::from_secs(10));
        let reports: Vec<(u64, bool)> = rx.try_iter().collect();
        // Superseded versions may be skipped, but the last one is always written.
        assert_eq!(reports.last(), Some(&(200, true)));
        assert!(reports.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"v\":200}");
    }

    #[test]
    fn a_failed_settings_write_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the file should go: the rename fails.
        let path = dir.path().join("settings.json");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("keep"), b"x").unwrap();
        let (tx, rx) = mpsc::channel();
        let mut writer = SettingsWriter::start(path, move |generation, result| {
            tx.send((generation, result.is_ok())).unwrap();
        });
        assert!(writer.write(1, b"{}".to_vec()));
        writer.finish(Duration::from_secs(10));
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![(1, false)]);
    }
}
