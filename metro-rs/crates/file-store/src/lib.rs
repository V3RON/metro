//! A read replica of metro-file-map's `TreeFS` and a port of Metro's
//! `PackageCache`, answering `fileSystemLookup`, `doesFileExist`, `getPackage`
//! and `getPackageForModule` for the Rust resolver. Results must be identical
//! to the JS for every input.
//!
//! TODO(windows): POSIX separators only.

mod file_store;
mod invariant;
mod json;
mod jsstr;
#[cfg(test)]
mod oracle_data;
mod package_cache;
mod root_path_utils;
mod tree;

pub use file_store::{
    ChangeBatch, ChangeSink, EntryKind, FileStore, FileStoreError, NodeInfo, NodeLookup,
};
pub use invariant::Invariant;
pub use json::{array_index, parse_like_js};
pub use metro_rs_resolver::{Lookup, PackageForModule};
pub use package_cache::{Defer, PackageCache, ReadPackageJsonFn, StoreAnswer};
pub use root_path_utils::RootPathUtils;
pub use tree::FileId;

#[cfg(test)]
mod tests;
