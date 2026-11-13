//! Volume leveling for files without ReplayGain tags: the app measures their loudness
//! itself (see `audio::loudness`) and keeps the result in its own database. The files
//! are only read, never written.
//!
//! One run, started by the user, goes through the albums that have something to
//! measure. An album none of whose tracks is tagged is measured as a whole, so that
//! album mode keeps the level differences between its tracks; its results are stored
//! together or not at all. Tracks of albums that are partly tagged, and of "albums"
//! too large to be one (a folder of loose files), are measured one by one.
use super::{Inner, lock};
use crate::audio::loudness::{Measurement, gain_db, integrated, measure_file};
use crate::audio::{PlaybackError, PlaybackErrorKind};
use crate::model::{ReplayGainTags, TrackId};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Albums with more tracks than this are not measured as one programme.
const ALBUM_TRACK_LIMIT: usize = 200;
/// Files measured at the same time, at most (playback must not be starved).
const MAX_WORKERS: usize = 4;

/// Progress of the measuring run, for the settings window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoudnessStatus {
    pub running: bool,
    /// Tracks the current (or last) run set out to measure.
    pub total: u64,
    /// Tracks of that run that have been dealt with.
    pub done: u64,
    /// Tracks of that run that could not be decoded.
    pub failed: u64,
}

#[derive(Default)]
pub(crate) struct LoudnessJob {
    running: AtomicBool,
    cancel: AtomicBool,
    total: AtomicU64,
    done: AtomicU64,
    failed: AtomicU64,
}

impl LoudnessJob {
    pub(crate) fn status(&self) -> LoudnessStatus {
        LoudnessStatus {
            running: self.running.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
            done: self.done.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Claims the job. False if a run is in progress already.
    pub(crate) fn begin(&self) -> bool {
        if self.running.swap(true, Ordering::SeqCst) {
            return false;
        }
        self.cancel.store(false, Ordering::Relaxed);
        self.total.store(0, Ordering::Relaxed);
        self.done.store(0, Ordering::Relaxed);
        self.failed.store(0, Ordering::Relaxed);
        true
    }
}

/// One file to measure, as it was when the run was planned.
#[derive(Debug, Clone, PartialEq)]
struct Job {
    id: TrackId,
    path: PathBuf,
    mtime_ns: i64,
    file_size: u64,
}

/// Files whose results are stored together.
#[derive(Debug, PartialEq)]
struct Group {
    /// The jobs are all the tracks of an album: it gets album values too.
    album: bool,
    jobs: Vec<Job>,
}

/// What a planned track looks like to the planner.
struct Candidate {
    job: Job,
    album_key: u64,
    available: bool,
    tagged: bool,
    measured: bool,
}

/// Decides what to measure and what belongs together. `tracks` in library order.
fn plan(tracks: impl Iterator<Item = Candidate>) -> Vec<Group> {
    let mut order: Vec<u64> = Vec::new();
    let mut albums: HashMap<u64, Vec<Candidate>> = HashMap::new();
    for track in tracks {
        albums
            .entry(track.album_key)
            .or_insert_with(|| {
                order.push(track.album_key);
                Vec::new()
            })
            .push(track);
    }
    let mut groups = Vec::new();
    for key in order {
        let Some(tracks) = albums.remove(&key) else {
            continue;
        };
        let wanting = |t: &Candidate| t.available && !t.tagged && !t.measured;
        if !tracks.iter().any(wanting) {
            continue;
        }
        let untagged = tracks.iter().all(|t| !t.tagged);
        if untagged && tracks.len() <= ALBUM_TRACK_LIMIT {
            // The whole album again, also the tracks measured before: a track that
            // joined the album changes what the album as a whole reads.
            let whole = tracks.iter().all(|t| t.available);
            let jobs: Vec<Job> = tracks
                .into_iter()
                .filter(|t| t.available)
                .map(|t| t.job)
                .collect();
            if whole {
                groups.push(Group { album: true, jobs });
            } else {
                groups.extend(jobs.into_iter().map(|job| Group {
                    album: false,
                    jobs: vec![job],
                }));
            }
        } else {
            groups.extend(tracks.into_iter().filter(wanting).map(|t| Group {
                album: false,
                jobs: vec![t.job],
            }));
        }
    }
    groups
}

/// The values to store for a group from what was measured (`None` = could not be).
fn values_of(album: bool, measured: &[Option<Measurement>]) -> Vec<Option<ReplayGainTags>> {
    let complete: Option<Vec<Measurement>> = if album {
        measured.iter().cloned().collect()
    } else {
        None
    };
    let (album_gain_db, album_peak) = match &complete {
        Some(all) => (
            Some(gain_db(integrated(all))),
            Some(all.iter().fold(0f32, |peak, m| peak.max(m.peak))),
        ),
        None => (None, None),
    };
    measured
        .iter()
        .map(|m| {
            m.as_ref().map(|m| ReplayGainTags {
                track_gain_db: Some(gain_db(m.loudness())),
                track_peak: Some(m.peak),
                album_gain_db,
                album_peak,
            })
        })
        .collect()
}

impl Inner {
    /// Stores the results of a group and shows them. A track whose file changed since
    /// the run was planned (or that is gone) is left out.
    fn commit_loudness(&self, jobs: &[Job], values: Vec<Option<ReplayGainTags>>) {
        let stored: Vec<TrackId> = {
            let mut db = lock(&self.db);
            let rows: Vec<(TrackId, i64, u64, ReplayGainTags)> = {
                let state = lock(&self.state);
                jobs.iter()
                    .zip(values)
                    .filter_map(|(job, values)| {
                        let current = &state.tracks.get(&job.id)?.track;
                        (current.mtime_ns == job.mtime_ns && current.file_size == job.file_size)
                            .then_some((job.id, job.mtime_ns, job.file_size, values?))
                    })
                    .collect()
            };
            if let Err(err) = db.set_loudness(&rows) {
                tracing::warn!(%err, "could not store measured loudness");
                return;
            }
            let mut state = lock(&self.state);
            rows.iter()
                .map(|(id, _, _, values)| {
                    state.update(*id, |t| t.measured = Some(*values));
                    *id
                })
                .collect()
        };
        self.publish_patch(&stored);
    }

    /// The measuring run. `self.loudness` has been claimed by the caller.
    pub(crate) fn run_loudness(&self) {
        let groups = {
            let snapshot = self.snapshot();
            plan(snapshot.tracks.iter().map(|t| Candidate {
                job: Job {
                    id: t.id,
                    path: t.path.clone(),
                    mtime_ns: t.mtime_ns,
                    file_size: t.file_size,
                },
                album_key: t.album_key,
                available: t.available,
                tagged: t.replaygain.track_gain_db.is_some()
                    || t.replaygain.album_gain_db.is_some(),
                measured: t.measured.is_some(),
            }))
        };
        let work: Vec<(usize, usize)> = groups
            .iter()
            .enumerate()
            .flat_map(|(g, group)| (0..group.jobs.len()).map(move |j| (g, j)))
            .collect();
        let job = &self.loudness;
        job.total.store(work.len() as u64, Ordering::Relaxed);
        self.notify();
        tracing::info!(tracks = work.len(), "measuring loudness");

        // Per group: what has been measured so far, and how many are still to come.
        type Pending = (Vec<Option<Measurement>>, usize);
        let pending: Vec<Mutex<Pending>> = groups
            .iter()
            .map(|g| Mutex::new((vec![None; g.jobs.len()], g.jobs.len())))
            .collect();
        let next = AtomicUsize::new(0);
        let stop = || job.cancel.load(Ordering::Relaxed) || self.shutdown.load(Ordering::Relaxed);
        let workers = std::thread::available_parallelism()
            .map_or(1, |n| n.get() / 2)
            .clamp(1, MAX_WORKERS)
            .min(work.len().max(1));
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| {
                    while !stop() {
                        let Some(&(g, j)) = work.get(next.fetch_add(1, Ordering::Relaxed)) else {
                            break;
                        };
                        let group = &groups[g];
                        let path = &group.jobs[j].path;
                        let measured = match guarded(|| measure_file(path, &job.cancel)) {
                            Ok(Some(measurement)) => Some(measurement),
                            // Cancelled in the middle of the file: not a result.
                            Ok(None) => break,
                            Err(err) => {
                                tracing::debug!(track = group.jobs[j].id, %err, "not measured");
                                job.failed.fetch_add(1, Ordering::Relaxed);
                                None
                            }
                        };
                        let finished = {
                            let mut slot = lock(&pending[g]);
                            slot.0[j] = measured;
                            slot.1 -= 1;
                            (slot.1 == 0).then(|| std::mem::take(&mut slot.0))
                        };
                        job.done.fetch_add(1, Ordering::Relaxed);
                        match finished {
                            Some(all) => {
                                self.commit_loudness(&group.jobs, values_of(group.album, &all));
                            }
                            None => self.notify(),
                        }
                    }
                });
            }
        });
        tracing::info!(
            done = job.done.load(Ordering::Relaxed),
            failed = job.failed.load(Ordering::Relaxed),
            "measuring loudness ended"
        );
    }

    /// Forgets everything that was measured.
    pub(crate) fn forget_loudness(&self) {
        let cleared: Vec<TrackId> = {
            let db = lock(&self.db);
            if let Err(err) = db.clear_loudness() {
                tracing::warn!(%err, "could not clear measured loudness");
                return;
            }
            let mut state = lock(&self.state);
            let ids: Vec<TrackId> = state
                .tracks
                .values()
                .filter(|e| e.track.measured.is_some())
                .map(|e| e.track.id)
                .collect();
            for &id in &ids {
                state.update(id, |t| t.measured = None);
            }
            ids
        };
        self.publish_patch(&cleared);
    }
}

/// Runs the job on a library thread and releases it afterwards, whatever happens.
pub(crate) fn start(inner: &Arc<Inner>) -> bool {
    if inner.shutdown.load(Ordering::Relaxed) || !inner.loudness.begin() {
        return false;
    }
    inner.spawn("oneum-loudness", |inner| {
        struct Release<'a>(&'a Inner);
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                self.0.loudness.running.store(false, Ordering::SeqCst);
                self.0.notify();
            }
        }
        let _release = Release(&inner);
        inner.run_loudness();
    });
    true
}

/// Runs a measurement so that a decoder that panics on a damaged file costs that file
/// only: the worker goes on, the file is counted as not measured, and its album is
/// still committed. (Playback and the scan guard their decoding the same way.)
fn guarded<T>(measure: impl FnOnce() -> Result<T, PlaybackError>) -> Result<T, PlaybackError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(measure)).unwrap_or_else(|_| {
        Err(PlaybackError {
            kind: PlaybackErrorKind::Corrupt,
            detail: "decoder panicked".into(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: TrackId, album_key: u64) -> Candidate {
        Candidate {
            job: Job {
                id,
                path: PathBuf::from(format!("/music/{id}.flac")),
                mtime_ns: 1,
                file_size: 1,
            },
            album_key,
            available: true,
            tagged: false,
            measured: false,
        }
    }

    fn ids(groups: &[Group]) -> Vec<(bool, Vec<TrackId>)> {
        groups
            .iter()
            .map(|g| (g.album, g.jobs.iter().map(|j| j.id).collect()))
            .collect()
    }

    #[test]
    fn untagged_albums_are_measured_as_a_whole() {
        let groups = plan([candidate(1, 10), candidate(2, 10), candidate(3, 20)].into_iter());
        assert_eq!(ids(&groups), [(true, vec![1, 2]), (true, vec![3])]);
    }

    #[test]
    fn what_is_tagged_or_measured_already_is_left_alone() {
        // A tagged album: nothing to do. A measured album: nothing to do.
        let mut tagged = candidate(1, 10);
        tagged.tagged = true;
        let mut measured = candidate(2, 20);
        measured.measured = true;
        assert!(plan([tagged, measured].into_iter()).is_empty());

        // An album that is partly tagged: only the untagged track, on its own.
        let mut tagged = candidate(1, 10);
        tagged.tagged = true;
        let groups = plan([tagged, candidate(2, 10)].into_iter());
        assert_eq!(ids(&groups), [(false, vec![2])]);

        // A track joined a measured album: the album is measured again as a whole.
        let mut measured = candidate(1, 10);
        measured.measured = true;
        let groups = plan([measured, candidate(2, 10)].into_iter());
        assert_eq!(ids(&groups), [(true, vec![1, 2])]);
    }

    #[test]
    fn an_album_with_a_missing_file_gets_no_album_values() {
        let mut offline = candidate(1, 10);
        offline.available = false;
        let groups = plan([offline, candidate(2, 10), candidate(3, 10)].into_iter());
        assert_eq!(ids(&groups), [(false, vec![2]), (false, vec![3])]);
        // Nothing but missing files: nothing to do.
        let mut offline = candidate(1, 10);
        offline.available = false;
        assert!(plan([offline].into_iter()).is_empty());
    }

    #[test]
    fn a_folder_of_loose_files_is_not_an_album() {
        let many = (0..=ALBUM_TRACK_LIMIT as i64).map(|i| candidate(i, 10));
        let groups = plan(many);
        assert_eq!(groups.len(), ALBUM_TRACK_LIMIT + 1);
        assert!(groups.iter().all(|g| !g.album && g.jobs.len() == 1));
    }

    #[test]
    fn album_values_need_every_track() {
        let loud = Measurement {
            blocks: vec![0.1; 50],
            peak: 0.9,
        };
        let soft = Measurement {
            blocks: vec![0.025; 50],
            peak: 0.4,
        };
        let values = values_of(true, &[Some(loud.clone()), Some(soft.clone())]);
        let (a, b) = (values[0].expect("measured"), values[1].expect("measured"));
        assert_eq!(a.album_gain_db, b.album_gain_db);
        assert_eq!(a.album_peak, Some(0.9));
        assert_eq!(a.track_peak, Some(0.9));
        assert_eq!(b.track_peak, Some(0.4));
        // The soft track is 6 dB below the loud one; the album sits between them.
        let (ga, gb) = (
            a.track_gain_db.expect("gain"),
            b.track_gain_db.expect("gain"),
        );
        assert!((gb - ga - 6.02).abs() < 0.01, "{ga} {gb}");
        let album = a.album_gain_db.expect("album gain");
        assert!(album > ga && album < gb);

        // A track that could not be measured: the others get their own values only.
        let values = values_of(true, &[Some(loud.clone()), None]);
        assert_eq!(values[1], None);
        let only = values[0].expect("measured");
        assert_eq!(only.album_gain_db, None);
        assert_eq!(only.album_peak, None);
        assert_eq!(only.track_gain_db, a.track_gain_db);
        // Not an album: never album values.
        assert_eq!(
            values_of(false, &[Some(loud)])[0]
                .expect("measured")
                .album_gain_db,
            None
        );
    }

    #[test]
    fn a_decoder_that_panics_is_one_file_not_measured() {
        let quiet = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let outcome = guarded::<()>(|| panic!("index out of bounds in a decoder"));
        std::panic::set_hook(quiet);
        let error = outcome.expect_err("a failure, not a panic");
        assert_eq!(error.kind, PlaybackErrorKind::Corrupt);
        // What does not panic comes through as it is.
        assert_eq!(guarded(|| Ok(7)), Ok(7));
    }
}
