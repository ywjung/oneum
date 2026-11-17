//! Talking to media servers on the local network (DLNA/UPnP): finding them (`ssdp`),
//! asking what they hold (`upnp`) and fetching it (`http`), with the little XML reader
//! the three need (`xml`).
//!
//! Only the standard library's sockets are used, so the same code runs on every system
//! the app is built for. Media servers in a home speak plain `http://`, and nothing
//! else is accepted from them; the folders of a NAS (`dav`) and the lyrics service may
//! be reached over TLS (`tls`), and a NAS may ask for a name and a password (`auth`).
//!
//! Whatever arrives is taken for what any device on the network could have sent: every
//! length is limited, nothing is expanded or followed without a bound, and damaged
//! input ends in an error (or in fewer results), never in a panic.
mod auth;
pub mod dav;
pub mod folders;
pub mod http;
pub mod ssdp;
pub mod tls;
pub mod upnp;
pub mod xml;

/// Why something from the network could not be had.
#[derive(Debug)]
pub enum NetError {
    /// Not an `http://host[:port]/path` address.
    BadAddress,
    Connect(std::io::Error),
    Io(std::io::Error),
    Timeout,
    /// More than the caller allowed.
    TooLarge,
    /// The server answered with this status.
    Status(u16),
    /// The answer is not what the protocol says (what was wrong).
    BadResponse(&'static str),
    /// A UPnP error the server reported (its description).
    Upnp(String),
    /// The server's certificate is not one that is trusted: no authority stands
    /// behind it (a NAS that made its own), or it is for another name, or it has
    /// run out. With the fingerprint of what the server showed.
    Untrusted {
        fingerprint: String,
    },
}

impl std::fmt::Display for NetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetError::BadAddress => write!(f, "not an http:// address"),
            NetError::Connect(error) => write!(f, "could not connect: {error}"),
            NetError::Io(error) => write!(f, "the connection failed: {error}"),
            NetError::Timeout => write!(f, "the server did not answer in time"),
            NetError::TooLarge => write!(f, "the answer is larger than allowed"),
            NetError::Status(status) => write!(f, "the server answered with status {status}"),
            NetError::BadResponse(what) => write!(f, "unexpected answer: {what}"),
            NetError::Upnp(description) => write!(f, "the server reported: {description}"),
            NetError::Untrusted { .. } => write!(f, "the server's certificate is not trusted"),
        }
    }
}

impl std::error::Error for NetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            NetError::Connect(error) | NetError::Io(error) => Some(error),
            _ => None,
        }
    }
}

/// The first `max` characters of `text` (titles and ids from a server are kept short:
/// a list row shows a line, not a megabyte).
pub(crate) fn capped(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((end, _)) => &text[..end],
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capped_cuts_between_characters() {
        assert_eq!(capped("가나다라", 2), "가나");
        assert_eq!(capped("abc", 3), "abc");
        assert_eq!(capped("abc", 10), "abc");
        assert_eq!(capped("", 0), "");
    }

    #[test]
    fn errors_say_what_happened() {
        assert_eq!(
            NetError::Status(404).to_string(),
            "the server answered with status 404"
        );
        assert_eq!(
            NetError::Upnp("No such object".into()).to_string(),
            "the server reported: No such object"
        );
        let refused = std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
        let error = NetError::Connect(refused);
        assert!(std::error::Error::source(&error).is_some());
        assert!(std::error::Error::source(&NetError::Timeout).is_none());
    }
}
