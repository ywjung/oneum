//! Answering a server that asks who is there (HTTP status 401): Digest where the
//! server offers it, which proves the password without sending it, and Basic
//! otherwise, which sends it (over `https://` inside the encrypted connection; over
//! `http://` as good as in the open, which the listener is told when such a folder is
//! added).
//!
//! What a server asked is remembered for as long as the app runs, and the next
//! request to it is answered before it asks: a song read from a share is many
//! requests, and each would otherwise be made twice. What is remembered is the
//! server's question, never the password.
use md5::{Digest as _, Md5};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{LazyLock, Mutex};

/// The longest challenge that is read, and how many of its fields.
const MAX_CHALLENGE_BYTES: usize = 4096;
const MAX_FIELDS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hash {
    Md5,
    Sha256,
}

impl Hash {
    fn of(self, text: &str) -> String {
        let bytes: Vec<u8> = match self {
            Hash::Md5 => Md5::digest(text.as_bytes()).to_vec(),
            Hash::Sha256 => ring::digest::digest(&ring::digest::SHA256, text.as_bytes())
                .as_ref()
                .to_vec(),
        };
        bytes.iter().fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
    }

    fn name(self) -> &'static str {
        match self {
            Hash::Md5 => "MD5",
            Hash::Sha256 => "SHA-256",
        }
    }
}

/// What a Digest challenge says.
#[derive(Debug, Default, PartialEq, Eq)]
struct Challenge {
    realm: String,
    nonce: String,
    opaque: Option<String>,
    /// Whether "auth" is among the kinds of protection offered.
    qop_auth: bool,
    /// Whether any kind of protection is named at all.
    qop_named: bool,
    algorithm: Option<String>,
}

/// The fields of `name=value, name="value"` after a scheme's name.
fn fields(text: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut rest = text.trim_start();
    while !rest.is_empty() && found.len() < MAX_FIELDS {
        let Some(equals) = rest.find('=') else { break };
        let name = rest[..equals].trim().trim_start_matches(',').trim();
        rest = rest[equals + 1..].trim_start();
        let value;
        if let Some(quoted) = rest.strip_prefix('"') {
            let mut text = String::new();
            let mut chars = quoted.char_indices();
            let mut end = quoted.len();
            while let Some((at, c)) = chars.next() {
                match c {
                    '\\' => {
                        if let Some((_, escaped)) = chars.next() {
                            text.push(escaped);
                        }
                    }
                    '"' => {
                        end = at + 1;
                        break;
                    }
                    _ => text.push(c),
                }
            }
            value = text;
            rest = quoted.get(end..).unwrap_or("");
        } else {
            let end = rest.find(',').unwrap_or(rest.len());
            value = rest[..end].trim().to_string();
            rest = &rest[end..];
        }
        if !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            found.push((name.to_ascii_lowercase(), value));
        }
        rest = rest.trim_start().trim_start_matches(',').trim_start();
    }
    found
}

/// The scheme of a challenge and what follows it.
fn scheme(challenge: &str) -> (&str, &str) {
    let challenge = challenge.trim();
    match challenge.split_once(char::is_whitespace) {
        Some((scheme, rest)) => (scheme, rest),
        None => (challenge, ""),
    }
}

fn digest_challenge(text: &str) -> Option<Challenge> {
    let mut challenge = Challenge::default();
    let mut nonce = None;
    for (name, value) in fields(text) {
        match name.as_str() {
            "realm" => challenge.realm = value,
            "nonce" => nonce = Some(value),
            "opaque" => challenge.opaque = Some(value),
            "algorithm" => challenge.algorithm = Some(value),
            "qop" => {
                challenge.qop_named = true;
                challenge.qop_auth = value
                    .split(',')
                    .any(|kind| kind.trim().eq_ignore_ascii_case("auth"));
            }
            _ => {}
        }
    }
    challenge.nonce = nonce?;
    Some(challenge)
}

/// A value inside double quotes.
fn quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        if matches!(c, '"' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Whether a value can stand in a header (no line break in it, nothing that would
/// begin another header).
fn sendable(value: &str) -> bool {
    !value.chars().any(|c| c.is_control())
}

/// What makes one answer to a challenge differ from the next.
#[derive(Clone, Copy)]
struct Turn<'a> {
    /// A random value of the client's.
    cnonce: &'a str,
    /// The how-manieth answer to this challenge this is (a server may refuse a count
    /// it has seen: an answer that was overheard cannot be sent again).
    count: u32,
}

fn digest_answer(
    challenge: &Challenge,
    hash: Hash,
    user: &str,
    password: &str,
    method: &str,
    uri: &str,
    turn: Turn<'_>,
) -> Option<String> {
    let Turn { cnonce, count } = turn;
    if challenge.qop_named && !challenge.qop_auth {
        // Only "auth-int" (the body is hashed too) is offered: not spoken here.
        return None;
    }
    let ha1 = hash.of(&format!("{user}:{}:{password}", challenge.realm));
    let ha2 = hash.of(&format!("{method}:{uri}"));
    let mut header = format!(
        "Digest username={}, realm={}, nonce={}, uri={}, algorithm={}",
        quoted(user),
        quoted(&challenge.realm),
        quoted(&challenge.nonce),
        quoted(uri),
        hash.name(),
    );
    let response = if challenge.qop_auth {
        let nc = format!("{count:08x}");
        let _ = write!(header, ", qop=auth, nc={nc}, cnonce={}", quoted(cnonce));
        hash.of(&format!(
            "{ha1}:{}:{nc}:{cnonce}:auth:{ha2}",
            challenge.nonce
        ))
    } else {
        hash.of(&format!("{ha1}:{}:{ha2}", challenge.nonce))
    };
    let _ = write!(header, ", response=\"{response}\"");
    if let Some(opaque) = &challenge.opaque {
        let _ = write!(header, ", opaque={}", quoted(opaque));
    }
    Some(header)
}

/// Base64, as `Authorization: Basic` wants it.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let group = u32::from(chunk[0]) << 16
            | u32::from(*chunk.get(1).unwrap_or(&0)) << 8
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(ALPHABET[(group >> shift) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A random value of the client's for a Digest answer.
fn client_nonce() -> String {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 16];
    if ring::rand::SystemRandom::new().fill(&mut bytes).is_err() {
        // No system randomness: the value only has to differ from request to request.
        for byte in &mut bytes {
            *byte = fastrand::u8(..);
        }
    }
    bytes.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}

/// The `Authorization` header that answers a server's challenges (the values of its
/// `WWW-Authenticate` headers) for `user` and `password`, for the request `method` to
/// `uri` (path and query). `None` when nothing the server offers is spoken here, or
/// when name or password cannot be sent in a header.
pub(super) fn answer(
    challenges: &[String],
    user: &str,
    password: &str,
    method: &str,
    uri: &str,
) -> Option<String> {
    answer_counted(challenges, user, password, method, uri, 1)
}

/// The most servers and names whose questions are remembered.
const MAX_REMEMBERED: usize = 64;

/// What a server asked the last time, and how often it has been answered.
struct Asked {
    challenges: Vec<String>,
    answers: u32,
}

/// By server and name ("https://host:port" and who is there as).
static ASKED: LazyLock<Mutex<HashMap<(String, String), Asked>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// [`answer`] to a server that has just asked (`server`: its address up to the
/// path), which is remembered for [`answer_ahead`].
pub(super) fn answer_asked(
    server: &str,
    challenges: &[String],
    user: &str,
    password: &str,
    method: &str,
    uri: &str,
) -> Option<String> {
    let answer = answer(challenges, user, password, method, uri)?;
    let mut asked = ASKED.lock().unwrap_or_else(|p| p.into_inner());
    if asked.len() >= MAX_REMEMBERED {
        asked.clear();
    }
    asked.insert(
        (server.to_string(), user.to_string()),
        Asked {
            challenges: challenges.to_vec(),
            answers: 1,
        },
    );
    Some(answer)
}

/// The answer to what `server` asked `user` the last time, for a request that is
/// about to be made: `None` when it has not asked since the app started. A server
/// that no longer takes the answer asks again, and is answered anew.
pub(super) fn answer_ahead(
    server: &str,
    user: &str,
    password: &str,
    method: &str,
    uri: &str,
) -> Option<String> {
    let mut asked = ASKED.lock().unwrap_or_else(|p| p.into_inner());
    let asked = asked.get_mut(&(server.to_string(), user.to_string()))?;
    asked.answers = asked.answers.checked_add(1)?;
    answer_counted(
        &asked.challenges,
        user,
        password,
        method,
        uri,
        asked.answers,
    )
}

fn answer_counted(
    challenges: &[String],
    user: &str,
    password: &str,
    method: &str,
    uri: &str,
    count: u32,
) -> Option<String> {
    if !sendable(user) || !sendable(password) {
        return None;
    }
    let digests: Vec<Challenge> = challenges
        .iter()
        .filter(|c| c.len() <= MAX_CHALLENGE_BYTES)
        .filter_map(|c| {
            let (name, rest) = scheme(c);
            name.eq_ignore_ascii_case("digest")
                .then(|| digest_challenge(rest))
                .flatten()
        })
        .collect();
    // The stronger hash first; "-sess" variants and anything unknown are passed over.
    for hash in [Hash::Sha256, Hash::Md5] {
        let fits = |challenge: &&Challenge| match challenge.algorithm.as_deref() {
            None => hash == Hash::Md5,
            Some(name) => name.eq_ignore_ascii_case(hash.name()),
        };
        if let Some(answer) = digests.iter().find(fits).and_then(|c| {
            let cnonce = client_nonce();
            let turn = Turn {
                cnonce: &cnonce,
                count,
            };
            digest_answer(c, hash, user, password, method, uri, turn)
        }) {
            return Some(answer);
        }
    }
    let basic = challenges
        .iter()
        .any(|c| scheme(c).0.eq_ignore_ascii_case("basic"));
    // A name with a colon in it cannot be told from the password in this scheme.
    (basic && !user.contains(':'))
        .then(|| format!("Basic {}", base64(format!("{user}:{password}").as_bytes())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_is_padded_as_it_should_be() {
        for (plain, coded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
            ("Aladdin:open sesame", "QWxhZGRpbjpvcGVuIHNlc2FtZQ=="),
        ] {
            assert_eq!(base64(plain.as_bytes()), coded, "{plain}");
        }
    }

    /// The example of RFC 7616 (3.9.1), with both hashes.
    #[test]
    fn digest_answers_are_those_of_the_standard() {
        let challenge = |algorithm: &str| Challenge {
            realm: "http-auth@example.org".into(),
            nonce: "7ypf/xlj9XXwfDPEoM4URrv/xwf94BcCAzFZH4GiTo0v".into(),
            opaque: Some("FQhe/qaU925kfnzjCev0ciny7QMkPqMAFRtzCUYo5tdS".into()),
            qop_auth: true,
            qop_named: true,
            algorithm: Some(algorithm.into()),
        };
        let cnonce = "f2/wE4q74E6zIJEtWaHKaf5wv/H5QzzpXusqGemxURZJ";
        let answer = |hash: Hash| {
            digest_answer(
                &challenge(hash.name()),
                hash,
                "Mufasa",
                "Circle of Life",
                "GET",
                "/dir/index.html",
                Turn { cnonce, count: 1 },
            )
            .expect("an answer")
        };
        let md5 = answer(Hash::Md5);
        assert!(
            md5.contains("response=\"8ca523f5e9506fed4657c9700eebdbec\""),
            "{md5}"
        );
        assert!(md5.starts_with("Digest username=\"Mufasa\", realm=\"http-auth@example.org\""));
        assert!(md5.contains("qop=auth, nc=00000001"));
        assert!(md5.contains("opaque=\"FQhe/qaU925kfnzjCev0ciny7QMkPqMAFRtzCUYo5tdS\""));
        let sha = answer(Hash::Sha256);
        assert!(
            sha.contains(
                "response=\"753927fa0e85d155564e2e272a28d1802ca10daf4496794697cf8db5856cb6c1\""
            ),
            "{sha}"
        );
        assert!(sha.contains("algorithm=SHA-256"));
    }

    /// Without "qop" the answer is the older, shorter one (RFC 2069's example).
    #[test]
    fn a_challenge_without_qop_gets_the_old_answer() {
        let challenge = Challenge {
            realm: "testrealm@host.com".into(),
            nonce: "dcd98b7102dd2f0e8b11d0f600bfb0c093".into(),
            opaque: Some("5ccc069c403ebaf9f0171e9517f40e41".into()),
            ..Challenge::default()
        };
        let answer = digest_answer(
            &challenge,
            Hash::Md5,
            "Mufasa",
            "CircleOfLife",
            "GET",
            "/dir/index.html",
            Turn {
                cnonce: "unused",
                count: 1,
            },
        )
        .expect("an answer");
        assert!(
            answer.contains("response=\"1949323746fe6a43ef61f9606e7febea\""),
            "{answer}"
        );
        assert!(!answer.contains("qop"));
    }

    #[test]
    fn the_strongest_offer_that_is_spoken_is_taken() {
        let both = [
            "Basic realm=\"share\"".to_string(),
            "Digest realm=\"share\", nonce=\"abc\", qop=\"auth\", algorithm=MD5".to_string(),
            "Digest realm=\"share\", nonce=\"abc\", qop=\"auth\", algorithm=SHA-256".to_string(),
        ];
        let answer = answer(&both, "me", "word", "PROPFIND", "/dav/").expect("an answer");
        assert!(answer.starts_with("Digest ") && answer.contains("algorithm=SHA-256"));

        let basic = ["Basic realm=\"share\"".to_string()];
        assert_eq!(
            super::answer(&basic, "Aladdin", "open sesame", "GET", "/").as_deref(),
            Some("Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==")
        );
        // Kinds that are not spoken, and values that cannot be sent.
        let other = [
            "Negotiate".to_string(),
            "Digest realm=\"r\", nonce=\"n\", algorithm=MD5-sess".to_string(),
            "Digest realm=\"r\", nonce=\"n\", qop=\"auth-int\"".to_string(),
        ];
        assert_eq!(super::answer(&other, "me", "word", "GET", "/"), None);
        assert_eq!(super::answer(&basic, "me", "wo\r\nrd", "GET", "/"), None);
        assert_eq!(super::answer(&basic, "m:e", "word", "GET", "/"), None);
        assert_eq!(super::answer(&[], "me", "word", "GET", "/"), None);
    }

    #[test]
    fn challenges_are_read_whatever_they_look_like() {
        let read = digest_challenge(
            "realm=\"a \\\"quoted\\\" realm\", nonce=abc123, qop=\"auth,auth-int\", opaque=\"\", stale=FALSE",
        )
        .expect("a challenge");
        assert_eq!(read.realm, "a \"quoted\" realm");
        assert_eq!(read.nonce, "abc123");
        assert!(read.qop_auth && read.qop_named);
        assert_eq!(read.opaque.as_deref(), Some(""));
        assert_eq!(digest_challenge("realm=\"no nonce\""), None);
        // Damaged: read as far as it goes, never a panic.
        for text in [
            "",
            "=",
            "nonce",
            "nonce=\"unterminated",
            ",,,,",
            "a=b=c,,=,\"\"\"",
            "nonce=\"\\",
        ] {
            let _ = digest_challenge(text);
            let _ = fields(text);
        }
        let name = quoted("a\"b\\c");
        assert_eq!(name, "\"a\\\"b\\\\c\"");
    }
}
