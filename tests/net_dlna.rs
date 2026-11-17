//! The network layer against servers on 127.0.0.1: addresses, the HTTP client, the
//! search for media servers, descriptions, browsing, DIDL-Lite, the XML reader, and a
//! few thousand damaged inputs that must end in an error and never in a panic.
mod net_support;

use net_support::{FakeServer, HttpServer, Node, Reply, Request, Track, folder, response};
use oneum::net::http::{self, Limits, Url};
use oneum::net::upnp::{self, Entry, Folder, Item, Resource, Server};
use oneum::net::xml::{self, Event, Reader, XmlError};
use oneum::net::{NetError, ssdp};
use std::io::{ErrorKind, Read};
use std::net::{SocketAddr, TcpListener, UdpSocket};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Fails with what the value was when it does not match the pattern.
macro_rules! assert_is {
    ($value:expr, $pattern:pat $(if $guard:expr)?) => {
        match $value {
            $pattern $(if $guard)? => {}
            other => panic!("expected {}, got {other:?}", stringify!($pattern)),
        }
    };
}

/// The usual limits: a server on this machine answers long before they matter.
fn limits(max_body: u64) -> Limits {
    Limits::new(max_body)
}

/// Limits for servers that are meant not to answer: a short wait, so that the test
/// does not take the ten seconds a real one is given.
fn impatient(read_ms: u64) -> Limits {
    Limits {
        connect: Duration::from_secs(2),
        read: Duration::from_millis(read_ms),
        max_body: 1 << 20,
        redirects: 3,
    }
}

/// An answer written out by hand.
fn raw(text: &str) -> Reply {
    Reply::Bytes(text.as_bytes().to_vec())
}

/// A server that gives every request the same answer.
fn always(answer: &str) -> HttpServer {
    let answer = answer.as_bytes().to_vec();
    HttpServer::start(move |_| Reply::Bytes(answer.clone()))
}

/// A server that answers with `handler` and keeps the requests it was sent.
fn recording(
    handler: impl Fn(&Request) -> Reply + Send + Sync + 'static,
) -> (HttpServer, Arc<Mutex<Vec<Request>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let server = {
        let seen = seen.clone();
        HttpServer::start(move |request| {
            seen.lock().unwrap().push(request.clone());
            handler(request)
        })
    };
    (server, seen)
}

/// Bytes that differ from place to place, so that a wrong offset shows.
fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|at| (at % 251) as u8 ^ seed ^ (at / 251) as u8)
        .collect()
}

// ---------------------------------------------------------------------------------
// Addresses

#[test]
fn urls_are_taken_apart() {
    for (text, host, port, path, written) in [
        (
            "http://192.168.0.10:8200/MediaItems/22.flac",
            "192.168.0.10",
            8200,
            "/MediaItems/22.flac",
            "http://192.168.0.10:8200/MediaItems/22.flac",
        ),
        (
            "http://nas.local",
            "nas.local",
            80,
            "/",
            "http://nas.local/",
        ),
        (
            "HTTP://NAS.Local:80/A/b?C=d#e",
            "nas.local",
            80,
            "/A/b?C=d",
            "http://nas.local/A/b?C=d",
        ),
        (
            "http://nas.local:8080?x=1",
            "nas.local",
            8080,
            "/?x=1",
            "http://nas.local:8080/?x=1",
        ),
        (
            "http://[::1]:8200/desc.xml",
            "::1",
            8200,
            "/desc.xml",
            "http://[::1]:8200/desc.xml",
        ),
        ("http://[FE80::1]", "fe80::1", 80, "/", "http://[fe80::1]/"),
        ("http://h:/x", "h", 80, "/x", "http://h/x"),
        ("http://h/a%20b", "h", 80, "/a%20b", "http://h/a%20b"),
        (
            "http://h/음악/노래.flac",
            "h",
            80,
            "/%EC%9D%8C%EC%95%85/%EB%85%B8%EB%9E%98.flac",
            "http://h/%EC%9D%8C%EC%95%85/%EB%85%B8%EB%9E%98.flac",
        ),
    ] {
        let url = Url::parse(text).unwrap_or_else(|error| panic!("{text}: {error}"));
        assert_eq!(
            (url.host.as_str(), url.port, url.path.as_str()),
            (host, port, path),
            "{text}"
        );
        assert_eq!(url.to_string(), written, "{text}");
        assert_eq!(Url::parse(written).unwrap(), url, "{text}");
    }
    let longest = format!("http://h/{}", "a".repeat(2048 - 9));
    assert_eq!(longest.len(), 2048);
    assert!(Url::parse(&longest).is_ok());
}

#[test]
fn urls_that_are_not_plain_http_are_refused() {
    let too_long = format!("http://h/{}", "a".repeat(2048 - 8));
    assert_eq!(too_long.len(), 2049);
    for text in [
        "",
        "h/path",
        "/path",
        "http:/h/path",
        "http://",
        "http:///path",
        "https://h/path",
        "ftp://h/path",
        "file:///etc/passwd",
        "http://user@h/path",
        "http://user:secret@h/path",
        "http://h/a b",
        "http://h/a\tb",
        "http://h/a\r\nHost: other",
        "http://h/\u{7f}",
        "http://ho st/",
        "http://h\u{0}/",
        " http://h/",
        "http://h:0/",
        "http://h:65536/",
        "http://h:8a/",
        "http://h:-1/",
        "http://::1/",
        "http://[::1/",
        "http://[::1]x/",
        "http://[not-an-address]/",
        "http://höst/",
        too_long.as_str(),
    ] {
        assert_is!(Url::parse(text), Err(NetError::BadAddress));
    }
}

#[test]
fn references_are_joined_as_rfc_3986_says() {
    let base = Url::parse("http://10.0.0.2:8200/dev/desc.xml?x=1").unwrap();
    for (reference, expected) in [
        ("http://other:81/a", "http://other:81/a"),
        ("HTTP://Other/a", "http://other/a"),
        ("//other/x", "http://other/x"),
        ("/ctl/ContentDir", "http://10.0.0.2:8200/ctl/ContentDir"),
        ("ctl", "http://10.0.0.2:8200/dev/ctl"),
        ("ctl/cd?a=b", "http://10.0.0.2:8200/dev/ctl/cd?a=b"),
        ("../ctl", "http://10.0.0.2:8200/ctl"),
        ("../../../ctl", "http://10.0.0.2:8200/ctl"),
        ("./a/./b/../c", "http://10.0.0.2:8200/dev/a/c"),
        ("..", "http://10.0.0.2:8200/"),
        (".", "http://10.0.0.2:8200/dev/"),
        ("/a/../b/./c/", "http://10.0.0.2:8200/b/c/"),
        ("?y=2", "http://10.0.0.2:8200/dev/desc.xml?y=2"),
        ("a?b=../c", "http://10.0.0.2:8200/dev/a?b=../c"),
        ("", "http://10.0.0.2:8200/dev/desc.xml?x=1"),
        ("#top", "http://10.0.0.2:8200/dev/desc.xml?x=1"),
        ("a#b", "http://10.0.0.2:8200/dev/a"),
        (
            "노래.mp3",
            "http://10.0.0.2:8200/dev/%EB%85%B8%EB%9E%98.mp3",
        ),
    ] {
        let joined = base
            .join(reference)
            .unwrap_or_else(|error| panic!("{reference}: {error}"));
        assert_eq!(joined.to_string(), expected, "{reference}");
    }
    let too_long = "a".repeat(2049);
    let nearly_too_long = "a".repeat(2040);
    for reference in [
        "https://h/x",
        "ftp://h/x",
        "mailto:someone",
        "a b",
        "/a\nb",
        "//user@other/x",
        too_long.as_str(),
        // Short enough alone, too long behind the base's folder.
        nearly_too_long.as_str(),
    ] {
        assert_is!(base.join(reference), Err(NetError::BadAddress));
    }
}

// ---------------------------------------------------------------------------------
// GET and POST

#[test]
fn get_returns_the_body_and_sends_what_servers_expect() {
    let (server, seen) = recording(|_| Reply::Bytes(response(200, "OK", &[], b"hello")));
    let body = http::get(&server.url("/a/b?c=d"), &limits(1024)).unwrap();
    assert_eq!(body, b"hello");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    let request = &seen[0];
    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/a/b?c=d");
    assert_eq!(
        request.header("host"),
        Some(server.addr().to_string().as_str())
    );
    assert_eq!(request.header("user-agent"), Some(http::USER_AGENT));
    assert_eq!(request.header("connection"), Some("close"));
    assert_eq!(request.header("accept-encoding"), Some("identity"));
    assert_eq!(request.header("range"), None);
}

#[test]
fn a_body_without_a_length_ends_with_the_connection() {
    let server = always("HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\nuntil the end");
    assert_eq!(
        http::get(&server.url("/"), &limits(1024)).unwrap(),
        b"until the end"
    );
}

#[test]
fn answers_without_a_body_and_interim_answers() {
    let server = always("HTTP/1.1 204 No Content\r\n\r\n");
    assert_eq!(http::get(&server.url("/"), &limits(1024)).unwrap(), b"");
    let server =
        always("HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\ncontent-LENGTH:  2 \r\n\r\nok");
    assert_eq!(http::get(&server.url("/"), &limits(1024)).unwrap(), b"ok");
    let interim = "HTTP/1.1 100 Continue\r\n\r\n".repeat(20);
    let server = always(&format!("{interim}HTTP/1.1 200 OK\r\n\r\nlate"));
    assert_is!(
        http::get(&server.url("/"), &limits(1024)),
        Err(NetError::BadResponse(_))
    );
}

#[test]
fn chunked_bodies_are_put_together() {
    let chunked = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
                   5\r\nhello\r\n1;ext=1\r\n,\r\n0006\r\n world\r\n0\r\nX-Trailer: yes\r\n\r\n";
    let server = always(chunked);
    assert_eq!(
        http::get(&server.url("/"), &limits(1024)).unwrap(),
        b"hello, world"
    );
    // The same arriving a byte at a time: sizes and line ends split across reads.
    let (head, body) = chunked.split_at(chunked.find("5\r\n").unwrap());
    let (head, body) = (head.as_bytes().to_vec(), body.as_bytes().to_vec());
    let server = HttpServer::start(move |_| Reply::Drip {
        first: head.clone(),
        rest: body.clone(),
        every: Duration::from_millis(1),
    });
    assert_eq!(
        http::get(&server.url("/"), &limits(1024)).unwrap(),
        b"hello, world"
    );
    // Some servers close right after the last chunk's size.
    let server = always("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n");
    assert_eq!(
        http::get(&server.url("/"), &limits(1024)).unwrap(),
        b"hello"
    );
    // With both, the chunks count and the length does not.
    let server = always(
        "HTTP/1.1 200 OK\r\nContent-Length: 3\r\nTransfer-Encoding: gzip, Chunked\r\n\r\n\
         5\r\nhello\r\n0\r\n\r\n",
    );
    assert_eq!(
        http::get(&server.url("/"), &limits(1024)).unwrap(),
        b"hello"
    );
}

#[test]
fn damaged_chunks_are_errors() {
    const HEAD: &str = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
    for body in [
        "zz\r\nhello\r\n0\r\n\r\n",
        "-5\r\nhello\r\n0\r\n\r\n",
        "\r\nhello\r\n0\r\n\r\n",
        "5\r\nhello!\r\n0\r\n\r\n",
        "0000000000000000000000000000000000000005\r\nhello\r\n0\r\n\r\n",
        "11111111111111111\r\nhello\r\n0\r\n\r\n",
    ] {
        let server = always(&format!("{HEAD}{body}"));
        assert_is!(
            http::get(&server.url("/"), &limits(1024)),
            Err(NetError::BadResponse(_))
        );
    }
    for body in ["a\r\nhello", "5\r\nhello\r\n", "5\r\nhello", "5"] {
        let server = always(&format!("{HEAD}{body}"));
        assert_is!(
            http::get(&server.url("/"), &limits(1024)),
            Err(NetError::Io(error)) if error.kind() == ErrorKind::UnexpectedEof
        );
    }
}

#[test]
fn a_status_that_is_not_success_is_an_error_for_get() {
    let server = always("HTTP/1.1 404 Not Found\r\nContent-Length: 4\r\n\r\ngone");
    assert_is!(
        http::get(&server.url("/"), &limits(1024)),
        Err(NetError::Status(404))
    );
    let server = always("HTTP/1.1 500 Internal Server Error\r\n\r\n");
    assert_is!(
        http::get(&server.url("/"), &limits(1024)),
        Err(NetError::Status(500))
    );
}

#[test]
fn post_sends_the_body_and_returns_any_status() {
    let (server, seen) = recording(|request| {
        let status = if request.path == "/fault" { 500 } else { 200 };
        let mut echo = b"got: ".to_vec();
        echo.extend_from_slice(&request.body);
        Reply::Bytes(response(status, "Whatever", &[], &echo))
    });
    let headers = [
        ("Content-Type", "text/xml; charset=\"utf-8\""),
        ("SOAPACTION", "\"urn:x#Browse\""),
    ];
    let (status, body) = http::post(
        &server.url("/ctl"),
        &headers,
        "<a>한글</a>".as_bytes(),
        &limits(1024),
    )
    .unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, "got: <a>한글</a>".as_bytes());
    let (status, body) = http::post(&server.url("/fault"), &[], b"x", &limits(1024)).unwrap();
    assert_eq!((status, body.as_slice()), (500, &b"got: x"[..]));

    let seen = seen.lock().unwrap();
    let request = &seen[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/ctl");
    assert_eq!(
        request.header("content-type"),
        Some("text/xml; charset=\"utf-8\"")
    );
    assert_eq!(request.header("soapaction"), Some("\"urn:x#Browse\""));
    assert_eq!(request.header("content-length"), Some("13"));
    assert_eq!(request.header("user-agent"), Some(http::USER_AGENT));
    assert_eq!(request.header("connection"), Some("close"));
}

#[test]
fn post_refuses_headers_that_would_break_the_request() {
    let (server, seen) = recording(|_| Reply::Bytes(response(200, "OK", &[], b"")));
    for headers in [
        [("X-Test", "a\r\nInjected: yes")],
        [("Bad Name", "a")],
        [("", "a")],
    ] {
        assert_is!(
            http::post(&server.url("/"), &headers, b"", &limits(1024)),
            Err(NetError::Io(error)) if error.kind() == ErrorKind::InvalidInput
        );
    }
    assert!(seen.lock().unwrap().is_empty());
}

fn redirect(status: u16, location: &str) -> Reply {
    Reply::Bytes(response(
        status,
        "Moved",
        &[("Location", location)],
        b"moved",
    ))
}

/// A server whose paths lead to one another.
fn maze() -> (HttpServer, Arc<Mutex<Vec<Request>>>) {
    recording(|request| match request.path.as_str() {
        "/start" => redirect(302, "/hop/one"),
        "/hop/one" => redirect(301, "two?x=1"),
        "/hop/two?x=1" => redirect(307, &format!("http://{}/end", request.server)),
        "/see-other" => redirect(303, "end"),
        "/permanent" => redirect(308, "/end"),
        "/end" => {
            let mut body = request.method.clone().into_bytes();
            body.extend_from_slice(b" arrived ");
            body.extend_from_slice(&request.body);
            Reply::Bytes(response(200, "OK", &[], &body))
        }
        "/loop" => redirect(302, "/loop"),
        "/nowhere" => Reply::Bytes(response(302, "Found", &[], b"")),
        "/secure" => redirect(302, "https://example.invalid/"),
        "/multiple" => redirect(300, "/end"),
        _ => Reply::Bytes(response(404, "Not Found", &[], b"")),
    })
}

#[test]
fn redirects_are_followed_to_the_end() {
    let (server, seen) = maze();
    assert_eq!(
        http::get(&server.url("/start"), &limits(1024)).unwrap(),
        b"GET arrived "
    );
    let paths: Vec<String> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|request| request.path.clone())
        .collect();
    assert_eq!(paths, ["/start", "/hop/one", "/hop/two?x=1", "/end"]);
    for path in ["/see-other", "/permanent"] {
        assert_eq!(
            http::get(&server.url(path), &limits(1024)).unwrap(),
            b"GET arrived ",
            "{path}"
        );
    }
    // 300 names choices; it sends nobody anywhere.
    assert_is!(
        http::get(&server.url("/multiple"), &limits(1024)),
        Err(NetError::Status(300))
    );
}

#[test]
fn redirects_are_counted() {
    let (server, seen) = maze();
    // Three redirects lead from /start to /end: one fewer allowed is one too few.
    let two = Limits {
        redirects: 2,
        ..limits(1024)
    };
    assert_is!(
        http::get(&server.url("/start"), &two),
        Err(NetError::BadResponse("too many redirects"))
    );
    let none = Limits {
        redirects: 0,
        ..limits(1024)
    };
    assert_is!(
        http::get(&server.url("/permanent"), &none),
        Err(NetError::BadResponse("too many redirects"))
    );
    seen.lock().unwrap().clear();
    assert_is!(
        http::get(&server.url("/loop"), &limits(1024)),
        Err(NetError::BadResponse("too many redirects"))
    );
    // The first request and the three redirects that are allowed.
    assert_eq!(seen.lock().unwrap().len(), 4);
    assert_is!(
        http::open(&server.url("/loop"), 0, &limits(0)).err(),
        Some(NetError::BadResponse("too many redirects"))
    );
}

#[test]
fn redirects_to_nowhere_are_errors() {
    let (server, _) = maze();
    assert_is!(
        http::get(&server.url("/nowhere"), &limits(1024)),
        Err(NetError::BadResponse(_))
    );
    assert_is!(
        http::get(&server.url("/secure"), &limits(1024)),
        Err(NetError::BadAddress)
    );
}

#[test]
fn post_follows_only_the_redirects_that_keep_it_a_post() {
    let (server, _) = maze();
    let (status, body) =
        http::post(&server.url("/permanent"), &[], b"command", &limits(1024)).unwrap();
    assert_eq!(
        (status, body.as_slice()),
        (200, &b"POST arrived command"[..])
    );
    let (status, body) =
        http::post(&server.url("/hop/two?x=1"), &[], b"command", &limits(1024)).unwrap();
    assert_eq!(
        (status, body.as_slice()),
        (200, &b"POST arrived command"[..])
    );
    // 302 and 303 would make a GET of it: they are the answer.
    for path in ["/start", "/see-other"] {
        let (status, body) = http::post(&server.url(path), &[], b"command", &limits(1024)).unwrap();
        assert_eq!(
            (status, body.as_slice()),
            (status.clamp(302, 303), &b"moved"[..])
        );
    }
}

#[test]
fn a_body_larger_than_allowed_is_too_large() {
    // Announced: refused before a byte of the body is read. The server sends none;
    // reading on would end in another error.
    let server = always("HTTP/1.1 200 OK\r\nContent-Length: 1001\r\n\r\n");
    assert_is!(
        http::get(&server.url("/"), &limits(1000)),
        Err(NetError::TooLarge)
    );
    assert_is!(
        http::post(&server.url("/"), &[], b"", &limits(1000)),
        Err(NetError::TooLarge)
    );
    let server = always("HTTP/1.1 200 OK\r\nContent-Length: 18446744073709551615\r\n\r\n");
    assert_is!(
        http::get(&server.url("/"), &limits(u64::MAX - 1)),
        Err(NetError::TooLarge)
    );

    // Exactly as much as allowed is fine.
    let exact = pattern(1000, 1);
    let answer = response(200, "OK", &[], &exact);
    let server = HttpServer::start(move |_| Reply::Bytes(answer.clone()));
    assert_eq!(http::get(&server.url("/"), &limits(1000)).unwrap(), exact);
    assert_is!(
        http::get(&server.url("/"), &limits(999)),
        Err(NetError::TooLarge)
    );

    // Not announced: found out while reading.
    let big = "x".repeat(5000);
    let closing = always(&format!("HTTP/1.1 200 OK\r\n\r\n{big}"));
    let chunked = always(&format!(
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1388\r\n{big}\r\n0\r\n\r\n"
    ));
    for server in [&closing, &chunked] {
        assert_eq!(
            http::get(&server.url("/"), &limits(5000)).unwrap().len(),
            5000
        );
        assert_is!(
            http::get(&server.url("/"), &limits(4999)),
            Err(NetError::TooLarge)
        );
        assert_is!(
            http::post(&server.url("/"), &[], b"", &limits(100)),
            Err(NetError::TooLarge)
        );
        assert_is!(
            http::get(&server.url("/"), &limits(0)),
            Err(NetError::TooLarge)
        );
    }
}

#[test]
fn a_head_beyond_the_limits_is_refused() {
    let padded = |bytes: usize| {
        format!(
            "HTTP/1.1 200 OK\r\nX-Pad: {}\r\nContent-Length: 2\r\n\r\nok",
            "a".repeat(bytes)
        )
    };
    let server = always(&padded(60_000));
    assert_eq!(http::get(&server.url("/"), &limits(1024)).unwrap(), b"ok");
    let server = always(&padded(70_000));
    assert_is!(
        http::get(&server.url("/"), &limits(1024)),
        Err(NetError::BadResponse(_))
    );
    // The limit is on the whole block, not on one line of it.
    let lines = format!("X-Pad: {}\r\n", "a".repeat(1000)).repeat(70);
    let server = always(&format!("HTTP/1.1 200 OK\r\n{lines}\r\nok"));
    assert_is!(
        http::open(&server.url("/"), 0, &limits(0)).err(),
        Some(NetError::BadResponse(_))
    );

    let many = |count: usize| {
        format!(
            "HTTP/1.1 200 OK\r\n{}Content-Length: 2\r\n\r\nok",
            "X-A: b\r\n".repeat(count)
        )
    };
    let server = always(&many(190));
    assert_eq!(http::get(&server.url("/"), &limits(1024)).unwrap(), b"ok");
    let server = always(&many(250));
    assert_is!(
        http::get(&server.url("/"), &limits(1024)),
        Err(NetError::BadResponse(_))
    );
}

#[test]
fn answers_that_are_not_http_are_errors() {
    for answer in [
        "SSH-2.0-OpenSSH_9.6\r\n",
        "<html>hello</html>\r\n\r\n",
        "HTTP/2 200\r\n\r\n",
        "HTTP/1.1 abc OK\r\n\r\n",
        "HTTP/1.1 200 OK\r\nno colon here\r\n\r\n",
        "HTTP/1.1 200 OK\r\nContent-Length: many\r\n\r\n",
        "HTTP/1.1 200 OK\r\nContent-Length: -1\r\n\r\n",
        "HTTP/1.1 200 OK\r\nContent-Length: 99999999999999999999\r\n\r\n",
        "HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nab",
    ] {
        let server = always(answer);
        assert_is!(
            http::get(&server.url("/"), &limits(1024)),
            Err(NetError::BadResponse(_))
        );
    }
    // A connection closed before the head is complete.
    for answer in ["", "HTTP/1.1 200 OK\r\nContent-Le"] {
        let server = always(answer);
        assert_is!(
            http::get(&server.url("/"), &limits(1024)),
            Err(NetError::Io(error)) if error.kind() == ErrorKind::UnexpectedEof
        );
    }
    // A body that ends before its length.
    let server = always("HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n12345");
    assert_is!(
        http::get(&server.url("/"), &limits(1024)),
        Err(NetError::Io(error)) if error.kind() == ErrorKind::UnexpectedEof
    );
    // The same length twice is one length.
    let server = always("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\nab");
    assert_eq!(http::get(&server.url("/"), &limits(1024)).unwrap(), b"ab");
}

#[test]
fn nobody_listening_is_a_connect_error() {
    // A port that was free a moment ago and has no listener now.
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let url = format!("http://127.0.0.1:{port}/desc.xml");
    assert_is!(http::get(&url, &impatient(300)), Err(NetError::Connect(_)));
    assert_is!(
        http::post(&url, &[], b"", &impatient(300)),
        Err(NetError::Connect(_))
    );
    assert_is!(
        http::open(&url, 0, &impatient(300)).err(),
        Some(NetError::Connect(_))
    );
    assert_is!(upnp::describe(&url), Err(NetError::Connect(_)));
    assert_is!(
        http::get("https://127.0.0.1/desc.xml", &impatient(300)),
        Err(NetError::BadAddress)
    );
}

// ---------------------------------------------------------------------------------
// Servers that do not answer

/// Runs `request`, which must time out, and returns how long that took.
fn timed_out<T: std::fmt::Debug>(request: impl FnOnce() -> Result<T, NetError>) -> Duration {
    let started = Instant::now();
    assert_is!(request(), Err(NetError::Timeout));
    started.elapsed()
}

#[test]
fn a_silent_server_is_given_up_on_within_the_limit() {
    let server = FakeServer::start(vec![
        Track::new("Song", "audio/flac", pattern(100, 0)).into(),
    ]);
    server.set_stall(true);
    let quick = impatient(300);
    for took in [
        timed_out(|| http::get(&server.location(), &quick)),
        timed_out(|| http::post(&server.url("/ctl"), &[], b"<a/>", &quick)),
        timed_out(|| http::open(&server.media_url(0), 0, &quick).map(|_| ())),
    ] {
        assert!(took >= Duration::from_millis(250), "{took:?}");
        assert!(took < Duration::from_secs(3), "{took:?}");
    }
    // It answers again once it is let.
    server.set_stall(false);
    assert!(http::get(&server.location(), &quick).is_ok());
    assert_eq!(server.requests(), 1);
}

#[test]
fn a_head_that_trickles_in_is_given_up_on() {
    // Every byte comes well within the read timeout; the head as a whole would take
    // seconds.
    let head =
        b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\n\r\nok".to_vec();
    let server = HttpServer::start(move |_| Reply::Drip {
        first: Vec::new(),
        rest: head.clone(),
        every: Duration::from_millis(40),
    });
    let took = timed_out(|| http::get(&server.url("/"), &impatient(400)));
    assert!(took >= Duration::from_millis(350), "{took:?}");
    assert!(took < Duration::from_secs(3), "{took:?}");
    let took = timed_out(|| http::open(&server.url("/"), 0, &impatient(400)).map(|_| ()));
    assert!(took < Duration::from_secs(3), "{took:?}");
}

#[test]
fn a_body_that_trickles_in_ends_a_get_but_not_a_stream() {
    // 80 bytes, one every 15 ms: 1.2 s in all, each byte far within the read timeout
    // of 150 ms. A whole body gets six read timeouts (0.9 s); a stream is read for as
    // long as it plays.
    let body = pattern(80, 3);
    let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
    let server = {
        let body = body.clone();
        HttpServer::start(move |_| Reply::Drip {
            first: head.clone(),
            rest: body.clone(),
            every: Duration::from_millis(15),
        })
    };
    let slow = impatient(150);
    let took = timed_out(|| http::get(&server.url("/"), &slow));
    assert!(took >= Duration::from_millis(800), "{took:?}");
    assert!(took < Duration::from_secs(3), "{took:?}");
    let took = timed_out(|| http::post(&server.url("/"), &[], b"", &slow));
    assert!(took < Duration::from_secs(3), "{took:?}");

    let mut stream = http::open(&server.url("/"), 0, &slow).unwrap();
    let mut read = Vec::new();
    stream.read_to_end(&mut read).unwrap();
    assert_eq!(read, body);
}

#[test]
fn a_stream_that_falls_silent_times_out_on_every_read() {
    let server = HttpServer::start(|_| {
        Reply::Partial(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n12345".to_vec())
    });
    let mut stream = http::open(&server.url("/"), 0, &impatient(200)).unwrap();
    assert_eq!(stream.total_len(), Some(10));
    let mut start = [0u8; 5];
    stream.read_exact(&mut start).unwrap();
    assert_eq!(&start, b"12345");
    for _ in 0..2 {
        let started = Instant::now();
        let error = stream.read(&mut [0u8; 16]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::TimedOut);
        let took = started.elapsed();
        assert!(took >= Duration::from_millis(150), "{took:?}");
        assert!(took < Duration::from_secs(2), "{took:?}");
    }
}

// ---------------------------------------------------------------------------------
// Reading from an offset

#[test]
fn a_stream_starts_where_it_is_asked_to() {
    let body = pattern(50_000, 7);
    let server = FakeServer::start(vec![Track::new("Song", "audio/flac", body.clone()).into()]);
    let url = server.media_url(0);
    // `max_body` is of no concern to a stream.
    let limits = limits(0);

    let mut stream = http::open(&url, 0, &limits).unwrap();
    assert!(stream.ranged());
    assert_eq!(stream.total_len(), Some(50_000));
    assert_eq!(stream.content_type(), Some("audio/flac"));
    let mut read = Vec::new();
    stream.read_to_end(&mut read).unwrap();
    assert_eq!(read, body);
    // At the end it stays at the end.
    assert_eq!(stream.read(&mut [0u8; 8]).unwrap(), 0);

    let mut stream = http::open(&url, 12_345, &limits).unwrap();
    assert!(stream.ranged());
    assert_eq!(stream.total_len(), Some(50_000));
    let mut read = Vec::new();
    stream.read_to_end(&mut read).unwrap();
    assert_eq!(read, &body[12_345..]);

    let mut stream = http::open(&url, 49_999, &limits).unwrap();
    let mut read = Vec::new();
    stream.read_to_end(&mut read).unwrap();
    assert_eq!(read, &body[49_999..]);

    assert_is!(
        http::open(&url, 50_000, &limits).err(),
        Some(NetError::Status(416))
    );
    assert_is!(
        http::open(&server.media_url(9), 0, &limits).err(),
        Some(NetError::Status(404))
    );
    assert_eq!(server.media_requests(), 4);
    assert_eq!(server.range_requests(), 4);
    assert_eq!(server.offset_requests(), 3);
}

#[test]
fn a_server_that_ignores_ranges_sends_the_whole_file() {
    let body = pattern(20_000, 9);
    let server = FakeServer::start(vec![Track::new("Song", "audio/mpeg", body.clone()).into()]);
    server.set_refuse_ranges(true);
    let mut stream = http::open(&server.media_url(0), 12_345, &limits(0)).unwrap();
    assert!(!stream.ranged());
    assert_eq!(stream.total_len(), Some(20_000));
    let mut read = Vec::new();
    stream.read_to_end(&mut read).unwrap();
    assert_eq!(read, body);
    assert_eq!(server.offset_requests(), 1);

    server.set_refuse_ranges(false);
    let stream = http::open(&server.media_url(0), 12_345, &limits(0)).unwrap();
    assert!(stream.ranged());
}

#[test]
fn a_stream_can_be_handed_to_another_thread() {
    let body = pattern(10_000, 5);
    let server = FakeServer::start(vec![Track::new("Song", "audio/flac", body.clone()).into()]);
    let mut stream = http::open(&server.media_url(0), 100, &limits(0)).unwrap();
    let read = std::thread::spawn(move || {
        let mut read = Vec::new();
        stream.read_to_end(&mut read).map(|_| read)
    })
    .join()
    .unwrap()
    .unwrap();
    assert_eq!(read, &body[100..]);
}

#[test]
fn partial_answers_are_checked() {
    let answer = |range: &'static str| {
        HttpServer::start(move |request| {
            assert_eq!(request.header("range"), Some("bytes=5-"));
            let mut head = String::from("HTTP/1.1 206 Partial Content\r\nContent-Length: 5\r\n");
            if !range.is_empty() {
                head.push_str(&format!("Content-Range: {range}\r\n"));
            }
            raw(&format!("{head}\r\n56789"))
        })
    };
    let total = |range| {
        let server = answer(range);
        http::open(&server.url("/"), 5, &limits(0)).map(|stream| {
            assert!(stream.ranged());
            stream.total_len()
        })
    };
    assert_is!(total("bytes 5-9/10"), Ok(Some(10)));
    assert_is!(total("BYTES 5-9/10"), Ok(Some(10)));
    // Without a total, the rest that is announced tells it.
    assert_is!(total("bytes 5-9/*"), Ok(Some(10)));
    // The wrong place, or no word about the place: not played from.
    for range in [
        "bytes 0-4/10",
        "bytes 6-9/10",
        "",
        "5-9/10",
        "bytes five-nine/ten",
    ] {
        assert_is!(total(range).err(), Some(NetError::BadResponse(_)));
    }
}

#[test]
fn streams_decode_chunks_and_report_a_cut() {
    let server = always(
        "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nTransfer-Encoding: chunked\r\n\r\n\
         3\r\nabc\r\n3\r\ndef\r\n0\r\n\r\n",
    );
    let mut stream = http::open(&server.url("/"), 0, &limits(0)).unwrap();
    assert!(!stream.ranged());
    assert_eq!(stream.total_len(), None);
    assert_eq!(stream.content_type(), Some("audio/mpeg"));
    let mut read = String::new();
    stream.read_to_string(&mut read).unwrap();
    assert_eq!(read, "abcdef");

    let server = always("HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n12345");
    let mut stream = http::open(&server.url("/"), 0, &limits(0)).unwrap();
    let mut read = Vec::new();
    let error = stream.read_to_end(&mut read).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UnexpectedEof);
    assert_eq!(read, b"12345");

    // More than the length says is not part of the body.
    let server = always("HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\n12345");
    let mut stream = http::open(&server.url("/"), 0, &limits(0)).unwrap();
    let mut read = Vec::new();
    stream.read_to_end(&mut read).unwrap();
    assert_eq!(read, b"123");

    // A stream follows redirects like any GET.
    let (server, _) = maze();
    let mut stream = http::open(&server.url("/start"), 0, &limits(0)).unwrap();
    let mut read = String::new();
    stream.read_to_string(&mut read).unwrap();
    assert_eq!(read, "GET arrived ");
}

#[test]
fn a_real_file_arrives_whole_in_pieces() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/gap_a.flac");
    let file = std::fs::read(path).unwrap();
    assert!(file.len() > 3000, "the fixture is {} bytes", file.len());
    let server = FakeServer::start(vec![Track::new("Gap A", "audio/flac", file.clone()).into()]);
    // The way a player reads: the start, a jump to the end, then the middle.
    let mut pieces = vec![0u8; file.len()];
    for (from, len) in [
        (0, 1000),
        (file.len() - 500, 500),
        (1000, file.len() - 1500),
    ] {
        let mut stream = http::open(&server.media_url(0), from as u64, &limits(0)).unwrap();
        assert_eq!(stream.total_len(), Some(file.len() as u64));
        stream.read_exact(&mut pieces[from..from + len]).unwrap();
    }
    assert!(pieces == file);
    assert_eq!(
        http::get(&server.media_url(0), &limits(1 << 24)).unwrap(),
        file
    );
}

// ---------------------------------------------------------------------------------
// The search for servers

const SEARCH: &str = "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\n\
    MAN: \"ssdp:discover\"\r\nMX: 2\r\nST: urn:schemas-upnp-org:device:MediaServer:1\r\n\r\n";

fn ssdp_reply(location: &str, st: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=1800\r\nEXT:\r\nLocation: {location}\r\n\
         SERVER: Test/1.0 UPnP/1.0 Test/1.0\r\nst: {st}\r\nUSN: uuid:{location}::{st}\r\n\r\n"
    )
    .into_bytes()
}

/// A socket that answers the first search it gets with `replies`. The thread returns
/// what the search said.
fn search_responder(replies: Vec<Vec<u8>>) -> (SocketAddr, JoinHandle<String>) {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = socket.local_addr().unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let responder = std::thread::spawn(move || {
        let mut datagram = [0u8; 2048];
        let (len, from) = socket.recv_from(&mut datagram).expect("a search");
        for reply in &replies {
            socket.send_to(reply, from).unwrap();
        }
        String::from_utf8_lossy(&datagram[..len]).into_owned()
    });
    (addr, responder)
}

#[test]
fn a_search_finds_the_fake_server_once() {
    let server = FakeServer::start(Vec::new());
    // Long enough for the search to be sent twice; the server answers both.
    let found = ssdp::search_at(server.ssdp_addr(), Duration::from_millis(800));
    assert_eq!(
        found,
        [ssdp::Found {
            location: server.location(),
            usn: format!(
                "{}::urn:schemas-upnp-org:device:MediaServer:1",
                FakeServer::UDN
            ),
        }]
    );
    // What was found leads to the server.
    let described = upnp::describe(&found[0].location).unwrap();
    assert_eq!(described.name, FakeServer::NAME);
}

#[test]
fn only_answers_from_media_servers_count() {
    const SERVER: &str = "urn:schemas-upnp-org:device:MediaServer:1";
    let mut oversized = ssdp_reply("http://127.0.0.1:1005/desc.xml", SERVER);
    oversized.resize(8500, b' ');
    let (addr, responder) = search_responder(vec![
        b"\xff\xfe garbage \x00".to_vec(),
        ssdp_reply(
            "http://127.0.0.1:1001/desc.xml",
            "urn:schemas-upnp-org:device:MediaRenderer:1",
        ),
        ssdp_reply("http://127.0.0.1:1002/desc.xml", SERVER),
        ssdp_reply("HTTP://127.0.0.1:1002/desc.xml#again", SERVER),
        ssdp_reply("https://127.0.0.1:1003/desc.xml", SERVER),
        ssdp_reply("/desc.xml", SERVER),
        ssdp_reply("http://127.0.0.1:1004/desc.xml", SERVER)
            .strip_prefix(b"HTTP/1.1 200 OK")
            .map(|rest| [b"NOTIFY * HTTP/1.1", rest].concat())
            .unwrap(),
        oversized,
        ssdp_reply(
            "http://127.0.0.1:1006/desc.xml",
            "urn:schemas-upnp-org:device:MediaServer:4",
        ),
    ]);
    let found = ssdp::search_at(addr, Duration::from_millis(800));
    let locations: Vec<&str> = found.iter().map(|found| found.location.as_str()).collect();
    assert_eq!(
        locations,
        [
            "http://127.0.0.1:1002/desc.xml",
            "http://127.0.0.1:1006/desc.xml"
        ]
    );
    assert_eq!(
        found[0].usn,
        format!("uuid:http://127.0.0.1:1002/desc.xml::{SERVER}")
    );
    assert_eq!(responder.join().unwrap(), SEARCH);
}

#[test]
fn a_search_ends_when_the_wait_is_over_or_the_list_is_full() {
    // Nobody answers: the list is empty after the wait, not before and not much later.
    let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
    let started = Instant::now();
    let found = ssdp::search_at(silent.local_addr().unwrap(), Duration::from_millis(250));
    let took = started.elapsed();
    assert!(found.is_empty());
    assert!(took >= Duration::from_millis(250), "{took:?}");
    assert!(took < Duration::from_secs(2), "{took:?}");
    assert!(ssdp::search_at(silent.local_addr().unwrap(), Duration::ZERO).is_empty());

    // A hundred servers answer: sixty-four are taken, and the search does not wait on.
    let replies = (0..100)
        .map(|n| {
            ssdp_reply(
                &format!("http://127.0.0.1:2000/{n}.xml"),
                "urn:schemas-upnp-org:device:MediaServer:1",
            )
        })
        .collect();
    let (addr, responder) = search_responder(replies);
    let started = Instant::now();
    let found = ssdp::search_at(addr, Duration::from_secs(8));
    assert_eq!(found.len(), 64);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(found[63].location, "http://127.0.0.1:2000/63.xml");
    responder.join().unwrap();
}

// ---------------------------------------------------------------------------------
// Descriptions

/// A server whose description is `body` (with `{addr}` standing for its address), at
/// `/dev/desc.xml`.
fn described(body: &str) -> Result<(Server, HttpServer), NetError> {
    let body = body.to_string();
    let server = HttpServer::start(move |request| {
        let body = body.replace("{addr}", &request.server.to_string());
        Reply::Bytes(response(200, "OK", &[], body.as_bytes()))
    });
    upnp::describe(&server.url("/dev/desc.xml")).map(|described| (described, server))
}

const CONTENT_DIRECTORY: &str = "<service>\
    <serviceType>urn:schemas-upnp-org:service:ContentDirectory:1</serviceType>\
    <serviceId>urn:upnp-org:serviceId:ContentDirectory</serviceId>\
    <SCPDURL>cd.xml</SCPDURL><controlURL>CONTROL</controlURL><eventSubURL>ev</eventSubURL>\
    </service>";

fn device(control: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <root xmlns=\"urn:schemas-upnp-org:device-1-0\">URLBASE<device>\
         <deviceType>urn:schemas-upnp-org:device:MediaServer:1</deviceType>\
         <friendlyName> 거실 NAS &amp; Co </friendlyName><UDN>uuid:abc-123</UDN>\
         <iconList><icon><url>/icon.png</url></icon></iconList>\
         <serviceList><service>\
         <serviceType>urn:schemas-upnp-org:service:ConnectionManager:1</serviceType>\
         <controlURL>/wrong</controlURL></service>{}</serviceList>\
         </device></root>",
        CONTENT_DIRECTORY.replace("CONTROL", control)
    )
}

#[test]
fn the_fake_server_is_described() {
    let server = FakeServer::start(Vec::new());
    assert_eq!(
        upnp::describe(&server.location()).unwrap(),
        Server {
            name: FakeServer::NAME.to_string(),
            udn: FakeServer::UDN.to_string(),
            location: server.location(),
            control_url: server.url("/ctl"),
        }
    );
}

#[test]
fn the_control_address_is_resolved_against_the_description() {
    for (control, expected) in [
        ("/ctl/ContentDir", "/ctl/ContentDir"),
        ("ctl/cd", "/dev/ctl/cd"),
        ("../ctl", "/ctl"),
        ("\n    /spaced\n  ", "/spaced"),
    ] {
        let (described, server) = described(&device(control).replace("URLBASE", "")).unwrap();
        assert_eq!(described.control_url, server.url(expected), "{control}");
        assert_eq!(described.name, "거실 NAS & Co");
        assert_eq!(described.udn, "uuid:abc-123");
        assert_eq!(described.location, server.url("/dev/desc.xml"));
    }
    let (described, _server) =
        described(&device("http://other.example:9000/c").replace("URLBASE", "")).unwrap();
    assert_eq!(described.control_url, "http://other.example:9000/c");
}

#[test]
fn a_url_base_goes_before_the_description_s_own_address() {
    let with_base = |base: &str, control: &str| {
        let base = format!("<URLBase>{base}</URLBase>");
        described(&device(control).replace("URLBASE", &base))
    };
    let (described, server) = with_base("http://{addr}/base/", "ctl").unwrap();
    assert_eq!(described.control_url, server.url("/base/ctl"));
    let (described, _server) = with_base("http://10.9.8.7:49152", "/ctl").unwrap();
    assert_eq!(described.control_url, "http://10.9.8.7:49152/ctl");
    // A base that is no address is as good as none.
    let (described, server) = with_base("ftp://nowhere/", "ctl").unwrap();
    assert_eq!(described.control_url, server.url("/dev/ctl"));
}

#[test]
fn the_content_directory_may_belong_to_an_embedded_device() {
    let body = format!(
        "<root><device><friendlyName>Router</friendlyName><UDN>uuid:router</UDN>\
         <serviceList><service>\
         <serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType>\
         <controlURL>/wan</controlURL></service></serviceList>\
         <deviceList>\
         <device><friendlyName>Printer</friendlyName><UDN>uuid:printer</UDN></device>\
         <device><friendlyName>Router Media</friendlyName><UDN>uuid:media</UDN>\
         <serviceList>{}</serviceList></device>\
         </deviceList></device></root>",
        CONTENT_DIRECTORY.replace("CONTROL", "/media/ctl")
    );
    let (described, server) = described(&body).unwrap();
    assert_eq!(described.name, "Router Media");
    assert_eq!(described.udn, "uuid:media");
    assert_eq!(described.control_url, server.url("/media/ctl"));
}

#[test]
fn descriptions_without_a_content_directory_are_refused() {
    let renderer = device("/ctl").replace("ContentDirectory", "RenderingControl");
    for body in [
        renderer.replace("URLBASE", ""),
        // The type is there, the address is not.
        device("").replace("URLBASE", ""),
        "<root><device><friendlyName>Empty</friendlyName></device></root>".to_string(),
        "<root/>".to_string(),
        String::new(),
        "this is not XML".to_string(),
        // Cut off in the middle.
        device("/ctl")[..300].to_string(),
        device("https://secure.example/ctl").replace("URLBASE", ""),
    ] {
        assert_is!(
            described(&body).map(|(server, _)| server),
            Err(NetError::BadResponse(_))
        );
    }
    let server = always("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
    assert_is!(upnp::describe(&server.url("/")), Err(NetError::Status(404)));
    assert_is!(
        upnp::describe("nas.local/desc.xml"),
        Err(NetError::BadAddress)
    );
    // More than a megabyte is not a description.
    let padding = format!("<!--{}-->", "x".repeat(1 << 20));
    assert_is!(
        described(&device("/ctl").replace("URLBASE", &padding)).map(|(server, _)| server),
        Err(NetError::TooLarge)
    );
}

// ---------------------------------------------------------------------------------
// Browsing

/// A PNG's first bytes: enough for a picture that is only compared.
const COVER: &[u8] = b"\x89PNG\r\n\x1a\n cover of the album";

fn library() -> Vec<Node> {
    vec![
        folder(
            "가요",
            vec![
                Track::new("서울의 밤", "audio/flac", pattern(4000, 1))
                    .artist("한강 프로젝트")
                    .album("강변의 밤")
                    .duration_ms(231_500)
                    .art(COVER.to_vec())
                    .into(),
                Track::new("잠수교", "audio/mpeg", pattern(3000, 2))
                    .artist("한강 프로젝트")
                    .album("강변의 밤")
                    .into(),
                folder(
                    "라이브",
                    vec![Track::new("첫차", "audio/x-wav", pattern(500, 6)).into()],
                ),
            ],
        ),
        folder("Empty", Vec::new()),
        Track::new("Tom & Jerry <Live> \"1\"", "audio/wav", pattern(2000, 3)).into(),
        Track::new("Four", "audio/flac", pattern(100, 4)).into(),
        Track::new("Five", "audio/flac", pattern(100, 5)).into(),
    ]
}

fn titles(entries: &[Entry]) -> Vec<&str> {
    entries
        .iter()
        .map(|entry| match entry {
            Entry::Folder(folder) => folder.title.as_str(),
            Entry::Item(item) => item.title.as_str(),
        })
        .collect()
}

#[test]
fn a_folder_is_read_page_by_page() {
    let server = FakeServer::start(library());
    let described = upnp::describe(&server.location()).unwrap();

    let page = upnp::browse(&described, "0", 0, 2).unwrap();
    assert_eq!(page.total, 5);
    assert_eq!(
        page.entries,
        [
            Entry::Folder(Folder {
                id: "f1".into(),
                title: "가요".into(),
                child_count: Some(3),
            }),
            Entry::Folder(Folder {
                id: "f3".into(),
                title: "Empty".into(),
                child_count: Some(0),
            }),
        ]
    );
    let page = upnp::browse(&described, "0", 2, 2).unwrap();
    assert_eq!(page.total, 5);
    assert_eq!(titles(&page.entries), ["Tom & Jerry <Live> \"1\"", "Four"]);
    let page = upnp::browse(&described, "0", 4, 2).unwrap();
    assert_eq!(titles(&page.entries), ["Five"]);
    let page = upnp::browse(&described, "0", 5, 2).unwrap();
    assert_eq!((page.entries.len(), page.total), (0, 5));

    let page = upnp::browse(&described, "f3", 0, 50).unwrap();
    assert_eq!((page.entries.len(), page.total), (0, 0));
    let page = upnp::browse(&described, "f2", 0, 50).unwrap();
    assert_eq!(titles(&page.entries), ["첫차"]);
    // Asking for none is asking for one: 0 would mean "all" to the server.
    let page = upnp::browse(&described, "0", 0, 0).unwrap();
    assert_eq!((page.entries.len(), page.total), (1, 5));
    assert_eq!(server.browse_requests(), 7);
}

#[test]
fn songs_come_with_what_the_server_knows_and_can_be_fetched() {
    let server = FakeServer::start(library());
    let described = upnp::describe(&server.location()).unwrap();
    let page = upnp::browse(&described, "f1", 0, 100).unwrap();
    assert_eq!(page.total, 3);
    assert_eq!(titles(&page.entries), ["서울의 밤", "잠수교", "라이브"]);
    let Entry::Item(song) = &page.entries[0] else {
        panic!("{:?}", page.entries[0]);
    };
    assert_eq!(
        song,
        &Item {
            id: "t0".into(),
            title: "서울의 밤".into(),
            artist: Some("한강 프로젝트".into()),
            album: Some("강변의 밤".into()),
            album_artist: None,
            genre: None,
            track_number: None,
            art_url: Some(server.art_url(0)),
            audio: true,
            resources: vec![Resource {
                url: server.media_url(0),
                mime: "audio/flac".into(),
                size: Some(4000),
                duration_ms: Some(231_500),
                sample_rate: None,
                bit_depth: None,
                channels: None,
                converted: false,
            }],
        }
    );
    let Entry::Item(second) = &page.entries[1] else {
        panic!("{:?}", page.entries[1]);
    };
    assert_eq!(second.art_url, None);
    assert_eq!(second.resources[0].duration_ms, None);

    // From the entry to the bytes.
    let resource = upnp::best_resource(song).unwrap();
    let mut stream = http::open(&resource.url, 0, &limits(0)).unwrap();
    assert_eq!(stream.total_len(), resource.size);
    let mut file = Vec::new();
    stream.read_to_end(&mut file).unwrap();
    assert_eq!(file, pattern(4000, 1));
    let cover = http::get(song.art_url.as_deref().unwrap(), &limits(1 << 20)).unwrap();
    assert_eq!(cover, COVER);
}

#[test]
fn at_most_two_hundred_entries_are_asked_for_at_once() {
    let songs = (0..250)
        .map(|n| Track::new(&format!("Song {n}"), "audio/mpeg", vec![0; 10]).into())
        .collect();
    let server = FakeServer::start(vec![folder("Many", songs)]);
    let described = upnp::describe(&server.location()).unwrap();
    let page = upnp::browse(&described, "f1", 0, 1000).unwrap();
    assert_eq!((page.entries.len(), page.total), (200, 250));
    let page = upnp::browse(&described, "f1", 200, u32::MAX).unwrap();
    assert_eq!((page.entries.len(), page.total), (50, 250));
    assert_eq!(titles(&page.entries)[49], "Song 249");
}

#[test]
fn an_unknown_folder_is_the_server_s_error() {
    let server = FakeServer::start(library());
    let described = upnp::describe(&server.location()).unwrap();
    for id in ["nothing", "t0", "", "a&b<c>"] {
        assert_is!(
            upnp::browse(&described, id, 0, 10),
            Err(NetError::Upnp(description)) if description == "No such object"
        );
    }
}

/// A server that answers every command with `answer`, and what it described itself
/// as.
fn commanded(
    status: u16,
    answer: impl Into<Vec<u8>>,
) -> (Server, HttpServer, Arc<Mutex<Vec<Request>>>) {
    let answer = answer.into();
    let (server, seen) =
        recording(move |_| Reply::Bytes(response(status, "Whatever", &[], &answer)));
    let described = Server {
        name: "Test".into(),
        udn: "uuid:test".into(),
        location: server.url("/desc.xml"),
        control_url: server.url("/ctl/cd"),
    };
    (described, server, seen)
}

fn envelope(inner: &str) -> String {
    format!(
        "<?xml version=\"1.0\"?><s:Envelope \
         xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body>{inner}</s:Body>\
         </s:Envelope>"
    )
}

#[test]
fn the_command_is_what_a_content_directory_expects() {
    let (described, _server, seen) = commanded(
        200,
        envelope(
            "<u:BrowseResponse><Result>&lt;DIDL-Lite&gt;&lt;container id=\"1\"&gt;\
             &lt;dc:title&gt;One&lt;/dc:title&gt;&lt;/container&gt;&lt;/DIDL-Lite&gt;</Result>\
             <NumberReturned>1</NumberReturned><TotalMatches>1</TotalMatches>\
             </u:BrowseResponse>",
        ),
    );
    let page = upnp::browse(&described, "a&b<c>\"d'", 40, 20).unwrap();
    assert_eq!(titles(&page.entries), ["One"]);

    let seen = seen.lock().unwrap();
    let request = &seen[0];
    assert_eq!(
        (request.method.as_str(), request.path.as_str()),
        ("POST", "/ctl/cd")
    );
    assert_eq!(
        request.header("soapaction"),
        Some("\"urn:schemas-upnp-org:service:ContentDirectory:1#Browse\"")
    );
    assert_eq!(
        request.header("content-type"),
        Some("text/xml; charset=\"utf-8\"")
    );
    let command = String::from_utf8(request.body.clone()).unwrap();
    for part in [
        "<u:Browse xmlns:u=\"urn:schemas-upnp-org:service:ContentDirectory:1\">",
        "<ObjectID>a&amp;b&lt;c&gt;&quot;d&apos;</ObjectID>",
        "<BrowseFlag>BrowseDirectChildren</BrowseFlag>",
        "<Filter>*</Filter>",
        "<StartingIndex>40</StartingIndex>",
        "<RequestedCount>20</RequestedCount>",
        "<SortCriteria></SortCriteria>",
    ] {
        assert!(command.contains(part), "{part} is not in {command}");
    }
    // The command is itself a document the reader takes.
    let mut reader = Reader::new(&command);
    let mut events = 0;
    while let Some(event) = reader.next_event() {
        event.unwrap();
        events += 1;
    }
    assert!(events > 10);
}

#[test]
fn answers_come_in_more_than_one_shape() {
    let titles_of = |status: u16, answer: Vec<u8>| {
        let (described, _server, _) = commanded(status, answer);
        upnp::browse(&described, "0", 0, 10).map(|page| {
            let titles: Vec<String> = titles(&page.entries)
                .iter()
                .map(|t| t.to_string())
                .collect();
            (titles, page.total)
        })
    };
    const DIDL: &str = "<DIDL-Lite><container id=\"1\"><dc:title>R&amp;B</dc:title>\
                        </container></DIDL-Lite>";
    // The document escaped (as it should be), and in a CDATA section.
    let escaped = envelope(&format!(
        "<u:BrowseResponse><Result>{}</Result><TotalMatches>7</TotalMatches></u:BrowseResponse>",
        xml::escape(DIDL)
    ));
    assert_eq!(
        titles_of(200, escaped.into_bytes()).unwrap(),
        (vec!["R&B".to_string()], 7)
    );
    let cdata = envelope(&format!(
        "<m:BrowseResponse><Result><![CDATA[{DIDL}]]></Result></m:BrowseResponse>"
    ));
    assert_eq!(
        titles_of(200, cdata.into_bytes()).unwrap(),
        (vec!["R&B".to_string()], 0)
    );
    // An empty folder as an empty Result.
    let empty = envelope("<u:BrowseResponse><Result></Result></u:BrowseResponse>");
    assert_eq!(titles_of(200, empty.into_bytes()).unwrap(), (Vec::new(), 0));

    // Bytes that are not UTF-8 (a title in Latin-1) are replaced, not refused.
    let mut latin = envelope(&format!(
        "<u:BrowseResponse><Result>{}</Result></u:BrowseResponse>",
        xml::escape(
            "<DIDL-Lite><container id=\"1\"><dc:title>Caf_</dc:title></container></DIDL-Lite>"
        )
    ))
    .into_bytes();
    let at = latin.iter().position(|&byte| byte == b'_').unwrap();
    latin[at] = 0xE9;
    assert_eq!(
        titles_of(200, latin).unwrap(),
        (vec!["Caf\u{fffd}".to_string()], 0)
    );

    // No Result, a failure without a fault, a fault without a description.
    assert_is!(
        titles_of(200, envelope("<u:BrowseResponse/>").into_bytes()),
        Err(NetError::BadResponse(_))
    );
    assert_is!(
        titles_of(200, b"OK".to_vec()),
        Err(NetError::BadResponse(_))
    );
    assert_is!(
        titles_of(500, b"Internal Server Error".to_vec()),
        Err(NetError::Status(500))
    );
    assert_is!(titles_of(404, Vec::new()), Err(NetError::Status(404)));
    let fault = envelope(
        "<s:Fault><faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring><detail>\
         <UPnPError xmlns=\"urn:schemas-upnp-org:control-1-0\"><errorCode>720</errorCode>\
         <errorDescription> </errorDescription></UPnPError></detail></s:Fault>",
    );
    assert_is!(
        titles_of(500, fault.clone().into_bytes()),
        Err(NetError::Upnp(code)) if code == "720"
    );
    // A fault counts whatever the status says.
    assert_is!(titles_of(200, fault.into_bytes()), Err(NetError::Upnp(_)));
    // More than eight megabytes is not a page of a folder.
    let huge = envelope(&format!("<!--{}-->", "x".repeat(8 << 20)));
    assert_is!(titles_of(200, huge.into_bytes()), Err(NetError::TooLarge));
}

// ---------------------------------------------------------------------------------
// DIDL-Lite

fn base() -> Url {
    Url::parse("http://10.0.0.2:8200/ctl/ContentDir").unwrap()
}

fn didl(entries: &str) -> String {
    format!(
        "<DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\" \
         xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
         xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\" \
         xmlns:dlna=\"urn:schemas-dlna-org:metadata-1-0/\">\n{entries}\n</DIDL-Lite>"
    )
}

fn only_item(entries: &str) -> Item {
    match upnp::parse_didl(&didl(entries), &base()).as_slice() {
        [Entry::Item(item)] => item.clone(),
        other => panic!("expected one item, got {other:?}"),
    }
}

const SONG: &str = "<item id=\"64$1$2\" parentID=\"64$1\" restricted=\"1\">
  <dc:title>Tom &amp; Jerry &lt;Live&gt; &#xD55C;&#44544;</dc:title>
  <upnp:class>object.item.audioItem.musicTrack</upnp:class>
  <dc:creator>The Creator</dc:creator>
  <upnp:artist role=\"AlbumArtist\">Various Artists</upnp:artist>
  <upnp:artist role=\"Performer\">The Performer</upnp:artist>
  <upnp:artist>The Artist</upnp:artist>
  <upnp:artist>Another Artist</upnp:artist>
  <upnp:album><![CDATA[Rock & Roll <Deluxe>]]></upnp:album>
  <upnp:genre>Rock</upnp:genre>
  <upnp:genre>Pop</upnp:genre>
  <upnp:originalTrackNumber>7</upnp:originalTrackNumber>
  <upnp:albumArtURI dlna:profileID=\"JPEG_TN\">/AlbumArt/5-22.jpg</upnp:albumArtURI>
  <desc id=\"x\" nameSpace=\"urn:example\"><x:custom xmlns:x=\"urn:example\"><dc:title>Not this</dc:title></x:custom></desc>
  <res protocolInfo=\"http-get:*:audio/L16;rate=44100;channels=2:DLNA.ORG_PN=LPCM\" size=\"99\">http://10.0.0.2:8200/pcm/22.wav</res>
  <res protocolInfo=\"http-get:*:audio/mpeg:DLNA.ORG_PN=MP3;DLNA.ORG_OP=01;DLNA.ORG_CI=1\" bitrate=\"40000\">http://10.0.0.2:8200/transcode/22.mp3</res>
  <res protocolInfo=\"http-get:*:audio/flac:DLNA.ORG_CI=1;DLNA.ORG_OP=00\">http://10.0.0.2:8200/transcode/22.flac</res>
  <res protocolInfo=\"HTTP-GET:*:Audio/X-FLAC:DLNA.ORG_OP=01;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01700000000000000000000000000000\" size=\"31231231\" duration=\"0:04:12.500\" bitrate=\"176400\" sampleFrequency=\"96000\" bitsPerSample=\"24\" nrAudioChannels=\"2\">http://10.0.0.2:8200/MediaItems/22.flac</res>
  <res protocolInfo=\"rtsp-rtp-udp:*:audio/flac:*\">rtsp://10.0.0.2/22</res>
  <res protocolInfo=\"http-get:*:audio/flac:*\">https://10.0.0.2/secure/22.flac</res>
  <res protocolInfo=\"http-get:*:audio/flac:*\"></res>
  <res>http://10.0.0.2:8200/no-protocol.flac</res>
</item>";

#[test]
fn a_song_is_read_with_all_it_says() {
    let song = only_item(SONG);
    assert_eq!(song.id, "64$1$2");
    assert_eq!(song.title, "Tom & Jerry <Live> 한글");
    assert_eq!(song.artist.as_deref(), Some("The Artist"));
    assert_eq!(song.album_artist.as_deref(), Some("Various Artists"));
    assert_eq!(song.album.as_deref(), Some("Rock & Roll <Deluxe>"));
    assert_eq!(song.genre.as_deref(), Some("Rock"));
    assert_eq!(song.track_number, Some(7));
    assert_eq!(
        song.art_url.as_deref(),
        Some("http://10.0.0.2:8200/AlbumArt/5-22.jpg")
    );
    assert!(song.audio);
    let offered: Vec<(&str, &str, bool)> = song
        .resources
        .iter()
        .map(|resource| {
            (
                resource.mime.as_str(),
                resource.url.as_str(),
                resource.converted,
            )
        })
        .collect();
    assert_eq!(
        offered,
        [
            (
                "audio/l16;rate=44100;channels=2",
                "http://10.0.0.2:8200/pcm/22.wav",
                false
            ),
            ("audio/mpeg", "http://10.0.0.2:8200/transcode/22.mp3", true),
            ("audio/flac", "http://10.0.0.2:8200/transcode/22.flac", true),
            (
                "audio/x-flac",
                "http://10.0.0.2:8200/MediaItems/22.flac",
                false
            ),
        ]
    );
    // Not the raw samples, not what the server would convert: the file.
    assert_eq!(
        upnp::best_resource(&song),
        Some(&Resource {
            url: "http://10.0.0.2:8200/MediaItems/22.flac".into(),
            mime: "audio/x-flac".into(),
            size: Some(31_231_231),
            duration_ms: Some(252_500),
            sample_rate: Some(96_000),
            bit_depth: Some(24),
            channels: Some(2),
            converted: false,
        })
    );
}

#[test]
fn raw_samples_and_conversions_are_never_the_resource_to_play() {
    let with = |resources: &str| {
        only_item(&format!(
            "<item id=\"1\"><dc:title>T</dc:title>\
             <upnp:class>object.item.audioItem.musicTrack</upnp:class>{resources}</item>"
        ))
    };
    let pcm = "<res protocolInfo=\"http-get:*:audio/L16;rate=44100;channels=2:*\">/a.wav</res>\
               <res protocolInfo=\"http-get:*:audio/L24:*\">/a.flac</res>";
    assert_eq!(with(pcm).resources.len(), 2);
    assert_eq!(upnp::best_resource(&with(pcm)), None);
    let converted = "<res protocolInfo=\"http-get:*:audio/flac:DLNA.ORG_CI=1\">/a.flac</res>\
                     <res protocolInfo=\"http-get:*:audio/mpeg:dlna.org_op=01; dlna.org_ci=1 \">/a.mp3</res>";
    assert_eq!(upnp::best_resource(&with(converted)), None);
    // A lossy original goes before a lossless conversion and before raw samples.
    let mixed = format!(
        "{pcm}{converted}<res protocolInfo=\"http-get:*:audio/mpeg:DLNA.ORG_CI=0\">/b.mp3</res>\
         <res protocolInfo=\"http-get:*:audio/mp4:*\">/b.m4a</res>"
    );
    assert_eq!(
        upnp::best_resource(&with(&mixed)).map(|resource| resource.url.as_str()),
        Some("http://10.0.0.2:8200/b.mp3")
    );
    // Lossless goes before lossy wherever it is listed.
    let both = "<res protocolInfo=\"http-get:*:audio/mpeg:*\">/c.mp3</res>\
                <res protocolInfo=\"http-get:*:audio/x-aiff:*\">/c.aiff</res>\
                <res protocolInfo=\"http-get:*:audio/wav:*\">/c.wav</res>";
    assert_eq!(
        upnp::best_resource(&with(both)).map(|resource| resource.url.as_str()),
        Some("http://10.0.0.2:8200/c.aiff")
    );
    assert_eq!(upnp::best_resource(&with("")), None);
}

#[test]
fn what_is_missing_is_left_out() {
    let bare = only_item(
        "<item id=\"9\"><res protocolInfo=\"http-get:*:audio/mpeg:*\"> song.mp3 </res></item>",
    );
    assert_eq!(
        bare,
        Item {
            id: "9".into(),
            // No title: the id stands in.
            title: "9".into(),
            artist: None,
            album: None,
            album_artist: None,
            genre: None,
            track_number: None,
            art_url: None,
            // No class: not known to be a song.
            audio: false,
            resources: vec![Resource {
                // Relative to the address the document came from.
                url: "http://10.0.0.2:8200/ctl/song.mp3".into(),
                mime: "audio/mpeg".into(),
                size: None,
                duration_ms: None,
                sample_rate: None,
                bit_depth: None,
                channels: None,
                converted: false,
            }],
        }
    );
    assert!(upnp::best_resource(&bare).is_some());

    let numbers = only_item(
        "<item id=\"1\"><dc:title>  </dc:title>\
         <upnp:originalTrackNumber>seven</upnp:originalTrackNumber>\
         <upnp:albumArtURI>ftp://h/cover.jpg</upnp:albumArtURI>\
         <res protocolInfo=\"http-get:*:audio/flac:*\" size=\"-1\" duration=\"soon\" \
         sampleFrequency=\"\" bitsPerSample=\"300\" nrAudioChannels=\"two\">/a.flac</res></item>",
    );
    assert_eq!(numbers.title, "1");
    assert_eq!(numbers.track_number, None);
    assert_eq!(numbers.art_url, None);
    let resource = &numbers.resources[0];
    assert_eq!(
        (
            resource.size,
            resource.duration_ms,
            resource.sample_rate,
            resource.bit_depth,
            resource.channels
        ),
        (None, None, None, None, None)
    );
}

#[test]
fn the_artist_is_the_one_without_a_role() {
    let artists = |fields: &str| {
        let item = only_item(&format!("<item id=\"1\">{fields}</item>"));
        (item.artist, item.album_artist)
    };
    let some = |text: &str| Some(text.to_string());
    assert_eq!(
        artists(
            "<upnp:artist role=\"Composer\">C</upnp:artist><upnp:artist>A</upnp:artist>\
             <dc:creator>D</dc:creator>"
        ),
        (some("A"), None)
    );
    assert_eq!(
        artists("<dc:creator>D</dc:creator><upnp:artist role=\"Performer\">P</upnp:artist>"),
        (some("P"), None)
    );
    assert_eq!(artists("<dc:creator>D</dc:creator>"), (some("D"), None));
    assert_eq!(
        artists("<upnp:artist role=\"albumartist\">AA</upnp:artist>"),
        (None, some("AA"))
    );
    assert_eq!(
        artists("<upnp:artist role=\"AlbumArtist\">AA</upnp:artist><dc:creator>D</dc:creator>"),
        (some("D"), some("AA"))
    );
    assert_eq!(artists("<upnp:artist></upnp:artist>"), (None, None));
}

#[test]
fn folders_and_other_things_than_songs() {
    let entries = upnp::parse_didl(
        &didl(
            "<container id=\"64\" parentID=\"0\" restricted=\"1\" searchable=\"1\" childCount=\"4\">\
             <dc:title>Browse Folders</dc:title>\
             <upnp:class>object.container.storageFolder</upnp:class></container>\
             <container id=\"65\"><upnp:class>object.container</upnp:class></container>\
             <container id=\"66\" childCount=\"many\"><dc:title>&lt;All&gt;</dc:title></container>\
             <container><dc:title>No id</dc:title></container>\
             <item><dc:title>No id either</dc:title></item>\
             <item id=\"p1\"><dc:title>Holiday</dc:title>\
             <upnp:class>object.item.imageItem.photo</upnp:class>\
             <res protocolInfo=\"http-get:*:image/jpeg:*\">/p1.jpg</res></item>\
             <item id=\"v1\"><dc:title>Film</dc:title>\
             <upnp:class>object.item.videoItem</upnp:class>\
             <res protocolInfo=\"http-get:*:application/octet-stream:*\">/film.mp4</res></item>\
             <item id=\"s1\"><dc:title>Song</dc:title>\
             <upnp:class>object.item.audioItem</upnp:class>\
             <res protocolInfo=\"http-get:*:application/octet-stream:*\">/song.FLAC?x=1</res></item>",
        ),
        &base(),
    );
    assert_eq!(
        titles(&entries),
        ["Browse Folders", "65", "<All>", "Holiday", "Film", "Song"]
    );
    assert_eq!(
        entries[0],
        Entry::Folder(Folder {
            id: "64".into(),
            title: "Browse Folders".into(),
            child_count: Some(4),
        })
    );
    let items: Vec<&Item> = entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Item(item) => Some(item),
            Entry::Folder(_) => None,
        })
        .collect();
    let playable: Vec<(bool, bool)> = items
        .iter()
        .map(|item| (item.audio, upnp::best_resource(item).is_some()))
        .collect();
    // A type that says nothing is believed by the file's name, for songs only.
    assert_eq!(playable, [(false, false), (false, false), (true, true)]);
}

#[test]
fn documents_are_limited_and_damage_ends_them() {
    let long = "가".repeat(5000);
    let capped = upnp::parse_didl(
        &didl(&format!(
            "<item id=\"{long}\"><dc:title>{long}</dc:title>\
             <upnp:album>{long}</upnp:album></item>"
        )),
        &base(),
    );
    let [Entry::Item(item)] = capped.as_slice() else {
        panic!("{capped:?}");
    };
    assert_eq!(item.id.chars().count(), 1024);
    assert_eq!(item.title.chars().count(), 1024);
    assert_eq!(item.album.as_ref().unwrap().chars().count(), 1024);

    let many: String = (0..1500)
        .map(|n| format!("<container id=\"{n}\"><dc:title>F{n}</dc:title></container>"))
        .collect();
    let entries = upnp::parse_didl(&didl(&many), &base());
    assert_eq!(entries.len(), 1000);
    assert_eq!(titles(&entries)[999], "F999");

    let resources = "<res protocolInfo=\"http-get:*:audio/flac:*\">/a.flac</res>".repeat(100);
    let item = only_item(&format!("<item id=\"1\">{resources}</item>"));
    assert_eq!(item.resources.len(), 32);

    // What stands before the damage is kept.
    let cut = didl(
        "<container id=\"1\"><dc:title>One</dc:title></container>\
         <item id=\"2\"><dc:title>Two</dc:title></item>\
         <item id=\"3\"><dc:title>Thr",
    );
    let cut = &cut[..cut.find("Thr").unwrap() + 3];
    assert_eq!(titles(&upnp::parse_didl(cut, &base())), ["One", "Two"]);
    let mismatched = didl(
        "<container id=\"1\"><dc:title>One</dc:title></container>\
         <item id=\"2\"><dc:title>Two</dc:creator></item>\
         <item id=\"3\"><dc:title>Three</dc:title></item>",
    );
    assert_eq!(titles(&upnp::parse_didl(&mismatched, &base())), ["One"]);

    for nothing in [
        "",
        "hello",
        "<a><b/></a>",
        "<DIDL-Lite/>",
        "<item",
        "\u{feff}",
    ] {
        assert!(upnp::parse_didl(nothing, &base()).is_empty(), "{nothing}");
    }
}

// ---------------------------------------------------------------------------------
// The XML reader

fn events(text: &str) -> Vec<Result<Event, XmlError>> {
    let mut reader = Reader::new(text);
    let mut events = Vec::new();
    while let Some(event) = reader.next_event() {
        events.push(event);
    }
    // At the end it stays at the end.
    assert_eq!(reader.next_event(), None);
    assert_eq!(reader.next_event(), None);
    events
}

fn start(name: &str, attrs: &[(&str, &str)]) -> Result<Event, XmlError> {
    Ok(Event::Start {
        name: name.to_string(),
        attrs: attrs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect(),
    })
}

fn end(name: &str) -> Result<Event, XmlError> {
    Ok(Event::End {
        name: name.to_string(),
    })
}

fn text(text: &str) -> Result<Event, XmlError> {
    Ok(Event::Text(text.to_string()))
}

#[test]
fn the_reader_reads_what_documents_are_made_of() {
    assert_eq!(
        events(
            "\u{feff}<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n<!-- a comment -->\n\
             <s:Envelope xmlns:s=\"urn:soap\">\n  <s:Body>\n    <dc:title>제목 &amp; more</dc:title>\n\
             <empty/><also-empty attr='1' />\n  </s:Body>\n</s:Envelope >\n<!-- after -->\n"
        ),
        [
            start("s:Envelope", &[("xmlns:s", "urn:soap")]),
            start("s:Body", &[]),
            start("dc:title", &[]),
            text("제목 & more"),
            end("dc:title"),
            start("empty", &[]),
            end("empty"),
            start("also-empty", &[("attr", "1")]),
            end("also-empty"),
            end("s:Body"),
            end("s:Envelope"),
        ]
    );
    assert_eq!(xml::local("s:Envelope"), "Envelope");
    assert_eq!(events(""), []);
    assert_eq!(events("   \n "), []);
    assert_eq!(events("<a/>"), [start("a", &[]), end("a")]);
}

#[test]
fn text_keeps_its_spaces_and_white_space_alone_is_no_text() {
    assert_eq!(
        events("<a> <b>  two  words </b>\n<c>\n</c><d><![CDATA[ ]]></d> tail </a>"),
        [
            start("a", &[]),
            start("b", &[]),
            text("  two  words "),
            end("b"),
            start("c", &[]),
            end("c"),
            start("d", &[]),
            end("d"),
            text(" tail "),
            end("a"),
        ]
    );
}

#[test]
fn attribute_values_are_quoted_either_way() {
    assert_eq!(
        events(
            "<a one=\"it's\" two='say \"hi\"' three = \"R&amp;B &lt;1&gt; &#x41;\" four=''\n\
             five=\"a > b\" xml:lang=\"ko\"/>"
        ),
        [
            start(
                "a",
                &[
                    ("one", "it's"),
                    ("two", "say \"hi\""),
                    ("three", "R&B <1> A"),
                    ("four", ""),
                    ("five", "a > b"),
                    ("xml:lang", "ko"),
                ]
            ),
            end("a"),
        ]
    );
    for bad in [
        "<a b=c/>",
        "<a b/>",
        "<a b=\"1\"c=\"2\"x/>",
        "<a b=\"1/>",
        "<a b='1\"/>",
        "<a =\"1\"/>",
        "<a b=\"1\" / >",
    ] {
        let events = events(bad);
        assert_is!(events.as_slice(), [Err(_)]);
    }
}

#[test]
fn text_outside_the_document_is_passed_over() {
    // Some servers end their documents with a NUL byte.
    assert_eq!(
        events("junk before <a>inside</a> junk after\0"),
        [start("a", &[]), text("inside"), end("a")]
    );
    assert_eq!(
        events("<a/><b/>"),
        [start("a", &[]), end("a"), start("b", &[]), end("b")]
    );
}

#[test]
fn nesting_is_limited() {
    let nested = |depth: usize| format!("{}{}", "<a>".repeat(depth), "</a>".repeat(depth));
    let fine = events(&nested(64));
    assert_eq!(fine.len(), 128);
    assert!(fine.iter().all(Result::is_ok));

    let too_deep = events(&nested(65));
    assert_eq!(too_deep.len(), 65);
    assert!(too_deep[..64].iter().all(Result::is_ok));
    assert_eq!(too_deep[64], Err(XmlError("elements are nested too deep")));

    // However deep: an error, not a stack that runs out.
    let abyss = events(&"<a>".repeat(200_000));
    assert_eq!(abyss.len(), 65);
    // Elements side by side are not nested.
    assert_eq!(
        events(&format!("<r>{}</r>", "<a/>".repeat(1000))).len(),
        2002
    );
}

#[test]
fn attributes_are_limited() {
    let with = |count: usize| {
        let attrs: String = (0..count).map(|n| format!(" a{n}=\"{n}\"")).collect();
        events(&format!("<a{attrs}/>"))
    };
    assert_is!(
        with(64).as_slice(),
        [Ok(Event::Start { attrs, .. }), Ok(Event::End { .. })] if attrs.len() == 64
    );
    assert_eq!(
        with(65),
        [Err(XmlError("an element has too many attributes"))]
    );
}

const BOMB: &str = "<?xml version=\"1.0\"?>
<!DOCTYPE lolz [
 <!ENTITY lol \"lol\">
 <!ELEMENT lolz (#PCDATA)>
 <!-- a comment with a ' and a ]> in it -->
 <!ENTITY lol1 \"&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;\">
 <!ENTITY lol2 \"&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;\">
 <!ENTITY lol3 \"&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;\">
 <!ENTITY lol4 \"&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;\">
 <!ENTITY lol5 \"&lol4;&lol4;&lol4;&lol4;&lol4;&lol4;&lol4;&lol4;&lol4;&lol4;\">
 <!ENTITY lol6 \"&lol5;&lol5;&lol5;&lol5;&lol5;&lol5;&lol5;&lol5;&lol5;&lol5;\">
 <!ENTITY lol7 \"&lol6;&lol6;&lol6;&lol6;&lol6;&lol6;&lol6;&lol6;&lol6;&lol6;\">
 <!ENTITY lol8 \"&lol7;&lol7;&lol7;&lol7;&lol7;&lol7;&lol7;&lol7;&lol7;&lol7;\">
 <!ENTITY lol9 \"&lol8;&lol8;&lol8;&lol8;&lol8;&lol8;&lol8;&lol8;&lol8;&lol8;\">
 <!ENTITY xxe SYSTEM \"file:///etc/passwd\">
 <!ENTITY close ']>'>
]>
<lolz attr=\"&lol9;\">&lol9; &xxe; &lt;</lolz>";

#[test]
fn a_doctype_is_passed_over_and_its_entities_are_never_expanded() {
    // A billion laughs, and a file of this machine: both stay the few bytes they
    // were written as.
    assert_eq!(
        events(BOMB),
        [
            start("lolz", &[("attr", "&lol9;")]),
            text("&lol9; &xxe; <"),
            end("lolz"),
        ]
    );
    assert_eq!(
        events("<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0//EN\" \"http://x/y.dtd\"><a/>"),
        [start("a", &[]), end("a")]
    );
    assert_eq!(events("<!doctype html><a/>"), [start("a", &[]), end("a")]);
    assert_eq!(xml::unescape("&lol9;&xxe;"), "&lol9;&xxe;");
}

#[test]
fn input_that_ends_early_or_does_not_fit_is_an_error_once() {
    for (document, good_events) in [
        ("<a", 0),
        ("<a ", 0),
        ("<a><b>text", 2),
        ("<a>text", 1),
        ("<a attr=\"x", 0),
        ("<a attr=", 0),
        ("<a><!-- never closed", 1),
        ("<a><![CDATA[ never closed", 1),
        ("<?xml version=\"1.0\"", 0),
        ("<!DOCTYPE a [ <!ENTITY b \"c\">", 0),
        ("<!DOCTYPE a", 0),
        ("<a></a", 1),
        ("<a></b>", 1),
        ("<a><b></a></b>", 2),
        ("</a>", 0),
        ("<a/></a>", 2),
        ("< a/>", 0),
        ("<a><", 1),
        ("<a><1/></a>", 1),
        ("<a>x<!ELEMENT y></a>", 2),
        ("<a>&amp;<", 2),
    ] {
        let events = events(document);
        assert_eq!(events.len(), good_events + 1, "{document}: {events:?}");
        assert!(
            events[..good_events].iter().all(Result::is_ok),
            "{document}"
        );
        assert!(events[good_events].is_err(), "{document}: {events:?}");
    }
}

// ---------------------------------------------------------------------------------
// Whatever arrives

const ANSWER_CHUNKED: &str = "HTTP/1.1 200 OK\r\nContent-Type: text/xml\r\n\
    Transfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6;x=y\r\n world\r\n0\r\nT: 1\r\n\r\n";
const ANSWER_PLAIN: &str = "HTTP/1.1 206 Partial Content\r\nContent-Length: 5\r\n\
    Content-Range: bytes 5-9/10\r\nLocation: /elsewhere\r\n\r\n56789";
const SEARCH_ANSWER: &str = "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=1800\r\nEXT:\r\n\
    LOCATION: http://192.168.0.10:8200/rootDesc.xml\r\n\
    ST: urn:schemas-upnp-org:device:MediaServer:1\r\nUSN: uuid:1::upnp:rootdevice\r\n\r\n";

/// Pieces that documents are made of, for putting together at random.
const PIECES: &[&str] = &[
    "<a>",
    "</a>",
    "<b x='1'>",
    "<b y=\"2\" z = '3'>",
    "</b>",
    "<c/>",
    "<item id=\"1\">",
    "</item>",
    "<container id=\"2\" childCount=\"3\">",
    "</container>",
    "<dc:title>",
    "</dc:title>",
    "<res protocolInfo=\"http-get:*:audio/flac:*\">",
    "</res>",
    "text",
    "http://h/a.flac",
    "&amp;",
    "&#x41;",
    "&#0;",
    "&bogus;",
    "&",
    "<!--",
    "-->",
    "<![CDATA[",
    "]]>",
    "<?",
    "?>",
    "<!DOCTYPE x [",
    "<!ENTITY a \"b\">",
    "]>",
    "\"",
    "'",
    "=",
    " ",
    "\n",
    "<",
    ">",
    "/",
    "한글",
    "\0",
    "\u{feff}",
    "HTTP/1.1 200 OK\r\n",
    "Content-Length: 3\r\n",
    "Transfer-Encoding: chunked\r\n",
    "\r\n",
    "3\r\n",
    "0\r\n",
    "ffffffffffffffff\r\n",
    "LOCATION: http://h/d.xml\r\n",
    "ST: MediaServer\r\n",
    ":",
    "1:02:03.5",
    "../",
    "?",
    "#",
];

fn damaged(rng: &mut fastrand::Rng, sample: &[u8]) -> Vec<u8> {
    let mut bytes = sample.to_vec();
    for _ in 0..rng.usize(1..=4) {
        if bytes.is_empty() {
            break;
        }
        let at = rng.usize(..bytes.len());
        match rng.u8(..6) {
            0 => bytes[at] = rng.u8(..),
            1 => bytes.truncate(at),
            2 => {
                let end = (at + rng.usize(1..40)).min(bytes.len());
                bytes.drain(at..end);
            }
            3 => {
                let piece = PIECES[rng.usize(..PIECES.len())];
                bytes.splice(at..at, piece.bytes());
            }
            4 => {
                let end = (at + rng.usize(1..60)).min(bytes.len());
                let copy = bytes[at..end].to_vec();
                bytes.splice(at..at, copy);
            }
            _ => bytes[at] = *b"<>&\"'/=;:\r\n\0 ".get(rng.usize(..13)).unwrap(),
        }
    }
    bytes
}

#[test]
fn damaged_and_random_input_never_panics() {
    let base = base();
    let didl_sample = didl(SONG);
    let description = device("/ctl");
    let samples: [&[u8]; 7] = [
        didl_sample.as_bytes(),
        description.as_bytes(),
        BOMB.as_bytes(),
        ANSWER_CHUNKED.as_bytes(),
        ANSWER_PLAIN.as_bytes(),
        SEARCH_ANSWER.as_bytes(),
        SEARCH.as_bytes(),
    ];
    // The samples themselves are what they claim to be.
    assert_eq!(upnp::parse_didl(&didl_sample, &base).len(), 1);
    assert_is!(http::parse_response(samples[3], 100), Ok((200, body)) if body == b"hello world");
    assert_is!(http::parse_response(samples[4], 100), Ok((206, body)) if body == b"56789");
    assert_is!(http::parse_response(samples[4], 4), Err(NetError::TooLarge));
    assert!(ssdp::parse_reply(samples[5]).is_some());

    let mut rng = fastrand::Rng::with_seed(0x0e0e_2026);
    let (mut events, mut entries, mut answers, mut replies) = (0usize, 0usize, 0usize, 0usize);
    for round in 0..6000 {
        let bytes = match round % 3 {
            0 => {
                let sample = samples[rng.usize(..samples.len())];
                damaged(&mut rng, sample)
            }
            1 => {
                let mut soup = String::new();
                for _ in 0..rng.usize(1..40) {
                    soup.push_str(PIECES[rng.usize(..PIECES.len())]);
                }
                soup.into_bytes()
            }
            _ => (0..rng.usize(..300)).map(|_| rng.u8(..)).collect(),
        };
        let text = String::from_utf8_lossy(&bytes);

        let mut reader = Reader::new(&text);
        let mut count = 0;
        while let Some(event) = reader.next_event() {
            count += 1;
            // Every event takes at least a byte of the text (the end of `<a/>` two).
            assert!(
                count <= 2 * text.len() + 1,
                "the reader does not end: {text:?}"
            );
            if event.is_err() {
                assert_eq!(reader.next_event(), None);
            }
        }
        events += count;

        let found = upnp::parse_didl(&text, &base);
        assert!(found.len() <= 1000);
        for entry in &found {
            let (id, title) = match entry {
                Entry::Folder(folder) => (&folder.id, &folder.title),
                Entry::Item(item) => {
                    for resource in &item.resources {
                        assert!(resource.url.starts_with("http://"), "{resource:?}");
                    }
                    upnp::best_resource(item);
                    (&item.id, &item.title)
                }
            };
            assert!(!id.is_empty() && id.chars().count() <= 1024);
            assert!(!title.is_empty() && title.chars().count() <= 1024);
        }
        entries += found.len();

        if let Ok((status, body)) = http::parse_response(&bytes, 64) {
            assert!((200..600).contains(&status), "{status}");
            assert!(body.len() <= 64);
            answers += 1;
        }
        if let Some(found) = ssdp::parse_reply(&bytes) {
            assert!(Url::parse(&found.location).is_ok());
            replies += 1;
        }
        if let Ok(url) = Url::parse(&text) {
            assert_eq!(Url::parse(&url.to_string()).unwrap(), url, "{text:?}");
        }
        if let Ok(url) = base.join(&text) {
            assert!(url.path.starts_with('/'), "{text:?}");
            assert!(url.path.is_ascii(), "{text:?}");
        }
        upnp::parse_duration(&text);
        xml::unescape(&text);
        assert_eq!(xml::unescape(&xml::escape(&text)), text);
    }
    // The damage is not so thorough that nothing gets anywhere: each reader has also
    // been deep inside its input.
    assert!(events > 10_000, "{events} events");
    assert!(entries > 20, "{entries} entries");
    assert!(answers > 100, "{answers} answers");
    assert!(replies > 30, "{replies} replies");
}
