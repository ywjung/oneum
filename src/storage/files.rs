//! Reading small files that are not trusted to be what their name says.
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// Reads `path` whole if it is a regular file of at most `max` bytes. `Ok(None)` when it
/// is there but not that (larger, or a folder, pipe or device: opening a pipe would wait
/// for a writer, so the kind is looked at first); `Err` when it cannot be read, which
/// says nothing about what it holds. The size is checked again while reading, since the
/// file may grow.
pub fn read_bounded(path: &Path, max: u64) -> io::Result<Option<Vec<u8>>> {
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() || meta.len() > max {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    // One more byte than is allowed: its presence says the file is too large.
    File::open(path)?.take(max + 1).read_to_end(&mut bytes)?;
    Ok((bytes.len() as u64 <= max).then_some(bytes))
}

/// Whether two files hold the same bytes (compared by their lengths first).
pub fn same_content(a: &Path, b: &Path) -> io::Result<bool> {
    let (len_a, len_b) = (std::fs::metadata(a)?.len(), std::fs::metadata(b)?.len());
    if len_a != len_b {
        return Ok(false);
    }
    let (mut a, mut b) = (File::open(a)?, File::open(b)?);
    let (mut piece_a, mut piece_b) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let read = a.read(&mut piece_a)?;
        if read == 0 {
            return Ok(true);
        }
        b.read_exact(&mut piece_b[..read])?;
        if piece_a[..read] != piece_b[..read] {
            return Ok(false);
        }
    }
}

/// A file or a folder as the log names it: not by its path, which tells of the person
/// (the home folder's name) and of their music (artist, album and song in the names of
/// folders and files), but by a short number made from the path, with the file's kind
/// behind it: `<file 3f9a2c.flac>`, `<path 81c0d4>`. The same path gets the same number,
/// so that the lines about one file can be told from those about another. The log is
/// what a person passes on with a report (see `app::logging`).
pub fn for_log(path: &Path) -> ForLog {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    // A kind as files have them: a few letters and digits. Anything else may be part
    // of a name ("Vol. 2 (Live)").
    let kind = path
        .extension()
        .and_then(|kind| kind.to_str())
        .filter(|kind| kind.len() <= 8 && kind.bytes().all(|b| b.is_ascii_alphanumeric()))
        .map(str::to_ascii_lowercase);
    ForLog {
        number: (hasher.finish() & 0xff_ffff) as u32,
        kind,
    }
}

/// See [`for_log`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForLog {
    number: u32,
    kind: Option<String>,
}

impl std::fmt::Display for ForLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            Some(kind) => write!(f, "<file {:06x}.{kind}>", self.number),
            None => write!(f, "<path {:06x}>", self.number),
        }
    }
}

/// Brings `from` (a file, or a folder with all that is in it; links are not followed)
/// into the folder `into` under its own name, and says where it is then.
///
/// What is there already is not there twice afterwards. A file of that name with the
/// same bytes is the file: nothing is copied. A folder of that name takes in what it
/// does not have yet, file by file by the same rule. Only a file that is another one
/// under the same name gets a name of its own beside it ("song 2.flac"). So bringing
/// the same thing in again (the same song opened twice from another app, a folder
/// chosen again after three songs were put into it) adds what is new and no more.
///
/// A file inside a folder that cannot be brought in is left out; the rest still comes.
pub fn take_in(from: &Path, into: &Path) -> io::Result<PathBuf> {
    let name = from
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "nothing to name it by"))?;
    std::fs::create_dir_all(into)?;
    let kind = std::fs::symlink_metadata(from)?.file_type();
    let named = into.join(name);
    if kind.is_dir() {
        let to = if named.exists() && !named.is_dir() {
            free_name(into, name, None)?
        } else {
            named
        };
        std::fs::create_dir_all(&to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            if let Err(error) = take_in(&entry.path(), &to) {
                tracing::warn!("left out {}: {error}", for_log(&entry.path()));
            }
        }
        Ok(to)
    } else if kind.is_file() {
        let to = free_name(into, name, Some(from))?;
        if !to.exists() {
            std::fs::copy(from, &to)?;
        }
        Ok(to)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "neither a file nor a folder",
        ))
    }
}

/// Puts what is in `from` (the sample music that comes with the app) into the folder
/// `name` of `music`, and returns that folder. What is there already with the same
/// bytes is left as it is, so that asking twice adds nothing (see [`take_in`]).
pub fn add_samples(from: &Path, music: &Path, name: &str) -> io::Result<PathBuf> {
    let to = music.join(name);
    std::fs::create_dir_all(&to)?;
    let mut added = 0usize;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        match take_in(&entry.path(), &to) {
            Ok(_) => added += 1,
            Err(error) => tracing::warn!("left out {}: {error}", for_log(&entry.path())),
        }
    }
    if added == 0 {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "there is no sample music to add",
        ));
    }
    Ok(to)
}

/// A path in `folder` for something named `name`: the name itself, or "name 2",
/// "name 3" … (before the extension). With `same_as`, the first of them that is a
/// file with the bytes of that one is the answer, though it is there; otherwise the
/// first that nothing has.
pub fn free_name(
    folder: &Path,
    name: &std::ffi::OsStr,
    same_as: Option<&Path>,
) -> io::Result<PathBuf> {
    let as_path = Path::new(name);
    let stem = as_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let extension = as_path
        .extension()
        .map(|e| e.to_string_lossy().into_owned());
    let numbered = (2..10_000).map(|n| match &extension {
        Some(extension) => folder.join(format!("{stem} {n}.{extension}")),
        None => folder.join(format!("{stem} {n}")),
    });
    for candidate in std::iter::once(folder.join(name)).chain(numbered) {
        if !candidate.exists() {
            return Ok(candidate);
        }
        if let Some(same_as) = same_as
            && candidate.is_file()
            && same_content(same_as, &candidate).unwrap_or(false)
        {
            return Ok(candidate);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no name is free for it",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_names_a_file_by_a_number_and_its_kind() {
        let song = Path::new("/Users/someone/Music/아이유/Palette/01 팔레트 (feat. G-DRAGON).FLAC");
        let named = for_log(song).to_string();
        assert!(
            named.starts_with("<file ") && named.ends_with(".flac>"),
            "{named}"
        );
        for part in ["someone", "Music", "아이유", "Palette", "팔레트", "/"] {
            assert!(!named.contains(part), "{named} tells {part}");
        }
        // The same file again, and another one.
        assert_eq!(for_log(song).to_string(), named);
        let other = for_log(Path::new(
            "/Users/someone/Music/아이유/Palette/02 이 지금.flac",
        ));
        assert_ne!(other.to_string(), named);
        // What stands behind the last dot of a folder's name is not a kind of file.
        let folder = for_log(Path::new("/Users/someone/Music/Vol. 2 (Live)")).to_string();
        assert!(
            folder.starts_with("<path ") && !folder.contains("Live"),
            "{folder}"
        );
        assert!(for_log(Path::new("/m/a")).to_string().starts_with("<path "));
    }

    /// Brought in again, what is there already stays as it is: the same file is the
    /// same file, a folder takes in only what is new, and only another file of the
    /// same name gets a name of its own.
    /// The sample music is put into a folder of its own in the music folder, as it is
    /// laid out (artist, album, files); asked for again, it is not there twice.
    #[test]
    fn the_sample_music_is_added_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let samples = dir.path().join("samples");
        let album = samples.join("Artist").join("Album");
        std::fs::create_dir_all(&album).expect("mkdir");
        std::fs::write(album.join("01 Song.mp3"), b"one").expect("write");
        std::fs::write(album.join("cover.jpg"), b"picture").expect("write");
        let music = dir.path().join("Documents");
        let to = add_samples(&samples, &music, "Sample Music").expect("added");
        assert_eq!(to, music.join("Sample Music"));
        let song = to.join("Artist").join("Album").join("01 Song.mp3");
        assert_eq!(std::fs::read(&song).expect("read"), b"one");
        assert!(to.join("Artist").join("Album").join("cover.jpg").is_file());
        // Again: nothing more.
        add_samples(&samples, &music, "Sample Music").expect("added again");
        let files: Vec<_> = std::fs::read_dir(to.join("Artist").join("Album"))
            .expect("read_dir")
            .collect();
        assert_eq!(files.len(), 2, "{files:?}");
        // Nothing to add is said so.
        let empty = dir.path().join("empty");
        std::fs::create_dir_all(&empty).expect("mkdir");
        assert!(add_samples(&empty, &music, "Other").is_err());
    }

    #[test]
    fn what_is_brought_in_again_is_not_there_twice() {
        let dir = tempfile::tempdir().unwrap();
        let (outside, inside) = (dir.path().join("outside"), dir.path().join("inside"));
        let album = outside.join("Album");
        std::fs::create_dir_all(album.join("Disc 2")).unwrap();
        std::fs::write(album.join("one.flac"), b"one").unwrap();
        std::fs::write(album.join("Disc 2/two.flac"), b"two").unwrap();
        std::fs::write(outside.join("single.flac"), b"single").unwrap();
        let listed = |folder: &Path| -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(folder)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        };

        // A file, twice: one file.
        let first = take_in(&outside.join("single.flac"), &inside).unwrap();
        assert_eq!(first, inside.join("single.flac"));
        assert_eq!(
            take_in(&outside.join("single.flac"), &inside).unwrap(),
            first
        );
        // Another file under the same name: beside it; and that one again: itself.
        std::fs::write(outside.join("single.flac"), b"another take").unwrap();
        let second = take_in(&outside.join("single.flac"), &inside).unwrap();
        assert_eq!(second, inside.join("single 2.flac"));
        assert_eq!(
            take_in(&outside.join("single.flac"), &inside).unwrap(),
            second
        );
        assert_eq!(std::fs::read(&first).unwrap(), b"single");

        // A folder, then again with a song more in it: the one folder, with the song.
        let folder = take_in(&album, &inside).unwrap();
        assert_eq!(folder, inside.join("Album"));
        std::fs::write(album.join("three.flac"), b"three").unwrap();
        assert_eq!(take_in(&album, &inside).unwrap(), folder);
        assert_eq!(listed(&folder), ["Disc 2", "one.flac", "three.flac"]);
        assert_eq!(listed(&folder.join("Disc 2")), ["two.flac"]);
        assert_eq!(
            listed(&inside),
            ["Album", "single 2.flac", "single.flac"],
            "no \"Album 2\""
        );
        assert!(same_content(&album.join("one.flac"), &folder.join("one.flac")).unwrap());
        assert!(!same_content(&first, &second).unwrap());
    }

    #[test]
    fn only_a_regular_file_within_the_limit_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let small = dir.path().join("small.txt");
        std::fs::write(&small, b"12345").unwrap();
        assert_eq!(read_bounded(&small, 5).unwrap(), Some(b"12345".to_vec()));
        assert_eq!(
            read_bounded(&small, 4).unwrap(),
            None,
            "larger than allowed"
        );
        assert_eq!(read_bounded(dir.path(), 1 << 20).unwrap(), None, "a folder");
        let gone = read_bounded(&dir.path().join("gone"), 10).unwrap_err();
        assert_eq!(gone.kind(), io::ErrorKind::NotFound);
        #[cfg(unix)]
        {
            let pipe = dir.path().join("pipe");
            let made = std::process::Command::new("mkfifo")
                .arg(&pipe)
                .status()
                .is_ok_and(|s| s.success());
            assert!(made);
            assert_eq!(read_bounded(&pipe, 10).unwrap(), None, "not waited for");
        }
    }
}
