//! A small HTTP/1.1 client over `std::net::TcpStream`: what is needed to read a media
//! server's description, to send it a command and to read a music file from any byte
//! on, and nothing else. One connection carries one request (`Connection: close`).
//!
//! Media servers are spoken to in `http://` only (`get`, `post`, `open`). The folders
//! of a NAS and the lyrics service may be at an `https://` address and may ask who is
//! there (`send`, `get_web`, `open_web`, with an [`Access`]): TLS is in `tls`, the
//! answer to a server's question in `auth`.
//!
//! The server is not trusted: the header block, the lines in it and (for `get`, `post`
//! and `send`) the body are limited, a silent server is given up on after the read
//! timeout, and an answer that is not HTTP is an error.
use super::{NetError, auth, tls};
use std::fmt::Write as _;
use std::io::{self, BufRead, BufReader, Read, Write as _};
use std::net::{IpAddr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

/// The longest address taken. Those of media servers are a few dozen bytes.
const MAX_URL_BYTES: usize = 2048;
/// The longest host name there is.
const MAX_HOST_BYTES: usize = 253;
/// The header block of an answer may take this much…
const MAX_HEAD_BYTES: usize = 64 * 1024;
/// …and this many lines.
const MAX_HEADER_LINES: usize = 200;
/// The line that says how long a chunk is: a hexadecimal number of a few digits.
const MAX_CHUNK_LINE_BYTES: usize = 32;
/// Answers like `100 Continue` passed over before the one that counts.
const MAX_INTERIM_ANSWERS: usize = 4;
/// A name can stand for many addresses; this many are tried.
const MAX_ADDRESSES: usize = 4;
/// The whole body of a `get` or `post` has to arrive within this many read timeouts.
/// The read timeout alone would let a server that sends a byte every few seconds keep
/// a request going for hours.
const BODY_TIMEOUTS: u32 = 6;

/// An `http://` (or `https://`) address, taken apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    /// `https://`: the connection speaks TLS.
    pub secure: bool,
    /// Lower case; an IPv6 address without its brackets.
    pub host: String,
    pub port: u16,
    /// Path and query, starting with '/'.
    pub path: String,
}

impl Url {
    /// Reads `http://host[:port][/path][?query]`. The scheme may be written in any
    /// case, the host is a name, an IPv4 address or an IPv6 address in brackets, the
    /// port is 80 unless given. Refused: every other scheme, a user name or password,
    /// control characters and spaces, more than 2048 bytes. A `#fragment` is dropped
    /// (it is never sent to a server), and characters outside ASCII in the path are
    /// percent-encoded, as they have to be on the request line.
    ///
    /// What is returned can be written out (`to_string`) and read again to the same
    /// address, so an address that only fits the 2048 bytes before it is encoded is
    /// refused as well.
    pub fn parse(text: &str) -> Result<Url, NetError> {
        Url::read(text, false)
    }

    /// Like [`Url::parse`], and `https://` addresses as well (port 443 unless given).
    pub fn parse_web(text: &str) -> Result<Url, NetError> {
        Url::read(text, true)
    }

    fn read(text: &str, secure_too: bool) -> Result<Url, NetError> {
        if text.len() > MAX_URL_BYTES {
            return Err(NetError::BadAddress);
        }
        let (secure, rest) = strip_scheme(text).ok_or(NetError::BadAddress)?;
        if secure && !secure_too {
            return Err(NetError::BadAddress);
        }
        let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(authority_end);
        let default_port = if secure { 443 } else { 80 };
        let (host, port) = parse_authority(authority, default_port).ok_or(NetError::BadAddress)?;
        let path = clean_path(tail).ok_or(NetError::BadAddress)?;
        Url {
            secure,
            host,
            port,
            path,
        }
        .if_short_enough()
    }

    fn scheme(&self) -> &'static str {
        if self.secure { "https" } else { "http" }
    }

    fn default_port(&self) -> u16 {
        if self.secure { 443 } else { 80 }
    }

    /// The server an address is at: what name and password are for, and what a
    /// trusted certificate belongs to.
    pub fn origin(&self) -> (bool, &str, u16) {
        (self.secure, &self.host, self.port)
    }

    /// The path without its query as a place: `%XX` read as what it stands for (all
    /// but `%2F`, which is no separator) and ASCII letters in lower case. Servers
    /// write one place in more than one way ("%ec" for "%EC", "%28" for "(",
    /// "/Music/" for "/music/" where the disk does not tell them apart): as places
    /// they are the same text.
    pub fn place(&self) -> String {
        let path = self.path.split('?').next().unwrap_or("/");
        let bytes = path.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let hex = |b: Option<&u8>| b.and_then(|b| (*b as char).to_digit(16));
        let mut i = 0;
        while i < bytes.len() {
            match (bytes[i], hex(bytes.get(i + 1)), hex(bytes.get(i + 2))) {
                (b'%', Some(a), Some(b)) => {
                    match (a * 16 + b) as u8 {
                        b'/' => out.extend_from_slice(b"%2f"),
                        byte => out.push(byte.to_ascii_lowercase()),
                    }
                    i += 3;
                }
                (byte, ..) => {
                    out.push(byte.to_ascii_lowercase());
                    i += 1;
                }
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    /// The address up to the path: "https://host:port".
    pub fn server(&self) -> String {
        let scheme = if self.secure { "https" } else { "http" };
        format!("{scheme}://{}", self.authority())
    }

    /// The address `reference` means when it is found in a document at this address:
    /// a whole `http://` address, a path from the root (`/a/b`) or a path relative to
    /// this one (`b`, `../b`, `?query`), merged as RFC 3986 says.
    pub fn join(&self, reference: &str) -> Result<Url, NetError> {
        if reference.len() > MAX_URL_BYTES {
            return Err(NetError::BadAddress);
        }
        // An `https://` address leads to `https://` addresses only (never from an
        // encrypted connection to one that is not), an `http://` one to `http://`.
        if has_scheme(reference) {
            let url = Url::read(reference, self.secure)?;
            return if url.secure == self.secure {
                Ok(url)
            } else {
                Err(NetError::BadAddress)
            };
        }
        if reference.starts_with("//") {
            return Url::read(&format!("{}:{reference}", self.scheme()), self.secure);
        }
        let reference = reference.split('#').next().unwrap_or("");
        let base_path = self.path.split('?').next().unwrap_or("/");
        let merged = if reference.is_empty() {
            self.path.clone()
        } else if reference.starts_with('/') {
            reference.to_string()
        } else if reference.starts_with('?') {
            format!("{base_path}{reference}")
        } else {
            let folder = base_path.rfind('/').map_or("/", |at| &base_path[..=at]);
            format!("{folder}{reference}")
        };
        let merged = match merged.split_once('?') {
            Some((path, query)) => format!("{}?{query}", remove_dot_segments(path)),
            None => remove_dot_segments(&merged),
        };
        Url {
            secure: self.secure,
            host: self.host.clone(),
            port: self.port,
            path: clean_path(&merged).ok_or(NetError::BadAddress)?,
        }
        .if_short_enough()
    }

    /// The address, unless it is longer written out than an address may be.
    fn if_short_enough(self) -> Result<Url, NetError> {
        let written = self.scheme().len() + 3 + self.authority().len() + self.path.len();
        if written > MAX_URL_BYTES {
            return Err(NetError::BadAddress);
        }
        Ok(self)
    }

    /// Host and port as they go into the `Host` header and into the address.
    fn authority(&self) -> String {
        let mut text = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        if self.port != self.default_port() {
            let _ = write!(text, ":{}", self.port);
        }
        text
    }
}

impl std::fmt::Display for Url {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}://{}{}", self.scheme(), self.authority(), self.path)
    }
}

/// What follows `http://` or `https://`, when `text` begins with one of them, and
/// whether it was the second.
fn strip_scheme(text: &str) -> Option<(bool, &str)> {
    [(false, "http://"), (true, "https://")]
        .into_iter()
        .find_map(|(secure, scheme)| {
            let start = text.get(..scheme.len())?;
            start
                .eq_ignore_ascii_case(scheme)
                .then(|| (secure, &text[scheme.len()..]))
        })
}

/// Whether `reference` begins with a scheme (`mailto:`, `https://…`), which makes it
/// an address of its own rather than a path.
fn has_scheme(reference: &str) -> bool {
    let Some(end) = reference.find([':', '/', '?', '#']) else {
        return false;
    };
    let scheme = &reference[..end];
    reference[end..].starts_with(':')
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// Host and port of `host`, `host:port`, `[v6]` or `[v6]:port`.
fn parse_authority(authority: &str, default_port: u16) -> Option<(String, u16)> {
    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let (address, after) = bracketed.split_once(']')?;
        let address: Ipv6Addr = address.parse().ok()?;
        let port = match after.strip_prefix(':') {
            Some(port) => Some(port),
            None if after.is_empty() => None,
            None => return None,
        };
        (address.to_string(), port)
    } else {
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        };
        // This also refuses `user@host`: an '@' is no character of a host name.
        let plain = !host.is_empty()
            && host.len() <= MAX_HOST_BYTES
            && host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'));
        if !plain {
            return None;
        }
        (host.to_ascii_lowercase(), port)
    };
    let port = match port {
        None | Some("") => default_port,
        Some(digits) => {
            if digits.len() > 5 || !digits.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            digits.parse::<u16>().ok().filter(|&port| port != 0)?
        }
    };
    Some((host, port))
}

/// Path and query as they are sent: without the fragment, starting with '/', with
/// everything outside ASCII percent-encoded. `None` when a control character or a
/// space is in it (either would break the request line apart).
fn clean_path(tail: &str) -> Option<String> {
    let tail = tail.split('#').next().unwrap_or("");
    let mut path = String::with_capacity(tail.len() + 1);
    if !tail.starts_with('/') {
        path.push('/');
    }
    for c in tail.chars() {
        if c.is_control() || c == ' ' {
            return None;
        }
        if c.is_ascii() {
            path.push(c);
        } else {
            let mut utf8 = [0u8; 4];
            for byte in c.encode_utf8(&mut utf8).bytes() {
                let _ = write!(path, "%{byte:02X}");
            }
        }
    }
    Some(path)
}

/// Resolves `.` and `..` in a path that starts with '/' (RFC 3986, 5.2.4). A `..` at
/// the root stays at the root.
fn remove_dot_segments(path: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    // After a final "." or ".." the path names a folder and ends with '/'.
    let mut folder = false;
    for segment in path.split('/').skip(1) {
        folder = matches!(segment, "." | "..");
        match segment {
            "." => {}
            ".." => {
                kept.pop();
            }
            _ => kept.push(segment),
        }
    }
    let mut out = String::with_capacity(path.len());
    for segment in &kept {
        out.push('/');
        out.push_str(segment);
    }
    if folder || out.is_empty() {
        out.push('/');
    }
    out
}

/// How the app names itself to a server: its name and version, as services ask of
/// their clients (the lyrics service counts the requests of a client that says who it
/// is more generously). Nothing of the device or the person is in it.
pub const USER_AGENT: &str = concat!("OnEum/", env!("CARGO_PKG_VERSION"));

/// How long to wait and how much to take.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// For the connection to be made.
    pub connect: Duration,
    /// For the server to send something, each time it is waited for. The header block
    /// as a whole also has to arrive within it, and the whole body of a `get` or
    /// `post` within six times it: a server that sends a byte every few seconds is
    /// not waited on for hours. A stream (`open`) is read for as long as it plays.
    pub read: Duration,
    /// The largest body `get` and `post` return.
    pub max_body: u64,
    /// How many redirects are followed.
    pub redirects: u8,
}

impl Limits {
    /// The usual limits: 3 seconds to connect, 10 seconds for the server to send
    /// something, 3 redirects.
    pub const fn new(max_body: u64) -> Limits {
        Limits {
            connect: Duration::from_secs(3),
            read: Duration::from_secs(10),
            max_body,
            redirects: 3,
        }
    }
}

/// Who asks, and what is trusted of the server, for the requests that may go to an
/// `https://` address or to a server that asks for a name.
#[derive(Clone, Copy, Default)]
pub struct Access<'a> {
    /// Name and password. They are given only to a server that asks for them, only to
    /// the server the request was made to (not to one it sends on to), and where the
    /// server offers it in a way that does not send the password itself (`auth`).
    pub account: Option<(&'a str, &'a str)>,
    /// The fingerprint of a certificate that is trusted as it is (see `tls`).
    pub pinned: Option<&'a str>,
}

// Not derived: a password must never be printed with the rest.
impl std::fmt::Debug for Access<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Access")
            .field("account", &self.account.map(|(user, _)| user))
            .field("pinned", &self.pinned)
            .finish()
    }
}

/// The whole body of a GET (follows redirects; non-2xx is `NetError::Status`).
pub fn get(url: &str, limits: &Limits) -> Result<Vec<u8>, NetError> {
    get_from(Url::parse(url)?, limits, &Access::default())
}

/// Like [`get`], for an `http://` or `https://` address and with an [`Access`].
pub fn get_web(url: &str, limits: &Limits, access: &Access<'_>) -> Result<Vec<u8>, NetError> {
    get_from(Url::parse_web(url)?, limits, access)
}

fn get_from(url: Url, limits: &Limits, access: &Access<'_>) -> Result<Vec<u8>, NetError> {
    let (head, mut body) = fetch(url, &Request::Get { range_from: None }, limits, access)?;
    if head.status == 429 {
        return Err(NetError::TooMany {
            wait_secs: head.retry_after,
        });
    }
    if !(200..300).contains(&head.status) {
        return Err(NetError::Status(head.status));
    }
    body.give_time(limits.read);
    read_whole(&mut body, head.length(), limits.max_body)
}

/// A POST; returns the status and the body also for 4xx/5xx (SOAP faults come as 500).
///
/// Of the redirects only 307 and 308 are followed, which say to send the same request
/// again elsewhere. The others would turn the POST into a GET, which is of no use for
/// a command; they are returned like any other status.
pub fn post(
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    limits: &Limits,
) -> Result<(u16, Vec<u8>), NetError> {
    send_to(
        Url::parse(url)?,
        "POST",
        headers,
        body,
        limits,
        &Access::default(),
    )
}

/// A request with a method of its own and a body (WebDAV's `PROPFIND`), to an
/// `http://` or `https://` address; returns the status and the body whatever the
/// status is. Redirects are followed as [`post`] follows them.
pub fn send(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    limits: &Limits,
    access: &Access<'_>,
) -> Result<(u16, Vec<u8>), NetError> {
    send_to(Url::parse_web(url)?, method, headers, body, limits, access)
}

fn send_to(
    url: Url,
    method: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    limits: &Limits,
    access: &Access<'_>,
) -> Result<(u16, Vec<u8>), NetError> {
    let plain_method = !method.is_empty() && method.bytes().all(|b| b.is_ascii_uppercase());
    if !plain_method || !headers.iter().all(|(name, value)| is_header(name, value)) {
        // The caller's mistake, not the network's; it must still not reach the wire,
        // where a line break in a value would begin a header of its own.
        return Err(NetError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a request header with characters that cannot be sent",
        )));
    }
    let request = Request::Send {
        method,
        headers,
        body,
    };
    let (head, mut answer) = fetch(url, &request, limits, access)?;
    answer.give_time(limits.read);
    let bytes = read_whole(&mut answer, head.length(), limits.max_body)?;
    Ok((head.status, bytes))
}

/// Opens a resource for reading from byte `from`.
///
/// `Range: bytes=from-` is always sent, also for `from` 0 (as ffmpeg and VLC do): the
/// first answer then already tells whether the server can start anywhere (`ranged`),
/// which is what seeking needs to know. `limits.max_body` is not looked at; the
/// caller reads as much as it wants.
pub fn open(url: &str, from: u64, limits: &Limits) -> Result<Stream, NetError> {
    open_at(Url::parse(url)?, from, limits, &Access::default())
}

/// Like [`open`], for an `http://` or `https://` address and with an [`Access`].
pub fn open_web(
    url: &str,
    from: u64,
    limits: &Limits,
    access: &Access<'_>,
) -> Result<Stream, NetError> {
    open_at(Url::parse_web(url)?, from, limits, access)
}

fn open_at(url: Url, from: u64, limits: &Limits, access: &Access<'_>) -> Result<Stream, NetError> {
    let request = Request::Get {
        range_from: Some(from),
    };
    let (head, mut body) = fetch(url, &request, limits, access)?;
    // From here on only the read timeout counts: a music file is read for as long as
    // it plays.
    body.reader.get_mut().deadline = None;
    let (ranged, total_len) = match head.status {
        206 => {
            let range = head
                .content_range
                .as_deref()
                .and_then(parse_content_range)
                .ok_or(NetError::BadResponse("a partial answer without its range"))?;
            if range.start != from {
                // Playing on from here would put the wrong bytes at this position.
                return Err(NetError::BadResponse(
                    "the server sent another range than asked for",
                ));
            }
            let total = range
                .total
                .or_else(|| head.length().and_then(|rest| from.checked_add(rest)));
            (true, total)
        }
        200..=299 => (false, head.length()),
        status => return Err(NetError::Status(status)),
    };
    Ok(Stream {
        body,
        total_len,
        ranged,
        content_type: head.content_type,
    })
}

/// A resource being read (see [`open`]).
pub struct Stream {
    body: Body<Socket>,
    total_len: Option<u64>,
    ranged: bool,
    content_type: Option<String>,
}

// The player reads a stream on another thread than the one that opened it.
const _: () = {
    const fn is_send<T: Send>() {}
    is_send::<Stream>();
};

impl Stream {
    /// Length of the whole resource in bytes, when the server says (Content-Range
    /// total, or Content-Length of a 200).
    pub fn total_len(&self) -> Option<u64> {
        self.total_len
    }

    /// Whether the server honoured the range (206). False with from > 0 means it sent
    /// the resource from its start.
    pub fn ranged(&self) -> bool {
        self.ranged
    }

    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }
}

/// Reads the body: chunks are decoded, and the end is where `Content-Length` (or the
/// last chunk, or the closed connection) puts it. A body that ends before that is
/// `ErrorKind::UnexpectedEof`; a server silent for the read timeout is
/// `ErrorKind::TimedOut` on every system.
impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.body.read(buf)
    }
}

/// One whole answer read from bytes that are already there, the way `post` reads it
/// from a connection: the status and the body. For tests, which feed it damaged
/// answers without a server.
pub fn parse_response(bytes: &[u8], max_body: u64) -> Result<(u16, Vec<u8>), NetError> {
    let mut reader = BufReader::new(bytes);
    let head = read_head(&mut reader)?;
    let mut body = Body {
        framing: head.framing(),
        reader,
    };
    let bytes = read_whole(&mut body, head.length(), max_body)?;
    Ok((head.status, bytes))
}

enum Request<'a> {
    Get {
        range_from: Option<u64>,
    },
    Send {
        method: &'a str,
        headers: &'a [(&'a str, &'a str)],
        body: &'a [u8],
    },
}

impl Request<'_> {
    /// Whether an answer with this status sends the request on to another address.
    fn follows(&self, status: u16) -> bool {
        match self {
            Request::Get { .. } => matches!(status, 301 | 302 | 303 | 307 | 308),
            Request::Send { .. } => matches!(status, 307 | 308),
        }
    }

    fn method(&self) -> &str {
        match self {
            Request::Get { .. } => "GET",
            Request::Send { method, .. } => method,
        }
    }

    /// The request as it is sent. `authorization`: the answer to the server's
    /// question who is there, when it has asked.
    fn bytes(&self, url: &Url, authorization: Option<&str>) -> Vec<u8> {
        let method = self.method();
        // `identity`: the body is wanted as it is stored, so that byte positions in
        // it mean what they say. `close`: one request per connection, so the end of
        // the connection can end a body.
        let mut head = format!(
            "{method} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {USER_AGENT}\r\n\
             Accept-Encoding: identity\r\nConnection: close\r\n",
            url.path,
            url.authority()
        );
        if let Some(authorization) = authorization {
            let _ = write!(head, "Authorization: {authorization}\r\n");
        }
        match self {
            Request::Get { range_from } => {
                if let Some(from) = range_from {
                    let _ = write!(head, "Range: bytes={from}-\r\n");
                }
                head.push_str("\r\n");
                head.into_bytes()
            }
            Request::Send { headers, body, .. } => {
                for (name, value) in *headers {
                    let _ = write!(head, "{name}: {value}\r\n");
                }
                let _ = write!(head, "Content-Length: {}\r\n\r\n", body.len());
                // Head and body leave in one piece: some servers read a command with
                // a single call and take half a request for a whole one.
                let mut bytes = head.into_bytes();
                bytes.extend_from_slice(body);
                bytes
            }
        }
    }
}

/// Whether a header of the caller's can be sent as it is.
fn is_header(name: &str, value: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
        && !value.chars().any(|c| c.is_control())
}

/// Sends the request, following redirects, and reads the head of the answer.
fn fetch(
    mut url: Url,
    request: &Request<'_>,
    limits: &Limits,
    access: &Access<'_>,
) -> Result<(Head, Body<Socket>), NetError> {
    // Name, password and a trusted certificate are for the server the request is
    // made to: an address that server sends on to gets none of them.
    let first = url.clone();
    let server = first.server();
    let mut redirects = 0;
    // A server that has asked who is there before (since the app started) is told
    // with the request: asked for every piece of a song, it would be asked twice for
    // each. Only that server, and only in the way it asked.
    let ahead = |url: &Url| {
        access.account.and_then(|(user, password)| {
            auth::answer_ahead(
                &server,
                user,
                password,
                request.method(),
                &url.path,
                url.secure,
            )
        })
    };
    let mut authorization: Option<String> = ahead(&url);
    let mut asked = false;
    loop {
        let here = url.origin() == first.origin();
        let pinned = access.pinned.filter(|_| here);
        let (head, body) = exchange(
            &url,
            request,
            authorization.as_deref().filter(|_| here),
            limits,
            pinned,
        )?;
        // The server asks who is there (or no longer takes the answer to what it
        // asked before): answered once.
        if head.status == 401
            && here
            && !asked
            && let Some((user, password)) = access.account
            && let Some(answer) = auth::answer_asked(
                &server,
                &head.challenges,
                user,
                password,
                request.method(),
                &url.path,
            )
        {
            authorization = Some(answer);
            asked = true;
            continue;
        }
        if !request.follows(head.status) {
            return Ok((head, body));
        }
        if redirects >= limits.redirects {
            return Err(NetError::BadResponse("too many redirects"));
        }
        redirects += 1;
        let location = head
            .location
            .as_deref()
            .ok_or(NetError::BadResponse("a redirect without an address"))?;
        url = url.join(location)?;
        // An answer names the address it was made for: another address of the same
        // server is answered anew (and one of another server not at all).
        authorization = (url.origin() == first.origin())
            .then(|| ahead(&url))
            .flatten();
        asked = false;
    }
}

/// One request on one connection, up to the end of the answer's head.
fn exchange(
    url: &Url,
    request: &Request<'_>,
    authorization: Option<&str>,
    limits: &Limits,
    pinned: Option<&str>,
) -> Result<(Head, Body<Socket>), NetError> {
    let mut socket = connect(url, limits, pinned)?;
    socket
        .stream
        .write_all(&request.bytes(url, authorization))
        .and_then(|()| socket.stream.flush())
        .map_err(from_io)?;
    // The head is a few hundred bytes that leave the server at once.
    socket.deadline = Instant::now().checked_add(socket.read);
    let mut reader = BufReader::with_capacity(16 * 1024, socket);
    let head = read_head(&mut reader)?;
    let framing = head.framing();
    Ok((head, Body { reader, framing }))
}

/// A zero timeout is refused by the system; the shortest there is does what is meant.
fn not_zero(timeout: Duration) -> Duration {
    timeout.max(Duration::from_millis(1))
}

fn connect(url: &Url, limits: &Limits, pinned: Option<&str>) -> Result<Socket, NetError> {
    let addresses: Vec<SocketAddr> = match url.host.parse::<IpAddr>() {
        Ok(address) => vec![SocketAddr::new(address, url.port)],
        // A name is looked up by the system, which takes the time it takes (there is
        // no timeout to give it). Media servers announce themselves with addresses.
        Err(_) => (url.host.as_str(), url.port)
            .to_socket_addrs()
            .map_err(NetError::Connect)?
            .take(MAX_ADDRESSES)
            .collect(),
    };
    let timeout = not_zero(limits.connect);
    let started = Instant::now();
    let mut failure = None;
    for address in addresses {
        // The addresses share the time: three seconds for the server, not for each.
        let Some(left) = timeout
            .checked_sub(started.elapsed())
            .filter(|left| !left.is_zero())
        else {
            return Err(NetError::Timeout);
        };
        match TcpStream::connect_timeout(&address, left) {
            Ok(stream) => return Socket::new(stream, not_zero(limits.read), url, pinned),
            Err(error) => failure = Some(error),
        }
    }
    Err(match failure {
        Some(error) if is_timeout(&error) => NetError::Timeout,
        Some(error) => NetError::Connect(error),
        None => NetError::Connect(io::Error::new(
            io::ErrorKind::NotFound,
            "the name stands for no address",
        )),
    })
}

/// The connection as the answer is read from it: every read gives up after the read
/// timeout, and while a deadline is set also when that has passed.
struct Socket {
    stream: Wire,
    read: Duration,
    /// The timeout the system has been told for this socket: `read`, or what is left
    /// until the deadline when that is less.
    timeout: Duration,
    deadline: Option<Instant>,
}

/// What the bytes travel over: the connection itself, or TLS on it.
enum Wire {
    Plain(TcpStream),
    Secure(Box<tls::Secure>),
}

impl Wire {
    fn tcp(&self) -> &TcpStream {
        match self {
            Wire::Plain(stream) => stream,
            Wire::Secure(secure) => &secure.sock,
        }
    }
}

impl Read for Wire {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Wire::Plain(stream) => stream.read(buf),
            Wire::Secure(secure) => secure.read(buf),
        }
    }
}

impl io::Write for Wire {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Wire::Plain(stream) => stream.write(buf),
            Wire::Secure(secure) => secure.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Wire::Plain(stream) => stream.flush(),
            Wire::Secure(secure) => secure.flush(),
        }
    }
}

impl Socket {
    /// `stream`: a connection just made to the server of `url`. For an `https://`
    /// address TLS is spoken on it first, to the end of the handshake.
    fn new(
        stream: TcpStream,
        read: Duration,
        url: &Url,
        pinned: Option<&str>,
    ) -> Result<Socket, NetError> {
        stream
            .set_read_timeout(Some(read))
            .and_then(|()| stream.set_write_timeout(Some(read)))
            .map_err(NetError::Io)?;
        // Without it a small request can wait for the system to gather more to send.
        // Not every system offers it, and the request leaves without it too.
        let _ = stream.set_nodelay(true);
        let stream = if url.secure {
            Wire::Secure(Box::new(tls::connect(&url.host, stream, pinned)?))
        } else {
            Wire::Plain(stream)
        };
        Ok(Socket {
            stream,
            read,
            timeout: read,
            deadline: None,
        })
    }

    /// Tells the system the timeout, when it is another than the one it has.
    fn set_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        if timeout != self.timeout {
            self.stream.tcp().set_read_timeout(Some(timeout))?;
            self.timeout = timeout;
        }
        Ok(())
    }
}

impl Read for Socket {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let timeout = match self.deadline {
            Some(deadline) => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(timed_out());
                }
                left.min(self.read)
            }
            None => self.read,
        };
        self.set_timeout(timeout)?;
        loop {
            match self.stream.read(buf) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                // A socket's timeout is `WouldBlock` on Unix and `TimedOut` on Windows.
                Err(error) if is_timeout(&error) => return Err(timed_out()),
                result => return result,
            }
        }
    }
}

fn is_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}

fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "the server did not answer in time")
}

fn cut_short() -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "the connection closed before the answer was complete",
    )
}

fn malformed(what: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}

fn from_io(error: io::Error) -> NetError {
    match error.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => NetError::Timeout,
        io::ErrorKind::InvalidData => {
            NetError::BadResponse("the body is not put together as HTTP says")
        }
        _ => NetError::Io(error),
    }
}

/// What the head of an answer says, as far as it is used.
struct Head {
    status: u16,
    content_length: Option<u64>,
    chunked: bool,
    location: Option<String>,
    content_type: Option<String>,
    content_range: Option<String>,
    /// For how many seconds the server wants to be left alone (`Retry-After`, where
    /// it is a number of seconds).
    retry_after: Option<u64>,
    /// What the server offers for telling it who is there (`WWW-Authenticate`).
    challenges: Vec<String>,
}

impl Head {
    /// The body's length in bytes, when the head announces it.
    fn length(&self) -> Option<u64> {
        match self.framing() {
            Framing::Length(length) => Some(length),
            Framing::Done => Some(0),
            _ => None,
        }
    }

    /// How the end of the body is found (RFC 9112, 6.3).
    fn framing(&self) -> Framing {
        if matches!(self.status, 204 | 304) {
            Framing::Done
        } else if self.chunked {
            Framing::Chunked {
                left: 0,
                started: false,
            }
        } else if let Some(length) = self.content_length {
            Framing::Length(length)
        } else {
            Framing::UntilClose
        }
    }

    fn take_header(&mut self, line: &[u8]) -> Result<(), NetError> {
        if line.starts_with(b" ") || line.starts_with(b"\t") {
            // The old way of continuing a value on the next line. None of the headers
            // used here is ever written that way.
            return Ok(());
        }
        let colon = line
            .iter()
            .position(|&byte| byte == b':')
            .ok_or(NetError::BadResponse("a header line without a colon"))?;
        let (name, value) = line.split_at(colon);
        let name = name.trim_ascii();
        let value = String::from_utf8_lossy(&value[1..]);
        let value = value.trim();
        if name.eq_ignore_ascii_case(b"content-length") {
            let length = parse_decimal(value)
                .ok_or(NetError::BadResponse("Content-Length is not a number"))?;
            // Two lengths that differ are how a body is smuggled past a reader.
            if self.content_length.is_some_and(|known| known != length) {
                return Err(NetError::BadResponse(
                    "two different Content-Length headers",
                ));
            }
            self.content_length = Some(length);
        } else if name.eq_ignore_ascii_case(b"transfer-encoding") {
            self.chunked |= value
                .split(',')
                .any(|coding| coding.trim().eq_ignore_ascii_case("chunked"));
        } else if name.eq_ignore_ascii_case(b"location") {
            self.location.get_or_insert_with(|| value.to_string());
        } else if name.eq_ignore_ascii_case(b"retry-after") {
            self.retry_after = self.retry_after.or_else(|| value.trim().parse().ok());
        } else if name.eq_ignore_ascii_case(b"content-type") {
            self.content_type.get_or_insert_with(|| value.to_string());
        } else if name.eq_ignore_ascii_case(b"content-range") {
            self.content_range.get_or_insert_with(|| value.to_string());
        } else if name.eq_ignore_ascii_case(b"www-authenticate") && self.challenges.len() < 8 {
            self.challenges.push(value.to_string());
        }
        Ok(())
    }
}

/// A number written with digits only (no sign, no spaces).
fn parse_decimal(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// The status of `HTTP/1.x NNN …`.
fn parse_status_line(line: &[u8]) -> Option<u16> {
    let rest = line.strip_prefix(b"HTTP/1.")?;
    let (minor, rest) = rest.split_first()?;
    let rest = rest.strip_prefix(b" ")?;
    let digits = rest.get(..3)?;
    let plain = minor.is_ascii_digit()
        && digits.iter().all(u8::is_ascii_digit)
        && rest.get(3).is_none_or(|&byte| byte == b' ');
    if !plain {
        return None;
    }
    let status = digits
        .iter()
        .fold(0u16, |status, digit| status * 10 + u16::from(digit - b'0'));
    (100..600).contains(&status).then_some(status)
}

struct ContentRange {
    start: u64,
    /// The length of the whole resource, unless the server wrote `*`.
    total: Option<u64>,
}

/// Reads `bytes 100-999/1000` (or `…/*`).
fn parse_content_range(value: &str) -> Option<ContentRange> {
    let unit = value.get(..5)?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }
    let (range, total) = value[5..].trim_start().split_once('/')?;
    let (start, end) = range.trim().split_once('-')?;
    let start = parse_decimal(start.trim())?;
    let end = parse_decimal(end.trim())?;
    if end < start {
        return None;
    }
    let total = match total.trim() {
        "*" => None,
        // A total that the range itself does not fit in says nothing.
        total => Some(parse_decimal(total)?).filter(|&total| total > end),
    };
    Some(ContentRange { start, total })
}

/// Reads the head of an answer, passing over answers like `100 Continue`.
fn read_head<R: BufRead>(reader: &mut R) -> Result<Head, NetError> {
    let mut budget = MAX_HEAD_BYTES;
    let mut lines = 0;
    for _ in 0..=MAX_INTERIM_ANSWERS {
        let line = head_line(reader, &mut budget, &mut lines)?;
        let status =
            parse_status_line(&line).ok_or(NetError::BadResponse("not an HTTP status line"))?;
        let mut head = Head {
            status,
            content_length: None,
            chunked: false,
            location: None,
            content_type: None,
            content_range: None,
            retry_after: None,
            challenges: Vec::new(),
        };
        loop {
            let line = head_line(reader, &mut budget, &mut lines)?;
            if line.is_empty() {
                break;
            }
            head.take_header(&line)?;
        }
        if status >= 200 {
            return Ok(head);
        }
    }
    Err(NetError::BadResponse("too many interim answers"))
}

/// One line of the head, counted against what the whole head may take.
fn head_line<R: BufRead>(
    reader: &mut R,
    budget: &mut usize,
    lines: &mut usize,
) -> Result<Vec<u8>, NetError> {
    if *lines == MAX_HEADER_LINES {
        return Err(NetError::BadResponse("too many header lines"));
    }
    *lines += 1;
    let line = read_line(reader, *budget).map_err(|error| match error.kind() {
        io::ErrorKind::InvalidData => NetError::BadResponse("the header block is too large"),
        _ => from_io(error),
    })?;
    *budget = budget.saturating_sub(line.len() + 2);
    Ok(line)
}

/// Reads up to the next line feed and returns the line without its end (`\r\n` or
/// `\n`). A line of more than `limit` bytes is `InvalidData`; the end of the stream
/// before the end of the line is `UnexpectedEof`.
fn read_line<R: BufRead>(reader: &mut R, limit: usize) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    loop {
        let available = match reader.fill_buf() {
            Ok(available) => available,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if available.is_empty() {
            return Err(cut_short());
        }
        let (piece, taken, ended) = match available.iter().position(|&byte| byte == b'\n') {
            Some(at) => (&available[..at], at + 1, true),
            None => (available, available.len(), false),
        };
        // One byte more than the limit is let in for the '\r' taken off below.
        if line.len() + piece.len() > limit.saturating_add(1) {
            return Err(malformed("a line is too long"));
        }
        line.extend_from_slice(piece);
        reader.consume(taken);
        if ended {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.len() > limit {
                return Err(malformed("a line is too long"));
            }
            return Ok(line);
        }
    }
}

/// How the end of a body is found, and how far the reading has come.
enum Framing {
    /// `Content-Length`: this many bytes are still to come.
    Length(u64),
    /// `Transfer-Encoding: chunked`: `left` bytes of the chunk being read are still
    /// to come; `started` once the first chunk's size has been read.
    Chunked {
        left: u64,
        started: bool,
    },
    /// Neither: the body ends where the server closes the connection.
    UntilClose,
    Done,
    /// The chunks could not be followed; where the next one begins is unknown.
    Failed,
}

struct Body<R: Read> {
    reader: BufReader<R>,
    framing: Framing,
}

impl Body<Socket> {
    /// Sets the time the whole body may take (see [`BODY_TIMEOUTS`]).
    fn give_time(&mut self, read: Duration) {
        self.reader.get_mut().deadline = not_zero(read)
            .checked_mul(BODY_TIMEOUTS)
            .and_then(|time| Instant::now().checked_add(time));
    }
}

impl<R: Read> Body<R> {
    /// Reads the line that begins the next chunk and returns the chunk's size; 0
    /// after the last chunk, when the body is complete.
    fn next_chunk(&mut self, started: bool) -> io::Result<u64> {
        if started {
            // The line end that follows a chunk's bytes.
            if !read_line(&mut self.reader, 0)?.is_empty() {
                return Err(malformed("a chunk is longer than its size says"));
            }
        }
        let line = read_line(&mut self.reader, MAX_CHUNK_LINE_BYTES)?;
        // After a ';' come extensions, which nothing here has a use for.
        let digits = line
            .split(|&byte| byte == b';')
            .next()
            .unwrap_or_default()
            .trim_ascii();
        let size = std::str::from_utf8(digits)
            .ok()
            .filter(|digits| digits.len() <= 16 && digits.bytes().all(|b| b.is_ascii_hexdigit()))
            .and_then(|digits| u64::from_str_radix(digits, 16).ok())
            .ok_or_else(|| malformed("not a chunk size"))?;
        if size == 0 {
            self.skip_trailers()?;
        }
        Ok(size)
    }

    /// Passes over the header lines that may follow the last chunk.
    fn skip_trailers(&mut self) -> io::Result<()> {
        let mut budget = MAX_HEAD_BYTES;
        for _ in 0..MAX_HEADER_LINES {
            let line = match read_line(&mut self.reader, budget) {
                Ok(line) => line,
                // Some servers close right after the last chunk's size. The body is
                // complete all the same.
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
                Err(error) => return Err(error),
            };
            if line.is_empty() {
                return Ok(());
            }
            budget = budget.saturating_sub(line.len() + 2);
        }
        Err(malformed("too many trailer lines"))
    }

    /// Reads up to `limit` bytes of the body. The stream must not end here.
    fn read_some(&mut self, buf: &mut [u8], limit: u64) -> io::Result<usize> {
        let want = usize::try_from(limit).map_or(buf.len(), |limit| limit.min(buf.len()));
        match self.reader.read(&mut buf[..want])? {
            0 => Err(cut_short()),
            count => Ok(count),
        }
    }
}

impl<R: Read> Read for Body<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            match self.framing {
                Framing::Done => return Ok(0),
                Framing::Failed => return Err(malformed("the chunks could not be followed")),
                Framing::UntilClose => {
                    let count = self.reader.read(buf)?;
                    if count == 0 {
                        self.framing = Framing::Done;
                    }
                    return Ok(count);
                }
                Framing::Length(0) => self.framing = Framing::Done,
                Framing::Length(left) => {
                    let count = self.read_some(buf, left)?;
                    self.framing = Framing::Length(left - count as u64);
                    return Ok(count);
                }
                Framing::Chunked { left: 0, started } => {
                    // An error here (a timeout in the middle of the line, too) leaves
                    // the reader at an unknown place between two chunks. Reading on
                    // could take a piece of a size for the whole and end the body
                    // early without a word, so the stream is given up.
                    self.framing = match self.next_chunk(started) {
                        Ok(0) => Framing::Done,
                        Ok(size) => Framing::Chunked {
                            left: size,
                            started: true,
                        },
                        Err(error) => {
                            self.framing = Framing::Failed;
                            return Err(error);
                        }
                    };
                }
                Framing::Chunked { left, started } => {
                    let count = self.read_some(buf, left)?;
                    self.framing = Framing::Chunked {
                        left: left - count as u64,
                        started,
                    };
                    return Ok(count);
                }
            }
        }
    }
}

/// Reads a body to its end. `TooLarge` as soon as it is known to be more than `max`
/// bytes: before the first byte when the head announced the length.
fn read_whole<R: Read>(
    body: &mut Body<R>,
    announced: Option<u64>,
    max: u64,
) -> Result<Vec<u8>, NetError> {
    if announced.is_some_and(|length| length > max) {
        return Err(NetError::TooLarge);
    }
    let mut whole = Vec::new();
    let mut piece = [0u8; 8192];
    loop {
        let count = body.read(&mut piece).map_err(from_io)?;
        if count == 0 {
            return Ok(whole);
        }
        if (whole.len() + count) as u64 > max {
            return Err(NetError::TooLarge);
        }
        whole.extend_from_slice(&piece[..count]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_segments_are_resolved() {
        for (path, expected) in [
            ("/", "/"),
            ("/a/b/c", "/a/b/c"),
            ("/a/b/../c", "/a/c"),
            ("/a/./b", "/a/b"),
            ("/a/b/..", "/a/"),
            ("/a/b/.", "/a/b/"),
            ("/a/b/", "/a/b/"),
            ("/..", "/"),
            ("/../../a", "/a"),
            ("/a/../..", "/"),
            ("/a//b", "/a//b"),
        ] {
            assert_eq!(remove_dot_segments(path), expected, "{path}");
        }
    }

    #[test]
    fn status_lines() {
        assert_eq!(parse_status_line(b"HTTP/1.1 200 OK"), Some(200));
        assert_eq!(parse_status_line(b"HTTP/1.0 404 Not Found"), Some(404));
        assert_eq!(parse_status_line(b"HTTP/1.1 206"), Some(206));
        for bad in [
            &b"HTTP/2 200"[..],
            b"HTTP/1.1 2000 OK",
            b"HTTP/1.1  200 OK",
            b"HTTP/1.1 20",
            b"HTTP/1.1 099 Low",
            b"HTTP/1.1 600 High",
            b"HTTP/1.x 200 OK",
            b"ICY 200 OK",
            b"",
        ] {
            assert_eq!(parse_status_line(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn content_ranges() {
        let range = parse_content_range("bytes 100-999/1000").unwrap();
        assert_eq!((range.start, range.total), (100, Some(1000)));
        let range = parse_content_range("Bytes 5-9/*").unwrap();
        assert_eq!((range.start, range.total), (5, None));
        // A total smaller than the range is not believed.
        let range = parse_content_range("bytes 100-999/50").unwrap();
        assert_eq!((range.start, range.total), (100, None));
        for bad in [
            "",
            "bytes",
            "bytes */1000",
            "items 0-1/2",
            "bytes 9-5/20",
            "bytes -5/20",
        ] {
            assert!(parse_content_range(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn lines_are_limited() {
        let mut reader = BufReader::new(&b"abc\r\ndef\nlonger\r\nrest"[..]);
        assert_eq!(read_line(&mut reader, 3).unwrap(), b"abc");
        assert_eq!(read_line(&mut reader, 3).unwrap(), b"def");
        let error = read_line(&mut reader, 3).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        let mut reader = BufReader::new(&b"no end"[..]);
        let error = read_line(&mut reader, 100).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn request_headers_are_checked() {
        assert!(is_header("SOAPACTION", "\"urn:x#Browse\""));
        assert!(is_header("Content-Type", "text/xml; charset=\"utf-8\""));
        assert!(!is_header("", "x"));
        assert!(!is_header("Bad Name", "x"));
        assert!(!is_header("Name:", "x"));
        assert!(!is_header("Name", "x\r\nInjected: y"));
    }

    #[test]
    fn requests_are_written_as_specified() {
        let url = Url::parse("http://Example.local:8200/a%20b?c=d").unwrap();
        let get = Request::Get {
            range_from: Some(7),
        };
        assert_eq!(
            String::from_utf8(get.bytes(&url, None)).unwrap(),
            format!(
                "GET /a%20b?c=d HTTP/1.1\r\nHost: example.local:8200\r\nUser-Agent: {USER_AGENT}\r\n\
                 Accept-Encoding: identity\r\nConnection: close\r\nRange: bytes=7-\r\n\r\n"
            )
        );
        let url = Url::parse("http://[::1]/ctl").unwrap();
        let post = Request::Send {
            method: "POST",
            headers: &[("SOAPACTION", "\"x\"")],
            body: b"<a/>",
        };
        assert_eq!(
            String::from_utf8(post.bytes(&url, None)).unwrap(),
            format!(
                "POST /ctl HTTP/1.1\r\nHost: [::1]\r\nUser-Agent: {USER_AGENT}\r\n\
                 Accept-Encoding: identity\r\nConnection: close\r\nSOAPACTION: \"x\"\r\n\
                 Content-Length: 4\r\n\r\n<a/>"
            )
        );
    }
}
