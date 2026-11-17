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
//!
//! A name and password that a server has refused are not given to it again for a
//! while ([`note_refused`]): a NAS counts failed sign-ins and shuts out the device
//! that fails too often, and a queue of songs would otherwise fail twice for each.
//!
//! Over a connection that is not TLS nothing tells who answers at an address. A
//! folder remembers how its server asked when it was added ([`how_asked`]), and
//! over such a connection the password itself (Basic) is then given only to a
//! server that asks in that same way ([`Kept`]).
use md5::{Digest as _, Md5};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

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
#[cfg(test)]
pub(super) fn answer(
    challenges: &[String],
    user: &str,
    password: &str,
    method: &str,
    uri: &str,
) -> Option<String> {
    answer_counted(challenges, user, password, method, uri, 1, Kept::Any)
}

/// Which way of asking may be answered with the password itself (Basic).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kept<'a> {
    /// Any: the connection is TLS (the certificate says who answers), or nothing is
    /// known of how the server asked before.
    Any,
    /// The connection is not TLS, and this is how the server asked when the folder
    /// was added ([`how_asked`]): `digest`, or `basic` and the realm it named. The
    /// password goes only to a server that asks for Basic with that same realm.
    Only(&'a str),
}

impl Kept<'_> {
    fn allows_basic(self, realm: &str) -> bool {
        match self {
            Kept::Any => true,
            Kept::Only(kept) => kept
                .strip_prefix("basic ")
                .is_some_and(|kept_realm| kept_realm == realm),
        }
    }
}

/// The most servers and names whose questions are remembered.
const MAX_REMEMBERED: usize = 64;

/// What a server asked the last time, and how often it has been answered.
struct Asked {
    challenges: Vec<String>,
    answers: u32,
    /// The server has taken an answer to it in this run.
    accepted: bool,
}

/// By server and name ("https://host:port" and who is there as).
static ASKED: LazyLock<Mutex<HashMap<(String, String), Asked>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// How long a name and password that were refused are not given again.
const REFUSED_FOR: Duration = Duration::from_secs(600);

/// A name and password a server has refused: the password by its hash (a new
/// password is tried at once), and when.
struct Refused {
    password: [u8; 32],
    at: Instant,
}

static REFUSED: LazyLock<Mutex<HashMap<(String, String), Refused>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn hash_of(password: &str) -> [u8; 32] {
    let mut hash = [0u8; 32];
    hash.copy_from_slice(ring::digest::digest(&ring::digest::SHA256, password.as_bytes()).as_ref());
    hash
}

fn is_refused(server: &str, user: &str, password: &str) -> bool {
    let mut refused = REFUSED.lock().unwrap_or_else(|p| p.into_inner());
    let key = (server.to_string(), user.to_string());
    match refused.get(&key) {
        Some(found) if found.at.elapsed() < REFUSED_FOR && found.password == hash_of(password) => {
            true
        }
        Some(_) => {
            refused.remove(&key);
            false
        }
        None => false,
    }
}

/// The server answered 401 to an answer made for what it had just asked: name or
/// password is not right. They are not given to it again for [`REFUSED_FOR`] (the
/// requests are made as nobody), unless the password changes or the listener tries
/// them anew ([`try_anew`]).
///
/// Not when the server took them earlier in this run: it then refuses this one
/// address (a folder the name has no right to), and the rest goes on working. And
/// not when it says its question had grown old (`stale`): the answer was right.
pub(super) fn note_refused(server: &str, challenges: &[String], user: &str, password: &str) {
    let key = (server.to_string(), user.to_string());
    let asked = ASKED.lock().unwrap_or_else(|p| p.into_inner());
    if asked.get(&key).is_some_and(|asked| asked.accepted) {
        return;
    }
    let stale = challenges.iter().flat_map(|c| challenges_of(c)).any(|c| {
        let (name, rest) = scheme(&c);
        name.eq_ignore_ascii_case("digest")
            && fields(rest)
                .iter()
                .any(|(name, value)| name == "stale" && value.eq_ignore_ascii_case("true"))
    });
    if stale {
        return;
    }
    // (What the server asked stays remembered: another password answers it ahead.)
    drop(asked);
    let mut refused = REFUSED.lock().unwrap_or_else(|p| p.into_inner());
    if refused.len() >= MAX_REMEMBERED {
        refused.clear();
    }
    refused.insert(
        key,
        Refused {
            password: hash_of(password),
            at: Instant::now(),
        },
    );
    tracing::warn!(
        "a server refused the name and password it was given: not given again for a while"
    );
}

/// Whether a 401 to a request that was answered ahead says that name or password
/// is not right: the server asks the very question that was answered (the same
/// nonce, not grown old), and it has not taken an answer in this run. Answering it
/// once more would be one more failed sign-in with the same end.
pub(super) fn refused_ahead(server: &str, user: &str, challenges: &[String]) -> bool {
    let asked = ASKED.lock().unwrap_or_else(|p| p.into_inner());
    asked
        .get(&(server.to_string(), user.to_string()))
        .is_some_and(|before| !before.accepted && before.challenges == challenges)
}

/// The server took an answer (anything but 401 came back to a request with one).
pub(super) fn note_accepted(server: &str, user: &str) {
    let mut asked = ASKED.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(asked) = asked.get_mut(&(server.to_string(), user.to_string())) {
        asked.accepted = true;
    }
}

/// The listener is trying a name and password now (a folder's form): an earlier
/// refusal does not stand in the way.
pub(super) fn try_anew(server: &str, user: &str) {
    REFUSED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&(server.to_string(), user.to_string()));
}

/// How `server` asked `user` the last time, in the words a folder keeps
/// (`Folder::sign_in`): `digest` when it offered a way that does not send the
/// password, `basic <realm>` when the password itself is what it takes. `None` when
/// it has not asked since the app started.
pub fn how_asked(server: &str, user: &str) -> Option<String> {
    let asked = ASKED.lock().unwrap_or_else(|p| p.into_inner());
    let asked = asked.get(&(server.to_string(), user.to_string()))?;
    way_of(&asked.challenges)
}

/// [`how_asked`] for the challenges themselves.
fn way_of(challenges: &[String]) -> Option<String> {
    let challenges: Vec<String> = challenges
        .iter()
        .filter(|c| c.len() <= MAX_CHALLENGE_BYTES)
        .flat_map(|c| challenges_of(c))
        .collect();
    let usable_digest = challenges.iter().any(|c| {
        let (name, rest) = scheme(c);
        name.eq_ignore_ascii_case("digest")
            && digest_challenge(rest).is_some_and(|challenge| {
                (!challenge.qop_named || challenge.qop_auth)
                    && challenge.algorithm.as_deref().is_none_or(|name| {
                        name.eq_ignore_ascii_case("MD5") || name.eq_ignore_ascii_case("SHA-256")
                    })
            })
    });
    if usable_digest {
        return Some("digest".to_string());
    }
    basic_realm(&challenges).map(|realm| format!("basic {realm}"))
}

/// The realm of the Basic challenge among `challenges` (each one challenge), when
/// there is one.
fn basic_realm(challenges: &[String]) -> Option<String> {
    challenges.iter().find_map(|c| {
        let (name, rest) = scheme(c);
        name.eq_ignore_ascii_case("basic").then(|| {
            fields(rest)
                .into_iter()
                .find(|(name, _)| name == "realm")
                .map(|(_, realm)| realm)
                .unwrap_or_default()
        })
    })
}

/// The answer to a server that has just asked (`server`: its address up to the
/// path), which is remembered for [`answer_ahead`]. `None` when nothing it offers is
/// spoken here, when `kept` does not allow what it asks for, or while name and
/// password count as refused.
pub(super) fn answer_asked(
    server: &str,
    challenges: &[String],
    user: &str,
    password: &str,
    method: &str,
    uri: &str,
    kept: Kept<'_>,
) -> Option<String> {
    if is_refused(server, user, password) {
        return None;
    }
    let key = (server.to_string(), user.to_string());
    let mut asked = ASKED.lock().unwrap_or_else(|p| p.into_inner());
    // A server that offered a way without the password earlier in this run and now
    // wants the password itself, over a connection that says nothing of who it is:
    // not the same server, as far as can be told.
    let kept = match (kept, asked.get(&key)) {
        (Kept::Only(_), _) => kept,
        (Kept::Any, _) if uri_is_secure(server) => kept,
        (Kept::Any, Some(before)) if way_of(&before.challenges).as_deref() == Some("digest") => {
            Kept::Only("digest")
        }
        (Kept::Any, _) => kept,
    };
    let answer = answer_counted(challenges, user, password, method, uri, 1, kept)?;
    if asked.len() >= MAX_REMEMBERED {
        asked.clear();
    }
    let accepted = asked.get(&key).is_some_and(|before| before.accepted);
    asked.insert(
        key,
        Asked {
            challenges: challenges.to_vec(),
            answers: 1,
            accepted,
        },
    );
    Some(answer)
}

/// Whether a server's address ("https://host:port") is one that is reached over TLS.
fn uri_is_secure(server: &str) -> bool {
    server.starts_with("https://")
}

/// The answer to what `server` asked `user` the last time, for a request that is
/// about to be made: `None` when it has not asked since the app started. A server
/// that no longer takes the answer asks again, and is answered anew.
///
/// `secure`: the request goes over TLS, to a server whose certificate has been
/// checked by then. Over a connection that is not, only an answer that does not hold
/// the password is given ahead: who answers at that address now (after the device
/// moved to another network, say) need not be who asked.
pub(super) fn answer_ahead(
    server: &str,
    user: &str,
    password: &str,
    method: &str,
    uri: &str,
    secure: bool,
) -> Option<String> {
    if is_refused(server, user, password) {
        return None;
    }
    let mut asked = ASKED.lock().unwrap_or_else(|p| p.into_inner());
    let asked = asked.get_mut(&(server.to_string(), user.to_string()))?;
    let answers = asked.answers.checked_add(1)?;
    let answer = answer_counted(
        &asked.challenges,
        user,
        password,
        method,
        uri,
        answers,
        Kept::Any,
    )?;
    if !secure && answer.starts_with("Basic ") {
        return None;
    }
    asked.answers = answers;
    Some(answer)
}

/// The challenges of one `WWW-Authenticate` value, which may hold several
/// (`Basic realm="x", Digest realm="x", nonce="…"`): a new one begins where a word is
/// followed by something other than `=`.
fn challenges_of(header: &str) -> Vec<String> {
    // The parts between commas that are not inside quotes.
    let mut parts: Vec<&str> = Vec::new();
    let (mut start, mut quoted, mut escaped) = (0, false, false);
    for (at, c) in header.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ',' if !quoted => {
                parts.push(&header[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(&header[start..]);
    let mut challenges: Vec<String> = Vec::new();
    for part in parts
        .iter()
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
    {
        let (word, rest) = match part.split_once(char::is_whitespace) {
            Some((word, rest)) => (word, rest.trim_start()),
            None => (part, ""),
        };
        // "Digest realm=…" and a bare "Negotiate" begin a challenge; "realm=…" and
        // "nonce = …" go on with the one before.
        let begins = !word.contains('=') && !word.contains('"') && !rest.starts_with('=');
        match challenges.last_mut() {
            Some(last) if !begins => {
                last.push_str(", ");
                last.push_str(part);
            }
            _ => challenges.push(part.to_string()),
        }
    }
    challenges
}

fn answer_counted(
    challenges: &[String],
    user: &str,
    password: &str,
    method: &str,
    uri: &str,
    count: u32,
    kept: Kept<'_>,
) -> Option<String> {
    if !sendable(user) || !sendable(password) {
        return None;
    }
    let challenges: Vec<String> = challenges
        .iter()
        .filter(|c| c.len() <= MAX_CHALLENGE_BYTES)
        .flat_map(|c| challenges_of(c))
        .collect();
    let digests: Vec<Challenge> = challenges
        .iter()
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
    let basic = basic_realm(&challenges).is_some_and(|realm| kept.allows_basic(&realm));
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

    /// One header may hold several challenges. The one that proves the password
    /// without sending it is taken whatever comes first, and what was asked over a
    /// connection that is not encrypted is answered ahead only when the answer does
    /// not hold the password.
    #[test]
    fn several_challenges_on_one_line_are_told_apart() {
        let line =
            r#"Basic realm="a, b", Digest realm="x", qop="auth,auth-int", nonce="n1", Negotiate"#;
        assert_eq!(
            challenges_of(line),
            [
                r#"Basic realm="a, b""#,
                r#"Digest realm="x", qop="auth,auth-int", nonce="n1""#,
                "Negotiate",
            ]
        );
        assert_eq!(challenges_of(r#"Digest realm = "x", nonce="n""#).len(), 1);
        assert_eq!(challenges_of("Negotiate, Basic realm=\"x\"").len(), 2);
        assert!(challenges_of(" , ").is_empty());
        let both = [line.to_string()];
        let answer = answer(&both, "me", "word", "GET", "/").expect("an answer");
        assert!(answer.starts_with("Digest "), "{answer}");
        let negotiate = ["Negotiate, Basic realm=\"x\"".to_string()];
        let answer = super::answer(&negotiate, "me", "word", "GET", "/").expect("an answer");
        assert!(answer.starts_with("Basic "), "{answer}");

        // Remembered and answered ahead: Digest anywhere, Basic only over TLS.
        let digest = [r#"Digest realm="x", qop="auth", nonce="n2""#.to_string()];
        let basic = [r#"Basic realm="x""#.to_string()];
        assert!(
            answer_asked(
                "http://plain.test",
                &digest,
                "me",
                "word",
                "GET",
                "/",
                Kept::Any
            )
            .is_some()
        );
        let ahead = answer_ahead("http://plain.test", "me", "word", "GET", "/a", false).unwrap();
        assert!(
            ahead.contains("nc=00000002") && ahead.contains(r#"uri="/a""#),
            "{ahead}"
        );
        assert!(
            answer_asked(
                "http://basic.test",
                &basic,
                "me",
                "word",
                "GET",
                "/",
                Kept::Any
            )
            .is_some()
        );
        assert_eq!(
            answer_ahead("http://basic.test", "me", "word", "GET", "/", false),
            None
        );
        assert!(
            answer_asked(
                "https://basic.test",
                &basic,
                "me",
                "word",
                "GET",
                "/",
                Kept::Any
            )
            .is_some()
        );
        assert!(answer_ahead("https://basic.test", "me", "word", "GET", "/", true).is_some());
        // Another name, another server: nothing to answer ahead with.
        assert_eq!(
            answer_ahead("https://basic.test", "you", "word", "GET", "/", true),
            None
        );
        assert_eq!(
            answer_ahead("https://other.test", "me", "word", "GET", "/", true),
            None
        );
    }
    fn asked(server: &str, challenge: &str, password: &str, kept: Kept<'_>) -> Option<String> {
        answer_asked(
            server,
            &[challenge.to_string()],
            "me",
            password,
            "GET",
            "/",
            kept,
        )
    }

    /// A name and password that were refused are kept from the server for a while:
    /// neither ahead nor when it asks again. A new password, or the listener trying
    /// anew, is not held back.
    #[test]
    fn what_a_server_refused_is_not_given_again() {
        let server = "https://refusing.test";
        let digest = r#"Digest realm="x", qop="auth", nonce="n1""#;
        assert!(asked(server, digest, "wrong", Kept::Any).is_some());
        note_refused(server, &[digest.to_string()], "me", "wrong");
        assert_eq!(asked(server, digest, "wrong", Kept::Any), None);
        assert_eq!(answer_ahead(server, "me", "wrong", "GET", "/", true), None);
        // Another password is another try; so is another name.
        assert!(asked(server, digest, "right", Kept::Any).is_some());
        assert!(
            answer_asked(
                server,
                &[digest.to_string()],
                "you",
                "wrong",
                "GET",
                "/",
                Kept::Any
            )
            .is_some()
        );
        // Refused again with the first one, then tried anew from the form.
        note_refused(server, &[digest.to_string()], "me", "right");
        assert_eq!(asked(server, digest, "right", Kept::Any), None);
        try_anew(server, "me");
        assert!(asked(server, digest, "right", Kept::Any).is_some());
    }

    /// A server that has taken the answer in this run refuses one address, not the
    /// name; and one that says its question had grown old refuses nothing.
    #[test]
    fn a_refusal_counts_only_where_it_says_something_of_the_password() {
        let server = "https://accepting.test";
        let digest = r#"Digest realm="x", qop="auth", nonce="n1""#;
        assert!(asked(server, digest, "word", Kept::Any).is_some());
        note_accepted(server, "me");
        note_refused(server, &[digest.to_string()], "me", "word");
        assert!(answer_ahead(server, "me", "word", "GET", "/", true).is_some());
        // (Asked anew, it is still the server that took the answer.)
        assert!(asked(server, digest, "word", Kept::Any).is_some());
        note_refused(server, &[digest.to_string()], "me", "word");
        assert!(asked(server, digest, "word", Kept::Any).is_some());

        let server = "https://stale.test";
        let stale = r#"Digest realm="x", qop="auth", nonce="n2", stale=true"#;
        assert!(asked(server, digest, "word", Kept::Any).is_some());
        note_refused(server, &[stale.to_string()], "me", "word");
        assert!(asked(server, stale, "word", Kept::Any).is_some());
    }

    /// Over a connection that is not TLS the password itself goes only to a server
    /// that asks as the folder's server did when the folder was added.
    #[test]
    fn the_password_itself_goes_only_to_who_asks_as_before() {
        let digest = r#"Digest realm="nas", qop="auth", nonce="n1""#;
        let basic = r#"Basic realm="nas""#;
        let other = r#"Basic realm="camera""#;
        // How a server asked, in the words a folder keeps.
        assert!(asked("http://way-a.test", digest, "word", Kept::Any).is_some());
        assert_eq!(
            how_asked("http://way-a.test", "me").as_deref(),
            Some("digest")
        );
        assert!(asked("http://way-b.test", basic, "word", Kept::Any).is_some());
        assert_eq!(
            how_asked("http://way-b.test", "me").as_deref(),
            Some("basic nas")
        );
        assert_eq!(how_asked("http://way-c.test", "me"), None);
        // Both offered: the way without the password is the one that counts.
        let both = format!("{basic}, {digest}");
        assert!(asked("http://way-d.test", &both, "word", Kept::Any).is_some());
        assert_eq!(
            how_asked("http://way-d.test", "me").as_deref(),
            Some("digest")
        );

        // A folder of a server that asked for Digest: Basic is not answered.
        let server = "http://kept-a.test";
        assert_eq!(asked(server, basic, "word", Kept::Only("digest")), None);
        assert!(
            asked(server, digest, "word", Kept::Only("digest"))
                .is_some_and(|answer| answer.starts_with("Digest "))
        );
        // A folder of a server that asked for Basic: the same realm, and no other.
        let server = "http://kept-b.test";
        assert!(asked(server, basic, "word", Kept::Only("basic nas")).is_some());
        assert_eq!(asked(server, other, "word", Kept::Only("basic nas")), None);
        assert!(
            asked(server, digest, "word", Kept::Only("basic nas"))
                .is_some_and(|answer| answer.starts_with("Digest "))
        );

        // A folder whose server did not ask when it was added: never the password
        // itself, whoever asks for it now.
        let server = "http://kept-c.test";
        assert_eq!(asked(server, basic, "word", Kept::Only("none")), None);
        assert!(asked(server, digest, "word", Kept::Only("none")).is_some());

        // Nothing kept (a folder of an earlier version): a server that offered Digest
        // in this run is not given the password itself afterwards, over http.
        let server = "http://run.test";
        assert!(asked(server, digest, "word", Kept::Any).is_some());
        assert_eq!(asked(server, basic, "word", Kept::Any), None);
        // (Over TLS the certificate says who it is.)
        let server = "https://run.test";
        assert!(asked(server, digest, "word", Kept::Any).is_some());
        assert!(asked(server, basic, "word", Kept::Any).is_some());
    }
}
