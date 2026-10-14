//! `FileMetadata` (`flow-types.js`), indexed by `H` in `constants.js`:
//! `MTIME: 0, SIZE: 1, VISITED: 2, SHA1: 3, SYMLINK: 4, PLUGINDATA: 5`.

use metro_rs_file_store::EntryKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `metadata[H.SYMLINK]`: `0` for a regular file, `1` for a symlink whose
/// target has not been read, or the target as a normal path with POSIX
/// separators.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Symlink {
    Regular,
    Unresolved,
    Target(String),
}

/// One file's metadata. The path is not part of it: the tree is the single
/// source of truth for paths, and the metadata hangs off the node's `FileId`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileMetadata {
    /// `H.MTIME`: `stat.mtime.getTime()`, or `None` (JS `null`).
    pub mtime: Option<f64>,
    /// `H.SIZE`.
    pub size: f64,
    /// `H.VISITED === 1`.
    pub visited: bool,
    /// `H.SHA1` (hex), or `None` (JS `null`).
    pub sha1: Option<String>,
    /// `H.SYMLINK`.
    pub symlink: Symlink,
    /// `H.PLUGINDATA + i`: plugin `i`'s data as JSON text. `None` is JS
    /// `undefined` ("no worker has run"); the JSON `null` is `Some("null")`.
    /// Plugin data is anything the workers return through jest-worker that is
    /// also JSON (Map, Set, Date and the like are not supported).
    pub plugin_data: Vec<Option<String>>,
}

impl FileMetadata {
    /// A fresh entry as the crawler makes it: `[mtime, size, 0, null, 0 | 1]`.
    pub fn new(mtime: Option<f64>, size: f64, is_symlink: bool) -> Self {
        Self {
            mtime,
            size,
            visited: false,
            sha1: None,
            symlink: if is_symlink {
                Symlink::Unresolved
            } else {
                Symlink::Regular
            },
            plugin_data: Vec::new(),
        }
    }

    /// `isRegularFile(metadata)`.
    pub fn is_regular_file(&self) -> bool {
        self.symlink == Symlink::Regular
    }

    pub fn entry_kind(&self) -> EntryKind<'_> {
        match &self.symlink {
            Symlink::Regular => EntryKind::File,
            Symlink::Unresolved => EntryKind::UnresolvedSymlink,
            Symlink::Target(t) => EntryKind::Symlink { target: t },
        }
    }

    /// `metadata[H.PLUGINDATA + slot]`, or `None` for `undefined`.
    pub fn plugin_slot(&self, slot: usize) -> Option<&str> {
        self.plugin_data.get(slot).and_then(|d| d.as_deref())
    }

    /// `metadata[H.PLUGINDATA + slot] = json`.
    pub fn set_plugin_slot(&mut self, slot: usize, json: Option<String>) {
        if self.plugin_data.len() <= slot {
            self.plugin_data.resize(slot + 1, None);
        }
        self.plugin_data[slot] = json;
    }
}

impl FileMetadata {
    /// The JS tuple `[mtime, size, visited, sha1, symlink, ...pluginData]`.
    /// A plugin slot that is `undefined` in JS has no JSON form and is `null`.
    pub fn to_tuple(&self) -> Value {
        let mut tuple = vec![
            self.mtime.map_or(Value::Null, number),
            number(self.size),
            Value::from(u8::from(self.visited)),
            self.sha1.clone().map_or(Value::Null, Value::from),
            match &self.symlink {
                Symlink::Regular => Value::from(0),
                Symlink::Unresolved => Value::from(1),
                Symlink::Target(t) => Value::from(t.clone()),
            },
        ];
        for slot in &self.plugin_data {
            tuple.push(match slot {
                None => Value::Null,
                Some(json) => serde_json::from_str(json).unwrap_or(Value::Null),
            });
        }
        Value::Array(tuple)
    }

    /// The inverse of [`FileMetadata::to_tuple`]. A `null` plugin slot is the
    /// JSON `null`, not `undefined`. `None` if the tuple is malformed.
    pub fn from_tuple(tuple: &Value) -> Option<Self> {
        let items = tuple.as_array()?;
        if items.len() < 5 {
            return None;
        }
        let mtime = match &items[0] {
            Value::Null => None,
            v => Some(v.as_f64()?),
        };
        let sha1 = match &items[3] {
            Value::Null => None,
            v => Some(v.as_str()?.to_string()),
        };
        let symlink = match &items[4] {
            Value::String(t) => Symlink::Target(t.clone()),
            v if v.as_f64() == Some(0.0) => Symlink::Regular,
            v if v.as_f64() == Some(1.0) => Symlink::Unresolved,
            _ => return None,
        };
        Some(Self {
            mtime,
            size: items[1].as_f64()?,
            visited: items[2].as_f64()? == 1.0,
            sha1,
            symlink,
            plugin_data: items[5..].iter().map(|v| Some(v.to_string())).collect(),
        })
    }
}

/// A JS number: an integer prints as an integer.
fn number(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 9.0e15 {
        Value::from(n as i64)
    } else {
        serde_json::Number::from_f64(n).map_or(Value::Null, Value::Number)
    }
}
