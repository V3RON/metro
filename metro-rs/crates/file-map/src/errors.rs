//! Errors shaped like the Node errors the JS file map throws or warns about:
//! a `code` (`ENOENT`) and the message Node builds
//! (`ENOENT: no such file or directory, open '/x'`).

use metro_rs_file_store::{FileStoreError, Invariant};
use std::fmt;
use std::io;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileMapError {
    /// `error.code`; `None` for errors without one (invariants, TreeFS throws).
    pub code: Option<String>,
    pub message: String,
}

impl FileMapError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            code: None,
            message: message.into(),
        }
    }

    /// A Node `fs` error: `{code}: {description}, {syscall} '{path}'`. For
    /// `read` Node omits the path.
    pub fn from_io(err: &io::Error, syscall: &str, path: &str) -> Self {
        match err.raw_os_error().and_then(errno_info) {
            Some((code, description)) => {
                let message = if syscall == "read" {
                    format!("{code}: {description}, {syscall}")
                } else {
                    format!("{code}: {description}, {syscall} '{path}'")
                };
                Self {
                    code: Some(code.to_string()),
                    message,
                }
            }
            None => Self {
                code: None,
                message: err.to_string(),
            },
        }
    }
}

impl fmt::Display for FileMapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FileMapError {}

impl From<Invariant> for FileMapError {
    fn from(e: Invariant) -> Self {
        Self::new(e.0)
    }
}

impl From<FileStoreError> for FileMapError {
    fn from(e: FileStoreError) -> Self {
        Self::new(e.0)
    }
}

/// `(code, libuv description)` for the errnos a file map run can meet.
fn errno_info(errno: i32) -> Option<(&'static str, &'static str)> {
    #[cfg(target_os = "macos")]
    const ELOOP: i32 = 62;
    #[cfg(target_os = "macos")]
    const ENAMETOOLONG: i32 = 63;
    #[cfg(not(target_os = "macos"))]
    const ELOOP: i32 = 40;
    #[cfg(not(target_os = "macos"))]
    const ENAMETOOLONG: i32 = 36;
    Some(match errno {
        1 => ("EPERM", "operation not permitted"),
        2 => ("ENOENT", "no such file or directory"),
        5 => ("EIO", "i/o error"),
        13 => ("EACCES", "permission denied"),
        20 => ("ENOTDIR", "not a directory"),
        21 => ("EISDIR", "illegal operation on a directory"),
        22 => ("EINVAL", "invalid argument"),
        23 => ("ENFILE", "file table overflow"),
        24 => ("EMFILE", "too many open files"),
        e if e == ELOOP => ("ELOOP", "too many symbolic links encountered"),
        e if e == ENAMETOOLONG => ("ENAMETOOLONG", "name too long"),
        _ => return None,
    })
}
