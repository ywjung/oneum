//! A folder of a NAS that is shared over WebDAV: what it holds.
//!
//! One question is asked (`PROPFIND` with `Depth: 1`), and its answer names the
//! folder's files and folders with their addresses. Reading a file is an ordinary
//! `GET`, as for a media server's file (`http::open_web`). Nothing is ever written.
//!
//! The answer comes from the network: its size, the number of entries taken from it
//! and the length of a name are limited, and an address in it that leads away from
//! the server it came from is left out.
use super::http::{self, Access, Limits, Url};
use super::xml::{self, Event};
use super::{NetError, capped};

/// The largest answer to a listing that is read…
const MAX_LISTING_BYTES: u64 = 16 << 20;
/// …and the most entries taken from it.
pub const MAX_ENTRIES: usize = 10_000;
/// A name is cut at this many characters.
const MAX_NAME_CHARS: usize = 255;

const QUESTION: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
<D:propfind xmlns:D=\"DAV:\"><D:prop>\
<D:resourcetype/><D:getcontentlength/>\
</D:prop></D:propfind>";

/// A file or a folder in a folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Its name, as it is written in its address (decoded).
    pub name: String,
    /// Its address, as it is asked for.
    pub url: String,
    pub folder: bool,
    /// A file's size, when the server says it.
    pub size: Option<u64>,
}

/// What the folder at `url` holds: folders first, each kind by name (numbers in names
/// by their value, so that "2" comes before "10").
pub fn list(url: &str, access: &Access<'_>) -> Result<Vec<Entry>, NetError> {
    let base = Url::parse_web(url)?;
    let headers = [
        ("Depth", "1"),
        ("Content-Type", "application/xml; charset=utf-8"),
    ];
    let (status, body) = http::send(
        "PROPFIND",
        url,
        &headers,
        QUESTION.as_bytes(),
        &Limits::new(MAX_LISTING_BYTES),
        access,
    )?;
    if status != 207 {
        return Err(NetError::Status(status));
    }
    let mut entries = read_listing(&String::from_utf8_lossy(&body), &base)?;
    entries.sort_by(|a, b| {
        b.folder
            .cmp(&a.folder)
            .then_with(|| natural_key(&a.name).cmp(&natural_key(&b.name)))
    });
    Ok(entries)
}

/// A name as it is ordered: lower case, with every run of digits as its number.
fn natural_key(name: &str) -> Vec<(u8, u64, String)> {
    let mut key = Vec::new();
    let mut chars = name.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            let mut number = 0u64;
            while let Some(d) = chars.peek().and_then(|c| c.to_digit(10)) {
                number = number.saturating_mul(10).saturating_add(u64::from(d));
                chars.next();
            }
            key.push((0, number, String::new()));
        } else {
            let mut text = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_digit() {
                    break;
                }
                text.extend(c.to_lowercase());
                chars.next();
            }
            key.push((1, 0, text));
        }
    }
    key
}

/// `%41` and the like in one segment of a path, as the bytes they stand for (what is
/// no valid escape stays as written; what is no UTF-8 afterwards is replaced).
fn decoded(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        match (
            bytes[i],
            bytes.get(i + 1).copied(),
            bytes.get(i + 2).copied(),
        ) {
            (b'%', Some(a), Some(b)) if hex(a).is_some() && hex(b).is_some() => {
                out.push((hex(a).unwrap_or(0) * 16 + hex(b).unwrap_or(0)) as u8);
                i += 3;
            }
            (byte, ..) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The path of an address without its query, and without the slash a folder's ends
/// with.
fn trimmed(path: &str) -> &str {
    let path = path.split('?').next().unwrap_or(path);
    path.strip_suffix('/').unwrap_or(path)
}

/// The entries of a `multistatus` answer to a listing of `base`, as the server orders
/// them. The folder itself (which the answer names too) is left out, and so is
/// everything that is not directly in it or not on the same server.
fn read_listing(text: &str, base: &Url) -> Result<Vec<Entry>, NetError> {
    #[derive(Default)]
    struct Response {
        href: Option<String>,
        folder: bool,
        size: Option<u64>,
    }
    let mut reader = xml::Reader::new(text);
    let mut entries = Vec::new();
    let mut current: Option<Response> = None;
    // The element whose text is being read ("href", "getcontentlength").
    let mut reading: Option<&'static str> = None;
    let mut text_so_far = String::new();
    let mut seen_multistatus = false;
    let own = decoded(trimmed(&base.path));
    while let Some(event) = reader.next_event() {
        let event = event.map_err(|_| NetError::BadResponse("the listing is not XML"))?;
        match event {
            Event::Start { name, .. } => match xml::local(&name) {
                "multistatus" => seen_multistatus = true,
                "response" => current = Some(Response::default()),
                "href" if current.is_some() => {
                    reading = Some("href");
                    text_so_far.clear();
                }
                "getcontentlength" if current.is_some() => {
                    reading = Some("getcontentlength");
                    text_so_far.clear();
                }
                "collection" => {
                    if let Some(response) = current.as_mut() {
                        response.folder = true;
                    }
                }
                _ => {}
            },
            Event::Text(piece) => {
                if reading.is_some() && text_so_far.len() < 8192 {
                    text_so_far.push_str(&piece);
                }
            }
            Event::End { name } => match (xml::local(&name), reading) {
                ("href", Some("href")) => {
                    if let Some(response) = current.as_mut() {
                        // The first one names the entry.
                        response
                            .href
                            .get_or_insert_with(|| text_so_far.trim().to_string());
                    }
                    reading = None;
                }
                ("getcontentlength", Some("getcontentlength")) => {
                    if let Some(response) = current.as_mut() {
                        response.size = text_so_far.trim().parse().ok();
                    }
                    reading = None;
                }
                ("response", _) => {
                    reading = None;
                    let Some(response) = current.take() else {
                        continue;
                    };
                    let Some(url) = response.href.and_then(|href| base.join(&href).ok()) else {
                        continue;
                    };
                    // On this server, and directly in this folder.
                    if url.origin() != base.origin() {
                        continue;
                    }
                    let path = decoded(trimmed(&url.path));
                    let Some(name) = path
                        .strip_prefix(own.as_str())
                        .and_then(|rest| rest.strip_prefix('/'))
                        .filter(|name| !name.is_empty() && !name.contains('/'))
                    else {
                        continue;
                    };
                    if name.starts_with('.') || name.chars().any(char::is_control) {
                        continue;
                    }
                    let mut address = url.to_string();
                    if response.folder && !trimmed_ends_with_slash(&address) {
                        address.push('/');
                    }
                    entries.push(Entry {
                        name: capped(name, MAX_NAME_CHARS).to_string(),
                        url: address,
                        folder: response.folder,
                        size: response.size.filter(|_| !response.folder),
                    });
                    if entries.len() >= MAX_ENTRIES {
                        break;
                    }
                }
                _ => {}
            },
        }
    }
    if !seen_multistatus {
        return Err(NetError::BadResponse("not the answer to a listing"));
    }
    Ok(entries)
}

fn trimmed_ends_with_slash(address: &str) -> bool {
    address.split('?').next().unwrap_or(address).ends_with('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse_web("https://nas.local:5006/music/My%20Albums/").unwrap()
    }

    fn listing(responses: &str) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
             <D:multistatus xmlns:D=\"DAV:\">{responses}</D:multistatus>"
        )
    }

    fn response(href: &str, folder: bool, size: Option<u64>) -> String {
        let kind = if folder {
            "<D:resourcetype><D:collection/></D:resourcetype>"
        } else {
            "<D:resourcetype/>"
        };
        let size = size.map_or(String::new(), |n| {
            format!("<D:getcontentlength>{n}</D:getcontentlength>")
        });
        format!(
            "<D:response><D:href>{href}</D:href><D:propstat><D:prop>{kind}{size}</D:prop>\
             <D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"
        )
    }

    #[test]
    fn a_listing_gives_what_is_in_the_folder() {
        let text = listing(
            &[
                response("/music/My%20Albums/", true, None),
                response("/music/My%20Albums/Abbey%20Road/", true, None),
                response(
                    "/music/My%20Albums/01%20%EB%B0%A4%ED%8E%B8%EC%A7%80.flac",
                    false,
                    Some(1234),
                ),
                // A whole address, as some servers write it.
                response(
                    "https://nas.local:5006/music/My%20Albums/b.mp3",
                    false,
                    Some(5),
                ),
                // Left out: deeper, elsewhere, another server, hidden, the parent.
                response("/music/My%20Albums/Abbey%20Road/01.flac", false, Some(1)),
                response("/other/x.flac", false, Some(1)),
                response(
                    "https://elsewhere.example/music/My%20Albums/c.flac",
                    false,
                    Some(1),
                ),
                response(
                    "http://nas.local:5006/music/My%20Albums/d.flac",
                    false,
                    Some(1),
                ),
                response("/music/My%20Albums/.DS_Store", false, Some(1)),
                response("/music/", true, None),
            ]
            .concat(),
        );
        let entries = read_listing(&text, &base()).unwrap();
        assert_eq!(
            entries,
            vec![
                Entry {
                    name: "Abbey Road".into(),
                    url: "https://nas.local:5006/music/My%20Albums/Abbey%20Road/".into(),
                    folder: true,
                    size: None,
                },
                Entry {
                    name: "01 밤편지.flac".into(),
                    url: "https://nas.local:5006/music/My%20Albums/01%20%EB%B0%A4%ED%8E%B8%EC%A7%80.flac"
                        .into(),
                    folder: false,
                    size: Some(1234),
                },
                Entry {
                    name: "b.mp3".into(),
                    url: "https://nas.local:5006/music/My%20Albums/b.mp3".into(),
                    folder: false,
                    size: Some(5),
                },
            ]
        );
    }

    /// Other servers write the same answer with other prefixes, or with none.
    #[test]
    fn prefixes_do_not_matter() {
        let text = "<multistatus xmlns=\"DAV:\"><response><href>/music/My%20Albums/x.wv</href>\
                    <propstat><prop><lp1:resourcetype xmlns:lp1=\"DAV:\"/>\
                    <lp1:getcontentlength xmlns:lp1=\"DAV:\"> 77 </lp1:getcontentlength></prop>\
                    </propstat></response><d:response xmlns:d=\"DAV:\"><d:href>/music/My%20Albums/sub</d:href>\
                    <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop>\
                    </d:propstat></d:response></multistatus>";
        let entries = read_listing(text, &base()).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(
            (entries[0].name.as_str(), entries[0].size),
            ("x.wv", Some(77))
        );
        assert!(entries[1].folder);
        assert_eq!(
            entries[1].url,
            "https://nas.local:5006/music/My%20Albums/sub/"
        );
    }

    #[test]
    fn what_is_no_listing_is_an_error_and_damage_gives_fewer_entries() {
        assert!(read_listing("<html><body>Login</body></html>", &base()).is_err());
        assert!(read_listing("not xml <", &base()).is_err());
        assert!(read_listing("", &base()).is_err());
        // An entry without an address, a size that is no number.
        let text = listing(
            &[
                "<D:response><D:propstat/></D:response>".to_string(),
                "<D:response><D:href>/music/My%20Albums/a.flac</D:href><D:propstat><D:prop>\
             <D:getcontentlength>many</D:getcontentlength></D:prop></D:propstat></D:response>"
                    .to_string(),
            ]
            .concat(),
        );
        let entries = read_listing(&text, &base()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].size, None);
    }

    #[test]
    fn names_are_ordered_as_a_person_orders_them() {
        let mut names = vec![
            "10 b.flac",
            "2 a.flac",
            "1.flac",
            "B",
            "a10",
            "a9",
            "가",
            "A",
        ];
        names.sort_by_key(|name| natural_key(name));
        assert_eq!(
            names,
            [
                "1.flac",
                "2 a.flac",
                "10 b.flac",
                "A",
                "a9",
                "a10",
                "B",
                "가"
            ]
        );
        assert_eq!(decoded("a%20b%2Fc%zz%4"), "a b/c%zz%4");
    }
}
