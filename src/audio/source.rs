//! Memory playback: a file that is read into memory in the background while it is
//! already being decoded.
//!
//! The decoder never waits for the load. What has been loaded is served from memory;
//! anything else is read straight from the file, so start and seek are as quick as
//! without it. Once the whole file is in memory the file is closed and the rest of the
//! track no longer depends on the disk: a drive that spins down, a slow network share or
//! a cable pulled mid-track cannot interrupt it. The feed loads the next track the same
//! way ahead of time (see `feed.rs`).
//!
//! The same serves a track of a media server on the network (`remote.rs`): there the
//! bytes come over one connection in order, and a reader that is ahead of them waits
//! for them rather than asking the server again.
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use symphonia::core::io::MediaSource;

/// Larger files are streamed from disk as usual. Two files can be in memory at a time
/// (the track that plays and the one that follows), together within [`Budget`].
pub const MAX_FILE_BYTES: u64 = 1 << 30;
const CHUNK_BYTES: usize = 1 << 20;
/// The files in memory together take at most this share of the installed memory…
const MEMORY_SHARE: u64 = 8;
/// …and never more than this.
const MAX_TOTAL_BYTES: u64 = 3 << 29; // 1.5 GiB
/// …and on a phone never more than this.
const PHONE_MAX_TOTAL_BYTES: u64 = 256 << 20;
/// How long a reader sleeps between looks at what the loader has brought (see
/// [`Origin::wait_for_loader`]).
const LOADER_WAIT: std::time::Duration = std::time::Duration::from_millis(2);
/// The budget when the installed memory cannot be found out.
const FALLBACK_TOTAL_BYTES: u64 = 1 << 29; // 512 MiB

/// How many bytes all files in memory may take together. A file that does not fit in
/// what is left is streamed from disk as usual (and plays the same way).
pub struct Budget {
    limit: u64,
    used: AtomicU64,
}

impl Budget {
    pub fn new(limit: u64) -> Self {
        Self {
            limit,
            used: AtomicU64::new(0),
        }
    }

    /// The app's budget: a share of the installed memory.
    pub fn global() -> &'static Arc<Budget> {
        static GLOBAL: std::sync::OnceLock<Arc<Budget>> = std::sync::OnceLock::new();
        GLOBAL.get_or_init(|| {
            let limit = crate::platform::system::physical_memory_bytes()
                .map_or(FALLBACK_TOTAL_BYTES, |installed| {
                    (installed / MEMORY_SHARE).min(MAX_TOTAL_BYTES)
                });
            // A phone ends an app that takes much memory, long before what is
            // installed is used up, and sooner when the app is in the background,
            // which is where a music player spends its time.
            let limit = if crate::platform::PHONE {
                limit.min(PHONE_MAX_TOTAL_BYTES)
            } else {
                limit
            };
            Arc::new(Budget::new(limit))
        })
    }

    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// Bytes taken by files in memory now.
    pub fn used(&self) -> u64 {
        self.used.load(Ordering::Acquire)
    }

    /// Takes `bytes` of the budget if they fit.
    fn take(&self, bytes: u64) -> bool {
        self.used
            .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).filter(|&total| total <= self.limit)
            })
            .is_ok()
    }

    fn give_back(&self, bytes: u64) {
        self.used.fetch_sub(bytes, Ordering::AcqRel);
    }
}

/// Where the bytes are before they are in memory: a file on disk, or an address on the
/// network.
pub(crate) trait Origin: Send + Sync {
    /// Reads at `offset`: as much as is there, up to `buf.len()` (0 at the end).
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize>;

    /// Whether a reader that is `ahead` bytes ahead of the loader should wait for the
    /// loader rather than read there itself. A file is read anywhere at once; reading
    /// a network address out of order costs a request of its own (and some servers
    /// cannot do it at all).
    fn wait_for_loader(&self, _ahead: u64) -> bool {
        false
    }
}

impl Origin for File {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        read_at(self, buf, offset)
    }
}

struct Shared {
    data: Mutex<Vec<u8>>,
    /// Bytes at the start of the file that are in `data`.
    loaded: AtomicU64,
    complete: AtomicBool,
    /// The loader ended without the whole file (it failed, or the file shrank): what
    /// is not in memory has to be read from the origin.
    stopped: AtomicBool,
    cancel: AtomicBool,
    /// What this file takes of the budget, given back when the bytes are freed (the
    /// loader may hold them a moment longer than the [`FileInMemory`]).
    budget: Arc<Budget>,
    reserved: u64,
    /// The file on disk (or the address on the network), for reading what is not in
    /// memory yet. Let go of once the whole file is in memory, whoever still holds the
    /// bytes (a decoder, the feed that keeps them for the next track of the same
    /// file): the drive it is on can then be ejected while it plays.
    origin: Mutex<Option<Arc<dyn Origin>>>,
}

impl Drop for Shared {
    fn drop(&mut self) {
        self.budget.give_back(self.reserved);
    }
}

impl Shared {
    fn data(&self) -> MutexGuard<'_, Vec<u8>> {
        // The loader only appends; a panic there leaves valid bytes behind.
        self.data.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Where the file comes from, while that is still held (see `Shared::origin`).
    fn origin(&self) -> Option<Arc<dyn Origin>> {
        self.origin
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    fn let_go_of_origin(&self) {
        *self.origin.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
}

/// A file being loaded (or already loaded) into memory.
pub struct FileInMemory {
    path: PathBuf,
    len: u64,
    shared: Arc<Shared>,
    /// Common to every handle of this file in memory (see [`FileInMemory::share`]):
    /// the loader is stopped when the last of them goes.
    _alive: Arc<Alive>,
}

/// Stops the loader when dropped: it is not joined, so a read stuck on a dead drive
/// cannot block whoever lets go of the file.
struct Alive(Arc<Shared>);

impl Drop for Alive {
    fn drop(&mut self) {
        self.0.cancel.store(true, Ordering::Relaxed);
    }
}

impl FileInMemory {
    /// Opens `path` and starts loading it. `Ok(None)` means the file is not suited
    /// (larger than [`MAX_FILE_BYTES`], empty, not a regular file, more than is left of
    /// the app's [`Budget`], or no memory to hold it) and should be streamed as usual.
    pub fn start(path: &Path) -> io::Result<Option<Self>> {
        Self::start_within(path, Budget::global())
    }

    /// [`FileInMemory::start`] within the given budget.
    pub fn start_within(path: &Path, budget: &Arc<Budget>) -> io::Result<Option<Self>> {
        // (Asked before it is opened: Windows refuses to open a folder as a file.)
        if std::fs::metadata(path)?.is_dir() {
            return Ok(None);
        }
        let file = File::open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Ok(None);
        }
        Ok(Self::start_from(
            path.to_path_buf(),
            Arc::new(file),
            meta.len(),
            budget,
        ))
    }

    /// Starts loading `len` bytes from `origin` (named `path` to whoever asks). `None`
    /// as for [`FileInMemory::start`].
    pub(crate) fn start_from(
        path: PathBuf,
        origin: Arc<dyn Origin>,
        len: u64,
        budget: &Arc<Budget>,
    ) -> Option<Self> {
        if len == 0 || len > MAX_FILE_BYTES || !budget.take(len) {
            return None;
        }
        // From here on the budget is given back when `shared` goes (or right here).
        let mut data = Vec::new();
        if data.try_reserve_exact(len as usize).is_err() {
            budget.give_back(len);
            return None;
        }
        let shared = Arc::new(Shared {
            data: Mutex::new(data),
            loaded: AtomicU64::new(0),
            complete: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            cancel: AtomicBool::new(false),
            budget: budget.clone(),
            reserved: len,
            origin: Mutex::new(Some(origin.clone())),
        });
        let loader_shared = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("oneum-preload".into())
            .spawn(move || {
                load(&*origin, &loader_shared, len);
                if !loader_shared.complete.load(Ordering::Acquire) {
                    loader_shared.stopped.store(true, Ordering::Release);
                }
            });
        if spawned.is_err() {
            return None;
        }
        Some(Self {
            path,
            len,
            _alive: Arc::new(Alive(shared.clone())),
            shared,
        })
    }

    /// Another handle to the same bytes: one file in memory read by two decoders (two
    /// tracks of one CUE sheet's audio file), loaded once and counted once against the
    /// budget.
    pub fn share(&self) -> Self {
        Self {
            path: self.path.clone(),
            len: self.len,
            shared: self.shared.clone(),
            _alive: self._alive.clone(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether `other` is a handle to the same file in memory.
    pub fn is(&self, other: &FileInMemory) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
    }

    /// Size of the file in bytes (never 0: empty files are not loaded).
    pub fn byte_len(&self) -> u64 {
        self.len
    }

    /// True once the whole file is in memory.
    pub fn is_complete(&self) -> bool {
        self.shared.complete.load(Ordering::Acquire)
    }

    /// A flag that turns true when the whole file is in memory; outlives `self`.
    pub fn completion(&self) -> LoadCompletion {
        LoadCompletion(self.shared.clone())
    }

    pub fn into_source(self) -> MemorySource {
        MemorySource { file: self, pos: 0 }
    }
}

/// The file of the track that plays, for as long as it is in memory: what a new feed
/// of the same track (after a seek, for the next round of a repeated section, after
/// a setting that begins the feed anew) reads, instead of loading the file once more
/// or asking its server for it from the first byte. One for an engine, handed to
/// each of its feeds; let go of when the feeds are stopped.
#[derive(Default)]
pub struct KeptFile(Mutex<Option<FileInMemory>>);

impl KeptFile {
    fn slot(&self) -> MutexGuard<'_, Option<FileInMemory>> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A handle to the kept file, if it is the one at `path`.
    pub fn get(&self, path: &Path) -> Option<FileInMemory> {
        self.slot()
            .as_ref()
            .filter(|file| file.path() == path)
            .map(FileInMemory::share)
    }

    /// Keeps `file` in the place of what was kept.
    pub fn set(&self, file: &FileInMemory) {
        let mut slot = self.slot();
        if !slot.as_ref().is_some_and(|kept| kept.is(file)) {
            *slot = Some(file.share());
        }
    }

    pub fn clear(&self) {
        *self.slot() = None;
    }
}

/// Tells whether a [`FileInMemory`] has finished loading.
pub struct LoadCompletion(Arc<Shared>);

impl LoadCompletion {
    pub fn is_complete(&self) -> bool {
        self.0.complete.load(Ordering::Acquire)
    }
}

/// Reads from `file` at `offset` without relying on (or, on Unix, moving) its cursor.
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    #[cfg(unix)]
    {
        std::os::unix::fs::FileExt::read_at(file, buf, offset)
    }
    #[cfg(windows)]
    {
        // Moves the file's cursor, which nothing here uses: every read names its offset.
        std::os::windows::fs::FileExt::seek_read(file, buf, offset)
    }
}

fn load(origin: &dyn Origin, shared: &Shared, len: u64) {
    let mut chunk = vec![0u8; CHUNK_BYTES.min(len as usize)];
    let mut offset = 0u64;
    while offset < len {
        if shared.cancel.load(Ordering::Relaxed) {
            return;
        }
        let want = chunk.len().min((len - offset) as usize);
        match origin.read_at(&mut chunk[..want], offset) {
            // The file shrank, or the drive failed. What is in memory stays valid; the
            // reader goes on reading the file itself and reports whatever that returns.
            Ok(0) => return,
            Ok(n) => {
                shared.data().extend_from_slice(&chunk[..n]);
                offset += n as u64;
                shared.loaded.store(offset, Ordering::Release);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
    shared.complete.store(true, Ordering::Release);
    // Everything is in memory: nobody reads the file any more.
    shared.let_go_of_origin();
}

/// Reads a [`FileInMemory`]: from memory where it is loaded, from the file elsewhere.
pub struct MemorySource {
    file: FileInMemory,
    pos: u64,
}

impl Read for MemorySource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = self.file.len;
        if self.pos >= len || buf.is_empty() {
            return Ok(0);
        }
        let want = buf.len().min((len - self.pos) as usize);
        let shared = &self.file.shared;
        let n = loop {
            let loaded = shared.loaded.load(Ordering::Acquire);
            if self.pos < loaded {
                let n = want.min((loaded - self.pos) as usize);
                let start = self.pos as usize;
                buf[..n].copy_from_slice(&shared.data()[start..start + n]);
                break n;
            }
            match shared.origin() {
                // The bytes are on their way (a network address): they are waited
                // for, unless the loader has ended without them.
                Some(origin)
                    if origin.wait_for_loader(self.pos - loaded)
                        && !shared.stopped.load(Ordering::Acquire) =>
                {
                    std::thread::sleep(LOADER_WAIT);
                }
                Some(origin) => break origin.read_at(&mut buf[..want], self.pos)?,
                // Let go of because everything has come into memory meanwhile: read
                // it there. (Otherwise the loader gave up and so does this.)
                None if shared.complete.load(Ordering::Acquire) => {}
                None => break 0,
            }
        };
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for MemorySource {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::End(delta) => self.file.len.checked_add_signed(delta),
            SeekFrom::Current(delta) => self.pos.checked_add_signed(delta),
        };
        match target {
            Some(pos) => {
                self.pos = pos;
                Ok(pos)
            }
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before the start of the file",
            )),
        }
    }
}

impl MediaSource for MemorySource {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.file.len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    fn wait_complete(file: &FileInMemory) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !file.is_complete() {
            assert!(Instant::now() < deadline, "file never finished loading");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn reads_the_same_bytes_as_the_file() {
        let path = fixture("gap_a.flac");
        let expected = std::fs::read(&path).expect("fixture");
        // Read at once, i.e. mostly before the loader got anywhere.
        let mut source = FileInMemory::start(&path)
            .expect("opens")
            .expect("fits in memory")
            .into_source();
        assert_eq!(source.byte_len(), Some(expected.len() as u64));
        let mut all = Vec::new();
        source.read_to_end(&mut all).expect("reads");
        assert_eq!(all, expected);
        // And again once everything is in memory, in odd-sized pieces.
        let file = FileInMemory::start(&path).expect("opens").expect("fits");
        wait_complete(&file);
        let mut source = file.into_source();
        let mut all = Vec::new();
        let mut piece = [0u8; 777];
        loop {
            let n = source.read(&mut piece).expect("reads");
            if n == 0 {
                break;
            }
            all.extend_from_slice(&piece[..n]);
        }
        assert_eq!(all, expected);
    }

    #[test]
    fn seeks_like_a_file() {
        let path = fixture("gap_a.wav");
        let expected = std::fs::read(&path).expect("fixture");
        let mut source = FileInMemory::start(&path)
            .expect("opens")
            .expect("fits")
            .into_source();
        let mut four = [0u8; 4];
        assert_eq!(
            source.seek(SeekFrom::End(-4)).expect("seek"),
            expected.len() as u64 - 4
        );
        source.read_exact(&mut four).expect("read");
        assert_eq!(four, expected[expected.len() - 4..]);
        assert_eq!(source.seek(SeekFrom::Start(100)).expect("seek"), 100);
        assert_eq!(source.seek(SeekFrom::Current(-60)).expect("seek"), 40);
        source.read_exact(&mut four).expect("read");
        assert_eq!(four, expected[40..44]);
        assert!(source.seek(SeekFrom::Current(-1000)).is_err());
        // Past the end is allowed and reads nothing.
        source.seek(SeekFrom::End(10)).expect("seek");
        assert_eq!(source.read(&mut four).expect("read"), 0);
    }

    /// The point of memory playback: once loaded, the file is not needed any more.
    #[test]
    fn a_loaded_file_survives_losing_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("track.flac");
        std::fs::copy(fixture("gap_a.flac"), &path).expect("copy");
        let expected = std::fs::read(&path).expect("read");
        let file = FileInMemory::start(&path).expect("opens").expect("fits");
        let done = file.completion();
        wait_complete(&file);
        assert!(done.is_complete());
        // Emptied in place: reading the file itself (even through a handle opened
        // earlier) would now return nothing. Deleting would not show the difference,
        // because an open handle keeps a deleted file readable.
        std::fs::File::create(&path).expect("truncate");
        let mut all = Vec::new();
        file.into_source().read_to_end(&mut all).expect("reads");
        assert_eq!(all, expected);
    }

    /// Two tracks of one CUE sheet read the same file in memory: loaded once, counted
    /// once, and still loaded and readable through one handle when the other is gone.
    #[test]
    fn a_shared_file_in_memory_is_loaded_and_counted_once() {
        use std::io::Read;
        let path = fixture("gap_a.flac");
        let len = std::fs::metadata(&path).expect("fixture").len();
        let budget = Arc::new(Budget::new(len + len / 2));
        let first = FileInMemory::start_within(&path, &budget)
            .expect("opens")
            .expect("fits");
        let second = first.share();
        assert_eq!(budget.used(), len, "one copy");
        drop(first);
        wait_complete(&second);
        // With everything in memory the file on disk is let go of, although a handle to
        // the bytes is still kept (as the feed keeps one for the next track of the
        // same file): the drive can be ejected. The loader lets go right after it marks
        // the load complete.
        let deadline = Instant::now() + Duration::from_secs(10);
        while second.shared.origin().is_some() {
            assert!(Instant::now() < deadline, "the file is still held");
            std::thread::sleep(Duration::from_millis(1));
        }
        let mut bytes = Vec::new();
        second
            .share()
            .into_source()
            .read_to_end(&mut bytes)
            .expect("reads");
        assert_eq!(bytes, std::fs::read(&path).expect("reads the file"));
        drop(second);
        let deadline = Instant::now() + Duration::from_secs(10);
        while budget.used() != 0 {
            assert!(Instant::now() < deadline, "the budget was not given back");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn files_in_memory_stay_within_the_budget() {
        let path = fixture("gap_a.flac");
        let len = std::fs::metadata(&path).expect("fixture").len();
        // Room for two of them, not three.
        let budget = Arc::new(Budget::new(2 * len + len / 2));
        let first = FileInMemory::start_within(&path, &budget)
            .expect("opens")
            .expect("fits");
        let second = FileInMemory::start_within(&path, &budget)
            .expect("opens")
            .expect("fits");
        assert_eq!(budget.used(), 2 * len);
        assert!(
            FileInMemory::start_within(&path, &budget)
                .expect("opens")
                .is_none(),
            "a third does not fit: it is streamed"
        );
        // Given back once the bytes are freed (the loader lets go of them too).
        wait_complete(&first);
        drop(first);
        let deadline = Instant::now() + Duration::from_secs(10);
        while budget.used() != len {
            assert!(Instant::now() < deadline, "the budget was not given back");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            FileInMemory::start_within(&path, &budget)
                .expect("opens")
                .is_some()
        );
        drop(second);
        // The app's own budget is a share of the installed memory, within the cap.
        let global = Budget::global().limit();
        assert!(global > 0 && global <= MAX_TOTAL_BYTES, "{global}");
    }

    #[test]
    fn unsuitable_and_missing_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let empty = dir.path().join("empty.flac");
        std::fs::write(&empty, b"").expect("write");
        assert!(FileInMemory::start(&empty).expect("opens").is_none());
        assert!(FileInMemory::start(dir.path()).is_ok_and(|f| f.is_none()));
        let missing = FileInMemory::start(&dir.path().join("nope.flac"));
        assert!(missing.is_err_and(|e| e.kind() == io::ErrorKind::NotFound));
    }
    /// The kept file is handed out for its own path only, as one more handle to the
    /// same memory (its room is counted once), and let go of when it is cleared.
    #[test]
    fn a_kept_file_is_the_same_memory_for_whoever_asks_for_its_path() {
        let path = fixture("gap_a.flac");
        let len = std::fs::metadata(&path).unwrap().len();
        let budget = Arc::new(Budget::new(1 << 30));
        let file = FileInMemory::start_within(&path, &budget)
            .unwrap()
            .expect("fits");
        let kept = KeptFile::default();
        assert!(kept.get(&path).is_none());
        kept.set(&file);
        kept.set(&file);
        let again = kept.get(&path).expect("kept");
        assert!(again.is(&file));
        assert!(kept.get(&fixture("gap_b.flac")).is_none());
        assert_eq!(budget.used(), len);
        // The handles go, the kept one stays; cleared, the room is free again.
        drop((file, again));
        assert_eq!(budget.used(), len);
        assert!(kept.get(&path).is_some());
        kept.clear();
        assert!(kept.get(&path).is_none());
        // (The thread that loaded the file lets go of it a moment after its last
        // byte.)
        let started = std::time::Instant::now();
        while budget.used() != 0 && started.elapsed() < std::time::Duration::from_secs(10) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(budget.used(), 0);
    }
}
