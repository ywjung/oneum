//! A track of a media server on the network, or of a network folder, as something to
//! decode: the bytes behind an `http://` (a folder's may be `https://`) address, read
//! in order over one connection and, where the decoder jumps (a seek, a table at the
//! end of the file), by asking the server for the part from there on.
//!
//! An address that lies in a network folder is asked for as that folder's account,
//! with the certificate the listener trusted for it (`net::folders`).
//!
//! Such a track is loaded into memory while it plays whenever it fits (see
//! `source.rs`), whatever the memory playback setting says: once it is there, the
//! network dropping out for a moment cannot interrupt it.
use super::source::{Budget, FileInMemory, Origin};
use crate::net::folders::{self, Credentials};
use crate::net::http::{self, Limits, Stream};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use symphonia::core::io::MediaSource;

/// Connections kept at a time: the loader's, and one for a reader that is elsewhere.
const CURSORS: usize = 2;
/// A gap up to this is read through rather than asked for anew.
const SKIP_BYTES: u64 = 256 << 10;
/// A reader this far ahead of the loader, or nearer, waits for it.
const NEAR_BYTES: u64 = 4 << 20;

fn limits() -> Limits {
    // The size of the file is bounded by what reads it, not here.
    Limits::new(u64::MAX)
}

fn to_io(error: crate::net::NetError) -> io::Error {
    use crate::net::NetError;
    let kind = match &error {
        NetError::Status(404 | 410) => io::ErrorKind::NotFound,
        NetError::Status(401 | 403) | NetError::Untrusted { .. } => io::ErrorKind::PermissionDenied,
        NetError::Timeout => io::ErrorKind::TimedOut,
        NetError::Connect(_) => io::ErrorKind::ConnectionRefused,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, error.to_string())
}

/// A connection that gives the file's bytes from `at` on.
struct Cursor {
    at: u64,
    stream: Stream,
}

/// The file behind an address.
pub(crate) struct HttpOrigin {
    url: String,
    /// Who asks: the account of the network folder the address lies in, if it does.
    credentials: Credentials,
    /// Connections that are not being read at the moment, each where its last read
    /// ended. A read that goes on where one of them stands takes it.
    idle: Mutex<Vec<Cursor>>,
    /// False once the server has answered a request for a part with the whole file.
    ranges: AtomicBool,
}

impl HttpOrigin {
    /// Connects, and tells the file's length if the server says it.
    fn connect(url: &str) -> io::Result<(Self, Option<u64>)> {
        let credentials = folders::credentials_for(url);
        let open = || http::open_web(url, 0, &limits(), &credentials.access());
        let stream = match open() {
            // A server that did not answer in time is asked once more: a NAS whose
            // disks were asleep takes longer for its first answer than for any other.
            Err(crate::net::NetError::Timeout) => open(),
            other => other,
        }
        .map_err(to_io)?;
        let len = stream.total_len();
        // The request asked for "everything from 0 on" as a part: a server that
        // answers with the plain file does not give parts.
        let ranges = stream.ranged();
        let origin = HttpOrigin {
            url: url.to_owned(),
            credentials,
            idle: Mutex::new(vec![Cursor { at: 0, stream }]),
            ranges: AtomicBool::new(ranges),
        };
        Ok((origin, len))
    }

    fn idle(&self) -> std::sync::MutexGuard<'_, Vec<Cursor>> {
        self.idle.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A connection standing at `offset`: one that is there, one a little before it
    /// read on to it, or a new one.
    fn cursor_at(&self, offset: u64) -> io::Result<Cursor> {
        let near = {
            let mut idle = self.idle();
            let best = idle
                .iter()
                .enumerate()
                .filter(|(_, c)| c.at <= offset && offset - c.at <= SKIP_BYTES)
                .max_by_key(|(_, c)| c.at)
                .map(|(i, _)| i);
            best.map(|i| idle.swap_remove(i))
        };
        if let Some(mut cursor) = near {
            let mut gap = offset - cursor.at;
            let mut skipped = [0u8; 8192];
            while gap > 0 {
                let want = skipped.len().min(gap as usize);
                match cursor.stream.read(&mut skipped[..want]) {
                    Ok(0) => break,
                    Ok(n) => gap -= n as u64,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            if gap == 0 {
                cursor.at = offset;
                return Ok(cursor);
            }
        }
        self.new_cursor_at(offset)
    }

    /// A new connection standing at `offset`.
    fn new_cursor_at(&self, offset: u64) -> io::Result<Cursor> {
        if offset > 0 && !self.ranges.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the server gives the file only from its start",
            ));
        }
        let stream = http::open_web(&self.url, offset, &limits(), &self.credentials.access())
            .map_err(to_io)?;
        if offset > 0 && !stream.ranged() {
            self.ranges.store(false, Ordering::Release);
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the server gives the file only from its start",
            ));
        }
        Ok(Cursor { at: offset, stream })
    }
}

impl Origin for HttpOrigin {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        fn read_some(stream: &mut Stream, buf: &mut [u8]) -> io::Result<usize> {
            loop {
                match stream.read(buf) {
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    other => break other,
                }
            }
        }
        let mut cursor = self.cursor_at(offset)?;
        let n = match read_some(&mut cursor.stream, buf) {
            Ok(n) => n,
            // The connection stalled or was cut (the network away for a moment, a
            // server that closes what has been open for long): the same place is
            // asked for once more on a new one before the file counts as
            // unreadable. A failed read ends the loading into memory for good, and
            // with it what the loading is for.
            Err(error) => {
                tracing::debug!(%error, "a connection to a track's server failed: asking anew");
                cursor = self.new_cursor_at(offset)?;
                read_some(&mut cursor.stream, buf)?
            }
        };
        if n > 0 {
            cursor.at += n as u64;
            let mut idle = self.idle();
            if idle.len() >= CURSORS {
                idle.remove(0);
            }
            idle.push(cursor);
        }
        Ok(n)
    }

    fn wait_for_loader(&self, ahead: u64) -> bool {
        ahead <= NEAR_BYTES || !self.ranges.load(Ordering::Acquire)
    }
}

/// A track's file on a server, connected to.
pub struct RemoteFile {
    url: String,
    origin: Arc<HttpOrigin>,
    len: Option<u64>,
}

impl RemoteFile {
    pub fn open(url: &str) -> io::Result<Self> {
        let (origin, len) = HttpOrigin::connect(url)?;
        Ok(RemoteFile {
            url: url.to_owned(),
            origin: Arc::new(origin),
            len,
        })
    }

    /// Starts loading the file into memory if it fits (see [`FileInMemory::start`]);
    /// gives itself back if it does not, to be read as it comes.
    pub fn into_memory(self, budget: &Arc<Budget>) -> Result<FileInMemory, Self> {
        let Some(len) = self.len else {
            return Err(self);
        };
        let origin: Arc<dyn Origin> = self.origin.clone();
        FileInMemory::start_from(PathBuf::from(&self.url), origin, len, budget).ok_or(self)
    }

    /// The address, as a path (what a file's path is to the decoder: a name to tell
    /// the kind of file by).
    pub fn name(&self) -> PathBuf {
        PathBuf::from(&self.url)
    }

    /// Reads the file as it comes, without keeping it.
    pub fn into_source(self) -> HttpSource {
        HttpSource {
            origin: self.origin,
            len: self.len,
            pos: 0,
        }
    }
}

/// Reads a file of a server as it comes (one too large for memory, or of unknown
/// length).
pub struct HttpSource {
    origin: Arc<HttpOrigin>,
    len: Option<u64>,
    pos: u64,
}

impl Read for HttpSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.len.is_some_and(|len| self.pos >= len) {
            return Ok(0);
        }
        let n = self.origin.read_at(buf, self.pos)?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for HttpSource {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::End(delta) => self.len.and_then(|len| len.checked_add_signed(delta)),
            SeekFrom::Current(delta) => self.pos.checked_add_signed(delta),
        };
        match target {
            Some(pos) => {
                self.pos = pos;
                Ok(pos)
            }
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot seek there in a file of a server",
            )),
        }
    }
}

impl MediaSource for HttpSource {
    fn is_seekable(&self) -> bool {
        self.len.is_some() && self.origin.ranges.load(Ordering::Acquire)
    }

    fn byte_len(&self) -> Option<u64> {
        self.len
    }
}
