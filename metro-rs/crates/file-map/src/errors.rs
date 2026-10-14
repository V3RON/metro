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
/// ESTALE is deliberately absent: libuv (and so Node) has no name for it.
fn errno_info(errno: i32) -> Option<(&'static str, &'static str)> {
    #[cfg(target_os = "macos")]
    const EAGAIN: i32 = 35;
    #[cfg(not(target_os = "macos"))]
    const EAGAIN: i32 = 11;
    #[cfg(target_os = "macos")]
    const EBUSY: i32 = 16;
    #[cfg(not(target_os = "macos"))]
    const EBUSY: i32 = 16;
    #[cfg(target_os = "macos")]
    const EEXIST: i32 = 17;
    #[cfg(not(target_os = "macos"))]
    const EEXIST: i32 = 17;
    #[cfg(target_os = "macos")]
    const EXDEV: i32 = 18;
    #[cfg(not(target_os = "macos"))]
    const EXDEV: i32 = 18;
    #[cfg(target_os = "macos")]
    const ENODEV: i32 = 19;
    #[cfg(not(target_os = "macos"))]
    const ENODEV: i32 = 19;
    #[cfg(target_os = "macos")]
    const ENXIO: i32 = 6;
    #[cfg(not(target_os = "macos"))]
    const ENXIO: i32 = 6;
    #[cfg(target_os = "macos")]
    const EFBIG: i32 = 27;
    #[cfg(not(target_os = "macos"))]
    const EFBIG: i32 = 27;
    #[cfg(target_os = "macos")]
    const ENOSPC: i32 = 28;
    #[cfg(not(target_os = "macos"))]
    const ENOSPC: i32 = 28;
    #[cfg(target_os = "macos")]
    const EROFS: i32 = 30;
    #[cfg(not(target_os = "macos"))]
    const EROFS: i32 = 30;
    #[cfg(target_os = "macos")]
    const ENAMETOOLONG: i32 = 63;
    #[cfg(not(target_os = "macos"))]
    const ENAMETOOLONG: i32 = 36;
    #[cfg(target_os = "macos")]
    const ENOTEMPTY: i32 = 66;
    #[cfg(not(target_os = "macos"))]
    const ENOTEMPTY: i32 = 39;
    #[cfg(target_os = "macos")]
    const ELOOP: i32 = 62;
    #[cfg(not(target_os = "macos"))]
    const ELOOP: i32 = 40;
    #[cfg(target_os = "macos")]
    const ETIMEDOUT: i32 = 60;
    #[cfg(not(target_os = "macos"))]
    const ETIMEDOUT: i32 = 110;
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
        e if e == EAGAIN => ("EAGAIN", "resource temporarily unavailable"),
        e if e == EBUSY => ("EBUSY", "resource busy or locked"),
        e if e == EEXIST => ("EEXIST", "file already exists"),
        e if e == EXDEV => ("EXDEV", "cross-device link not permitted"),
        e if e == ENODEV => ("ENODEV", "no such device"),
        e if e == ENXIO => ("ENXIO", "no such device or address"),
        e if e == EFBIG => ("EFBIG", "file too large"),
        e if e == ENOSPC => ("ENOSPC", "no space left on device"),
        e if e == EROFS => ("EROFS", "read-only file system"),
        e if e == ENOTEMPTY => ("ENOTEMPTY", "directory not empty"),
        e if e == ETIMEDOUT => ("ETIMEDOUT", "connection timed out"),
        e if e == ELOOP => ("ELOOP", "too many symbolic links encountered"),
        e if e == ENAMETOOLONG => ("ENAMETOOLONG", "name too long"),
        _ => return None,
    })
}
