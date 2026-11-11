//! Stable identities: FNV-1a hashing and the album key.
//!
//! These values are persisted (album keys in the database, artwork ids as cache file
//! names), so the hash must never change between builds — hence a hand-written FNV-1a
//! instead of the standard library's unspecified `DefaultHasher`.
use super::search::normalize;
use std::path::Path;

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Incremental 64-bit FNV-1a.
#[derive(Clone, Copy)]
pub(crate) struct Fnv1a(u64);

impl Fnv1a {
    pub(crate) fn new() -> Self {
        Fnv1a(FNV_OFFSET)
    }

    pub(crate) fn write(&mut self, bytes: &[u8]) {
        let mut hash = self.0;
        for &byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        self.0 = hash;
    }

    pub(crate) fn finish(self) -> u64 {
        self.0
    }
}

/// 64-bit FNV-1a of `bytes`.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hasher = Fnv1a::new();
    hasher.write(bytes);
    hasher.finish()
}

/// Identity of the album a track belongs to.
///
/// The key is the normalised album title plus one discriminator:
/// 1. the album artist, when tagged;
/// 2. otherwise a "compilation" marker, when the file's compilation flag is set;
/// 3. otherwise the folder containing the file — so that two different artists'
///    "Greatest Hits" in different folders stay separate. A folder named like `CD1`,
///    `Disc 2` or `disk3` counts as its parent so multi-disc sets stay together.
///
/// Tracks without an album title share one "unknown album" per folder.
pub fn album_key(album: &str, album_artist: &str, compilation: bool, path: &Path) -> u64 {
    let album = normalize(album.trim());
    let album_artist = normalize(album_artist.trim());
    let mut hasher = Fnv1a::new();
    if album.is_empty() {
        hasher.write(b"unknown\0");
        if let Some(folder) = path.parent() {
            hasher.write(folder.as_os_str().as_encoded_bytes());
        }
        return hasher.finish();
    }
    hasher.write(b"album\0");
    hasher.write(album.as_bytes());
    if !album_artist.is_empty() {
        hasher.write(b"\0artist\0");
        hasher.write(album_artist.as_bytes());
    } else if compilation {
        hasher.write(b"\0compilation");
    } else {
        hasher.write(b"\0folder\0");
        if let Some(folder) = album_folder(path) {
            hasher.write(folder.as_os_str().as_encoded_bytes());
        }
    }
    hasher.finish()
}

/// Folder that represents the album of the file at `path`: its parent, or the
/// grandparent when the parent is a per-disc folder.
fn album_folder(path: &Path) -> Option<&Path> {
    let parent = path.parent()?;
    let is_disc = parent
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(is_disc_folder);
    if is_disc && let Some(grandparent) = parent.parent() {
        return Some(grandparent);
    }
    Some(parent)
}

/// True for folder names such as `CD1`, `cd 02`, `Disc 2`, `disk3`, `Disc-1`.
fn is_disc_folder(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    let Some(rest) = ["disc", "disk", "cd"]
        .iter()
        .find_map(|prefix| lower.strip_prefix(prefix))
    else {
        return false;
    };
    let digits = rest.trim_start_matches([' ', '-', '_', '.']);
    !digits.is_empty() && digits.len() <= 3 && digits.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn fnv_matches_reference_vectors() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn disc_folder_names() {
        for name in [
            "CD1", "cd 2", "Disc 2", "disk3", "DISC-01", "Disc_1", "cd.10",
        ] {
            assert!(is_disc_folder(name), "{name}");
        }
        for name in [
            "CD",
            "Discography",
            "Disc One",
            "cd1 bonus",
            "Greatest Hits",
            "1",
            "disc 1234",
        ] {
            assert!(!is_disc_folder(name), "{name}");
        }
    }

    #[test]
    fn same_title_in_different_folders_does_not_merge() {
        let a = album_key(
            "Greatest Hits",
            "",
            false,
            &PathBuf::from("/m/Queen/Greatest Hits/01.mp3"),
        );
        let b = album_key(
            "Greatest Hits",
            "",
            false,
            &PathBuf::from("/m/ABBA/Greatest Hits/01.mp3"),
        );
        assert_ne!(a, b);
        // Same folder, same title: one album.
        let c = album_key(
            "greatest hits",
            "",
            false,
            &PathBuf::from("/m/Queen/Greatest Hits/02.mp3"),
        );
        assert_eq!(a, c);
    }

    #[test]
    fn different_album_artists_do_not_merge_even_in_one_folder() {
        let a = album_key(
            "Greatest Hits",
            "Queen",
            false,
            &PathBuf::from("/m/mixed/01.mp3"),
        );
        let b = album_key(
            "Greatest Hits",
            "ABBA",
            false,
            &PathBuf::from("/m/mixed/02.mp3"),
        );
        assert_ne!(a, b);
    }

    #[test]
    fn album_artist_merges_across_folders_and_track_artists() {
        // A compilation tagged with album artist "Various Artists": the track artist is
        // not part of the key and neither is the folder.
        let a = album_key(
            "응답하라 1988 OST",
            "Various Artists",
            true,
            &PathBuf::from("/m/ost/part1/01.flac"),
        );
        let b = album_key(
            "응답하라 1988 OST",
            "various artists",
            true,
            &PathBuf::from("/m/ost/part2/07.flac"),
        );
        assert_eq!(a, b);
    }

    #[test]
    fn compilation_flag_without_album_artist_merges() {
        let a = album_key("Now 100", "", true, &PathBuf::from("/m/a/01.mp3"));
        let b = album_key("Now 100", "", true, &PathBuf::from("/m/b/02.mp3"));
        assert_eq!(a, b);
        let c = album_key("Now 100", "", false, &PathBuf::from("/m/b/02.mp3"));
        assert_ne!(a, c);
    }

    #[test]
    fn disc_subfolders_merge() {
        let a = album_key(
            "The Wall",
            "",
            false,
            &PathBuf::from("/m/The Wall/CD1/01.flac"),
        );
        let b = album_key(
            "The Wall",
            "",
            false,
            &PathBuf::from("/m/The Wall/CD2/01.flac"),
        );
        let c = album_key(
            "The Wall",
            "",
            false,
            &PathBuf::from("/m/The Wall/Disc 3/01.flac"),
        );
        let d = album_key("The Wall", "", false, &PathBuf::from("/m/The Wall/01.flac"));
        assert_eq!(a, b);
        assert_eq!(a, c);
        assert_eq!(a, d);
    }

    #[test]
    fn unknown_album_is_per_folder() {
        let a = album_key("", "", false, &PathBuf::from("/m/x/01.mp3"));
        let b = album_key("  ", "Someone", false, &PathBuf::from("/m/x/02.mp3"));
        let c = album_key("", "", false, &PathBuf::from("/m/y/01.mp3"));
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn normalisation_makes_keys_tolerant_of_unicode_forms() {
        // NFD (decomposed jamo) vs NFC, and full-width vs ASCII.
        let nfc = album_key("꽃갈피", "아이유", false, &PathBuf::from("/m/a/1.mp3"));
        let nfd = album_key(
            "\u{1101}\u{1169}\u{11BE}\u{1100}\u{1161}\u{11AF}\u{1111}\u{1175}",
            "\u{110B}\u{1161}\u{110B}\u{1175}\u{110B}\u{1172}",
            false,
            &PathBuf::from("/m/b/1.mp3"),
        );
        assert_eq!(nfc, nfd);
        let ascii = album_key("ABC", "X", false, &PathBuf::from("/m/a/1.mp3"));
        let wide = album_key("ＡＢＣ", "ｘ", false, &PathBuf::from("/m/a/1.mp3"));
        assert_eq!(ascii, wide);
    }
}
