//! Reading small files that are not trusted to be what their name says.
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

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

#[cfg(test)]
mod tests {
    use super::*;

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
