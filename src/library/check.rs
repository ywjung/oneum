//! The file check run (see `audio::check`): every available track whose file, as it is
//! now, has no result yet is decoded once, to tell whether it is intact and to measure
//! its dynamic range and true peak. The results are kept in the app's database, tied to
//! the state of the file (a changed file is checked again). The files are only read.
use super::loudness::guarded;
use super::{Inner, lock, unix_now};
use crate::audio::check::check_file;
use crate::model::{CheckVerdict, FileCheck, TrackId};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Files checked at the same time, at most (playback must not be starved).
const MAX_WORKERS: usize = 4;
/// Results stored (and shown) together.
const COMMIT_EVERY: usize = 32;

/// Progress of the check run, for the settings window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CheckStatus {
    pub running: bool,
    /// Files the current (or last) run set out to check.
    pub total: u64,
    /// Files of that run that have been dealt with.
    pub done: u64,
    /// Files of that run found damaged.
    pub damaged: u64,
    /// Files of that run that could not be read at all (gone, not readable).
    pub failed: u64,
}

#[derive(Default)]
pub(crate) struct CheckJob {
    running: AtomicBool,
    cancel: AtomicBool,
    total: AtomicU64,
    done: AtomicU64,
    damaged: AtomicU64,
    failed: AtomicU64,
}

impl CheckJob {
    pub(crate) fn status(&self) -> CheckStatus {
        CheckStatus {
            running: self.running.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
            done: self.done.load(Ordering::Relaxed),
            damaged: self.damaged.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Claims the job. False if a run is in progress already.
    fn begin(&self) -> bool {
        if self.running.swap(true, Ordering::SeqCst) {
            return false;
        }
        self.cancel.store(false, Ordering::Relaxed);
        for counter in [&self.total, &self.done, &self.damaged, &self.failed] {
            counter.store(0, Ordering::Relaxed);
        }
        true
    }
}

/// One file to check, as it was when the run was planned.
#[derive(Debug, Clone)]
struct Job {
    id: TrackId,
    path: PathBuf,
    mtime_ns: i64,
    file_size: u64,
}

impl Inner {
    /// Stores results and shows them. A track whose file changed since the run was
    /// planned (or that is gone) is left out.
    fn commit_checks(&self, results: Vec<(Job, FileCheck)>) {
        if results.is_empty() {
            return;
        }
        let stored: Vec<TrackId> = {
            let mut db = lock(&self.db);
            let rows: Vec<(TrackId, i64, u64, FileCheck)> = {
                let state = lock(&self.state);
                results
                    .into_iter()
                    .filter(|(job, _)| {
                        state.tracks.get(&job.id).is_some_and(|e| {
                            e.track.mtime_ns == job.mtime_ns && e.track.file_size == job.file_size
                        })
                    })
                    .map(|(job, check)| (job.id, job.mtime_ns, job.file_size, check))
                    .collect()
            };
            if let Err(err) = db.set_checks(&rows, unix_now()) {
                tracing::warn!(%err, "could not store file check results");
                return;
            }
            let mut state = lock(&self.state);
            rows.iter()
                .map(|(id, _, _, check)| {
                    state.update(*id, |t| t.check = Some(*check));
                    *id
                })
                .collect()
        };
        self.publish_patch(&stored);
    }

    /// The check run. `self.checks` has been claimed by the caller.
    fn run_checks(&self) {
        let jobs: Vec<Job> = self
            .snapshot()
            .tracks
            .iter()
            .filter(|t| t.available && t.check.is_none())
            .map(|t| Job {
                id: t.id,
                path: t.path.clone(),
                mtime_ns: t.mtime_ns,
                file_size: t.file_size,
            })
            .collect();
        let job = &self.checks;
        job.total.store(jobs.len() as u64, Ordering::Relaxed);
        self.notify();
        tracing::info!(files = jobs.len(), "checking files");

        let next = AtomicUsize::new(0);
        let pending: Mutex<Vec<(Job, FileCheck)>> = Mutex::new(Vec::new());
        let stop = || job.cancel.load(Ordering::Relaxed) || self.shutdown.load(Ordering::Relaxed);
        let workers = std::thread::available_parallelism()
            .map_or(1, |n| n.get() / 2)
            .clamp(1, MAX_WORKERS)
            .min(jobs.len().max(1));
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| {
                    while !stop() {
                        let Some(file) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) else {
                            break;
                        };
                        match guarded(|| check_file(&file.path, &job.cancel)) {
                            Ok(Some(check)) => {
                                if check.verdict == CheckVerdict::Damaged {
                                    job.damaged.fetch_add(1, Ordering::Relaxed);
                                }
                                let full = {
                                    let mut pending = lock(&pending);
                                    pending.push((file.clone(), check));
                                    (pending.len() >= COMMIT_EVERY)
                                        .then(|| std::mem::take(&mut *pending))
                                };
                                if let Some(results) = full {
                                    self.commit_checks(results);
                                }
                            }
                            // Cancelled in the middle of the file: not a result.
                            Ok(None) => break,
                            Err(err) => {
                                tracing::debug!(track = file.id, %err, "not checked");
                                job.failed.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        job.done.fetch_add(1, Ordering::Relaxed);
                        self.notify();
                    }
                });
            }
        });
        self.commit_checks(std::mem::take(&mut *lock(&pending)));
        tracing::info!(
            done = job.done.load(Ordering::Relaxed),
            damaged = job.damaged.load(Ordering::Relaxed),
            failed = job.failed.load(Ordering::Relaxed),
            "checking files ended"
        );
    }

    /// Forgets every check result.
    pub(crate) fn forget_checks(&self) {
        let cleared: Vec<TrackId> = {
            let db = lock(&self.db);
            if let Err(err) = db.clear_checks() {
                tracing::warn!(%err, "could not clear file check results");
                return;
            }
            let mut state = lock(&self.state);
            let ids: Vec<TrackId> = state
                .tracks
                .values()
                .filter(|e| e.track.check.is_some())
                .map(|e| e.track.id)
                .collect();
            for &id in &ids {
                state.update(id, |t| t.check = None);
            }
            ids
        };
        self.publish_patch(&cleared);
    }
}

/// Runs the check on a library thread and releases the job afterwards, whatever happens.
pub(crate) fn start(inner: &Arc<Inner>) -> bool {
    if inner.shutdown.load(Ordering::Relaxed) || !inner.checks.begin() {
        return false;
    }
    let started = inner.spawn("oneum-check", |inner| {
        struct Release<'a>(&'a Inner);
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                self.0.checks.running.store(false, Ordering::SeqCst);
                self.0.notify();
            }
        }
        let _release = Release(&inner);
        inner.run_checks();
    });
    if !started {
        // No thread, so no run: the job is free again.
        inner.checks.running.store(false, Ordering::SeqCst);
    }
    started
}
