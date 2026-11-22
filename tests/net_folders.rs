//! Network folders (a share of a NAS over WebDAV) against servers that run here: one
//! that asks for a name and a password in the two ways there are, and one that speaks
//! TLS with a certificate it made itself. The servers check what they are sent with
//! code of their own, not with the app's.
mod net_support;

use md5::{Digest as _, Md5};
use net_support::{HttpServer, Reply, Request, response};
use oneum::audio::decoder::TrackDecoder;
use oneum::audio::remote::RemoteFile;
use oneum::audio::source::Budget;
use oneum::net::folders::{self, Folder};
use oneum::net::http::{self, Access, Limits};
use oneum::net::{NetError, dav, tls};
use oneum::platform::secrets::{self, Secret};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const USER: &str = "listener";
const WORD: &str = "open sesame 열려라";
const REALM: &str = "music share";
const NONCE: &str = "9f8e7d6c5b4a";

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read(path).expect("fixture")
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Asks {
    Nobody,
    Basic,
    Digest,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn md5(text: &str) -> String {
    hex(&Md5::digest(text.as_bytes()))
}

/// A field of an `Authorization: Digest …` header.
fn field<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(',').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key.trim_start_matches("Digest ").trim() == name).then(|| value.trim_matches('"'))
    })
}

/// Whether the request comes from who it should.
fn allowed(request: &Request, asks: Asks) -> bool {
    let given = request.header("authorization").unwrap_or("");
    match asks {
        Asks::Nobody => true,
        // base64("listener:open sesame 열려라")
        Asks::Basic => given == "Basic bGlzdGVuZXI6b3BlbiBzZXNhbWUg7Je066Ck6528",
        Asks::Digest => answers(request, NONCE),
    }
}

/// Whether the request answers a Digest challenge that gave `nonce`.
fn answers(request: &Request, nonce: &str) -> bool {
    let given = request.header("authorization").unwrap_or("");
    let get = |name: &str| field(given, name).unwrap_or("");
    let ha1 = md5(&format!("{USER}:{REALM}:{WORD}"));
    let ha2 = md5(&format!("{}:{}", request.method, get("uri")));
    let expected = md5(&format!(
        "{ha1}:{nonce}:{}:{}:auth:{ha2}",
        get("nc"),
        get("cnonce")
    ));
    given.starts_with("Digest ")
        && get("username") == USER
        && get("nonce") == nonce
        && get("uri") == request.path
        && get("response") == expected
}

fn refusal(asks: Asks) -> Vec<u8> {
    let challenge = match asks {
        Asks::Digest => {
            format!("Digest realm=\"{REALM}\", qop=\"auth\", nonce=\"{NONCE}\", algorithm=MD5")
        }
        _ => format!("Basic realm=\"{REALM}\""),
    };
    response(
        401,
        "Unauthorized",
        &[("WWW-Authenticate", &challenge)],
        b"",
    )
}

/// The share: `/dav/` holds a folder and a song, the folder another song.
fn share(request: &Request, asks: Asks) -> Vec<u8> {
    if !allowed(request, asks) {
        return refusal(asks);
    }
    let entry = |href: &str, folder: bool, size: usize| {
        let kind = if folder {
            "<D:resourcetype><D:collection/></D:resourcetype>".to_string()
        } else {
            format!("<D:resourcetype/><D:getcontentlength>{size}</D:getcontentlength>")
        };
        format!(
            "<D:response><D:href>{href}</D:href><D:propstat><D:prop>{kind}</D:prop>\
             <D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"
        )
    };
    let listing = |entries: String| {
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
             <D:multistatus xmlns:D=\"DAV:\">{entries}</D:multistatus>"
        );
        response(
            207,
            "Multi-Status",
            &[("Content-Type", "application/xml; charset=utf-8")],
            body.as_bytes(),
        )
    };
    let flac = fixture("gap_a.flac");
    let wv = fixture("gap_a.wv");
    match (request.method.as_str(), request.path.as_str()) {
        ("PROPFIND", "/dav/") => listing(
            [
                entry("/dav/", true, 0),
                entry("/dav/song%2010.flac", false, flac.len()),
                entry("/dav/Album%20%ED%95%98%EB%82%98/", true, 0),
                entry("/dav/song%202.flac", false, flac.len()),
            ]
            .concat(),
        ),
        ("PROPFIND", "/dav/Album%20%ED%95%98%EB%82%98/") => listing(
            [
                entry("/dav/Album%20%ED%95%98%EB%82%98/", true, 0),
                entry(
                    "/dav/Album%20%ED%95%98%EB%82%98/01%20one.wv",
                    false,
                    wv.len(),
                ),
            ]
            .concat(),
        ),
        ("GET", path) => {
            let body = match path {
                "/dav/song%2010.flac" | "/dav/song%202.flac" => flac,
                "/dav/Album%20%ED%95%98%EB%82%98/01%20one.wv" => wv,
                _ => return response(404, "Not Found", &[], b""),
            };
            let from = request
                .header("range")
                .and_then(|range| range.strip_prefix("bytes="))
                .and_then(|range| range.strip_suffix('-'))
                .and_then(|from| from.parse::<usize>().ok());
            match from {
                Some(from) if from < body.len() => response(
                    206,
                    "Partial Content",
                    &[(
                        "Content-Range",
                        &format!("bytes {from}-{}/{}", body.len() - 1, body.len()),
                    )],
                    &body[from..],
                ),
                _ => response(200, "OK", &[], &body),
            }
        }
        _ => response(404, "Not Found", &[], b""),
    }
}

/// A share on a server that writes down every request it gets.
fn serve(asks: Asks) -> (HttpServer, Arc<Mutex<Vec<Request>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let server = HttpServer::start(move |request| {
        log.lock().unwrap().push(request.clone());
        Reply::Bytes(share(request, asks))
    });
    (server, seen)
}

fn account() -> Access<'static> {
    Access {
        account: Some((USER, WORD)),
        pinned: None,
        ..Access::default()
    }
}

fn names(entries: &[dav::Entry]) -> Vec<(&str, bool)> {
    entries
        .iter()
        .map(|entry| (entry.name.as_str(), entry.folder))
        .collect()
}

#[test]
fn a_folder_is_listed_and_read_by_who_gives_the_right_name_and_password() {
    // (Basic over a connection that is not encrypted is never answered: see
    // `basic_over_plain_http_is_never_answered`; over TLS, see the tests below.)
    for asks in [Asks::Digest] {
        let (server, seen) = serve(asks);
        let root = server.url("/dav/");
        // Nobody, and the wrong password: refused, and the refusal says so.
        for access in [
            Access::default(),
            Access {
                account: Some((USER, "wrong")),
                pinned: None,
                ..Access::default()
            },
        ] {
            assert!(matches!(
                dav::list(&root, &access),
                Err(NetError::Status(401))
            ));
        }
        let entries = dav::list(&root, &account()).expect("the folder is listed");
        assert_eq!(
            names(&entries),
            [
                ("Album 하나", true),
                ("song 2.flac", false),
                ("song 10.flac", false)
            ],
            "folders first, names by their numbers"
        );
        assert_eq!(entries[1].size, Some(fixture("gap_a.flac").len() as u64));
        let inner = dav::list(&entries[0].url, &account()).expect("the folder inside");
        assert_eq!(names(&inner), [("01 one.wv", false)]);

        // A file from a place in its middle.
        let mut stream = http::open_web(&inner[0].url, 1_000, &Limits::new(u64::MAX), &account())
            .expect("the file opens");
        assert!(stream.ranged());
        let mut rest = Vec::new();
        stream.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, fixture("gap_a.wv")[1_000..]);

        // The name is given only after the server asked, and with Digest the
        // password never travels (neither as it is nor in base64).
        let seen = seen.lock().unwrap();
        let first = &seen[0];
        assert_eq!(first.header("authorization"), None);
        for request in seen.iter() {
            let given = request.header("authorization").unwrap_or("");
            assert!(
                !given.contains(WORD) && !given.contains("bGlzdGVuZXI6"),
                "{given}"
            );
        }
        // Once the server has asked, it is answered with the request: the two
        // listings and the file took one request each, not two. (Before them: one
        // request as nobody, and two with the wrong password, the second of which
        // was the answer to the server's question.)
        let given: Vec<Option<&str>> = seen.iter().map(|r| r.header("authorization")).collect();
        assert_eq!(given.len(), 6, "{given:?}");
        assert!(given[..2].iter().all(Option::is_none));
        assert!(given[2..].iter().all(Option::is_some));
        // Each answer to the same question is counted, as a server may demand.
        let counts: Vec<&str> = given[2..]
            .iter()
            .map(|given| field(given.unwrap(), "nc").unwrap())
            .collect();
        assert_eq!(counts, ["00000001", "00000002", "00000003", "00000004"]);
    }
}

/// A server at an `http://` address that takes nothing but the password itself
/// (Basic) is never given it: not the right one, not a wrong one. Whoever asks is
/// told so (and the listener to use the server's `https://` address), with no request
/// made twice.
#[test]
fn basic_over_plain_http_is_never_answered() {
    let (server, seen) = serve(Asks::Basic);
    let root = server.url("/dav/");
    let wrong = Access {
        account: Some((USER, "wrong")),
        fresh: true,
        ..Access::default()
    };
    for access in [account(), wrong, Access::default()] {
        assert!(matches!(
            dav::list(&root, &access),
            Err(NetError::PasswordInTheOpen)
        ));
    }
    assert!(matches!(
        http::open_web(
            &server.url("/dav/song%202.flac"),
            0,
            &Limits::new(u64::MAX),
            &account()
        ),
        Err(NetError::PasswordInTheOpen)
    ));
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 4, "one request each");
    assert!(
        seen.iter().all(|r| r.header("authorization").is_none()),
        "nothing of the password was sent"
    );
}

/// A name and password the server refuses are given to it once, not with every
/// request after: a NAS counts failed sign-ins, and a queue of songs is many
/// requests. The listener trying them anew, or another password, is not held back.
#[test]
fn a_refused_password_is_not_given_again_and_again() {
    for asks in [Asks::Digest] {
        let (server, seen) = serve(asks);
        let root = server.url("/dav/");
        let wrong = |fresh: bool| Access {
            account: Some((USER, "not it")),
            fresh,
            ..Access::default()
        };
        for _ in 0..5 {
            assert!(matches!(
                dav::list(&root, &wrong(false)),
                Err(NetError::Status(401))
            ));
        }
        let given = || -> Vec<bool> {
            seen.lock()
                .unwrap()
                .iter()
                .map(|request| request.header("authorization").is_some())
                .collect()
        };
        // Asked and answered once; the four requests after it are made as nobody.
        assert_eq!(
            given(),
            [false, true, false, false, false, false],
            "{asks:?}"
        );
        // Typed again in the folder's form: tried once more, and then left again.
        assert!(matches!(
            dav::list(&root, &wrong(true)),
            Err(NetError::Status(401))
        ));
        assert!(matches!(
            dav::list(&root, &wrong(false)),
            Err(NetError::Status(401))
        ));
        let after = given();
        assert_eq!(
            after[6..].iter().filter(|given| **given).count(),
            1,
            "{asks:?}: {after:?}"
        );
        // The right password is another matter.
        assert!(dav::list(&root, &account()).is_ok(), "{asks:?}");
    }
}

/// A server that sends a request on to another of its own addresses is answered
/// there for that address (an answer names the address it was made for), and not
/// asked twice.
#[test]
fn an_answer_follows_a_redirect_inside_the_server_with_the_new_address() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut server = {
        let seen = seen.clone();
        HttpServer::start(move |request| {
            let taken = allowed(request, Asks::Digest);
            seen.lock().unwrap().push((request.path.clone(), taken));
            if !taken {
                return Reply::Bytes(refusal(Asks::Digest));
            }
            if request.path == "/old/" {
                let moved = [("Location", "/dav/")];
                return Reply::Bytes(response(307, "Temporary Redirect", &moved, b""));
            }
            Reply::Bytes(share(request, Asks::Digest))
        })
    };
    let entries = dav::list(&server.url("/old/"), &account()).expect("the folder is listed");
    assert_eq!(entries.len(), 3);
    assert_eq!(
        *seen.lock().unwrap(),
        [
            ("/old/".to_string(), false),
            ("/old/".to_string(), true),
            ("/dav/".to_string(), true)
        ]
    );
    server.stop();
}

/// A server whose question changes after every answer it took (as servers do after a
/// while): the answer to the old question is refused, the new one is answered, and
/// the request goes through all the same.
#[test]
fn a_server_that_asks_anew_is_answered_anew() {
    let turn = Arc::new(Mutex::new(0u32));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut server = {
        let (turn, seen) = (turn.clone(), seen.clone());
        HttpServer::start(move |request| {
            let mut turn = turn.lock().unwrap();
            let nonce = format!("nonce-{}", *turn);
            let taken = answers(request, &nonce);
            seen.lock().unwrap().push(taken);
            if !taken {
                let challenge = format!(
                    "Digest realm=\"{REALM}\", qop=\"auth\", nonce=\"{nonce}\", algorithm=MD5, stale=true"
                );
                let refusal = response(
                    401,
                    "Unauthorized",
                    &[("WWW-Authenticate", &challenge)],
                    b"",
                );
                return Reply::Bytes(refusal);
            }
            *turn += 1;
            Reply::Bytes(share(request, Asks::Nobody))
        })
    };
    let root = server.url("/dav/");
    for _ in 0..3 {
        let entries = dav::list(&root, &account()).expect("the folder is listed");
        assert_eq!(entries.len(), 3);
    }
    // Asked and answered, three times over; never more than one question a request.
    assert_eq!(
        *seen.lock().unwrap(),
        [false, true, false, true, false, true]
    );
    // The wrong password is still the wrong password: one question, one answer, no.
    let wrong = Access {
        account: Some((USER, "wrong")),
        pinned: None,
        ..Access::default()
    };
    assert!(matches!(
        dav::list(&root, &wrong),
        Err(NetError::Status(401))
    ));
    assert_eq!(seen.lock().unwrap().len(), 8);
    server.stop();
}

/// A server that sends the request on to another one: that one gets neither the name
/// nor the password.
#[test]
fn name_and_password_do_not_follow_to_another_server() {
    let (other, other_seen) = serve(Asks::Nobody);
    let elsewhere = other.url("/dav/");
    let first = HttpServer::start(move |request| {
        if !allowed(request, Asks::Digest) {
            return Reply::Bytes(refusal(Asks::Digest));
        }
        Reply::Bytes(response(
            307,
            "Temporary Redirect",
            &[("Location", &elsewhere)],
            b"",
        ))
    });
    let entries = dav::list(&first.url("/dav/"), &account()).expect("listed where it was sent");
    assert_eq!(entries.len(), 3);
    let seen = other_seen.lock().unwrap();
    assert!(!seen.is_empty());
    assert!(
        seen.iter().all(|r| r.header("authorization").is_none()),
        "the other server was told who asks"
    );
}

// --- TLS ---------------------------------------------------------------------------

/// A server that speaks TLS with a certificate made for the test, and answers like
/// [`serve`]. One connection at a time is enough here.
struct SecureServer {
    addr: SocketAddr,
    /// The certificate it shows (DER).
    certificate: Vec<u8>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SecureServer {
    fn start(asks: Asks, redirect_to_plain: Option<String>) -> SecureServer {
        let made = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
            .expect("a certificate");
        let certificate = made.cert.der().clone();
        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(made.signing_key.serialize_der());
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![certificate.clone()], key.into())
        .unwrap();
        let config = Arc::new(config);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stopped.load(Ordering::Acquire) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let Ok(connection) = rustls::ServerConnection::new(config.clone()) else {
                    continue;
                };
                let mut secure = rustls::StreamOwned::new(connection, stream);
                let Some(request) = read_request(&mut secure, addr) else {
                    // (A client that does not trust the certificate ends here.)
                    continue;
                };
                let answer = match &redirect_to_plain {
                    Some(to) => response(302, "Found", &[("Location", to)], b""),
                    None => share(&request, asks),
                };
                let _ = secure.write_all(&answer);
                secure.conn.send_close_notify();
                let _ = secure.flush();
            }
        });
        SecureServer {
            addr,
            certificate: certificate.to_vec(),
            stop,
            thread: Some(thread),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("https://localhost:{}{path}", self.addr.port())
    }
}

impl Drop for SecureServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn read_request(stream: &mut impl Read, server: SocketAddr) -> Option<Request> {
    let mut bytes = Vec::new();
    let mut piece = [0u8; 4096];
    let head_end = loop {
        if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
        let read = stream.read(&mut piece).ok().filter(|n| *n > 0)?;
        bytes.extend_from_slice(&piece[..read]);
    };
    let head = String::from_utf8_lossy(&bytes[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut line = lines.next()?.split(' ');
    let (method, path) = (line.next()?.to_string(), line.next()?.to_string());
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
        let read = stream.read(&mut piece).ok().filter(|n| *n > 0)?;
        bytes.extend_from_slice(&piece[..read]);
    }
    Some(Request {
        method,
        path,
        headers,
        body: bytes[head_end..head_end + length].to_vec(),
        server,
    })
}

/// A NAS with a certificate of its own making: refused with the certificate's
/// fingerprint, read once that fingerprint is trusted, and refused again when it
/// shows another certificate than the trusted one.
#[test]
fn a_server_with_its_own_certificate_is_trusted_by_its_fingerprint_only() {
    let server = SecureServer::start(Asks::Basic, None);
    let root = server.url("/dav/");
    let shown = tls::fingerprint(&server.certificate);

    match dav::list(&root, &account()) {
        Err(NetError::Untrusted { fingerprint }) => assert_eq!(fingerprint, shown),
        other => panic!("not refused for its certificate: {other:?}"),
    }
    let trusting = Access {
        account: Some((USER, WORD)),
        pinned: Some(&shown),
        ..Access::default()
    };
    let entries = dav::list(&root, &trusting).expect("listed over TLS");
    assert_eq!(entries.len(), 3);
    assert!(
        entries
            .iter()
            .all(|e| e.url.starts_with("https://localhost:"))
    );
    let mut stream =
        http::open_web(&entries[1].url, 5_000, &Limits::new(u64::MAX), &trusting).expect("opens");
    let mut rest = Vec::new();
    stream.read_to_end(&mut rest).unwrap();
    assert_eq!(rest, fixture("gap_a.flac")[5_000..]);

    // Another certificate than the trusted one (a different server at the address).
    let other = "00".repeat(32);
    let wrong = Access {
        account: Some((USER, WORD)),
        pinned: Some(&other),
        ..Access::default()
    };
    assert!(matches!(
        dav::list(&root, &wrong),
        Err(NetError::Untrusted { .. })
    ));
    // The plain way of asking never reaches an `https://` address.
    assert!(matches!(
        http::get(&root, &Limits::new(1 << 20)),
        Err(NetError::BadAddress)
    ));
}

/// From an encrypted connection nothing leads to one that is not.
#[test]
fn an_https_address_does_not_lead_to_an_http_one() {
    let (plain, seen) = serve(Asks::Nobody);
    let server = SecureServer::start(Asks::Nobody, Some(plain.url("/dav/song%202.flac")));
    let shown = tls::fingerprint(&server.certificate);
    let trusting = Access {
        account: Some((USER, WORD)),
        pinned: Some(&shown),
        ..Access::default()
    };
    let result = http::get_web(&server.url("/dav/x"), &Limits::new(1 << 20), &trusting);
    assert!(matches!(result, Err(NetError::BadAddress)), "{result:?}");
    assert!(
        seen.lock().unwrap().is_empty(),
        "the plain server was asked"
    );
}

/// A song of a network folder is played as the folder's account: the address of the
/// song carries neither name nor password, and the decoder gets the same samples as
/// from the file. With the password forgotten the song cannot be opened.
#[test]
fn a_song_of_a_folder_plays_with_the_folders_account() {
    secrets::use_memory();
    let server = SecureServer::start(Asks::Digest, None);
    let folder = Folder {
        id: "folder-test".into(),
        name: "Test".into(),
        url: server.url("/dav/"),
        user: USER.into(),
        pinned: Some(tls::fingerprint(&server.certificate)),
    }
    .sanitized()
    .expect("a folder");
    folders::set_known(std::slice::from_ref(&folder));
    folders::keep_password(&folder.id, &Secret::new(WORD)).unwrap();

    let decode = |mut decoder: TrackDecoder| {
        let (mut all, mut buf) = (Vec::new(), Vec::new());
        while decoder.next(&mut buf).expect("decodes") {
            all.extend_from_slice(&buf);
        }
        all
    };
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gap_a.wv");
    let expected = decode(TrackDecoder::open(&path).unwrap());
    let url = server.url("/dav/Album%20%ED%95%98%EB%82%98/01%20one.wv");
    for in_memory in [true, false] {
        let file = RemoteFile::open(&url).expect("the song opens");
        let budget = Arc::new(Budget::new(if in_memory { 64 << 20 } else { 0 }));
        let (decoder, _) = TrackDecoder::open_remote_within(file, &budget).expect("decodes");
        assert_eq!(decoder.info().in_memory, in_memory);
        assert!(decode(decoder) == expected, "in memory: {in_memory}");
    }

    folders::forget_password(&folder.id);
    assert!(secrets::get(&folder.id).is_none());
    let refused = RemoteFile::open(&url)
        .err()
        .expect("refused without the password");
    assert_eq!(refused.kind(), std::io::ErrorKind::PermissionDenied);
    folders::set_known(&[]);
}
