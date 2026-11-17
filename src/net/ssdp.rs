//! Finding the media servers on the local network (SSDP, the discovery part of UPnP).
//! A search is sent to the multicast group every UPnP device listens on; each media
//! server answers the sender with the address of its description.
//!
//! The answers are datagrams anyone on the network can send, so each is checked
//! before it counts, and what it names is only an address to ask later (`upnp`).
use super::http::Url;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

/// Where UPnP devices listen for searches.
const GROUP: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(239, 255, 255, 250)), 1900);
/// What is searched for. A server of a later version answers to this one too.
const SEARCH_TARGET: &str = "urn:schemas-upnp-org:device:MediaServer:1";
/// An answer is a few hundred bytes. A larger datagram is something else.
const MAX_DATAGRAM_BYTES: usize = 8 * 1024;
/// More servers than a home network has; a flood of answers ends here.
const MAX_FOUND: usize = 64;
/// A datagram can be lost without a word, so the search is sent a second time, this
/// long after the first.
const REPEAT_AFTER: Duration = Duration::from_millis(300);
/// The socket failing this often in a row is not going to work; the search ends
/// rather than fail over and over until the time is up.
const MAX_ERRORS: u32 = 8;
/// The longest USN kept.
const MAX_USN_CHARS: usize = 1024;

/// A media server that answered a search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The device description's address.
    pub location: String,
    pub usn: String,
}

/// Asks the local network for media servers and collects the answers that arrive
/// within `wait`. Servers may take up to two seconds to answer (the `MX` of the
/// search), so a shorter wait can miss some.
///
/// Never fails: when the network cannot be asked (no network, no permission) the
/// list is empty, or holds what had been found until then.
pub fn search(wait: Duration) -> Vec<Found> {
    search_at(GROUP, wait)
}

/// The same, sent to `to` (tests use a socket on 127.0.0.1 instead of the multicast
/// group).
pub fn search_at(to: SocketAddr, wait: Duration) -> Vec<Found> {
    let mut found = Vec::new();
    if let Err(error) = collect(to, wait, &mut found) {
        tracing::debug!(%error, "the search for media servers ended early");
    }
    found
}

fn collect(to: SocketAddr, wait: Duration, found: &mut Vec<Found>) -> io::Result<()> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    // As UPnP says: far enough for the home network, not out into the world.
    socket.set_multicast_ttl_v4(2)?;
    let request = format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\n\
         MX: 2\r\nST: {SEARCH_TARGET}\r\n\r\n"
    );
    socket.send_to(request.as_bytes(), to)?;
    let started = Instant::now();
    let mut repeated = false;
    let mut errors = 0;
    // One byte more than an answer may have: a datagram that fills it was too large.
    let mut datagram = vec![0u8; MAX_DATAGRAM_BYTES + 1];
    while found.len() < MAX_FOUND {
        let elapsed = started.elapsed();
        if elapsed >= wait {
            break;
        }
        let mut until = wait.saturating_sub(elapsed);
        if !repeated {
            if elapsed >= REPEAT_AFTER {
                repeated = true;
                socket.send_to(request.as_bytes(), to)?;
                continue;
            }
            until = until.min(REPEAT_AFTER.saturating_sub(elapsed));
        }
        // A timeout of zero is refused by the system.
        socket.set_read_timeout(Some(until.max(Duration::from_millis(1))))?;
        match socket.recv_from(&mut datagram) {
            Ok((len, from)) => {
                errors = 0;
                let reply = datagram
                    .get(..len)
                    .and_then(parse_reply)
                    .filter(|reply| is_at(&reply.location, from.ip()));
                if let Some(reply) = reply
                    && !found.iter().any(|known| known.location == reply.location)
                {
                    found.push(reply);
                }
            }
            // The wait is over (reported as either kind, by system), or a signal
            // came in between.
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => {
                // Windows reports here that an earlier datagram found nobody at its
                // address, or that one arrived that was larger than the buffer.
                errors += 1;
                if errors == MAX_ERRORS {
                    return Err(error);
                }
            }
        }
    }
    Ok(())
}

/// Whether `location` is an address of the device that sent the answer. Anyone on
/// the network can answer a search, and the description is fetched without being
/// asked for: an answer that names another device's address (this computer's own, the
/// router's) would have the app make a request there on the sender's behalf.
fn is_at(location: &str, sender: IpAddr) -> bool {
    Url::parse(location)
        .ok()
        .and_then(|url| url.host.trim_matches(['[', ']']).parse::<IpAddr>().ok())
        .is_some_and(|host| host == sender)
}

/// Reads an answer to the search: `HTTP/1.1 200` with an `ST` that mentions
/// `MediaServer` and a `LOCATION` that is an `http://` address. `None` for every
/// other datagram. The location is returned in its normal form (`Url`'s), so that a
/// server that writes it in two ways is still found once.
pub fn parse_reply(datagram: &[u8]) -> Option<Found> {
    if datagram.len() > MAX_DATAGRAM_BYTES {
        return None;
    }
    let text = String::from_utf8_lossy(datagram);
    let mut lines = text.lines();
    let after = lines.next()?.strip_prefix("HTTP/1.1 200")?;
    if !(after.is_empty() || after.starts_with(' ')) {
        return None; // "HTTP/1.1 2000"
    }
    let mut location = None;
    let mut usn = None;
    let mut media_server = false;
    for line in lines {
        if line.trim().is_empty() {
            break; // the end of the headers
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let (name, value) = (name.trim(), value.trim());
        if name.eq_ignore_ascii_case("st") {
            media_server = value.to_ascii_lowercase().contains("mediaserver");
        } else if name.eq_ignore_ascii_case("location") {
            location.get_or_insert(value);
        } else if name.eq_ignore_ascii_case("usn") {
            usn.get_or_insert(value);
        }
    }
    if !media_server {
        return None;
    }
    Some(Found {
        location: Url::parse(location?).ok()?.to_string(),
        usn: super::capped(usn.unwrap_or_default(), MAX_USN_CHARS).to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPLY: &str = "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=1800\r\nEXT:\r\n\
        LOCATION: http://192.168.0.10:8200/rootDesc.xml\r\nSERVER: Linux DLNADOC/1.50 UPnP/1.0\r\n\
        ST: urn:schemas-upnp-org:device:MediaServer:1\r\n\
        USN: uuid:4d696e69-444c-164e-9d41-001122334455::urn:schemas-upnp-org:device:MediaServer:1\r\n\r\n";

    #[test]
    fn reads_a_reply() {
        assert_eq!(
            parse_reply(REPLY.as_bytes()),
            Some(Found {
                location: "http://192.168.0.10:8200/rootDesc.xml".into(),
                usn: "uuid:4d696e69-444c-164e-9d41-001122334455::urn:schemas-upnp-org:device:MediaServer:1".into(),
            })
        );
    }

    #[test]
    fn header_names_are_read_in_any_case() {
        let reply = REPLY
            .replace("LOCATION:", "Location:")
            .replace("ST:", "st:")
            .replace("USN:", "Usn:");
        assert!(parse_reply(reply.as_bytes()).is_some());
    }

    #[test]
    fn other_datagrams_are_not_replies() {
        let not_ok = REPLY.replace("200 OK", "404 Not Found");
        let renderer = REPLY.replace("MediaServer", "MediaRenderer");
        let secure = REPLY.replace("http://", "https://");
        let no_location = REPLY.replace("LOCATION:", "X-LOCATION:");
        let notify = REPLY.replace("HTTP/1.1 200 OK", "NOTIFY * HTTP/1.1");
        let status_2000 = REPLY.replace("200 OK", "2000 OK");
        for datagram in [
            not_ok.as_str(),
            renderer.as_str(),
            secure.as_str(),
            no_location.as_str(),
            notify.as_str(),
            status_2000.as_str(),
            "",
            "HTTP/1.1 200 OK",
        ] {
            assert_eq!(parse_reply(datagram.as_bytes()), None, "{datagram}");
        }
        let mut large = REPLY.as_bytes().to_vec();
        large.resize(MAX_DATAGRAM_BYTES + 1, b' ');
        assert_eq!(parse_reply(&large), None);
    }

    #[test]
    fn the_location_is_normalized() {
        let reply = REPLY.replace(
            "http://192.168.0.10:8200/rootDesc.xml",
            "HTTP://NAS.local:80/desc.xml#top",
        );
        let found = parse_reply(reply.as_bytes()).unwrap();
        assert_eq!(found.location, "http://nas.local/desc.xml");
    }
    #[test]
    fn an_answer_counts_only_for_its_senders_own_address() {
        let sender: IpAddr = "192.168.0.10".parse().unwrap();
        assert!(is_at("http://192.168.0.10:8200/rootDesc.xml", sender));
        // Another device's address, this computer's own, a name.
        assert!(!is_at("http://192.168.0.1/apply.cgi?reboot=1", sender));
        assert!(!is_at("http://127.0.0.1:631/printers", sender));
        assert!(!is_at("http://nas.local:8200/rootDesc.xml", sender));
        assert!(!is_at("not an address", sender));
    }
}
