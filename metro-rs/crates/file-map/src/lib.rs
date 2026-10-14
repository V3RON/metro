//! The Rust core of metro-file-map: file metadata, the node crawler,
//! `getDifference`, processing of a delta, the change aggregator, the
//! `TreeFS` queries and the disk cache. Behaviour must be identical to the
//! JS in `packages/metro-file-map/src`.
//!
//! The tree itself (`TreeFS`' maps and `lookup`) is `metro-rs-file-store`;
//! this crate keeps the metadata next to it, per `FileId`.
//!
//! TODO(windows): POSIX separators only.

mod aggregator;
mod cache;
mod crawl;
mod errors;
mod file_map;
mod js_map;
mod js_regex;
mod metadata;
mod process;
mod queries;

pub use aggregator::{Changes, FileSystemChangeAggregator};
pub use cache::{CacheExtras, FORMAT_VERSION};
pub use crawl::{find, mtime_ms, mtime_ms_from_parts, CrawlOptions, CrawlOutput, IgnoreRules};
pub use errors::FileMapError;
pub use file_map::{
    DeltaSession, Difference, FileMap, FileMapConfig, FileStats, MapLookup, MetadataEntry,
    PluginJob, PluginJobResult, TouchInput, TouchOutput, WatchEvent,
};
pub use js_map::{JsMap, JsSet};
pub use js_regex::{JsRegExp, RegExpError};
pub use metadata::{FileMetadata, Symlink};
pub use process::{
    get_worker_input, hash_file, read_and_hash, read_link, sha1_hex, PluginFilter, WorkerInput,
};
pub use queries::{MatchFilesOptions, Sha1Job, Sha1Result};

/// `FileData`: normal path to metadata, in JS `Map` order.
pub type FileData = JsMap<FileMetadata>;

#[cfg(test)]
mod tests;
