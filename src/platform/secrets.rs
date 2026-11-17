//! The passwords the app keeps (those of network folders): in the store the system
//! has for such things, and nowhere else. They are never written to the settings, to
//! the log, to the state a verification run reads, or to a backup of the library's
//! data.
//!
//! - macOS and iPhone: the keychain, as a generic password of the service
//!   "OnEum network folders" (on a Mac the login keychain: it can be looked at and
//!   removed in Keychain Access).
//! - Windows: the Credential Manager, as a generic credential.
//! - Android: a file of the app's own that holds the password encrypted with a key
//!   that is made in the Android Keystore and never leaves it (`Secrets.java`).
//!
//! Tests and verification runs keep them in memory instead ([`use_memory`]): nothing
//! of such a run gets into the keychain of the person at the machine.
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use zeroize::Zeroize;

/// A password in memory: wiped when it is let go of, and never printed.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(text: impl Into<String>) -> Self {
        Secret(text.into())
    }

    /// The password itself, for the one place that sends it.
    pub fn reveal(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(…)")
    }
}

/// The store of a test or a verification run, once it is asked for.
static MEMORY: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

/// From now on passwords are kept in memory only (until the process ends).
pub fn use_memory() {
    MEMORY.get_or_init(Mutex::default);
}

fn memory() -> Option<std::sync::MutexGuard<'static, HashMap<String, String>>> {
    #[cfg(test)]
    use_memory();
    MEMORY
        .get()
        .map(|store| store.lock().unwrap_or_else(|p| p.into_inner()))
}

/// Whether `id` can name an entry: the app makes its ids itself (letters, digits,
/// hyphens), and nothing else is passed on to a system's store.
fn plain(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Keeps `secret` under `id` (replacing what was there).
pub fn put(id: &str, secret: &Secret) -> Result<(), String> {
    if !plain(id) {
        return Err("not a name for a password".into());
    }
    if let Some(mut store) = memory() {
        store.insert(id.to_string(), secret.reveal().to_string());
        return Ok(());
    }
    system::put(id, secret.reveal())
}

/// The password kept under `id`, if there is one (and the system hands it out).
pub fn get(id: &str) -> Option<Secret> {
    if !plain(id) {
        return None;
    }
    if let Some(store) = memory() {
        return store.get(id).cloned().map(Secret);
    }
    system::get(id).map(Secret)
}

/// Forgets the password kept under `id`.
pub fn remove(id: &str) {
    if !plain(id) {
        return;
    }
    if let Some(mut store) = memory() {
        if let Some(mut gone) = store.remove(id) {
            gone.zeroize();
        }
        return;
    }
    system::remove(id);
}

/// Tries the system's own store with a password made for the purpose: kept, read
/// back, replaced, removed. For verification (also in a run that otherwise keeps
/// passwords in memory); `Ok` says what was done.
pub fn try_system_store() -> Result<&'static str, String> {
    const ID: &str = "oneum-selftest";
    system::remove(ID);
    system::put(ID, "first word")?;
    let read = system::get(ID);
    if read.as_deref() != Some("first word") {
        system::remove(ID);
        return Err("what was kept was not read back".into());
    }
    system::put(ID, "second wörd 암호")?;
    let read = system::get(ID);
    system::remove(ID);
    if read.as_deref() != Some("second wörd 암호") {
        return Err("a replaced password was not read back".into());
    }
    if system::get(ID).is_some() {
        return Err("a removed password is still there".into());
    }
    Ok("kept, read, replaced and removed")
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod system {
    use security_framework::passwords::{
        delete_generic_password, get_generic_password, set_generic_password,
    };

    const SERVICE: &str = "OnEum network folders";

    pub(super) fn put(id: &str, secret: &str) -> Result<(), String> {
        set_generic_password(SERVICE, id, secret.as_bytes())
            .map_err(|error| format!("the keychain refused: {error}"))
    }

    pub(super) fn get(id: &str) -> Option<String> {
        String::from_utf8(get_generic_password(SERVICE, id).ok()?).ok()
    }

    pub(super) fn remove(id: &str) {
        let _ = delete_generic_password(SERVICE, id);
    }
}

#[cfg(windows)]
mod system {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::Security::Credentials::{
        CRED_FLAGS, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW,
        CredFree, CredReadW, CredWriteW,
    };
    use windows::core::{PCWSTR, PWSTR};

    /// The name the credential has in the Credential Manager, as a wide string.
    fn target(id: &str) -> Vec<u16> {
        format!("OnEum network folder: {id}")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect()
    }

    pub(super) fn put(id: &str, secret: &str) -> Result<(), String> {
        let mut target = target(id);
        let mut user: Vec<u16> = "OnEum".encode_utf16().chain(std::iter::once(0)).collect();
        let mut blob = secret.as_bytes().to_vec();
        let credential = CREDENTIALW {
            Flags: CRED_FLAGS(0),
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(target.as_mut_ptr()),
            Comment: PWSTR::null(),
            LastWritten: FILETIME::default(),
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            AttributeCount: 0,
            Attributes: std::ptr::null_mut(),
            TargetAlias: PWSTR::null(),
            UserName: PWSTR(user.as_mut_ptr()),
        };
        // SAFETY: every pointer in `credential` is to a buffer that lives to the end
        // of this function, with the length given beside it; the system copies them.
        let written = unsafe { CredWriteW(&credential, 0) };
        zeroize::Zeroize::zeroize(&mut blob);
        written.map_err(|error| format!("the Credential Manager refused: {error}"))
    }

    pub(super) fn get(id: &str) -> Option<String> {
        let target = target(id);
        let mut found: *mut CREDENTIALW = std::ptr::null_mut();
        // SAFETY: `target` ends with a zero; on success `found` is a credential the
        // system allocated, valid until `CredFree`.
        unsafe {
            CredReadW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None, &mut found).ok()?;
            let credential = &*found;
            let secret = if credential.CredentialBlob.is_null() {
                None
            } else {
                let bytes = std::slice::from_raw_parts(
                    credential.CredentialBlob,
                    credential.CredentialBlobSize as usize,
                );
                String::from_utf8(bytes.to_vec()).ok()
            };
            CredFree(found.cast());
            secret
        }
    }

    pub(super) fn remove(id: &str) {
        let target = target(id);
        // SAFETY: `target` ends with a zero.
        let _ = unsafe { CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None) };
    }
}

#[cfg(target_os = "android")]
mod system {
    use crate::platform::android;

    pub(super) fn put(id: &str, secret: &str) -> Result<(), String> {
        if android::secret_put(id, secret) {
            Ok(())
        } else {
            Err("the phone's key store refused".into())
        }
    }

    pub(super) fn get(id: &str) -> Option<String> {
        android::secret_get(id)
    }

    pub(super) fn remove(id: &str) {
        android::secret_remove(id);
    }
}

/// Any other system (Linux, for development): there is no store that is spoken to
/// here, so passwords last for the run only.
#[cfg(not(any(target_os = "macos", target_os = "ios", windows, target_os = "android")))]
mod system {
    pub(super) fn put(id: &str, secret: &str) -> Result<(), String> {
        super::use_memory();
        super::put(id, &super::Secret::new(secret))
    }

    pub(super) fn get(_id: &str) -> Option<String> {
        None
    }

    pub(super) fn remove(_id: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_password_is_kept_replaced_and_forgotten() {
        assert_eq!(get("folder-1"), None);
        put("folder-1", &Secret::new("word")).unwrap();
        put("folder-2", &Secret::new("another")).unwrap();
        assert_eq!(get("folder-1").unwrap().reveal(), "word");
        put("folder-1", &Secret::new("new word")).unwrap();
        assert_eq!(get("folder-1").unwrap().reveal(), "new word");
        remove("folder-1");
        assert_eq!(get("folder-1"), None);
        assert_eq!(get("folder-2").unwrap().reveal(), "another");
        remove("folder-2");
    }

    #[test]
    fn only_the_apps_own_ids_name_an_entry() {
        for id in ["", "a b", "../x", "a/b", "a\0b", &"x".repeat(65)] {
            assert!(put(id, &Secret::new("word")).is_err(), "{id:?}");
            assert_eq!(get(id), None);
        }
    }

    #[test]
    fn a_secret_is_not_printed() {
        let secret = Secret::new("hunter2");
        assert_eq!(format!("{secret:?}"), "Secret(…)");
        assert!(!format!("{:?}", Some(&secret)).contains("hunter2"));
    }
}
