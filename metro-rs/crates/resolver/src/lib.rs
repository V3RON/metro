//! Module resolution: a pure Rust port of `metro-resolver`.
//!
//! The algorithm lives here; everything external (file system, package.json
//! reads, Haste, assets, warnings, user callbacks) is behind [`ResolverHost`].
//! No Node bindings here, see the `binding` crate.
//!
//! The JS `ResolutionContext` is split in two: the long-lived
//! [`ResolverConfig`] (built once, holds the host) and the per-call
//! [`ResolveRequest`].

mod config;
mod error;
mod host;
mod package_exports;
mod package_imports;
mod package_resolve;
pub mod path;
mod resolution_cache;
mod resolve;
mod resolve_asset;
mod utils;

pub use config::{ResolveRequest, ResolverConfig};
pub use error::{
    format_file_candidates, FileAndDirCandidates, FileCandidates, HostError, ResolveError,
};
pub use host::{Lookup, PackageForModule, PackageJson, ResolverHost};
pub use resolution_cache::{origin_key, ResolutionCache};
pub use resolve::{resolve, resolve_default};

/// `types.js` `Resolution` (without `virtualModule`, which only a custom
/// resolver can produce).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    SourceFile { file_path: String },
    AssetFiles { file_paths: Vec<String> },
    Empty,
}
