//! Servers for the network tests (included with `mod net_support;`), all on 127.0.0.1
//! with ports the system picks.
//!
//! [`FakeServer`] is a small DLNA media server: a device description, a
//! ContentDirectory that browses a tree given by the test, the tree's files with and
//! without `Range`, cover pictures, and a socket that answers searches. It counts
//! what it is asked and can be told to refuse ranges or to go silent.
//!
//! [`HttpServer`] is what it is built on: it answers every request with whatever a
//! closure returns, byte for byte, for tests that need answers no real server sends.
//!
//! Neither uses the code under test, so that a mistake there is not made twice.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// How often a waiting thread looks whether the server has been stopped.
const POLL: Duration = Duration::from_millis(20);
/// A request larger than this is not one a test sends.
const MAX_REQUEST_BYTES: usize = 1 << 20;

/// A request as a handler sees it.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    /// Path and query, as sent.
    pub path: String,
    /// Names in lower case.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// The address the server listens on.
    pub server: SocketAddr,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// What a handler answers.
pub enum Reply {
    /// These bytes as they are; then the connection is closed.
    Bytes(Vec<u8>),
    /// `first` at once, then `rest` one byte at a time with a pause before each, for
    /// as long as the client stays; then the connection is closed.
    Drip {
        first: Vec<u8>,
        rest: Vec<u8>,
        every: Duration,
    },
    /// These bytes, and then silence: the connection stays open until the client
    /// leaves or the server stops.
    Partial(Vec<u8>),
    /// Nothing at all, on a connection that stays open the same way.
    Stall,
}

/// A whole answer with `Content-Length` and `Connection: close`.
pub fn response(status: u16, reason: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut head = format!("HTTP/1.1 {status} {reason}\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    ));
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

type Handler = dyn Fn(&Request) -> Reply + Send + Sync;

/// An HTTP server that answers every request with what its handler returns. One
/// thread per connection, so a connection that hangs does not hold up the others.
pub struct HttpServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
    workers: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl HttpServer {
    pub fn start(handler: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> HttpServer {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let addr = listener.local_addr().expect("local address");
        let stop = Arc::new(AtomicBool::new(false));
        let workers = Arc::new(Mutex::new(Vec::new()));
        let handler: Arc<Handler> = Arc::new(handler);
        let accept = {
            let (stop, workers) = (stop.clone(), workers.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    let (stop, handler) = (stop.clone(), handler.clone());
                    let worker = std::thread::spawn(move || serve(stream, addr, &*handler, &stop));
                    workers.lock().unwrap().push(worker);
                }
            })
        };
        HttpServer {
            addr,
            stop,
            accept: Some(accept),
            workers,
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The address of `path` ("/desc.xml") on this server.
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    /// Ends the server's threads. Connections that are open are closed.
    pub fn stop(&mut self) {
        let Some(accept) = self.accept.take() else {
            return;
        };
        self.stop.store(true, Ordering::Release);
        // The accepting thread waits for a connection; this one lets it look at the
        // flag.
        let _ = TcpStream::connect(self.addr);
        let _ = accept.join();
        let workers = std::mem::take(&mut *self.workers.lock().unwrap());
        for worker in workers {
            let _ = worker.join();
        }
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn is_timeout(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::Interrupted
    )
}

/// One connection: reads a request, sends the handler's answer, closes.
fn serve(mut stream: TcpStream, server: SocketAddr, handler: &Handler, stop: &AtomicBool) {
    // Short timeouts, so that the thread notices the server being stopped whatever
    // the client does.
    let _ = stream.set_read_timeout(Some(POLL));
    let _ = stream.set_write_timeout(Some(POLL));
    let _ = stream.set_nodelay(true);
    let Some(request) = read_request(&mut stream, server, stop) else {
        return;
    };
    match handler(&request) {
        Reply::Bytes(bytes) => {
            send(&mut stream, &bytes, stop);
            let _ = stream.shutdown(Shutdown::Write);
        }
        Reply::Drip { first, rest, every } => {
            if !send(&mut stream, &first, stop) {
                return;
            }
            for byte in rest {
                std::thread::sleep(every);
                if !send(&mut stream, &[byte], stop) {
                    return;
                }
            }
            let _ = stream.shutdown(Shutdown::Write);
        }
        Reply::Partial(bytes) => {
            send(&mut stream, &bytes, stop);
        }
        Reply::Stall => {}
    }
    // The connection is closed once the client has closed its side (or the server
    // stops). Closing first could, with something of the client's still unread, be
    // reported to it as a reset in place of the last bytes of the answer.
    let mut rest = [0u8; 1024];
    while !stop.load(Ordering::Acquire) {
        match stream.read(&mut rest) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) if is_timeout(&error) => {}
            Err(_) => break,
        }
    }
}

/// Sends `bytes`; false when the client has gone or the server is stopped.
fn send(stream: &mut TcpStream, bytes: &[u8], stop: &AtomicBool) -> bool {
    let mut sent = 0;
    while sent < bytes.len() {
        if stop.load(Ordering::Acquire) {
            return false;
        }
        match stream.write(&bytes[sent..]) {
            Ok(0) => return false,
            Ok(count) => sent += count,
            Err(error) if is_timeout(&error) => {}
            Err(_) => return false,
        }
    }
    true
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn read_request(stream: &mut TcpStream, server: SocketAddr, stop: &AtomicBool) -> Option<Request> {
    let mut bytes = Vec::new();
    let mut piece = [0u8; 4096];
    let mut read_more = |bytes: &mut Vec<u8>| -> bool {
        loop {
            if stop.load(Ordering::Acquire) || bytes.len() > MAX_REQUEST_BYTES {
                return false;
            }
            match stream.read(&mut piece) {
                Ok(0) => return false,
                Ok(count) => {
                    bytes.extend_from_slice(&piece[..count]);
                    return true;
                }
                Err(error) if is_timeout(&error) => {}
                Err(_) => return false,
            }
        }
    };
    let head_end = loop {
        if let Some(at) = find(&bytes, b"\r\n\r\n") {
            break at + 4;
        }
        if !read_more(&mut bytes) {
            return None;
        }
    };
    let head = String::from_utf8_lossy(&bytes[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split(' ');
    let method = request_line.next()?.to_string();
    let path = request_line.next()?.to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    while bytes.len() < head_end + length {
        if !read_more(&mut bytes) {
            return None;
        }
    }
    Some(Request {
        method,
        path,
        headers,
        body: bytes[head_end..head_end + length].to_vec(),
        server,
    })
}

/// A song of the fake server's tree.
#[derive(Debug, Clone)]
pub struct Track {
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_ms: Option<u64>,
    /// What the server says the file is ("audio/flac").
    pub mime: String,
    /// The file.
    pub body: Vec<u8>,
    /// The cover picture, served at `/art/<n>`.
    pub art: Option<Vec<u8>>,
}

impl Track {
    pub fn new(title: &str, mime: &str, body: Vec<u8>) -> Track {
        Track {
            title: title.to_string(),
            artist: None,
            album: None,
            duration_ms: None,
            mime: mime.to_string(),
            body,
            art: None,
        }
    }

    pub fn artist(mut self, artist: &str) -> Track {
        self.artist = Some(artist.to_string());
        self
    }

    pub fn album(mut self, album: &str) -> Track {
        self.album = Some(album.to_string());
        self
    }

    pub fn duration_ms(mut self, duration_ms: u64) -> Track {
        self.duration_ms = Some(duration_ms);
        self
    }

    pub fn art(mut self, picture: Vec<u8>) -> Track {
        self.art = Some(picture);
        self
    }
}

/// What the fake server holds: folders and songs.
#[derive(Debug, Clone)]
pub enum Node {
    Folder(String, Vec<Node>),
    Track(Track),
}

/// A folder of the tree.
pub fn folder(title: &str, children: Vec<Node>) -> Node {
    Node::Folder(title.to_string(), children)
}

impl From<Track> for Node {
    fn from(track: Track) -> Node {
        Node::Track(track)
    }
}

enum Kind {
    Folder {
        children: Vec<usize>,
    },
    /// The song with this number (see [`FakeServer::media_url`]).
    Track(usize),
}

struct Object {
    id: String,
    parent: String,
    title: String,
    kind: Kind,
}

struct State {
    /// Every folder and song; the root (id "0") is the first.
    objects: Vec<Object>,
    tracks: Vec<Track>,
    requests: AtomicUsize,
    browse_requests: AtomicUsize,
    media_requests: AtomicUsize,
    range_requests: AtomicUsize,
    offset_requests: AtomicUsize,
    refuse_ranges: AtomicBool,
    stall: AtomicBool,
}

/// A DLNA media server for tests.
///
/// Ids: the root is "0", folders are "f1", "f2", … and songs "t0", "t1", …, both
/// numbered in the order the tree lists them, a folder before what is in it. A song's
/// number is also the `n` of its file's address (`/media/<n>`) and of its cover's
/// (`/art/<n>`).
pub struct FakeServer {
    http: HttpServer,
    state: Arc<State>,
    ssdp_addr: SocketAddr,
    ssdp: Option<JoinHandle<()>>,
}

impl FakeServer {
    pub const NAME: &'static str = "Fake Media Server";
    pub const UDN: &'static str = "uuid:0e0e0e0e-fa4e-4000-8000-0123456789ab";

    /// Starts a server that holds `tree` (the children of its root).
    pub fn start(tree: Vec<Node>) -> FakeServer {
        let mut objects = vec![Object {
            id: "0".to_string(),
            parent: "-1".to_string(),
            title: "root".to_string(),
            kind: Kind::Folder {
                children: Vec::new(),
            },
        }];
        let mut tracks = Vec::new();
        add_children(&mut objects, &mut tracks, 0, tree);
        let state = Arc::new(State {
            objects,
            tracks,
            requests: AtomicUsize::new(0),
            browse_requests: AtomicUsize::new(0),
            media_requests: AtomicUsize::new(0),
            range_requests: AtomicUsize::new(0),
            offset_requests: AtomicUsize::new(0),
            refuse_ranges: AtomicBool::new(false),
            stall: AtomicBool::new(false),
        });
        let http = {
            let state = state.clone();
            HttpServer::start(move |request| state.answer(request))
        };
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let ssdp_addr = socket.local_addr().expect("local address");
        let ssdp = {
            let stop = http.stop.clone();
            let location = http.url("/desc.xml");
            std::thread::spawn(move || answer_searches(&socket, &location, &stop))
        };
        FakeServer {
            http,
            state,
            ssdp_addr,
            ssdp: Some(ssdp),
        }
    }

    /// The address of the device description.
    pub fn location(&self) -> String {
        self.http.url("/desc.xml")
    }

    /// Where the server answers searches (for `ssdp::search_at`).
    pub fn ssdp_addr(&self) -> SocketAddr {
        self.ssdp_addr
    }

    /// The address of song `n`'s file.
    pub fn media_url(&self, n: usize) -> String {
        self.http.url(&format!("/media/{n}"))
    }

    /// The address of song `n`'s cover picture.
    pub fn art_url(&self, n: usize) -> String {
        self.http.url(&format!("/art/{n}"))
    }

    /// The address of `path` on the server.
    pub fn url(&self, path: &str) -> String {
        self.http.url(path)
    }

    /// How many HTTP requests have arrived, of every kind.
    pub fn requests(&self) -> usize {
        self.state.requests.load(Ordering::Acquire)
    }

    /// How many `Browse` commands have arrived.
    pub fn browse_requests(&self) -> usize {
        self.state.browse_requests.load(Ordering::Acquire)
    }

    /// How many requests for a song's file have arrived.
    pub fn media_requests(&self) -> usize {
        self.state.media_requests.load(Ordering::Acquire)
    }

    /// How many of them carried a `Range` header (`bytes=0-` counts).
    pub fn range_requests(&self) -> usize {
        self.state.range_requests.load(Ordering::Acquire)
    }

    /// How many of them asked for a range that begins after the first byte: what a
    /// seek or a resumed download sends.
    pub fn offset_requests(&self) -> usize {
        self.state.offset_requests.load(Ordering::Acquire)
    }

    /// While set, `Range` headers are ignored: every file is sent whole, with 200.
    pub fn set_refuse_ranges(&self, refuse: bool) {
        self.state.refuse_ranges.store(refuse, Ordering::Release);
    }

    /// While set, requests are accepted and never answered.
    pub fn set_stall(&self, stall: bool) {
        self.state.stall.store(stall, Ordering::Release);
    }

    /// Ends the server's threads.
    pub fn stop(&mut self) {
        self.http.stop();
        if let Some(ssdp) = self.ssdp.take() {
            let _ = ssdp.join();
        }
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn add_children(
    objects: &mut Vec<Object>,
    tracks: &mut Vec<Track>,
    parent: usize,
    nodes: Vec<Node>,
) {
    for node in nodes {
        let index = objects.len();
        let parent_id = objects[parent].id.clone();
        if let Kind::Folder { children } = &mut objects[parent].kind {
            children.push(index);
        }
        match node {
            Node::Folder(title, children) => {
                let folders = objects
                    .iter()
                    .filter(|object| matches!(object.kind, Kind::Folder { .. }))
                    .count();
                objects.push(Object {
                    id: format!("f{folders}"),
                    parent: parent_id,
                    title,
                    kind: Kind::Folder {
                        children: Vec::new(),
                    },
                });
                add_children(objects, tracks, index, children);
            }
            Node::Track(track) => {
                objects.push(Object {
                    id: format!("t{}", tracks.len()),
                    parent: parent_id,
                    title: track.title.clone(),
                    kind: Kind::Track(tracks.len()),
                });
                tracks.push(track);
            }
        }
    }
}

/// Answers every `M-SEARCH` that arrives with the server's location.
fn answer_searches(socket: &UdpSocket, location: &str, stop: &AtomicBool) {
    let _ = socket.set_read_timeout(Some(POLL));
    let reply = format!(
        "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=1800\r\nEXT:\r\nLOCATION: {location}\r\n\
         SERVER: Test/1.0 UPnP/1.0 FakeServer/1.0\r\n\
         ST: urn:schemas-upnp-org:device:MediaServer:1\r\n\
         USN: {}::urn:schemas-upnp-org:device:MediaServer:1\r\n\r\n",
        FakeServer::UDN
    );
    let mut datagram = [0u8; 2048];
    while !stop.load(Ordering::Acquire) {
        if let Ok((len, from)) = socket.recv_from(&mut datagram)
            && datagram[..len].starts_with(b"M-SEARCH ")
        {
            let _ = socket.send_to(reply.as_bytes(), from);
        }
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// What stands between `<name>` and `</name>` in `text`.
fn element<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let start = text.find(&format!("<{name}>"))? + name.len() + 2;
    let end = text[start..].find(&format!("</{name}>"))?;
    Some(&text[start..start + end])
}

/// "0:03:25.000"
fn duration(ms: u64) -> String {
    format!(
        "{}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

const XML: (&str, &str) = ("Content-Type", "text/xml; charset=\"utf-8\"");

impl State {
    fn answer(&self, request: &Request) -> Reply {
        if self.stall.load(Ordering::Acquire) {
            return Reply::Stall;
        }
        self.requests.fetch_add(1, Ordering::AcqRel);
        let path = request.path.split('?').next().unwrap_or_default();
        let number = |prefix: &str| path.strip_prefix(prefix)?.parse::<usize>().ok();
        Reply::Bytes(match (request.method.as_str(), path) {
            ("GET", "/desc.xml") => response(200, "OK", &[XML], description().as_bytes()),
            ("POST", "/ctl") => self.browse(request),
            ("GET", _) if path.starts_with("/media/") => match number("/media/") {
                Some(n) if n < self.tracks.len() => self.media(request, &self.tracks[n]),
                _ => not_found(),
            },
            ("GET", _) if path.starts_with("/art/") => {
                match number("/art/").and_then(|n| self.tracks.get(n)?.art.as_ref()) {
                    Some(picture) => response(200, "OK", &[("Content-Type", "image/png")], picture),
                    None => not_found(),
                }
            }
            _ => not_found(),
        })
    }

    fn browse(&self, request: &Request) -> Vec<u8> {
        self.browse_requests.fetch_add(1, Ordering::AcqRel);
        let command = String::from_utf8_lossy(&request.body);
        let id = unescape(element(&command, "ObjectID").unwrap_or_default());
        let number = |name| element(&command, name).and_then(|text| text.parse::<usize>().ok());
        let start = number("StartingIndex").unwrap_or(0);
        let count = match number("RequestedCount") {
            // As UPnP says: 0 asks for all of them.
            None | Some(0) => usize::MAX,
            Some(count) => count,
        };
        let folder = self.objects.iter().find(|object| object.id == id);
        let Some(Kind::Folder { children }) = folder.map(|object| &object.kind) else {
            return fault(701, "No such object");
        };
        let mut didl = String::from(
            "<DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\" \
             xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
             xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\">",
        );
        let page: Vec<&Object> = children
            .iter()
            .skip(start)
            .take(count)
            .map(|&index| &self.objects[index])
            .collect();
        for object in &page {
            didl.push_str(&self.entry(object, request.server));
        }
        didl.push_str("</DIDL-Lite>");
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
             <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
             s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body>\
             <u:BrowseResponse xmlns:u=\"urn:schemas-upnp-org:service:ContentDirectory:1\">\
             <Result>{}</Result><NumberReturned>{}</NumberReturned>\
             <TotalMatches>{}</TotalMatches><UpdateID>1</UpdateID>\
             </u:BrowseResponse></s:Body></s:Envelope>",
            escape(&didl),
            page.len(),
            children.len()
        );
        response(200, "OK", &[XML, ("EXT", "")], body.as_bytes())
    }

    /// One folder or song as DIDL-Lite.
    fn entry(&self, object: &Object, server: SocketAddr) -> String {
        let (id, parent) = (escape(&object.id), escape(&object.parent));
        let title = escape(&object.title);
        match &object.kind {
            Kind::Folder { children } => format!(
                "<container id=\"{id}\" parentID=\"{parent}\" restricted=\"1\" \
                 childCount=\"{}\"><dc:title>{title}</dc:title>\
                 <upnp:class>object.container.storageFolder</upnp:class></container>",
                children.len()
            ),
            Kind::Track(n) => {
                let track = &self.tracks[*n];
                let mut fields = String::new();
                if let Some(artist) = &track.artist {
                    let artist = escape(artist);
                    fields.push_str(&format!(
                        "<upnp:artist>{artist}</upnp:artist><dc:creator>{artist}</dc:creator>"
                    ));
                }
                if let Some(album) = &track.album {
                    fields.push_str(&format!("<upnp:album>{}</upnp:album>", escape(album)));
                }
                if track.art.is_some() {
                    fields.push_str(&format!(
                        "<upnp:albumArtURI>http://{server}/art/{n}</upnp:albumArtURI>"
                    ));
                }
                let length = track
                    .duration_ms
                    .map(|ms| format!(" duration=\"{}\"", duration(ms)))
                    .unwrap_or_default();
                format!(
                    "<item id=\"{id}\" parentID=\"{parent}\" restricted=\"1\">\
                     <dc:title>{title}</dc:title>\
                     <upnp:class>object.item.audioItem.musicTrack</upnp:class>{fields}\
                     <res protocolInfo=\"http-get:*:{}:DLNA.ORG_OP=01;DLNA.ORG_CI=0\" \
                     size=\"{}\"{length}>http://{server}/media/{n}</res></item>",
                    escape(&track.mime),
                    track.body.len()
                )
            }
        }
    }

    fn media(&self, request: &Request, track: &Track) -> Vec<u8> {
        self.media_requests.fetch_add(1, Ordering::AcqRel);
        let len = track.body.len();
        let range = request.header("range").map(|value| parse_range(value, len));
        if let Some(range) = &range {
            self.range_requests.fetch_add(1, Ordering::AcqRel);
            if !matches!(range, Range::Bytes(0, _)) {
                self.offset_requests.fetch_add(1, Ordering::AcqRel);
            }
        }
        let kind = ("Content-Type", track.mime.as_str());
        if self.refuse_ranges.load(Ordering::Acquire) {
            return response(200, "OK", &[kind], &track.body);
        }
        match range {
            Some(Range::Bytes(start, end)) => response(
                206,
                "Partial Content",
                &[
                    kind,
                    ("Accept-Ranges", "bytes"),
                    ("Content-Range", &format!("bytes {start}-{end}/{len}")),
                ],
                &track.body[start..=end],
            ),
            Some(Range::Outside) => response(
                416,
                "Range Not Satisfiable",
                &[("Content-Range", &format!("bytes */{len}"))],
                b"",
            ),
            Some(Range::Unreadable) | None => {
                response(200, "OK", &[kind, ("Accept-Ranges", "bytes")], &track.body)
            }
        }
    }
}

enum Range {
    /// First and last byte, both inside the file.
    Bytes(usize, usize),
    /// Nothing of the file is in the range.
    Outside,
    /// Not a range this server understands; such a header is ignored.
    Unreadable,
}

/// Reads `bytes=a-b`, `bytes=a-` and `bytes=-n` for a file of `len` bytes.
fn parse_range(value: &str, len: usize) -> Range {
    let Some((first, last)) = value
        .trim()
        .strip_prefix("bytes=")
        .and_then(|range| range.split_once('-'))
    else {
        return Range::Unreadable;
    };
    let (start, end) = match (first.trim().parse::<usize>(), last.trim().parse::<usize>()) {
        (Ok(start), Ok(end)) => (start, end.min(len.saturating_sub(1))),
        (Ok(start), Err(_)) if last.trim().is_empty() => (start, len.saturating_sub(1)),
        (Err(_), Ok(suffix)) if first.trim().is_empty() => {
            (len.saturating_sub(suffix), len.saturating_sub(1))
        }
        _ => return Range::Unreadable,
    };
    if start >= len || end < start {
        Range::Outside
    } else {
        Range::Bytes(start, end)
    }
}

fn not_found() -> Vec<u8> {
    response(
        404,
        "Not Found",
        &[("Content-Type", "text/plain")],
        b"not found",
    )
}

/// A UPnP error as servers send it: status 500 with a SOAP fault.
fn fault(code: u32, description: &str) -> Vec<u8> {
    let body = format!(
        "<?xml version=\"1.0\"?>\
         <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><s:Fault>\
         <faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring><detail>\
         <UPnPError xmlns=\"urn:schemas-upnp-org:control-1-0\"><errorCode>{code}</errorCode>\
         <errorDescription>{description}</errorDescription></UPnPError>\
         </detail></s:Fault></s:Body></s:Envelope>"
    );
    response(500, "Internal Server Error", &[XML], body.as_bytes())
}

/// The device description: a media server with the two services every one has.
fn description() -> String {
    format!(
        "<?xml version=\"1.0\"?>\n\
         <root xmlns=\"urn:schemas-upnp-org:device-1-0\">\n\
         <specVersion><major>1</major><minor>0</minor></specVersion>\n\
         <device>\n\
         <deviceType>urn:schemas-upnp-org:device:MediaServer:1</deviceType>\n\
         <friendlyName>{}</friendlyName>\n\
         <manufacturer>OnEum tests</manufacturer>\n\
         <modelName>FakeServer</modelName>\n\
         <UDN>{}</UDN>\n\
         <serviceList>\n\
         <service>\n\
         <serviceType>urn:schemas-upnp-org:service:ConnectionManager:1</serviceType>\n\
         <serviceId>urn:upnp-org:serviceId:ConnectionManager</serviceId>\n\
         <SCPDURL>/cm.xml</SCPDURL><controlURL>/cm</controlURL>\
         <eventSubURL>/cm-events</eventSubURL>\n\
         </service>\n\
         <service>\n\
         <serviceType>urn:schemas-upnp-org:service:ContentDirectory:1</serviceType>\n\
         <serviceId>urn:upnp-org:serviceId:ContentDirectory</serviceId>\n\
         <SCPDURL>/cd.xml</SCPDURL><controlURL>/ctl</controlURL>\
         <eventSubURL>/events</eventSubURL>\n\
         </service>\n\
         </serviceList>\n\
         </device>\n\
         </root>\n",
        FakeServer::NAME,
        FakeServer::UDN
    )
}
