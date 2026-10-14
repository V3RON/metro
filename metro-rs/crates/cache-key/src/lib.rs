//! Port of `metro-cache-key`: an MD5 over the contents of a list of files.

use md5::{Digest, Md5};
use std::fmt::Write;
use std::io;
use std::path::Path;

/// A file that could not be read, and why.
#[derive(Debug)]
pub struct ReadError {
    /// Index of the file in the list.
    pub index: usize,
    pub source: io::Error,
}

/// Hex MD5 of every file's contents, each preceded by a NUL byte, so that the
/// key changes when content moves from one file to another.
pub fn get_cache_key<P: AsRef<Path>>(files: &[P]) -> Result<String, ReadError> {
    let mut hasher = Md5::new();
    for (index, file) in files.iter().enumerate() {
        hasher.update([0u8]);
        let contents = std::fs::read(file).map_err(|source| ReadError { index, source })?;
        hasher.update(&contents);
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(32);
    for byte in digest {
        write!(hex, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(hex)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("metro-rs-cache-key-{}-{name}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn no_files_hash_the_empty_input() {
        let files: [&str; 0] = [];
        assert_eq!(
            get_cache_key(&files).unwrap(),
            "d41d8cd98f00b204e9800998ecf8427e"
        );
    }

    #[test]
    fn hashes_a_nul_before_each_file() {
        let dir = temp_dir("nul");
        let a = dir.join("a");
        fs::write(&a, "abc").unwrap();
        assert_eq!(get_cache_key(&[&a]).unwrap(), expected(b"\0abc"));
        fs::remove_dir_all(dir).unwrap();
    }

    fn expected(bytes: &[u8]) -> String {
        let mut h = Md5::new();
        h.update(bytes);
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn content_moving_between_files_changes_the_key() {
        let dir = temp_dir("move");
        let (a, b) = (dir.join("a"), dir.join("b"));
        fs::write(&a, "ab").unwrap();
        fs::write(&b, "").unwrap();
        let first = get_cache_key(&[&a, &b]).unwrap();
        fs::write(&a, "a").unwrap();
        fs::write(&b, "b").unwrap();
        assert_ne!(first, get_cache_key(&[&a, &b]).unwrap());
        assert_eq!(get_cache_key(&[&a, &b]).unwrap(), expected(b"\0a\0b"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reports_the_file_that_failed() {
        let dir = temp_dir("missing");
        let a = dir.join("a");
        fs::write(&a, "x").unwrap();
        let error = get_cache_key(&[a, dir.join("missing")]).unwrap_err();
        assert_eq!(error.index, 1);
        assert_eq!(error.source.kind(), io::ErrorKind::NotFound);
        fs::remove_dir_all(dir).unwrap();
    }
}
