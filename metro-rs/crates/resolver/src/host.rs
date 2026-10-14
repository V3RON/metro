//! The seam between the pure resolution algorithm and everything external:
//! the file system, package.json parsing and caching, Haste, assets, logging,
//! and user callbacks.

use crate::config::{ResolveRequest, ResolverConfig};
use crate::error::{HostError, ResolveError};
use crate::Resolution;
use serde_json::Value;
use std::sync::Arc;

/// A parsed package.json. Shared, so that a host that caches packages hands
/// them out without conversion or copying. Key order is JSON key order
/// (`serde_json` with `preserve_order`).
pub type PackageJson = Arc<Value>;

/// `types.js` `FileSystemLookup` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    NotFound,
    File { real_path: String },
    Dir { real_path: String },
}

/// `types.js` `PackageForModule`.
#[derive(Debug, Clone)]
pub struct PackageForModule {
    pub root_path: String,
    /// A system-separated subpath (with no './' prefix) that reflects the
    /// subpath of the given candidate relative to `root_path`.
    pub package_relative_path: String,
    pub package_json: PackageJson,
}

/// Every lookup except `resolve_asset` and `log_warning` can fail with a
/// `HostError` (a JS callback throwing); `resolve` propagates it unchanged as
/// `ResolveError::Host`, where the JS exception would propagate.
/// Everything the resolver needs from the outside world. All methods take
/// `&self`; use interior mutability for caches. The trait is object-safe.
pub trait ResolverHost {
    /// `fileSystemLookup`: whether `path` exists, whether it is a file or a
    /// directory, and its real path (symlinks resolved).
    fn file_system_lookup(&self, path: &str) -> Result<Lookup, HostError>;

    /// `doesFileExist`: whether a regular file exists at `path`.
    fn does_file_exist(&self, path: &str) -> Result<bool, HostError>;

    /// `getPackage`: the parsed contents of the given `package.json` file.
    fn get_package(&self, package_json_path: &str) -> Result<Option<PackageJson>, HostError>;

    /// `getPackageForModule`: the closest package scope, parsed `package.json`
    /// and relative subpath for an absolute candidate path (which need not
    /// exist), or `None` if there is no package.json closer than the nearest
    /// `node_modules` directory.
    fn get_package_for_module(
        &self,
        absolute_module_path: &str,
    ) -> Result<Option<PackageForModule>, HostError>;

    /// `resolveHasteModule`: the file providing a Haste module of that name.
    fn resolve_haste_module(&self, name: &str) -> Result<Option<String>, HostError>;

    /// `resolveHastePackage`: the package.json providing a Haste package.
    fn resolve_haste_package(&self, name: &str) -> Result<Option<String>, HostError>;

    /// `resolveAsset`: the files of an asset (e.g. `icon@2x.png`). The JS
    /// caller swallows exceptions from this callback; a host must map a
    /// failure to `None`.
    fn resolve_asset(
        &self,
        dir_path: &str,
        asset_name: &str,
        extension: &str,
    ) -> Option<Vec<String>>;

    /// `unstable_logWarning`.
    fn log_warning(&self, message: &str);

    /// `context.resolveRequest`, the user's custom resolver. Return `None`
    /// when there is none (or it is the default `resolve`); the algorithm then
    /// runs. When it returns `Some`, the algorithm does not run. A custom
    /// resolver that delegates back to default resolution (JS
    /// `context.resolveRequest(...)`) must call `crate::resolve_default`.
    fn custom_resolve_request(
        &self,
        _config: &ResolverConfig,
        _request: &ResolveRequest<'_>,
        _specifier: &str,
        _platform: Option<&str>,
    ) -> Option<Result<Resolution, ResolveError>> {
        None
    }

    /// `context.schemeResolvers[scheme]`. `scheme` is the lowercased part
    /// before the first ':'. Return `None` when no resolver is registered for
    /// it. Delegation back into default resolution uses
    /// `crate::resolve_default`, as above. A thrown JS error maps to
    /// `Err(ResolveError::Host(..))`, or to the resolution error it is.
    fn scheme_resolver(
        &self,
        _scheme: &str,
        _config: &ResolverConfig,
        _request: &ResolveRequest<'_>,
        _specifier: &str,
        _platform: Option<&str>,
    ) -> Option<Result<Resolution, ResolveError>> {
        None
    }
}
