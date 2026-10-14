//! The disk cache, in Rust's own format (serde + bincode). It is not the JS
//! `DiskCacheManager` file: the caller chooses the file name and the cache
//! key (which names the Rust format version), and a JS run never reads this
//! file or the other way round.
//!
//! The content is the tree plus the metadata: every file and symlink as
//! `(normal path, metadata)` in tree order (so loading it back restores the
//! `Map` order), the Watchman clocks and the plugins' serialised state.

use crate::file_map::{FileMap, FileMapConfig};
use crate::metadata::FileMetadata;
use crate::FileMapError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufReader, BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Bumped whenever the layout below changes. A cache with another version is
/// ignored (a miss).
pub const FORMAT_VERSION: u32 = 1;

const MAGIC: [u8; 8] = *b"MFMRS\0\0\0";

/// What goes next to the tree: `CacheData` minus `fileSystemData`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheExtras {
    /// `clocks`: path to `WatchmanClockSpec`, as JSON text.
    pub clocks: Vec<(String, String)>,
    /// `plugins`: plugin name to its `getSerializableSnapshot()`, as JSON text
    /// (`None`: `undefined`).
    pub plugins: Vec<(String, Option<String>)>,
}

#[derive(Serialize, Deserialize)]
struct CacheFile {
    format_version: u32,
    root_dir: String,
    extras: CacheExtras,
    files: Vec<(String, FileMetadata)>,
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

impl FileMap {
    /// Writes the cache atomically: a temporary file in the same directory,
    /// then a rename over `path`.
    pub fn write_cache(&self, path: &Path, extras: &CacheExtras) -> Result<(), FileMapError> {
        let files: Vec<(String, FileMetadata)> = self
            .metadata_iterator(true, true)
            .into_iter()
            .filter_map(|e| self.metadata(e.id).map(|m| (e.canonical_path, m.clone())))
            .collect();
        let file = CacheFile {
            format_version: FORMAT_VERSION,
            root_dir: self.root_dir().to_string(),
            extras: extras.clone(),
            files,
        };

        let io_err = |e: std::io::Error| {
            FileMapError::new(format!("Error writing cache to {}: {e}", path.display()))
        };
        let tmp = path.with_file_name(format!(
            "{}.{}.{}.tmp",
            path.file_name()
                .map_or("cache".into(), |n| n.to_string_lossy()),
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| {
            let mut writer = BufWriter::new(fs::File::create(&tmp)?);
            writer.write_all(&MAGIC)?;
            bincode::serialize_into(&mut writer, &file)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            writer.flush()?;
            drop(writer);
            fs::rename(&tmp, path)
        })();
        if let Err(e) = result {
            let _ = fs::remove_file(&tmp);
            return Err(io_err(e));
        }
        Ok(())
    }

    /// Reads a cache written by [`FileMap::write_cache`] for `root_dir`. Any
    /// failure (missing file, other format version, other root, corruption)
    /// is `None`: the caller does a full crawl.
    pub fn read_cache(
        path: &Path,
        root_dir: &str,
        config: FileMapConfig,
    ) -> Option<(FileMap, CacheExtras)> {
        let mut reader = BufReader::new(fs::File::open(path).ok()?);
        let mut magic = [0u8; 8];
        std::io::Read::read_exact(&mut reader, &mut magic).ok()?;
        if magic != MAGIC {
            return None;
        }
        let file: CacheFile = bincode::deserialize_from(reader).ok()?;
        if file.format_version != FORMAT_VERSION || file.root_dir != root_dir {
            return None;
        }
        let mut map = FileMap::new(root_dir, config);
        map.load(file.files).ok()?;
        Some((map, file.extras))
    }
}
