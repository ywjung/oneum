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
    listing(url, access).map(|listing| listing.entries)
}

/// What a folder holds, and the folder's own address as its server names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub entries: Vec<Entry>,
    /// The address of the folder itself. It is the one that was asked, unless the
    /// server sent the request on to another of its addresses: what is in the folder
    /// lies under this one.
    pub place: String,
}

/// [`list`], with the folder's own address.
pub fn listing(url: &str, access: &Access<'_>) -> Result<Listing, NetError> {
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
    let mut listing = read_listing(&String::from_utf8_lossy(&body), &base)?;
    listing.entries.sort_by(|a, b| {
        b.folder
            .cmp(&a.folder)
            .then_with(|| natural_key(&a.name).cmp(&natural_key(&b.name)))
    });
    Ok(listing)
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
fn read_listing(text: &str, base: &Url) -> Result<Listing, NetError> {
    #[derive(Default)]
    struct Response {
        href: Option<String>,
        folder: bool,
        size: Option<u64>,
    }
    let mut reader = xml::Reader::new(text);
    // What is listed: its address, its path as a place, whether it is a folder, its size.
    let mut found: Vec<(Url, String, bool, Option<u64>)> = Vec::new();
    let mut current: Option<Response> = None;
    // The element whose text is being read ("href", "getcontentlength").
    let mut reading: Option<&'static str> = None;
    let mut text_so_far = String::new();
    let mut seen_multistatus = false;
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
                    let Some(url) = response
                        .href
                        .and_then(|href| base.join(&path_of(&href)).ok())
                    else {
                        continue;
                    };
                    let path = decoded(trimmed(&url.path));
                    found.push((url, path, response.folder, response.size));
                    // (The folder itself is one of them.)
                    if found.len() > MAX_ENTRIES {
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
    // The folder itself is among what is listed. A server that was asked at one
    // address and answered for another (it sent the request on) names the folder as
    // it knows it, and what is in the folder is told by that name: the folder whose
    // path all the others begin with. (Letters may come in another case than they
    // were typed in, from a server whose disk does not tell them apart.)
    let begins = |path: &str, with: &str| {
        path.len() >= with.len()
            && path.is_char_boundary(with.len())
            && path[..with.len()].eq_ignore_ascii_case(with)
    };
    let asked = decoded(trimmed(&base.path));
    // (Named itself, or with something in it: the folder that was asked. A server
    // may leave the folder's own entry out; what it lists is in the folder still.)
    let is_asked = found.iter().any(|(_, path, ..)| {
        begins(path, &asked) && matches!(path.as_bytes().get(asked.len()), None | Some(b'/'))
    });
    let own = if is_asked {
        asked
    } else {
        found
            .iter()
            .filter(|(_, _, folder, _)| *folder)
            .map(|(_, path, ..)| path)
            .min_by_key(|path| path.len())
            .filter(|own| found.iter().all(|(_, path, ..)| begins(path, own)))
            .cloned()
            .unwrap_or(asked)
    };
    let mut place = base.clone();
    let mut entries = Vec::new();
    for (url, path, folder, size) in found {
        if path.eq_ignore_ascii_case(&own) {
            place = url;
            continue;
        }
        // Directly in this folder.
        let Some(name) = begins(&path, &own)
            .then(|| &path[own.len()..])
            .and_then(|rest| rest.strip_prefix('/'))
            .filter(|name| !name.is_empty() && !name.contains('/'))
        else {
            continue;
        };
        if name.starts_with('.') || name.chars().any(char::is_control) {
            continue;
        }
        let mut address = url.to_string();
        if folder && !trimmed_ends_with_slash(&address) {
            address.push('/');
        }
        entries.push(Entry {
            name: capped(name, MAX_NAME_CHARS).to_string(),
            url: address,
            folder,
            size: size.filter(|_| !folder),
        });
    }
    let mut place = place.to_string();
    if !trimmed_ends_with_slash(&place) {
        place.push('/');
    }
    Ok(Listing { entries, place })
}

/// The path an entry is named by. A server may name its entries by whole addresses,
/// and one that stands behind another (which speaks TLS for it) writes its own name
/// and `http://` there, not what it is reached by: only the path is taken, on the
/// server that was asked. A space that was left as it is gets its escape.
fn path_of(href: &str) -> String {
    let href = href.trim();
    let path = match href.split_once("://") {
        Some((scheme, rest)) if scheme.bytes().all(|b| b.is_ascii_alphabetic()) => {
            rest.find('/').map_or("/", |at| &rest[at..])
        }
        _ => href,
    };
    path.replace(' ', "%20")
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
                // A server behind another one (which speaks TLS for it) writes the
                // name and the scheme it knows itself by: the path is what counts.
                response(
                    "http://nas-inside:5005/music/My%20Albums/d.flac",
                    false,
                    Some(7),
                ),
                // A space left as it is, and the folder's own name in another case
                // (a server whose disk does not tell the cases apart).
                response("/music/my%20albums/e f.flac", false, Some(9)),
                // Left out: deeper, elsewhere, hidden, the parent.
                response("/music/My%20Albums/Abbey%20Road/01.flac", false, Some(1)),
                response("/other/x.flac", false, Some(1)),
                response("https://elsewhere.example/other/c.flac", false, Some(1)),
                response("/music/My%20Albums/.DS_Store", false, Some(1)),
                response("/music/", true, None),
            ]
            .concat(),
        );
        let entries = read_listing(&text, &base()).unwrap().entries;
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
                // (Asked for on the server that was asked for the listing.)
                Entry {
                    name: "d.flac".into(),
                    url: "https://nas.local:5006/music/My%20Albums/d.flac".into(),
                    folder: false,
                    size: Some(7),
                },
                Entry {
                    name: "e f.flac".into(),
                    url: "https://nas.local:5006/music/my%20albums/e%20f.flac".into(),
                    folder: false,
                    size: Some(9),
                },
            ]
        );
        assert_eq!(path_of("http://inside/a b/c"), "/a%20b/c");
        assert_eq!(path_of("https://inside"), "/");
        assert_eq!(path_of("/a/b"), "/a/b");
        assert_eq!(path_of("b c"), "b%20c");
    }

    /// A server that was asked at one address and answered for another (it sent the
    /// request on) lists what is in the folder as it names it: that is what is
    /// shown, and the folder's own address is the server's.
    #[test]
    fn a_folder_that_the_server_names_otherwise_is_read_by_that_name() {
        let text = listing(
            &[
                response("/remote/dav/files/me/", true, None),
                response("/remote/dav/files/me/Albums/", true, None),
                response("/remote/dav/files/me/a.flac", false, Some(3)),
            ]
            .concat(),
        );
        let found = read_listing(&text, &base()).unwrap();
        assert_eq!(found.place, "https://nas.local:5006/remote/dav/files/me/");
        let names: Vec<&str> = found.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Albums", "a.flac"]);
        assert_eq!(
            found.entries[1].url,
            "https://nas.local:5006/remote/dav/files/me/a.flac"
        );
        // Asked and answered at the same address: that address, and an empty folder
        // is an empty folder.
        let empty = listing(&response("/music/My%20Albums/", true, None));
        let found = read_listing(&empty, &base()).unwrap();
        assert_eq!(found.place, "https://nas.local:5006/music/My%20Albums/");
        assert!(found.entries.is_empty());
        // A server that leaves the folder's own entry out: what it lists is in the
        // folder that was asked, also where that is a single folder.
        let child = listing(&response("/music/My%20Albums/Only/", true, None));
        let found = read_listing(&child, &base()).unwrap();
        assert_eq!(found.place, "https://nas.local:5006/music/My%20Albums/");
        assert_eq!(found.entries.len(), 1);
        assert_eq!(found.entries[0].name, "Only");
        // Entries that have no folder in common are not made into one.
        let apart = listing(
            &[
                response("/one/", true, None),
                response("/two/b.flac", false, Some(1)),
            ]
            .concat(),
        );
        let found = read_listing(&apart, &base()).unwrap();
        assert_eq!(found.place, "https://nas.local:5006/music/My%20Albums/");
        assert!(found.entries.is_empty());
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
        let entries = read_listing(text, &base()).unwrap().entries;
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
        let entries = read_listing(&text, &base()).unwrap().entries;
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
