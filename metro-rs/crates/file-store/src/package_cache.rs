//! Port of `packages/metro/src/node-haste/PackageCache.js`, plus the callbacks
//! `createModuleResolver` builds on top of it (`getPackage`,
//! `getPackageForModule`).
//!
//! TODO(windows): `sep` is `/`.
//!
//! Interior mutability: lookups take `&self`. No `RefCell` borrow is ever held
//! across `hierarchical_lookup` or the package.json reader.

use crate::file_store::FileStore;
use crate::json;
use crate::tree::FxHasher;
use metro_rs_resolver::path;
use metro_rs_resolver::PackageForModule;
use serde_json::Value;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::hash::BuildHasherDefault;

// Keyed by absolute paths on every resolver lookup; FxHash as in the tree.
type FxHashMap<K, V> = HashMap<K, V, BuildHasherDefault<FxHasher>>;
type FxHashSet<K> = HashSet<K, BuildHasherDefault<FxHasher>>;
use std::sync::Arc;

/// An answer, or a request that the caller ask Metro's JS callback instead
/// (Rust cannot be sure it matches the JS, including its error text).
#[derive(Debug, Clone, PartialEq)]
pub enum StoreAnswer<T> {
    Answer(T),
    Defer,
}

/// Rust cannot be sure it matches `JSON.parse(readFileSync(p, 'utf8'))`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Defer;

pub type ReadPackageJsonFn = Box<dyn Fn(&str) -> Result<Arc<Value>, Defer>>;

#[derive(Clone)]
struct CachedPackage {
    root_path: String,
    package_json: Arc<Value>,
}

#[derive(Default)]
struct Caches {
    package_cache: FxHashMap<String, CachedPackage>,
    // Single cache: module path -> pre-built result object, or None (no package)
    result_by_module_path: FxHashMap<String, Option<PackageForModule>>,
    // Reverse index for invalidation: package.json path -> set of module paths
    module_paths_by_package_path: FxHashMap<String, FxHashSet<String>>,
    // Module paths that resolved to no package.json (None), for invalidation
    module_paths_with_no_package: FxHashSet<String>,
}

pub struct PackageCache {
    read_package_json: ReadPackageJsonFn,
    caches: RefCell<Caches>,
}

impl Default for PackageCache {
    fn default() -> Self {
        Self::new()
    }
}

impl PackageCache {
    pub fn new() -> Self {
        Self {
            read_package_json: Box::new(|p| json::read_package_json(p).map(Arc::new)),
            caches: RefCell::new(Caches::default()),
        }
    }

    pub fn set_reader(&mut self, reader: ReadPackageJsonFn) {
        self.read_package_json = reader;
    }

    /// `getPackage(filePath)`.
    fn get_package_inner(&self, file_path: &str) -> Result<CachedPackage, Defer> {
        if let Some(c) = self.caches.borrow().package_cache.get(file_path) {
            return Ok(c.clone());
        }
        // No borrow is held while reading.
        let package_json = (self.read_package_json)(file_path).map_err(|_| Defer)?;
        let cached = CachedPackage {
            root_path: path::dirname(file_path),
            package_json,
        };
        self.caches
            .borrow_mut()
            .package_cache
            .insert(file_path.to_string(), cached.clone());
        Ok(cached)
    }

    /// createModuleResolver's `getPackage`: `packageCache.getPackage(p).packageJson ?? null`,
    /// catch -> null. Failures are `Defer`.
    pub fn get_package(&self, package_json_path: &str) -> StoreAnswer<Option<Arc<Value>>> {
        match self.get_package_inner(package_json_path) {
            Ok(c) if c.package_json.is_null() => StoreAnswer::Answer(None),
            Ok(c) => StoreAnswer::Answer(Some(c.package_json)),
            Err(Defer) => StoreAnswer::Defer,
        }
    }

    /// `getPackageForModule`; `getClosestPackage` is `hierarchical_lookup(path,
    /// "package.json", Some("node_modules"), false)` on `fs`
    /// (`DependencyGraph._getClosestPackage`).
    pub fn get_package_for_module(
        &self,
        fs: &FileStore,
        absolute_module_path: &str,
    ) -> StoreAnswer<Option<PackageForModule>> {
        if let Some(cached) = self
            .caches
            .borrow()
            .result_by_module_path
            .get(absolute_module_path)
        {
            // Distinguish between `None` (positively no closest package) and
            // not cached yet.
            return StoreAnswer::Answer(cached.clone());
        }

        let closest = fs.hierarchical_lookup(
            absolute_module_path,
            "package.json",
            Some("node_modules"),
            false,
        );
        let Some((package_path, package_relative_path)) = closest else {
            let mut c = self.caches.borrow_mut();
            c.result_by_module_path
                .insert(absolute_module_path.to_string(), None);
            c.module_paths_with_no_package
                .insert(absolute_module_path.to_string());
            return StoreAnswer::Answer(None);
        };

        // Track module->package for invalidation (before reading, as the JS
        // does: a failed read leaves the entry behind).
        self.caches
            .borrow_mut()
            .module_paths_by_package_path
            .entry(package_path.clone())
            .or_default()
            .insert(absolute_module_path.to_string());

        let Ok(pkg) = self.get_package_inner(&package_path) else {
            return StoreAnswer::Defer;
        };

        // Cache the pre-built result object
        let result = PackageForModule {
            package_json: pkg.package_json,
            package_relative_path: package_relative_path.into(),
            root_path: pkg.root_path.into(),
        };
        self.caches
            .borrow_mut()
            .result_by_module_path
            .insert(absolute_module_path.to_string(), Some(result.clone()));
        StoreAnswer::Answer(Some(result))
    }

    pub fn invalidate(&self, file_path: &str) {
        let mut guard = self.caches.borrow_mut();
        let c = &mut *guard;
        c.package_cache.remove(file_path);

        // Clean up any cached result for this module path (including None).
        // Derive the package.json path from the cached result to clean up the
        // reverse index.
        let cached_result = c.result_by_module_path.remove(file_path);
        c.module_paths_with_no_package.remove(file_path);

        if let Some(Some(cached)) = cached_result {
            let package_path = format!("{}/package.json", cached.root_path);
            if let Some(modules) = c.module_paths_by_package_path.get_mut(&package_path) {
                modules.remove(file_path);
                if modules.is_empty() {
                    c.module_paths_by_package_path.remove(&package_path);
                }
            }
        }

        // If filePath is a package.json, invalidate all module lookups pointing to it
        if let Some(module_paths) = c.module_paths_by_package_path.remove(file_path) {
            for module_path in module_paths {
                c.result_by_module_path.remove(&module_path);
            }
        }

        // If a package.json was created, modified, or deleted, invalidate all
        // None-cached module results, since modules that previously had no
        // enclosing package.json may now resolve to this one.
        if file_path.ends_with("/package.json") {
            for module_path in std::mem::take(&mut c.module_paths_with_no_package) {
                c.result_by_module_path.remove(&module_path);
            }
        }
    }
}
