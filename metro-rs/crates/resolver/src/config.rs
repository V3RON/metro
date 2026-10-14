//! `ResolutionContext` split in three: a long-lived config (built once, reused
//! for every resolve), the host that answers lookups (passed per call, since it
//! may wrap per-call JS callbacks), and a small per-call request.

use crate::host::ResolverHost;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

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
