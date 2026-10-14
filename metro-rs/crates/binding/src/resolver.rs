//! The `resolver` component: `metro_rs_resolver` behind metro-resolver's
//! public `resolve()`, and the `FileStore` replica that answers Metro's own
//! lookups natively. The JS side is packages/metro-resolver/src/rust.js and
//! packages/metro/src/node-haste/DependencyGraph/RustFileStore.js; the design
//! is port/design/boundary.md.
//!
//! One call per resolve. Callbacks Rust cannot answer natively go through one
//! JS trampoline, synchronously, while no mutable borrow is held.

use crate::file_map::NativeFileMap;
use metro_rs_file_store::{EntryKind, FileStore as Store, StoreAnswer};
use metro_rs_haste::{HasteError, HasteMap};
use metro_rs_resolver::{
    FileAndDirCandidates, FileCandidates, HostError, Lookup, PackageForModule, PackageJson,
    Resolution, ResolveError, ResolveRequest, ResolverConfig as Config, ResolverHost,
};
use napi::bindgen_prelude::{ClassInstance, Either, Uint8Array};
use napi::{Env, JsFunction, JsObject, JsString, JsUnknown, Result, ValueType};
use napi_derive::napi;
use serde_json::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

// Bits of `flags`. Keep in sync with metro-resolver/src/rust.js.
const ALLOW_HASTE: u32 = 1;
const IS_ESM_IMPORT: u32 = 2;
const CAN_PARSE_URL: u32 = 4;
const NATIVE_FILE_SYSTEM_LOOKUP: u32 = 8;
const NATIVE_DOES_FILE_EXIST: u32 = 16;
const NATIVE_GET_PACKAGE: u32 = 32;
const NATIVE_GET_PACKAGE_FOR_MODULE: u32 = 64;
const VERIFY: u32 = 128;
const EXTRA_NODE_MODULES: u32 = 256;
const NATIVE_HASTE: u32 = 512;

// Trampoline indices. Keep in sync with metro-resolver/src/rust.js.
const CB_FILE_SYSTEM_LOOKUP: u32 = 0;
const CB_DOES_FILE_EXIST: u32 = 1;
const CB_GET_PACKAGE: u32 = 2;
const CB_GET_PACKAGE_FOR_MODULE: u32 = 3;
const CB_RESOLVE_HASTE_MODULE: u32 = 4;
const CB_RESOLVE_HASTE_PACKAGE: u32 = 5;
const CB_RESOLVE_ASSET: u32 = 6;
const CB_LOG_WARNING: u32 = 7;
const CB_EXTRA_NODE_MODULE: u32 = 8;

// Entry kinds of `FileStore.rebuild` / `applyChanges`. Keep in sync with
// RustFileStore.js.
const KIND_FILE: u8 = 0;
const KIND_SYMLINK: u8 = 1;

// `HostError::name` values that are not JS exceptions.
const THREW: &str = "\0threw";
const VERIFY_FAILED: &str = "\0verify";

// ------------------------------------------------------------------- config

#[napi(object)]
pub struct ResolverConfigOptions {
    pub asset_exts: Vec<String>,
    pub disable_hierarchical_lookup: bool,
    pub extra_node_modules: Option<HashMap<String, String>>,
    pub main_fields: Vec<String>,
    pub node_modules_paths: Vec<String>,
    pub prefer_native_platform: bool,
    pub source_exts: Vec<String>,
    pub unstable_condition_names: Vec<String>,
    pub unstable_conditions_by_platform: HashMap<String, Vec<String>>,
    pub unstable_enable_package_exports: bool,
    pub unstable_incremental_resolution: bool,
}

/// A resolver configuration, created once per configuration (rust.js caches
/// it by object identity) and passed to every `resolverResolve`.
#[napi]
pub struct ResolverConfig {
    config: Config,
}

#[napi]
impl ResolverConfig {
    #[napi(constructor)]
    pub fn new(options: ResolverConfigOptions) -> Self {
        let mut config = Config::new();
        config.asset_exts = options.asset_exts.into_iter().collect();
        config.disable_hierarchical_lookup = options.disable_hierarchical_lookup;
        config.extra_node_modules = options.extra_node_modules;
        config.main_fields = options.main_fields;
        config.node_modules_paths = options.node_modules_paths;
        config.prefer_native_platform = options.prefer_native_platform;
        config.source_exts = options.source_exts;
        config.unstable_condition_names = options.unstable_condition_names;
        config.unstable_conditions_by_platform = options.unstable_conditions_by_platform;
        config.unstable_enable_package_exports = options.unstable_enable_package_exports;
        config.unstable_incremental_resolution = options.unstable_incremental_resolution;
        ResolverConfig { config }
    }
}

// --------------------------------------------------------------- file store

/// The Rust read replica of Metro's file map and package cache.
#[napi]
pub struct FileStore {
    store: RefCell<Store>,
}

fn split_nul(joined: &str) -> impl Iterator<Item = &str> {
    // An empty string is no entries, not one empty entry.
    joined.split('\0').filter(move |_| !joined.is_empty())
}

#[napi]
impl FileStore {
    #[napi(constructor)]
    pub fn new(root_dir: String) -> Self {
        FileStore {
            store: RefCell::new(Store::new(&root_dir)),
        }
    }

    fn borrow_mut(&self) -> Result<std::cell::RefMut<'_, Store>> {
        self.store.try_borrow_mut().map_err(|_| {
            napi::Error::from_reason("metro-rs: the file store is already being changed")
        })
    }

    /// Replaces the tree with the given entries: normal paths joined with \0,
    /// a kind per path, and the symlink targets (one per symlink, in order)
    /// joined with \0. The package.json cache is kept, as the JS
    /// `PackageCache` is; callers invalidate changed files.
    #[napi]
    pub fn rebuild(&self, paths: String, kinds: Uint8Array, symlink_targets: String) -> Result<()> {
        let mut store = self.borrow_mut()?;
        store.clear_tree();
        let mut targets = split_nul(&symlink_targets);
        let mut entries = Vec::with_capacity(kinds.len());
        for (path, kind) in split_nul(&paths).zip(kinds.iter()) {
            let kind = match *kind {
                KIND_SYMLINK => EntryKind::Symlink {
                    target: targets.next().unwrap_or(""),
                },
                _ => EntryKind::File,
            };
            entries.push((path, kind));
        }
        store
            .bulk_add(entries)
            .map_err(|error| napi::Error::from_reason(error.to_string()))
    }

    /// `PackageCache.invalidate` for each absolute path, in order.
    #[napi]
    pub fn invalidate_packages(&self, absolute_paths: Vec<String>) -> Result<()> {
        let store = self
            .store
            .try_borrow()
            .map_err(|_| napi::Error::from_reason("metro-rs: the file store is being changed"))?;
        for path in &absolute_paths {
            store.invalidate_package(path);
        }
        Ok(())
    }

    /// Applies one change batch of the file map (see `apply_change_batch`).
    #[napi]
    pub fn apply_changes(
        &self,
        removed_files: Vec<String>,
        removed_directories: Vec<String>,
        added_directories: Vec<String>,
        changed_files: Vec<String>,
        changed_kinds: Uint8Array,
        changed_symlink_targets: Vec<String>,
    ) -> Result<()> {
        let mut store = self.borrow_mut()?;
        let mut targets = changed_symlink_targets.iter();
        let changed: Vec<(&str, EntryKind<'_>)> = changed_files
            .iter()
            .zip(changed_kinds.iter())
            .map(|(path, kind)| {
                let kind = if *kind == KIND_SYMLINK {
                    EntryKind::Symlink {
                        target: targets.next().map(String::as_str).unwrap_or(""),
                    }
                } else {
                    debug_assert_eq!(*kind, KIND_FILE);
                    EntryKind::File
                };
                (path.as_str(), kind)
            })
            .collect();
        store
            .apply_change_batch(
                &removed_files,
                &removed_directories,
                &added_directories,
                &changed,
            )
            .map_err(|error| napi::Error::from_reason(error.to_string()))
    }
}

// ----------------------------------------------------------------- the host

/// The store behind a resolve: phase 2's replica, or the tree of the Rust
/// file map (which also holds Haste).
#[derive(Clone, Copy)]
enum StoreRef<'a> {
    Replica(&'a RefCell<Store>),
    FileMap(&'a NativeFileMap),
}

impl StoreRef<'_> {
    /// Runs `query` on the store. `None` if the store is being changed.
    fn query<R>(&self, query: impl FnOnce(&Store) -> R) -> Option<R> {
        match self {
            StoreRef::Replica(cell) => {
                let store = cell.try_borrow().ok()?;
                Some(query(&store))
            }
            StoreRef::FileMap(map) => map.with_store(query),
        }
    }
}

/// Answers the resolver's lookups for one `resolverResolve` call: natively
/// from the store for the callbacks flagged as native, otherwise (and when the
/// store defers) through the JS trampoline.
struct Host<'a> {
    env: &'a Env,
    callback: &'a JsFunction,
    // Borrowed only for the duration of each native query, never while JS
    // runs, so a callback may change the store (rule 8).
    store: Option<StoreRef<'a>>,
    /// The platform of the resolve, which Metro's Haste callbacks are bound to.
    platform: Option<&'a str>,
    flags: u32,
}

fn napi_to_host(error: napi::Error) -> HostError {
    HostError {
        name: "Error".to_string(),
        message: error.reason,
    }
}

fn threw() -> HostError {
    HostError {
        name: THREW.to_string(),
        message: String::new(),
    }
}

impl Host<'_> {
    fn has(&self, flag: u32) -> bool {
        self.flags & flag != 0
    }

    /// Runs a query on the store when `flag` says the callback is answered
    /// natively. `None` (ask JS instead) when it is not, when the store is
    /// being changed, or when the store hits a case where the JS file map
    /// throws an invariant error, so that the JS exception is kept.
    fn native<T, E>(
        &self,
        flag: u32,
        query: impl FnOnce(&Store) -> std::result::Result<T, E>,
    ) -> Option<T> {
        if !self.has(flag) {
            return None;
        }
        self.store?.query(|store| query(store).ok())?
    }

    /// Runs a Haste query natively when the callback is flagged as native and
    /// the store is a file map with a Haste map. `None` (ask JS instead) also
    /// when Haste reports an error, so that JS throws its own error class.
    fn native_haste(
        &self,
        query: impl FnOnce(&HasteMap) -> std::result::Result<Option<String>, HasteError>,
    ) -> Option<Option<String>> {
        if !self.has(NATIVE_HASTE) {
            return None;
        }
        let StoreRef::FileMap(map) = self.store? else {
            return None;
        };
        map.with_haste(query)?.ok()
    }

    /// Calls the trampoline. A callback that threw comes back as a symbol.
    fn call(&self, index: u32, args: &[&str]) -> std::result::Result<JsUnknown, HostError> {
        self.call_inner(index, args)
            .map_err(napi_to_host)
            .and_then(|value| {
                if value.get_type().map_err(napi_to_host)? == ValueType::Symbol {
                    Err(threw())
                } else {
                    Ok(value)
                }
            })
    }

    fn call_inner(&self, index: u32, args: &[&str]) -> Result<JsUnknown> {
        let mut values: Vec<JsUnknown> = Vec::with_capacity(4);
        values.push(self.env.create_uint32(index)?.into_unknown());
        for arg in args {
            values.push(self.env.create_string(arg)?.into_unknown());
        }
        self.callback.call(None, &values)
    }

    fn js_lookup(&self, path: &str) -> std::result::Result<Lookup, HostError> {
        let value = self.call(CB_FILE_SYSTEM_LOOKUP, &[path])?;
        lookup_from_js(value).map_err(napi_to_host)
    }

    fn js_does_file_exist(&self, path: &str) -> std::result::Result<bool, HostError> {
        let value = self.call(CB_DOES_FILE_EXIST, &[path])?;
        if value.get_type().map_err(napi_to_host)? != ValueType::Boolean {
            return Ok(false);
        }
        value
            .coerce_to_bool()
            .and_then(|b| b.get_value())
            .map_err(napi_to_host)
    }

    fn js_get_package(&self, path: &str) -> std::result::Result<Option<PackageJson>, HostError> {
        let value = self.call(CB_GET_PACKAGE, &[path])?;
        optional_string(value)
            .map_err(napi_to_host)?
            .map(|json| parse_package_json(&json))
            .transpose()
    }

    fn js_get_package_for_module(
        &self,
        path: &str,
    ) -> std::result::Result<Option<PackageForModule>, HostError> {
        let value = self.call(CB_GET_PACKAGE_FOR_MODULE, &[path])?;
        if value.get_type().map_err(napi_to_host)? != ValueType::Object {
            return Ok(None);
        }
        let tuple: JsObject = value.coerce_to_object().map_err(napi_to_host)?;
        let get = |i: u32| -> std::result::Result<String, HostError> {
            let s: JsString = tuple.get_element(i).map_err(napi_to_host)?;
            s.into_utf8()
                .and_then(|s| s.into_owned())
                .map_err(napi_to_host)
        };
        Ok(Some(PackageForModule {
            root_path: get(0)?.into(),
            package_relative_path: get(1)?.into(),
            package_json: parse_package_json(&get(2)?)?,
        }))
    }

    fn js_string_or_null(
        &self,
        index: u32,
        args: &[&str],
    ) -> std::result::Result<Option<String>, HostError> {
        let value = self.call(index, args)?;
        optional_string(value).map_err(napi_to_host)
    }

    /// METRO_RUST_VERIFY: the native answer must equal the JS one.
    fn verify<T: std::fmt::Debug>(
        &self,
        name: &str,
        arg: &str,
        native: T,
        js: impl FnOnce() -> std::result::Result<T, HostError>,
        same: impl Fn(&T, &T) -> bool,
    ) -> std::result::Result<T, HostError> {
        if !self.has(VERIFY) {
            return Ok(native);
        }
        let js = js()?;
        if same(&native, &js) {
            Ok(native)
        } else {
            Err(HostError {
                name: VERIFY_FAILED.to_string(),
                message: format!(
                    "METRO_RUST_VERIFY: {name}({arg:?}) differs.\n  Rust: {native:?}\n  JS:   {js:?}"
                ),
            })
        }
    }
}

fn lookup_from_js(value: JsUnknown) -> Result<Lookup> {
    let object: JsObject = value.coerce_to_object()?;
    let exists: JsUnknown = object.get_named_property("exists")?;
    if !exists.coerce_to_bool()?.get_value()? {
        return Ok(Lookup::NotFound);
    }
    let kind: JsString = object.get_named_property("type")?;
    let real_path: JsString = object.get_named_property("realPath")?;
    let real_path = real_path.into_utf8()?.into_owned()?;
    Ok(if kind.into_utf8()?.as_str()? == "d" {
        Lookup::Dir { real_path }
    } else {
        Lookup::File { real_path }
    })
}

fn optional_string(value: JsUnknown) -> Result<Option<String>> {
    if value.get_type()? != ValueType::String {
        return Ok(None);
    }
    let s: JsString = value.coerce_to_string()?;
    Ok(Some(s.into_utf8()?.into_owned()?))
}

/// A package.json from a JS callback, as `JSON.stringify` of the object, so
/// key order is the JS object's order.
fn parse_package_json(json: &str) -> std::result::Result<PackageJson, HostError> {
    serde_json::from_str::<Value>(json)
        .map(Arc::new)
        .map_err(|error| HostError {
            name: "Error".to_string(),
            message: format!("metro-rs: could not read a package.json from JS: {error}"),
        })
}

/// Equal, including object key order (which decides "exports" conditions).
fn same_json(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .zip(b.iter())
                    .all(|((ka, va), (kb, vb))| ka == kb && same_json(va, vb))
        }
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| same_json(a, b))
        }
        _ => a == b,
    }
}

fn same_package(a: &Option<PackageForModule>, b: &Option<PackageForModule>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.root_path == b.root_path
                && a.package_relative_path == b.package_relative_path
                && same_json(&a.package_json, &b.package_json)
        }
        _ => false,
    }
}

fn same_package_json(a: &Option<PackageJson>, b: &Option<PackageJson>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => same_json(a, b),
        _ => false,
    }
}

impl ResolverHost for Host<'_> {
    fn file_system_lookup(&self, path: &str) -> std::result::Result<Lookup, HostError> {
        match self.native(NATIVE_FILE_SYSTEM_LOOKUP, |store| store.lookup(path)) {
            Some(native) => self.verify(
                "fileSystemLookup",
                path,
                native,
                || self.js_lookup(path),
                |a, b| a == b,
            ),
            None => self.js_lookup(path),
        }
    }

    fn does_file_exist(&self, path: &str) -> std::result::Result<bool, HostError> {
        match self.native(NATIVE_DOES_FILE_EXIST, |store| store.exists(path)) {
            Some(native) => self.verify(
                "doesFileExist",
                path,
                native,
                || self.js_does_file_exist(path),
                |a, b| a == b,
            ),
            None => self.js_does_file_exist(path),
        }
    }

    fn get_package(&self, path: &str) -> std::result::Result<Option<PackageJson>, HostError> {
        match self.native(NATIVE_GET_PACKAGE, |s| Ok::<_, ()>(s.get_package(path))) {
            Some(StoreAnswer::Answer(native)) => self.verify(
                "getPackage",
                path,
                native,
                || self.js_get_package(path),
                same_package_json,
            ),
            // Not native, or the store cannot be sure it matches JSON.parse.
            Some(StoreAnswer::Defer) | None => self.js_get_package(path),
        }
    }

    fn get_package_for_module(
        &self,
        path: &str,
    ) -> std::result::Result<Option<PackageForModule>, HostError> {
        match self.native(NATIVE_GET_PACKAGE_FOR_MODULE, |s| {
            Ok::<_, ()>(s.get_package_for_module(path))
        }) {
            Some(StoreAnswer::Answer(native)) => self.verify(
                "getPackageForModule",
                path,
                native,
                || self.js_get_package_for_module(path),
                same_package,
            ),
            Some(StoreAnswer::Defer) | None => self.js_get_package_for_module(path),
        }
    }

    fn resolve_haste_module(&self, name: &str) -> std::result::Result<Option<String>, HostError> {
        // `getHasteModulePath`: `hasteMap.getModule(name, platform, true)`.
        match self.native_haste(|haste| haste.get_module(name, self.platform, true, None)) {
            Some(native) => self.verify(
                "resolveHasteModule",
                name,
                native,
                || self.js_string_or_null(CB_RESOLVE_HASTE_MODULE, &[name]),
                |a, b| a == b,
            ),
            None => self.js_string_or_null(CB_RESOLVE_HASTE_MODULE, &[name]),
        }
    }

    fn resolve_haste_package(&self, name: &str) -> std::result::Result<Option<String>, HostError> {
        // `getHastePackagePath`: `hasteMap.getPackage(name, platform, true)`.
        match self.native_haste(|haste| haste.get_package(name, self.platform, true)) {
            Some(native) => self.verify(
                "resolveHastePackage",
                name,
                native,
                || self.js_string_or_null(CB_RESOLVE_HASTE_PACKAGE, &[name]),
                |a, b| a == b,
            ),
            None => self.js_string_or_null(CB_RESOLVE_HASTE_PACKAGE, &[name]),
        }
    }

    fn resolve_asset(
        &self,
        dir_path: &str,
        asset_name: &str,
        extension: &str,
    ) -> Option<Vec<String>> {
        // The trampoline maps a throwing callback to null, as resolveAsset.js.
        let value = self
            .call(CB_RESOLVE_ASSET, &[dir_path, asset_name, extension])
            .ok()?;
        if !value.is_array().ok()? {
            return None;
        }
        let array: JsObject = value.coerce_to_object().ok()?;
        let len = array.get_array_length().ok()?;
        let mut files = Vec::with_capacity(len as usize);
        for i in 0..len {
            let s: JsString = array.get_element(i).ok()?;
            files.push(s.into_utf8().ok()?.into_owned().ok()?);
        }
        Some(files)
    }

    fn log_warning(&self, message: &str) -> std::result::Result<(), HostError> {
        self.call(CB_LOG_WARNING, &[message]).map(|_| ())
    }

    fn extra_node_module(
        &self,
        _config: &Config,
        package_name: &str,
    ) -> std::result::Result<Option<String>, HostError> {
        if !self.has(EXTRA_NODE_MODULES) {
            return Ok(None);
        }
        // The trampoline returns null unless the entry is truthy, and throws
        // (as JS would when joining it) for a truthy non-string.
        self.js_string_or_null(CB_EXTRA_NODE_MODULE, &[package_name])
    }

    fn can_parse_url(&self, _specifier: &str) -> bool {
        self.has(CAN_PARSE_URL)
    }
}

// ------------------------------------------------------------------ resolve

/// Resolves `specifier` from `origin_module_path`. Returns a string (a source
/// file), an array of strings (asset files), null (the empty module), or an
/// object `{error, ...fields}` from which rust.js builds the same JS error.
#[napi]
#[allow(clippy::too_many_arguments)] // One crossing per resolve, by design.
pub fn resolver_resolve(
    env: Env,
    config: &ResolverConfig,
    store: Option<Either<ClassInstance<FileStore>, ClassInstance<NativeFileMap>>>,
    origin_module_path: String,
    specifier: String,
    platform: Option<String>,
    flags: u32,
    callback: JsFunction,
) -> Result<JsUnknown> {
    let host = Host {
        env: &env,
        callback: &callback,
        store: store.as_ref().map(|store| match store {
            Either::A(replica) => StoreRef::Replica(&replica.store),
            Either::B(file_map) => StoreRef::FileMap(file_map),
        }),
        platform: platform.as_deref(),
        flags,
    };
    let options = Value::Object(Default::default());
    let request = ResolveRequest {
        origin_module_path: &origin_module_path,
        allow_haste: flags & ALLOW_HASTE != 0,
        is_esm_import: flags & IS_ESM_IMPORT != 0,
        // Only custom resolvers read these, and they run in JS.
        dev: false,
        custom_resolver_options: &options,
    };
    let result = metro_rs_resolver::resolve_default(
        &config.config,
        &host,
        &request,
        &specifier,
        platform.as_deref(),
    );
    match result {
        Ok(Resolution::SourceFile { file_path }) => {
            Ok(env.create_string(&file_path)?.into_unknown())
        }
        Ok(Resolution::AssetFiles { file_paths }) => {
            Ok(string_array(&env, &file_paths)?.into_unknown())
        }
        Ok(Resolution::Empty) => Ok(env.get_null()?.into_unknown()),
        Err(error) => Ok(error_to_js(&env, &error)?.into_unknown()),
    }
}

fn string_array(env: &Env, items: &[String]) -> Result<JsObject> {
    let mut array = env.create_array_with_length(items.len())?;
    for (i, item) in items.iter().enumerate() {
        array.set_element(i as u32, env.create_string(item)?)?;
    }
    Ok(array)
}

fn file_candidates_to_js(env: &Env, candidates: &FileCandidates) -> Result<JsObject> {
    let mut object = env.create_object()?;
    match candidates {
        FileCandidates::Asset { name } => {
            object.set_named_property("type", env.create_string("asset")?)?;
            object.set_named_property("name", env.create_string(name)?)?;
        }
        FileCandidates::SourceFile {
            file_path_prefix,
            candidate_exts,
        } => {
            object.set_named_property("type", env.create_string("sourceFile")?)?;
            object.set_named_property("filePathPrefix", env.create_string(file_path_prefix)?)?;
            object.set_named_property("candidateExts", string_array(env, candidate_exts)?)?;
        }
    }
    Ok(object)
}

fn file_and_dir_candidates_to_js(env: &Env, candidates: &FileAndDirCandidates) -> Result<JsObject> {
    let mut object = env.create_object()?;
    for (key, value) in [("file", &candidates.file), ("dir", &candidates.dir)] {
        match value {
            Some(c) => object.set_named_property(key, file_candidates_to_js(env, c)?)?,
            None => object.set_named_property(key, env.get_null()?)?,
        }
    }
    Ok(object)
}

fn error_to_js(env: &Env, error: &ResolveError) -> Result<JsObject> {
    let mut object = env.create_object()?;
    let set_str = |object: &mut JsObject, key: &str, value: &str| -> Result<()> {
        object.set_named_property(key, env.create_string(value)?)
    };
    match error {
        ResolveError::Host(host_error) if host_error.name == THREW => {
            set_str(&mut object, "error", "Host")?;
        }
        ResolveError::Host(host_error) if host_error.name == VERIFY_FAILED => {
            set_str(&mut object, "error", "Verify")?;
            set_str(&mut object, "message", &host_error.message)?;
        }
        ResolveError::Host(host_error) => {
            // A failure inside the binding itself (not a JS exception).
            return Err(napi::Error::from_reason(host_error.message.clone()));
        }
        ResolveError::FailedToResolveNameError {
            dir_paths,
            extra_paths,
        } => {
            set_str(&mut object, "error", "FailedToResolveNameError")?;
            object.set_named_property("dirPaths", string_array(env, dir_paths)?)?;
            object.set_named_property("extraPaths", string_array(env, extra_paths)?)?;
        }
        ResolveError::FailedToResolvePathError { candidates } => {
            set_str(&mut object, "error", "FailedToResolvePathError")?;
            object.set_named_property(
                "candidates",
                file_and_dir_candidates_to_js(env, candidates)?,
            )?;
        }
        ResolveError::FailedToResolveUnsupportedError { message, .. } => {
            set_str(&mut object, "error", "FailedToResolveUnsupportedError")?;
            set_str(&mut object, "message", message)?;
        }
        ResolveError::InvalidPackageError {
            file_candidates,
            index_candidates,
            main_module_path,
            package_json_path,
        } => {
            set_str(&mut object, "error", "InvalidPackageError")?;
            object.set_named_property(
                "fileCandidates",
                file_candidates_to_js(env, file_candidates)?,
            )?;
            object.set_named_property(
                "indexCandidates",
                file_candidates_to_js(env, index_candidates)?,
            )?;
            set_str(&mut object, "mainModulePath", main_module_path)?;
            set_str(&mut object, "packageJsonPath", package_json_path)?;
        }
        ResolveError::InvalidPackageConfigurationError {
            reason,
            package_path,
        } => {
            set_str(&mut object, "error", "InvalidPackageConfigurationError")?;
            set_str(&mut object, "reason", reason)?;
            set_str(&mut object, "packagePath", package_path)?;
        }
        ResolveError::PackagePathNotExportedError { message } => {
            set_str(&mut object, "error", "PackagePathNotExportedError")?;
            set_str(&mut object, "message", message)?;
        }
        ResolveError::PackageImportNotResolvedError {
            import_specifier,
            reason,
        } => {
            set_str(&mut object, "error", "PackageImportNotResolvedError")?;
            set_str(&mut object, "importSpecifier", import_specifier)?;
            set_str(&mut object, "reason", reason)?;
        }
        ResolveError::MissingFileInHastePackageError {
            candidates,
            module_name,
            package_name,
            path_in_module,
        } => {
            set_str(&mut object, "error", "MissingFileInHastePackageError")?;
            object.set_named_property(
                "candidates",
                file_and_dir_candidates_to_js(env, candidates)?,
            )?;
            set_str(&mut object, "moduleName", module_name)?;
            set_str(&mut object, "packageName", package_name)?;
            set_str(&mut object, "pathInModule", path_in_module)?;
        }
    }
    Ok(object)
}
