//! The network folders the listener has added (a share of a NAS, over WebDAV): which
//! name, which password and which trusted certificate go with an address.
//!
//! A folder is kept in the settings without its password. The password is in the
//! system's store for passwords (`platform::secrets`), under the folder's id, and is
//! read from there the first time something of the folder is asked for; in memory it
//! is held as a [`Secret`] (wiped when let go of, never printed).
//!
//! An address gets a folder's name and password only when it lies inside that folder:
//! on the same server (scheme, host and port) and under the folder's path. A song's
//! address carries neither; whoever plays it asks here.
use super::http::{Access, Url};
use crate::platform::secrets::{self, Secret};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Mutex, RwLock};

/// The most network folders that are kept.
pub const MAX_FOLDERS: usize = 32;

/// A network folder, as the settings keep it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Folder {
    /// The app's own name for it (letters, digits and hyphens): what its password is
    /// kept under.
    pub id: String,
    /// What the listener calls it.
    pub name: String,
    /// Where it is: an `http://` or `https://` address of a folder, ending with '/'.
    /// Never with a name or a password in it.
    pub url: String,
    /// The name to be there as. Empty: the server asks nobody.
    pub user: String,
    /// The fingerprint of the server's certificate, when the listener said to trust
    /// one that no authority stands behind (see `net::tls`).
    pub pinned: Option<String>,
    // (Earlier versions kept here how the server asked who is there, `sign_in`, to
    // hold an `http://` server to it. The password itself is never given over such
    // a connection now, and a file that has it is read without it.)
}

impl Folder {
    /// The folder with what a settings file can hold but the app never writes taken
    /// out; `None` when it is no folder at all (an address that cannot be asked for,
    /// an id that names no password).
    pub fn sanitized(mut self) -> Option<Folder> {
        let url = Url::parse_web(&self.url).ok()?;
        self.url = url.to_string();
        if !self.url.split('?').next().unwrap_or("").ends_with('/') {
            return None;
        }
        let plain_id = !self.id.is_empty()
            && self.id.len() <= 64
            && self
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-');
        if !plain_id {
            return None;
        }
        let tidy = |text: &str, max: usize| -> String {
            text.chars()
                .filter(|c| !c.is_control())
                .take(max)
                .collect::<String>()
                .trim()
                .to_string()
        };
        self.name = tidy(&self.name, 80);
        if self.name.is_empty() {
            self.name = url.host.clone();
        }
        self.user = tidy(&self.user, 128);
        self.pinned = self
            .pinned
            .map(|print| print.to_ascii_lowercase())
            .filter(|print| print.len() == 64 && print.bytes().all(|b| b.is_ascii_hexdigit()));
        Some(self)
    }

    /// Whether `url` lies in this folder.
    fn holds(&self, url: &Url) -> bool {
        let Ok(own) = Url::parse_web(&self.url) else {
            return false;
        };
        // (As places: a server may write the folder's own path in another way than
        // it was typed here, and the entries of its listing come as it writes them.)
        own.origin() == url.origin() && url.place().starts_with(&own.place())
    }
}

/// A new id for a folder: nothing is told by it (not the server, not the name).
pub fn new_id() -> String {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 12];
    if ring::rand::SystemRandom::new().fill(&mut bytes).is_err() {
        for byte in &mut bytes {
            *byte = fastrand::u8(..);
        }
    }
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("folder-{hex}")
}

static KNOWN: RwLock<Vec<Folder>> = RwLock::new(Vec::new());
/// Passwords read from the system's store in this run, by folder id.
static PASSWORDS: Mutex<Option<HashMap<String, Secret>>> = Mutex::new(None);

fn passwords() -> std::sync::MutexGuard<'static, Option<HashMap<String, Secret>>> {
    PASSWORDS.lock().unwrap_or_else(|p| p.into_inner())
}

/// Tells this module the folders there are (at the start, and whenever they change).
/// What was held of a folder that is no longer among them is let go of.
pub fn set_known(folders: &[Folder]) {
    *KNOWN.write().unwrap_or_else(|p| p.into_inner()) = folders.to_vec();
    if let Some(held) = passwords().as_mut() {
        held.retain(|id, _| folders.iter().any(|folder| &folder.id == id));
    }
}

/// The folder an address lies in: the one with the longest path, should one folder
/// be inside another.
pub fn folder_of(url: &str) -> Option<Folder> {
    let url = Url::parse_web(url).ok()?;
    KNOWN
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .filter(|folder| folder.holds(&url))
        .max_by_key(|folder| folder.url.len())
        .cloned()
}

/// Keeps a folder's password: in the system's store, and at hand for this run.
pub fn keep_password(id: &str, password: &Secret) -> Result<(), String> {
    secrets::put(id, password)?;
    passwords()
        .get_or_insert_with(HashMap::new)
        .insert(id.to_string(), password.clone());
    Ok(())
}

/// Forgets a folder's password, here and in the system's store.
pub fn forget_password(id: &str) {
    if let Some(held) = passwords().as_mut() {
        held.remove(id);
    }
    secrets::remove(id);
}

fn password_of(id: &str) -> Option<Secret> {
    if let Some(found) = passwords().as_ref().and_then(|held| held.get(id)) {
        return Some(found.clone());
    }
    let read = secrets::get(id)?;
    passwords()
        .get_or_insert_with(HashMap::new)
        .insert(id.to_string(), read.clone());
    Some(read)
}

/// What a request to an address is made with.
#[derive(Debug, Default)]
pub struct Credentials {
    user: String,
    password: Secret,
    pinned: Option<String>,
    /// The folder has a name to sign in with, and the system's store did not hand
    /// its password out: the request is made as nobody.
    lacks_password: bool,
    /// The listener is trying these now (a folder's form).
    fresh: bool,
}

impl Credentials {
    /// For a folder that is being tried before it is added (nothing is kept yet).
    pub fn of(user: &str, password: &Secret, pinned: Option<&str>) -> Self {
        Credentials {
            user: user.to_string(),
            password: password.clone(),
            pinned: pinned.map(str::to_string),
            lacks_password: false,
            fresh: true,
        }
    }

    /// Whether the request is made as nobody because the password of the folder's
    /// name could not be read (see [`credentials_for`]). A refusal then says nothing
    /// about the name and the password.
    pub fn lacks_password(&self) -> bool {
        self.lacks_password
    }

    pub fn access(&self) -> Access<'_> {
        Access {
            account: (!self.user.is_empty()).then(|| (self.user.as_str(), self.password.reveal())),
            pinned: self.pinned.as_deref(),
            fresh: self.fresh,
        }
    }
}

/// The name, password and trusted certificate for a request to `url`: those of the
/// folder it lies in, or none.
pub fn credentials_for(url: &str) -> Credentials {
    let Some(folder) = folder_of(url) else {
        return Credentials::default();
    };
    let password = match folder.user.is_empty() {
        true => Some(Secret::default()),
        false => password_of(&folder.id),
    };
    match password {
        Some(password) => Credentials {
            user: folder.user,
            password,
            pinned: folder.pinned,
            lacks_password: false,
            fresh: false,
        },
        // The system's store does not hand the password out (locked, or the person
        // said no): the server is asked as nobody. A name with a password that is
        // not the right one would count as a failed sign-in there, request after
        // request, and a NAS shuts out who fails too often.
        None => {
            tracing::warn!(
                "the password of a network folder is not handed out by the system's store"
            );
            Credentials {
                user: String::new(),
                password: Secret::default(),
                pinned: folder.pinned,
                lacks_password: true,
                fresh: false,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: &str, url: &str, user: &str) -> Folder {
        Folder {
            id: id.into(),
            name: "NAS".into(),
            url: url.into(),
            user: user.into(),
            pinned: None,
        }
    }

    /// (One test: the folders that are known are one list for the whole process.)
    #[test]
    fn an_address_gets_the_account_of_the_folder_it_lies_in_and_no_other() {
        let music = folder("folder-a", "https://nas.local:5006/music/", "me");
        let inner = folder("folder-b", "https://nas.local:5006/music/kids/", "child");
        let open = folder("folder-c", "http://10.0.0.5/share/", "");
        set_known(&[music.clone(), inner.clone(), open]);
        keep_password("folder-a", &Secret::new("word a")).unwrap();
        keep_password("folder-b", &Secret::new("word b")).unwrap();

        let of = |url: &str| {
            let found = credentials_for(url);
            found
                .access()
                .account
                .map(|(user, word)| (user.to_string(), word.to_string()))
        };
        assert_eq!(
            of("https://nas.local:5006/music/a/b.flac"),
            Some(("me".into(), "word a".into()))
        );
        assert_eq!(
            of("https://nas.local:5006/music/kids/x.mp3"),
            Some(("child".into(), "word b".into())),
            "the folder inside goes first"
        );
        // Not inside: another path, port, scheme or host; a folder nobody is asked in.
        for url in [
            "https://nas.local:5006/video/a.flac",
            "https://nas.local:5006/musical/a.flac",
            "https://nas.local/music/a.flac",
            "http://nas.local:5006/music/a.flac",
            "https://nas.local.example:5006/music/a.flac",
            "http://10.0.0.5/share/a.flac",
            "not an address",
        ] {
            assert_eq!(of(url), None, "{url}");
        }
        assert_eq!(
            folder_of("https://nas.local:5006/music/x").unwrap().id,
            "folder-a"
        );

        // A folder that is gone takes what was held of it along; a password that is
        // forgotten is gone from the store as well.
        set_known(&[music]);
        assert_eq!(
            of("https://nas.local:5006/music/kids/x.mp3").unwrap().0,
            "me"
        );
        forget_password("folder-a");
        // (Without its password the folder is asked as nobody.)
        assert_eq!(of("https://nas.local:5006/music/a.flac"), None);
        assert!(secrets::get("folder-a").is_none());
        secrets::remove("folder-b");
        set_known(&[]);

        // A folder whose password the system's store does not hand out is asked as
        // nobody: never with the name and a password that is not the right one.
        // (In this test, which owns the list of folders: the tests run side by side.)
        let locked = folder("folder-locked", "https://locked.example/share/", "me");
        let open = folder("folder-open-2", "https://open.example/share/", "");
        set_known(&[locked, open]);
        let found = credentials_for("https://locked.example/share/a.flac");
        assert!(found.access().account.is_none());
        assert!(found.lacks_password());
        // A folder without a name has no password to miss, nor has an address that
        // lies in no folder.
        assert!(!credentials_for("https://open.example/share/a.flac").lacks_password());
        assert!(!credentials_for("https://elsewhere.example/a.flac").lacks_password());
        keep_password("folder-locked", &Secret::new("word")).unwrap();
        let found = credentials_for("https://locked.example/share/a.flac");
        assert!(!found.lacks_password());
        assert_eq!(found.access().account, Some(("me", "word")));
        forget_password("folder-locked");
    }

    /// A server writes the places in a folder its own way: the hex digits of an
    /// escape in lower case, an escape where none is needed, the folder's name in
    /// another case. They are in the folder all the same, and what only begins
    /// like it is not.
    #[test]
    fn a_place_written_another_way_is_still_in_its_folder() {
        let korean = folder(
            "folder-k",
            "https://nas.example/%EC%9D%8C%EC%95%85/Live%20(2020)/",
            "me",
        );
        let url = |text: &str| Url::parse_web(text).unwrap();
        for inside in [
            "https://nas.example/%EC%9D%8C%EC%95%85/Live%20(2020)/a.flac",
            "https://nas.example/%ec%9d%8c%ec%95%85/Live%20%282020%29/a.flac",
            "https://nas.example/음악/live%20(2020)/sub/b.flac",
            "https://NAS.example/%EC%9D%8C%EC%95%85/Live%20(2020)/",
        ] {
            assert!(korean.holds(&url(inside)), "{inside}");
        }
        for outside in [
            "https://nas.example/%EC%9D%8C%EC%95%85/Live%20(2020)2/a.flac",
            "https://nas.example/%EC%9D%8C%EC%95%85/a.flac",
            "https://nas.example/%EC%9D%8C%EC%95%85/Live%20(2020)%2Fa.flac",
            "http://nas.example/%EC%9D%8C%EC%95%85/Live%20(2020)/a.flac",
            "https://nas.example:8443/%EC%9D%8C%EC%95%85/Live%20(2020)/a.flac",
        ] {
            assert!(!korean.holds(&url(outside)), "{outside}");
        }
        assert_eq!(url("https://h/A%2fB/%ED%95%9C?x=%41").place(), "/a%2fb/한");
    }

    #[test]
    fn a_folder_of_a_settings_file_is_brought_into_shape_or_dropped() {
        let good = folder("folder-1", "HTTPS://NAS.local/Music/", " me ")
            .sanitized()
            .unwrap();
        assert_eq!(good.url, "https://nas.local/Music/");
        assert_eq!(good.user, "me");
        let mut pinned = folder("folder-1", "https://nas.local/m/", "");
        pinned.pinned = Some("AB".repeat(32));
        assert_eq!(pinned.sanitized().unwrap().pinned, Some("ab".repeat(32)));
        let mut bad_pin = folder("folder-1", "https://nas.local/m/", "");
        bad_pin.pinned = Some("xyz".into());
        assert_eq!(bad_pin.sanitized().unwrap().pinned, None);
        let mut unnamed = folder("folder-1", "http://10.0.0.5/share/", "");
        unnamed.name = " \u{7} ".into();
        assert_eq!(unnamed.sanitized().unwrap().name, "10.0.0.5");
        for (id, url) in [
            ("folder-1", "ftp://nas.local/music/"),
            ("folder-1", "https://nas.local/music"),
            ("folder-1", "https://user:word@nas.local/music/"),
            ("", "https://nas.local/music/"),
            ("../x", "https://nas.local/music/"),
        ] {
            assert_eq!(folder(id, url, "").sanitized(), None, "{id} {url}");
        }
        let (a, b) = (new_id(), new_id());
        assert!(a != b && a.starts_with("folder-") && a.len() == 31);
        assert!(folder(&a, "http://h/", "").sanitized().is_some());
    }
}
