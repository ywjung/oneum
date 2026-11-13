//! Memory playback: a file that is read into memory in the background while it is
//! already being decoded.
//!
//! The decoder never waits for the load. What has been loaded is served from memory;
//! anything else is read straight from the file, so start and seek are as quick as
//! without it. Once the whole file is in memory the file is closed and the rest of the
//! track no longer depends on the disk: a drive that spins down, a slow network share or
//! a cable pulled mid-track cannot interrupt it. The feed loads the next track the same
//! way ahead of time (see `feed.rs`).
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use symphonia::core::io::MediaSource;

/// Larger files are streamed from disk as usual. Two files can be in memory at a time
/// (the track that plays and the one that follows).
pub const MAX_FILE_BYTES: u64 = 1 << 30;
const CHUNK_BYTES: usize = 1 << 20;

struct Shared {
    data: Mutex<Vec<u8>>,
    /// Bytes at the start of the file that are in `data`.
    loaded: AtomicU64,
    complete: AtomicBool,
    cancel: AtomicBool,
}

impl Shared {
    fn data(&self) -> MutexGuard<'_, Vec<u8>> {
        // The loader only appends; a panic there leaves valid bytes behind.
        self.data.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// A file being loaded (or already loaded) into memory.
pub struct FileInMemory {
    path: PathBuf,
    len: u64,
    /// Open until the whole file is in memory.
    file: Option<Arc<File>>,
    shared: Arc<Shared>,
}

impl FileInMemory {
    /// Opens `path` and starts loading it. `Ok(None)` means the file is not suited
    /// (larger than [`MAX_FILE_BYTES`], empty, not a regular file, or no memory to hold
    /// it) and should be streamed as usual.
    pub fn start(path: &Path) -> io::Result<Option<Self>> {
        let file = File::open(path)?;
        let meta = file.metadata()?;
        let len = meta.len();
        if !meta.is_file() || len == 0 || len > MAX_FILE_BYTES {
            return Ok(None);
        }
        let mut data = Vec::new();
        if data.try_reserve_exact(len as usize).is_err() {
            return Ok(None);
        }
        let file = Arc::new(file);
        let shared = Arc::new(Shared {
            data: Mutex::new(data),
            loaded: AtomicU64::new(0),
            complete: AtomicBool::new(false),
            cancel: AtomicBool::new(false),
        });
        let (loader_file, loader_shared) = (file.clone(), shared.clone());
        let spawned = std::thread::Builder::new()
            .name("oneum-preload".into())
            .spawn(move || load(&loader_file, &loader_shared, len));
        if spawned.is_err() {
            return Ok(None);
        }
        Ok(Some(Self {
            path: path.to_path_buf(),
            len,
            file: Some(file),
            shared,
        }))
    }

    pub fn path(&self) -> &Path {
        &self.path
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

impl Drop for FileInMemory {
    fn drop(&mut self) {
        // The loader notices before its next chunk; it is not joined, so a read stuck
        // on a dead drive cannot block whoever drops this.
        self.shared.cancel.store(true, Ordering::Relaxed);
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

fn load(file: &File, shared: &Shared, len: u64) {
    let mut chunk = vec![0u8; CHUNK_BYTES.min(len as usize)];
    let mut offset = 0u64;
    while offset < len {
        if shared.cancel.load(Ordering::Relaxed) {
            return;
        }
        let want = chunk.len().min((len - offset) as usize);
        match read_at(file, &mut chunk[..want], offset) {
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
        let loaded = self.file.shared.loaded.load(Ordering::Acquire);
        let n = if self.pos < loaded {
            let n = want.min((loaded - self.pos) as usize);
            let start = self.pos as usize;
            buf[..n].copy_from_slice(&self.file.shared.data()[start..start + n]);
            n
        } else {
            match &self.file.file {
                Some(file) => read_at(file, &mut buf[..want], self.pos)?,
                None => 0,
            }
        };
        if self.file.file.is_some() && self.file.is_complete() {
            self.file.file = None; // everything is in memory: let go of the file
        }
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
}
