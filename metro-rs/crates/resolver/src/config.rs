//! `ResolutionContext` split in three: a long-lived config (built once, reused
//! for every resolve), the host that answers lookups (passed per call, since it
//! may wrap per-call JS callbacks), and a small per-call request.

use crate::host::{PackageJson, ResolverHost};
use serde_json::Value;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// The long-lived, plain-data part of `ResolutionContext`. Resolving never
/// clones it.
pub struct ResolverConfig {
    pub asset_exts: HashSet<String>,
    pub disable_hierarchical_lookup: bool,
    pub extra_node_modules: Option<HashMap<String, String>>,
    /// The ordered list of fields to read in `package.json` to resolve a main
    /// entry point based on the "browser" field spec.
    pub main_fields: Vec<String>,
    pub node_modules_paths: Vec<String>,
    pub prefer_native_platform: bool,
    pub source_exts: Vec<String>,
    pub unstable_condition_names: Vec<String>,
    pub unstable_conditions_by_platform: HashMap<String, Vec<String>>,
    pub unstable_enable_package_exports: bool,
    pub unstable_incremental_resolution: bool,
    /// See [`ExportsCache`].
    pub(crate) exports_cache: RefCell<ExportsCache>,
}

/// The "exports" field of a package after normalisation and reduction by the
/// condition names of a request, cached per package.json value, `isESMImport`
/// and platform (the inputs of the reduction besides this config). The JS
/// caches the normalised field per object in a WeakMap; this also keeps the
/// reduced map. Entries hold their package.json, so the key (the address of
/// its "exports" value) cannot be reused while the entry exists; a package.json
/// replaced after a change is a different value and gets a new entry.
/// The address of the "exports" value, `isESMImport` and the platform.
pub(crate) type ExportsCacheKey = (usize, bool, Option<Box<str>>);

#[derive(Default)]
pub(crate) struct ExportsCache {
    entries: std::collections::HashMap<ExportsCacheKey, Rc<ExportsEntry>>,
}

pub(crate) struct ExportsEntry {
    _package_json: PackageJson,
    /// The normalised map's keys, in order.
    pub keys: Vec<Box<str>>,
    /// The reduced map: subpath and target, in order.
    pub reduced: Vec<(Box<str>, Option<Box<str>>)>,
}

/// Replaced packages stay cached until this many entries, then all go.
const EXPORTS_CACHE_LIMIT: usize = 4096;

impl ExportsCache {
    pub fn get(&self, key: &ExportsCacheKey) -> Option<Rc<ExportsEntry>> {
        self.entries.get(key).cloned()
    }

    pub fn insert(
        &mut self,
        key: ExportsCacheKey,
        package_json: PackageJson,
        keys: Vec<Box<str>>,
        reduced: Vec<(Box<str>, Option<Box<str>>)>,
    ) -> Rc<ExportsEntry> {
        if self.entries.len() >= EXPORTS_CACHE_LIMIT {
            self.entries.clear();
        }
        let entry = Rc::new(ExportsEntry {
            _package_json: package_json,
            keys,
            reduced,
        });
        self.entries.insert(key, entry.clone());
        entry
    }
}

impl ResolverConfig {
    /// A config with the defaults of the JS test utility
    /// `createResolutionContext` (which closely match metro-config).
    pub fn new() -> Self {
        ResolverConfig {
            asset_exts: ["jpg", "png"].iter().map(|s| s.to_string()).collect(),
            disable_hierarchical_lookup: false,
            extra_node_modules: None,
            main_fields: vec!["browser".to_string(), "main".to_string()],
            node_modules_paths: Vec::new(),
            prefer_native_platform: false,
            source_exts: ["js", "jsx", "json", "ts", "tsx"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            unstable_condition_names: vec!["require".to_string()],
            unstable_conditions_by_platform: HashMap::from([(
                "web".to_string(),
                vec!["browser".to_string()],
            )]),
            unstable_enable_package_exports: false,
            unstable_incremental_resolution: false,
            exports_cache: RefCell::new(ExportsCache::default()),
        }
    }
}

impl Default for ResolverConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// The per-call part of `ResolutionContext`. Borrows everything.
#[derive(Debug, Clone, Copy)]
pub struct ResolveRequest<'a> {
    /// Full path of the module that is requiring or importing the module to be
    /// resolved.
    pub origin_module_path: &'a str,
    pub allow_haste: bool,
    /// Whether the dependency was declared with an ESM import (asserts the
    /// "import" condition rather than "require").
    pub is_esm_import: bool,
    /// Is resolving for a development bundle. Not used by the algorithm; for
    /// custom resolvers.
    pub dev: bool,
    /// Opaque; for custom resolvers.
    pub custom_resolver_options: &'a Value,
}

/// What the algorithm functions pass around: the shape of the JS `context`.
#[derive(Clone, Copy)]
pub(crate) struct Ctx<'a> {
    pub config: &'a ResolverConfig,
    pub host: &'a dyn ResolverHost,
    pub request: &'a ResolveRequest<'a>,
}

impl Ctx<'_> {
    pub fn host(&self) -> &dyn ResolverHost {
        self.host
    }
}
