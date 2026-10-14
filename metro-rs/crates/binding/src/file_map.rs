//! The `file-map` component: `metro_rs_file_map::FileMap` and
//! `metro_rs_haste::HasteMap` behind metro-file-map's `FileMap`. The JS side
//! is packages/metro-file-map/src/index.js (the Rust-mode paths),
//! lib/RustFileSystem.js and plugins/HastePlugin.js; the design is
//! port/design/phase-3.md.
//!
//! Crossing rules (RULES.md section 7): one call per unit of work (a crawl, a
//! delta, a watch batch, one `FileSystem` method); strings only at the public
//! edges; the state is borrowed for the duration of one call, never across a
//! call into JavaScript. This file map never calls JavaScript at all.
//!
//! Errors that Node would report with a `code` (`ENOENT`) are thrown as real
//! `Error` objects with that `code`, so that the JS can tell them apart.

use metro_rs_file_map::{
    find, read_cache_file, write_cache_file, CacheData, CacheExtras, Changes, CrawlOptions,
    CrawlOutput, DeltaIo, DeltaIoResult, DeltaSession, Difference, FileData, FileMap,
    FileMapConfig, FileMapError, FileMetadata, IgnoreRules, JsRegExp, MapLookup, MatchFilesOptions,
    PluginFilter, PluginJobResult, PreparedDelta, Sha1Job, Symlink, TouchInput, WatchEvent,
};
use metro_rs_haste::{HasteDelta, HasteError, HasteMap, HasteOptions, ModuleType};
use napi::bindgen_prelude::{AsyncTask, Buffer, External, Float64Array, Uint32Array, Uint8Array};
use napi::{Env, JsFunction, JsObject, JsUnknown, Result, Status, Task};
use napi_derive::napi;
use serde_json::Value;
use std::cell::{Ref, RefCell, RefMut};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

// ------------------------------------------------------------------- errors

/// An `Error` the way Node makes it: a message, and a `code` if there is one.
fn new_error(env: &Env, name: Option<&str>, code: Option<&str>, message: &str) -> Result<JsObject> {
    let constructor: JsFunction = env.get_global()?.get_named_property("Error")?;
    let mut error = constructor.new_instance(&[env.create_string(message)?])?;
    if let Some(code) = code {
        error.set_named_property("code", env.create_string(code)?)?;
    }
    if let Some(name) = name {
        error.set_named_property("name", env.create_string(name)?)?;
    }
    Ok(error)
}

fn throw(env: &Env, error: Result<JsObject>) -> napi::Error {
    match error.and_then(|error| env.throw(error)) {
        Ok(()) => napi::Error::new(Status::PendingException, ""),
        Err(error) => error,
    }
}

/// Throws what a failed `fs` call or JS throw would have thrown.
fn file_map_error(env: &Env, error: &FileMapError) -> napi::Error {
    throw(
        env,
        new_error(env, None, error.code.as_deref(), &error.message),
    )
}

/// Throws what `invariant(...)` throws.
fn invariant_error(env: &Env, error: &metro_rs_file_store::Invariant) -> napi::Error {
    throw(
        env,
        new_error(env, Some("Invariant Violation"), None, &error.0),
    )
}

// -------------------------------------------------------------------- state

struct HasteState {
    map: HasteMap,
    /// The plugin's data slot in `FileMetadata::plugin_data`.
    slot: usize,
    initialized: bool,
}

/// A watcher event that was processed and waits for the batch to be applied.
struct Touch {
    metadata: FileMetadata,
    plugins_to_run: Vec<usize>,
}

struct State {
    map: FileMap,
    haste: Option<HasteState>,
    ignore_pattern: JsRegExp,
    extensions: Vec<String>,
    include_symlinks: bool,
    retain_all_files: bool,
    health_check_file_prefix: String,
    /// The data slots of the plugins that JavaScript keeps (everything but
    /// Haste): their data is part of every change view.
    js_slots: Vec<usize>,
    plugin_count: usize,
    /// A crawl result waiting for `begin_delta`.
    pending: Option<Difference>,
    /// Keep what the node crawler found (for METRO_RUST_VERIFY).
    keep_crawl_input: bool,
    crawl_input: Option<FileData>,
    /// A delta waiting for its file I/O.
    prepared: Option<PreparedDelta>,
    /// A delta waiting for its plugin data.
    session: Option<DeltaSession>,
    /// The view of the last change that was applied (`changeView`).
    last_changes: Changes,
    touches: HashMap<u32, Touch>,
    next_ticket: u32,
}

impl State {
    fn haste_id(&self, metadata: &FileMetadata) -> Option<String> {
        let slot = self.haste.as_ref()?.slot;
        let json = metadata.plugin_slot(slot)?;
        match serde_json::from_str::<Value>(json) {
            Ok(Value::String(id)) => Some(id),
            _ => None,
        }
    }

    /// `HastePlugin.onChanged`, from the aggregator's view.
    fn haste_on_changed(&mut self, changes: &Changes) {
        if self.haste.is_none() {
            return;
        }
        let pairs = |files: &[(String, FileMetadata)]| -> Vec<(String, Option<String>)> {
            files
                .iter()
                .map(|(path, metadata)| (path.clone(), self.haste_id(metadata)))
                .collect()
        };
        let delta = HasteDelta {
            added_files: pairs(&changes.added_files),
            modified_files: pairs(&changes.modified_files),
            removed_files: pairs(&changes.removed_files),
        };
        if let Some(haste) = self.haste.as_mut() {
            haste.map.on_changed(&delta);
        }
    }

    /// The invalidation of the store's package cache that
    /// `DependencyGraph._onHasteChange` does for the JS `PackageCache`: each
    /// added, modified and removed file, in this order.
    fn invalidate_packages(&self, changes: &Changes) {
        let store = self.map.store();
        let root_dir = self.map.root_dir();
        for (path, _) in changes
            .added_files
            .iter()
            .chain(&changes.modified_files)
            .chain(&changes.removed_files)
        {
            store.invalidate_package(&metro_rs_resolver::path::join(&[root_dir, path]));
        }
    }

    fn take_warnings(&mut self) -> Vec<String> {
        self.haste
            .as_mut()
            .map(|h| h.map.take_warnings())
            .unwrap_or_default()
    }
}

// ----------------------------------------------------------------- JS types

#[napi(object)]
pub struct PluginFilterOptions {
    pub lazy: bool,
    pub node_modules: bool,
    pub other: bool,
    pub package_json: Option<bool>,
    pub excluded_extensions: Vec<String>,
}

#[napi(object)]
pub struct HasteOptionsJs {
    pub slot: u32,
    pub enable_haste_packages: bool,
    pub platforms: Vec<String>,
    pub fail_validation_on_conflicts: bool,
}

#[napi(object)]
pub struct FileMapOptions {
    pub root_dir: String,
    pub extensions: Vec<String>,
    pub compute_sha1: bool,
    pub include_symlinks: bool,
    pub retain_all_files: bool,
    pub ignore_source: String,
    pub ignore_flags: String,
    pub health_check_file_prefix: String,
    /// One per plugin that has a worker, in plugin order.
    pub plugins: Vec<PluginFilterOptions>,
    pub haste: Option<HasteOptionsJs>,
    /// Keep what the node crawler finds, for `crawlInput` (METRO_RUST_VERIFY).
    pub keep_crawl_input: bool,
}

/// `CacheData.clocks` and `CacheData.plugins`, as JSON text.
#[napi(object)]
pub struct CacheExtrasJs {
    pub clock_paths: Vec<String>,
    pub clock_specs: Vec<String>,
    pub plugin_names: Vec<String>,
    pub plugin_states: Vec<Option<String>>,
}

/// `FileData` as parallel arrays. A `null` mtime is NaN. `kinds` is 0 for a
/// regular file, 1 for a symlink whose target is not read, and 2 for a symlink
/// with a target (taken from `targets`, in order). `plugin_data` is one entry
/// per file and plugin slot (or empty: no plugin data).
#[napi(object)]
pub struct CompactFiles {
    pub paths: Vec<String>,
    pub mtimes: Float64Array,
    pub sizes: Float64Array,
    pub visited: Uint8Array,
    pub hashes: Vec<Option<String>>,
    pub kinds: Uint8Array,
    pub targets: Vec<String>,
    pub plugin_data: Vec<Option<String>>,
}

#[napi(object)]
pub struct DifferenceJs {
    pub changed: CompactFiles,
    pub removed: Vec<String>,
}

#[napi(object)]
pub struct CrawlSummary {
    pub changed: u32,
    pub removed: u32,
    pub warnings: Vec<String>,
}

#[napi(object)]
pub struct DeltaJobs {
    pub paths: Vec<String>,
    pub plugins: Vec<Vec<u32>>,
}

#[napi(object)]
pub struct PluginResultJs {
    pub path: String,
    /// One JSON text per plugin of the job (`null`: the worker returned
    /// `null` or `undefined`). Absent if the worker threw.
    pub data: Option<Vec<Option<String>>>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

#[napi(object)]
pub struct ChangeList {
    pub paths: Vec<String>,
    /// 1 for a symlink.
    pub symlinks: Uint8Array,
    /// NaN for `null`.
    pub mtimes: Float64Array,
    /// One JSON text per file and entry of `js_slots` (`null`: `undefined`).
    pub data: Vec<Option<String>>,
}

#[napi(object)]
pub struct ChangeView {
    pub added_directories: Vec<String>,
    pub removed_directories: Vec<String>,
    pub added: ChangeList,
    pub modified: ChangeList,
    pub removed: ChangeList,
}

#[napi(object)]
pub struct DeltaApplied {
    pub size: u32,
    pub warnings: Vec<String>,
}

#[napi(object)]
pub struct EventsApplied {
    pub size: u32,
    /// Present if `size` is not 0.
    pub view: Option<ChangeView>,
    pub warnings: Vec<String>,
}

#[napi(object)]
pub struct HasteInitialized {
    /// The files that have a Haste name.
    pub files: u32,
    pub warnings: Vec<String>,
}

#[napi(object)]
pub struct TouchTicket {
    pub ticket: u32,
    pub plugins: Vec<u32>,
}

#[napi(object)]
pub struct TouchStats {
    pub mtime: Option<f64>,
    pub size: f64,
    pub is_symlink: bool,
}

/// `kind`: 0 missing, 1 directory, 2 regular file.
#[napi(object)]
pub struct LookupAnswer {
    pub kind: u32,
    pub real_path: String,
}

#[napi(object)]
pub struct LinkStatsAnswer {
    pub is_symlink: bool,
    pub modified_time: Option<f64>,
    pub size: f64,
}

#[napi(object)]
pub struct HierarchicalAnswer {
    pub absolute_path: String,
    pub container_relative_path: String,
}

#[napi(object)]
pub struct MatchFilesArgs {
    pub filter_source: Option<String>,
    pub filter_flags: Option<String>,
    pub filter_compare_absolute: bool,
    pub filter_compare_posix: bool,
    pub follow: bool,
    pub recursive: bool,
    pub root_dir: Option<String>,
}

/// `kind`: 0 missing, 1 directory, 2 regular file. `data` is the plugin's JSON
/// text (`null`: `undefined`).
#[napi(object)]
pub struct PluginLookupAnswer {
    pub kind: u32,
    pub data: Option<String>,
}

#[napi(object)]
pub struct PluginFiles {
    pub base_names: Vec<String>,
    pub canonical_paths: Vec<String>,
    pub data: Vec<Option<String>>,
}

#[napi(object)]
pub struct HasteErrorJs {
    pub message: String,
    pub haste_name: String,
    pub platform: Option<String>,
    pub supports_native_platform: bool,
    pub duplicate_paths: Vec<String>,
    /// `H.MODULE` (0) or `H.PACKAGE` (1) per path.
    pub duplicate_types: Vec<u32>,
}

#[napi(object)]
pub struct HasteAnswer {
    pub path: Option<String>,
    pub error: Option<HasteErrorJs>,
}

#[napi(object)]
pub struct HasteConflictJs {
    pub id: String,
    pub platform: Option<String>,
    pub absolute_paths: Vec<String>,
    #[napi(js_name = "type")]
    pub conflict_type: String,
}

// -------------------------------------------------------------- conversions

fn nan_if_none(value: Option<f64>) -> f64 {
    value.unwrap_or(f64::NAN)
}

fn none_if_nan(value: f64) -> Option<f64> {
    (!value.is_nan()).then_some(value)
}

fn compact_files(files: &FileData, slots: usize) -> CompactFiles {
    let n = files.len();
    let mut paths = Vec::with_capacity(n);
    let mut mtimes = Vec::with_capacity(n);
    let mut sizes = Vec::with_capacity(n);
    let mut visited = Vec::with_capacity(n);
    let mut hashes = Vec::with_capacity(n);
    let mut kinds = Vec::with_capacity(n);
    let mut targets = Vec::new();
    let mut plugin_data = Vec::with_capacity(n * slots);
    for (path, metadata) in files.iter() {
        paths.push(path.to_string());
        mtimes.push(nan_if_none(metadata.mtime));
        sizes.push(metadata.size);
        visited.push(u8::from(metadata.visited));
        hashes.push(metadata.sha1.clone());
        kinds.push(match &metadata.symlink {
            Symlink::Regular => 0,
            Symlink::Unresolved => 1,
            Symlink::Target(target) => {
                targets.push(target.clone());
                2
            }
        });
        for slot in 0..slots {
            plugin_data.push(metadata.plugin_slot(slot).map(str::to_string));
        }
    }
    CompactFiles {
        paths,
        mtimes: Float64Array::new(mtimes),
        sizes: Float64Array::new(sizes),
        visited: Uint8Array::new(visited),
        hashes,
        kinds: Uint8Array::new(kinds),
        targets,
        plugin_data,
    }
}

fn file_data_from_compact(compact: &CompactFiles) -> Result<FileData> {
    let n = compact.paths.len();
    if compact.mtimes.len() != n
        || compact.sizes.len() != n
        || compact.visited.len() != n
        || compact.hashes.len() != n
        || compact.kinds.len() != n
    {
        return Err(napi::Error::from_reason(
            "metro-rs: the arrays of a file list differ in length",
        ));
    }
    let slots = compact.plugin_data.len().checked_div(n).unwrap_or(0);
    let mut targets = compact.targets.iter();
    let mut files = FileData::with_capacity(n);
    for i in 0..n {
        let symlink = match compact.kinds[i] {
            0 => Symlink::Regular,
            1 => Symlink::Unresolved,
            _ => Symlink::Target(targets.next().cloned().unwrap_or_default()),
        };
        files.insert(
            compact.paths[i].clone(),
            FileMetadata {
                mtime: none_if_nan(compact.mtimes[i]),
                size: compact.sizes[i],
                visited: compact.visited[i] == 1,
                sha1: compact.hashes[i].clone(),
                symlink,
                plugin_data: compact.plugin_data[i * slots..(i + 1) * slots].to_vec(),
            },
        );
    }
    Ok(files)
}

fn change_list(files: &[(String, FileMetadata)], js_slots: &[usize]) -> ChangeList {
    let mut paths = Vec::with_capacity(files.len());
    let mut symlinks = Vec::with_capacity(files.len());
    let mut mtimes = Vec::with_capacity(files.len());
    let mut data = Vec::with_capacity(files.len() * js_slots.len());
    for (path, metadata) in files {
        paths.push(path.clone());
        symlinks.push(u8::from(!metadata.is_regular_file()));
        mtimes.push(nan_if_none(metadata.mtime));
        for slot in js_slots {
            data.push(metadata.plugin_slot(*slot).map(str::to_string));
        }
    }
    ChangeList {
        paths,
        symlinks: Uint8Array::new(symlinks),
        mtimes: Float64Array::new(mtimes),
        data,
    }
}

fn change_view(changes: &Changes, js_slots: &[usize]) -> ChangeView {
    ChangeView {
        added_directories: changes.added_directories.clone(),
        removed_directories: changes.removed_directories.clone(),
        added: change_list(&changes.added_files, js_slots),
        modified: change_list(&changes.modified_files, js_slots),
        removed: change_list(&changes.removed_files, js_slots),
    }
}

fn haste_error(error: HasteError) -> HasteErrorJs {
    match error {
        HasteError::DuplicateHasteCandidates(e) => HasteErrorJs {
            message: e.message,
            haste_name: e.haste_name,
            platform: e.platform,
            supports_native_platform: e.supports_native_platform,
            duplicate_paths: e.duplicates_set.iter().map(|(p, _)| p.clone()).collect(),
            duplicate_types: e.duplicates_set.iter().map(|(_, t)| *t as u32).collect(),
        },
        // `getModule` and `getPackage` never report conflicts.
        HasteError::HasteConflicts(e) => HasteErrorJs {
            message: e.message,
            haste_name: String::new(),
            platform: None,
            supports_native_platform: false,
            duplicate_paths: Vec::new(),
            duplicate_types: Vec::new(),
        },
    }
}

fn conflicts_js(conflicts: &[metro_rs_haste::HasteConflict]) -> Vec<HasteConflictJs> {
    conflicts
        .iter()
        .map(|c| HasteConflictJs {
            id: c.id.clone(),
            platform: c.platform.clone(),
            absolute_paths: c.absolute_paths.clone(),
            conflict_type: c.r#type.as_str().to_string(),
        })
        .collect()
}

fn ordered_snapshot(node: &Value) -> Value {
    match node {
        Value::Object(entries) => {
            let entries = entries
                .iter()
                .map(|(name, child)| {
                    Value::Array(vec![name.clone().into(), ordered_snapshot(child)])
                })
                .collect();
            let mut object = serde_json::Map::new();
            object.insert("e".to_string(), Value::Array(entries));
            Value::Object(object)
        }
        file => file.clone(),
    }
}

/// Whether the file map can use this RegExp (`regress` has to accept it, and
/// it must not have the `g` or `y` flag). When it cannot, metro-file-map
/// stays on its JavaScript implementation.
#[napi]
pub fn file_map_supports_regexp(source: String, flags: String) -> bool {
    JsRegExp::new(&source, &flags).is_ok()
}

// -------------------------------------------------------------------- class

/// The file map: the tree with its metadata, and Haste.
#[napi]
pub struct NativeFileMap {
    state: RefCell<State>,
}

impl NativeFileMap {
    fn read(&self) -> Result<Ref<'_, State>> {
        self.state
            .try_borrow()
            .map_err(|_| napi::Error::from_reason("metro-rs: the file map is being changed"))
    }

    fn write(&self) -> Result<RefMut<'_, State>> {
        self.state
            .try_borrow_mut()
            .map_err(|_| napi::Error::from_reason("metro-rs: the file map is in use"))
    }

    /// Runs `query` on the tree, if it is not being changed. Used by the
    /// resolver, which asks the tree directly.
    pub(crate) fn with_store<R>(
        &self,
        query: impl FnOnce(&metro_rs_file_store::FileStore) -> R,
    ) -> Option<R> {
        let state = self.state.try_borrow().ok()?;
        Some(query(state.map.store()))
    }

    /// Runs `query` on the Haste map, if there is one and it is not being
    /// changed.
    pub(crate) fn with_haste<R>(&self, query: impl FnOnce(&HasteMap) -> R) -> Option<R> {
        let state = self.state.try_borrow().ok()?;
        state.haste.as_ref().map(|h| query(&h.map))
    }
}

#[napi]
impl NativeFileMap {
    #[napi(constructor)]
    pub fn new(options: FileMapOptions) -> Result<Self> {
        let ignore_pattern = JsRegExp::new(&options.ignore_source, &options.ignore_flags)
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        let plugins: Vec<PluginFilter> = options
            .plugins
            .iter()
            .map(|p| PluginFilter {
                lazy: p.lazy,
                node_modules: p.node_modules,
                other: p.other,
                package_json: p.package_json,
                excluded_extensions: p.excluded_extensions.clone(),
            })
            .collect();
        let plugin_count = plugins.len();
        let haste = options.haste.as_ref().map(|h| HasteState {
            map: HasteMap::new(HasteOptions {
                enable_haste_packages: h.enable_haste_packages,
                platforms: h.platforms.clone(),
                root_dir: options.root_dir.clone(),
                fail_validation_on_conflicts: h.fail_validation_on_conflicts,
            }),
            slot: h.slot as usize,
            initialized: false,
        });
        let haste_slot = haste.as_ref().map(|h| h.slot);
        let js_slots = (0..plugin_count)
            .filter(|slot| Some(*slot) != haste_slot)
            .collect();
        let map = FileMap::new(
            &options.root_dir,
            FileMapConfig {
                compute_sha1: options.compute_sha1,
                plugins,
            },
        );
        Ok(Self {
            state: RefCell::new(State {
                map,
                haste,
                ignore_pattern,
                extensions: options.extensions,
                include_symlinks: options.include_symlinks,
                retain_all_files: options.retain_all_files,
                health_check_file_prefix: options.health_check_file_prefix,
                js_slots,
                plugin_count,
                pending: None,
                keep_crawl_input: options.keep_crawl_input,
                crawl_input: None,
                prepared: None,
                session: None,
                last_changes: Changes::default(),
                touches: HashMap::new(),
                next_ticket: 0,
            }),
        })
    }

    // ---------------------------------------------------------------- cache

    /// Reads and decodes the cache file on a worker thread. The promise gives
    /// `null` for a miss (no file, another format version, corruption), else
    /// a handle for `loadCache`.
    #[napi]
    pub fn read_cache_file(&self, path: String) -> AsyncTask<ReadCacheTask> {
        AsyncTask::new(ReadCacheTask { path })
    }

    /// Replaces the contents with a cache read by `readCacheFile`. `null` for
    /// a miss (another root): the caller crawls everything.
    #[napi]
    pub fn load_cache(&self, data: Handoff<CacheData>) -> Result<Option<CacheExtrasJs>> {
        let Some(data) = take_handoff(&data) else {
            return Err(napi::Error::from_reason("metro-rs: the cache was used"));
        };
        let mut state = self.write()?;
        let root_dir = state.map.root_dir().to_string();
        let config = state.map.config().clone();
        let Some((map, extras)) = FileMap::from_cache(data, &root_dir, config) else {
            return Ok(None);
        };
        state.map = map;
        Ok(Some(CacheExtrasJs {
            clock_paths: extras.clocks.iter().map(|(p, _)| p.clone()).collect(),
            clock_specs: extras.clocks.iter().map(|(_, s)| s.clone()).collect(),
            plugin_names: extras.plugins.iter().map(|(n, _)| n.clone()).collect(),
            plugin_states: extras.plugins.iter().map(|(_, s)| s.clone()).collect(),
        }))
    }

    /// The data of the cache file, taken now, for `writeCacheFile`.
    #[napi]
    pub fn snapshot_cache(&self, extras: CacheExtrasJs) -> Result<Handoff<CacheData>> {
        let state = self.read()?;
        let extras = CacheExtras {
            clocks: extras
                .clock_paths
                .into_iter()
                .zip(extras.clock_specs)
                .collect(),
            plugins: extras
                .plugin_names
                .into_iter()
                .zip(extras.plugin_states)
                .collect(),
        };
        Ok(handoff(state.map.cache_snapshot(&extras)))
    }

    /// Writes a snapshot (atomically) on a worker thread.
    #[napi]
    pub fn write_cache_file(
        &self,
        path: String,
        data: Handoff<CacheData>,
    ) -> Result<AsyncTask<WriteCacheTask>> {
        let Some(data) = take_handoff(&data) else {
            return Err(napi::Error::from_reason("metro-rs: the cache was used"));
        };
        Ok(AsyncTask::new(WriteCacheTask { path, data }))
    }

    // ---------------------------------------------------------------- build

    /// `nodeCrawl`, first half: walks `roots` on a worker thread. The promise
    /// gives a handle for `finishCrawl`.
    #[napi]
    pub fn crawl_find(&self, roots: Vec<String>) -> Result<AsyncTask<CrawlTask>> {
        let state = self.read()?;
        Ok(AsyncTask::new(CrawlTask {
            extensions: state.extensions.clone(),
            ignore_pattern: state.ignore_pattern.clone(),
            retain_all_files: state.retain_all_files,
            health_check_file_prefix: state.health_check_file_prefix.clone(),
            include_symlinks: state.include_symlinks,
            root_dir: state.map.root_dir().to_string(),
            roots,
        }))
    }

    /// `nodeCrawl`, second half: keeps the difference between what the crawl
    /// found and the current state (`subpath`: only below this normal
    /// directory) for `prepareDelta`.
    #[napi]
    pub fn finish_crawl(
        &self,
        env: Env,
        found: Handoff<CrawlOutput>,
        subpath: Option<String>,
    ) -> Result<CrawlSummary> {
        let Some(found) = take_handoff(&found) else {
            return Err(napi::Error::from_reason("metro-rs: the crawl was used"));
        };
        let mut state = self.write()?;
        let difference = state
            .map
            .get_difference(&found.file_data, subpath.as_deref())
            .map_err(|e| file_map_error(&env, &e))?;
        let summary = CrawlSummary {
            changed: difference.changed_files.len() as u32,
            removed: difference.removed_files.len() as u32,
            warnings: found.warnings,
        };
        state.pending = Some(difference);
        if state.keep_crawl_input {
            state.crawl_input = Some(found.file_data);
        }
        Ok(summary)
    }

    /// What the last `nodeCrawl` found (the constructor option
    /// `keepCrawlInput`), for METRO_RUST_VERIFY.
    #[napi]
    pub fn crawl_input(&self) -> Result<CompactFiles> {
        let state = self.read()?;
        Ok(compact_files(
            state.crawl_input.as_ref().unwrap_or(&FileData::new()),
            0,
        ))
    }

    /// The difference waiting for `beginDelta`, for METRO_RUST_VERIFY.
    #[napi]
    pub fn pending_difference(&self) -> Result<DifferenceJs> {
        let state = self.read()?;
        let Some(difference) = state.pending.as_ref() else {
            return Err(napi::Error::from_reason(
                "metro-rs: there is no crawl result",
            ));
        };
        Ok(DifferenceJs {
            changed: compact_files(&difference.changed_files, state.plugin_count),
            removed: difference.removed_files.clone(),
        })
    }

    /// Keeps a delta made by a JavaScript crawler (Watchman) for `beginDelta`.
    #[napi]
    pub fn set_difference(&self, changed: CompactFiles, removed: Vec<String>) -> Result<()> {
        let changed_files = file_data_from_compact(&changed)?;
        self.write()?.pending = Some(Difference {
            changed_files,
            removed_files: removed,
        });
        Ok(())
    }

    /// `TreeFS.getDifference`. Also keeps the result for `beginDelta`, so that
    /// a crawler that passes it on (Watchman's) costs no second crossing.
    #[napi]
    pub fn get_difference(
        &self,
        env: Env,
        files: CompactFiles,
        subpath: Option<String>,
    ) -> Result<DifferenceJs> {
        let files = file_data_from_compact(&files)?;
        let mut state = self.write()?;
        let difference = state
            .map
            .get_difference(&files, subpath.as_deref())
            .map_err(|e| file_map_error(&env, &e))?;
        let result = DifferenceJs {
            changed: compact_files(&difference.changed_files, state.plugin_count),
            removed: difference.removed_files.clone(),
        };
        state.pending = Some(difference);
        Ok(result)
    }

    /// First part of `applyFileDelta` for the kept difference: removes the
    /// removed files and decides what to hash. The I/O is `deltaIo`.
    #[napi]
    pub fn prepare_delta(&self, env: Env) -> Result<()> {
        let mut state = self.write()?;
        let Some(difference) = state.pending.take() else {
            return Err(napi::Error::from_reason(
                "metro-rs: there is no crawl result to apply",
            ));
        };
        let prepared = state
            .map
            .prepare_delta(difference)
            .map_err(|e| file_map_error(&env, &e))?;
        state.prepared = Some(prepared);
        Ok(())
    }

    /// Hashes the files and reads the links of the prepared delta, on worker
    /// threads. The promise gives a handle for `beginDelta`.
    #[napi]
    pub fn delta_io(&self) -> Result<AsyncTask<DeltaIoTask>> {
        let mut state = self.write()?;
        let Some(prepared) = state.prepared.as_mut() else {
            return Err(napi::Error::from_reason(
                "metro-rs: there is no delta to hash",
            ));
        };
        Ok(AsyncTask::new(DeltaIoTask {
            io: Some(prepared.take_io()),
        }))
    }

    /// Last part of the first half of `applyFileDelta`. Returns the files that
    /// need plugin workers.
    #[napi]
    pub fn begin_delta(&self, io: Handoff<DeltaIoResult>) -> Result<DeltaJobs> {
        let mut state = self.write()?;
        let (Some(prepared), Some(io)) = (state.prepared.take(), take_handoff(&io)) else {
            return Err(napi::Error::from_reason(
                "metro-rs: there is no prepared delta",
            ));
        };
        let session = state.map.complete_delta(prepared, io);
        let jobs = session.plugin_jobs();
        let result = DeltaJobs {
            paths: jobs.iter().map(|j| j.normal_path.clone()).collect(),
            plugins: jobs
                .iter()
                .map(|j| j.plugins_to_run.iter().map(|p| *p as u32).collect())
                .collect(),
        };
        state.session = Some(session);
        Ok(result)
    }

    /// Second half of `applyFileDelta`: stores the plugin data, adds the
    /// files, and updates Haste. The view is `changeView()`.
    #[napi]
    pub fn finish_delta(&self, env: Env, results: Vec<PluginResultJs>) -> Result<DeltaApplied> {
        let mut state = self.write()?;
        let Some(session) = state.session.take() else {
            return Err(napi::Error::from_reason(
                "metro-rs: there is no delta to finish",
            ));
        };
        let results = results
            .into_iter()
            .map(|r| PluginJobResult {
                normal_path: r.path,
                outcome: match r.data {
                    Some(data) => Ok(data),
                    None => Err(FileMapError {
                        code: r.error_code,
                        message: r.error_message.unwrap_or_default(),
                    }),
                },
            })
            .collect();
        let changes = state
            .map
            .finish_delta(session, results)
            .map_err(|e| file_map_error(&env, &e))?;
        state.haste_on_changed(&changes);
        state.invalidate_packages(&changes);
        let size = changes.size() as u32;
        state.last_changes = changes;
        Ok(DeltaApplied {
            size,
            warnings: state.take_warnings(),
        })
    }

    /// The view of the last change applied by `finishDelta`.
    #[napi]
    pub fn change_view(&self) -> Result<ChangeView> {
        let state = self.read()?;
        Ok(change_view(&state.last_changes, &state.js_slots))
    }

    /// `HastePlugin.initialize`: builds the Haste map from the plugin data of
    /// the files in the store.
    #[napi]
    pub fn haste_initialize(&self) -> Result<HasteInitialized> {
        let mut state = self.write()?;
        let State { map, haste, .. } = &mut *state;
        let Some(haste) = haste.as_mut() else {
            return Ok(HasteInitialized {
                files: 0,
                warnings: Vec::new(),
            });
        };
        let entries: Vec<(String, Option<String>)> = map
            .metadata_iterator(false, false)
            .into_iter()
            .map(|entry| {
                let id = map
                    .metadata(entry.id)
                    .and_then(|m| m.plugin_slot(haste.slot))
                    .and_then(|json| match serde_json::from_str::<Value>(json) {
                        Ok(Value::String(id)) => Some(id),
                        _ => None,
                    });
                (entry.canonical_path, id)
            })
            .collect();
        haste
            .map
            .initialize(entries.iter().map(|(p, id)| (p.as_str(), id.as_deref())));
        haste.initialized = true;
        Ok(HasteInitialized {
            files: entries.iter().filter(|(_, id)| id.is_some()).count() as u32,
            warnings: haste.map.take_warnings(),
        })
    }

    // ---------------------------------------------------------------- watch

    /// `onChange` of watch mode for a touched file: reads a symlink, or hashes
    /// a regular file. The metadata stays here, under the ticket, until
    /// `applyEvents`; the plugins listed need `setTouchPluginData` first.
    /// Throws what Node throws (`ENOENT`, which the caller swallows).
    #[napi]
    pub fn process_touch(
        &self,
        env: Env,
        path: String,
        mtime: Option<f64>,
        size: f64,
        is_symlink: bool,
    ) -> Result<TouchTicket> {
        let mut state = self.write()?;
        let output = state
            .map
            .process_touch(&TouchInput {
                normal_path: path,
                mtime,
                size,
                is_symlink,
            })
            .map_err(|e| file_map_error(&env, &e))?;
        let ticket = state.next_ticket;
        state.next_ticket = state.next_ticket.wrapping_add(1);
        let plugins = output.plugins_to_run.iter().map(|p| *p as u32).collect();
        state.touches.insert(
            ticket,
            Touch {
                metadata: output.metadata,
                plugins_to_run: output.plugins_to_run,
            },
        );
        Ok(TouchTicket { ticket, plugins })
    }

    /// Stores what the plugin workers returned for a touch: one JSON text per
    /// plugin of the ticket (`null` for `null` and `undefined`).
    #[napi]
    pub fn set_touch_plugin_data(&self, ticket: u32, data: Vec<Option<String>>) -> Result<()> {
        let mut state = self.write()?;
        let Some(touch) = state.touches.get_mut(&ticket) else {
            return Err(napi::Error::from_reason("metro-rs: unknown touch"));
        };
        for (i, slot) in touch.plugins_to_run.clone().into_iter().enumerate() {
            let value = data.get(i).cloned().flatten();
            touch
                .metadata
                .set_plugin_slot(slot, Some(value.unwrap_or_else(|| "null".to_string())));
        }
        Ok(())
    }

    /// Forgets a touch that is not going to be applied.
    #[napi]
    pub fn discard_touch(&self, ticket: u32) -> Result<()> {
        self.write()?.touches.remove(&ticket);
        Ok(())
    }

    /// `fs.lstat` for a watcher that did not report the stats.
    #[napi]
    pub fn lstat_touch(&self, env: Env, path: String) -> Result<TouchStats> {
        let state = self.read()?;
        let input = state
            .map
            .lstat_touch(&path)
            .map_err(|e| file_map_error(&env, &e))?;
        Ok(TouchStats {
            mtime: input.mtime,
            size: input.size,
            is_symlink: input.is_symlink,
        })
    }

    /// `emitChange`: applies a batch of events in order (`kinds`: 0 delete, 1
    /// touch with its ticket) and returns the net change.
    #[napi]
    pub fn apply_events(
        &self,
        env: Env,
        kinds: Uint8Array,
        paths: Vec<String>,
        tickets: Uint32Array,
    ) -> Result<EventsApplied> {
        let mut state = self.write()?;
        let mut events = Vec::with_capacity(paths.len());
        for (i, path) in paths.into_iter().enumerate() {
            if kinds[i] == 0 {
                events.push(WatchEvent::Delete { normal_path: path });
            } else {
                let Some(touch) = state.touches.remove(&tickets[i]) else {
                    return Err(napi::Error::from_reason("metro-rs: unknown touch"));
                };
                events.push(WatchEvent::Touch {
                    normal_path: path,
                    metadata: touch.metadata,
                });
            }
        }
        let changes = state
            .map
            .apply_events(events)
            .map_err(|e| file_map_error(&env, &e))?;
        let size = changes.size();
        if size == 0 {
            return Ok(EventsApplied {
                size: 0,
                view: None,
                warnings: Vec::new(),
            });
        }
        state.haste_on_changed(&changes);
        state.invalidate_packages(&changes);
        let view = change_view(&changes, &state.js_slots);
        state.last_changes = changes;
        Ok(EventsApplied {
            size: size as u32,
            view: Some(view),
            warnings: state.take_warnings(),
        })
    }

    // ------------------------------------------------------------ FileSystem

    #[napi]
    pub fn lookup(&self, env: Env, path: String) -> Result<LookupAnswer> {
        let state = self.read()?;
        match state
            .map
            .lookup(&path)
            .map_err(|e| invariant_error(&env, &e))?
        {
            MapLookup::NotFound => Ok(LookupAnswer {
                kind: 0,
                real_path: String::new(),
            }),
            MapLookup::Dir { real_path } => Ok(LookupAnswer { kind: 1, real_path }),
            MapLookup::File { real_path, .. } => Ok(LookupAnswer { kind: 2, real_path }),
        }
    }

    /// The metadata tuple of the regular file at `path` as JSON text
    /// (`lookup(...).metadata`); `null` if there is none.
    #[napi]
    pub fn metadata_tuple(&self, env: Env, path: String) -> Result<Option<String>> {
        let state = self.read()?;
        let MapLookup::File { id, .. } = state
            .map
            .lookup(&path)
            .map_err(|e| invariant_error(&env, &e))?
        else {
            return Ok(None);
        };
        Ok(state.map.metadata(id).map(|m| m.to_tuple().to_string()))
    }

    #[napi]
    pub fn exists(&self, env: Env, path: String) -> Result<bool> {
        self.read()?
            .map
            .exists(&path)
            .map_err(|e| invariant_error(&env, &e))
    }

    #[napi]
    pub fn get_all_files(&self) -> Result<Vec<String>> {
        Ok(self.read()?.map.get_all_files())
    }

    #[napi]
    pub fn get_sha1(&self, env: Env, path: String) -> Result<Option<String>> {
        self.read()?
            .map
            .get_sha1(&path)
            .map_err(|e| invariant_error(&env, &e))
    }

    #[napi]
    pub fn get_size(&self, env: Env, path: String) -> Result<Option<f64>> {
        self.read()?
            .map
            .get_size(&path)
            .map_err(|e| invariant_error(&env, &e))
    }

    #[napi]
    pub fn link_stats(&self, env: Env, path: String) -> Result<Option<LinkStatsAnswer>> {
        Ok(self
            .read()?
            .map
            .link_stats(&path)
            .map_err(|e| invariant_error(&env, &e))?
            .map(|stats| LinkStatsAnswer {
                is_symlink: stats.file_type == 'l',
                modified_time: stats.modified_time,
                size: stats.size,
            }))
    }

    /// `matchFiles`. `null` if the filter is a RegExp that the Rust engine
    /// does not run (then the caller filters).
    #[napi]
    pub fn match_files(&self, env: Env, args: MatchFilesArgs) -> Result<Option<Vec<String>>> {
        let filter = match &args.filter_source {
            None => None,
            Some(source) => {
                match JsRegExp::new(source, args.filter_flags.as_deref().unwrap_or("")) {
                    Ok(filter) => Some(filter),
                    Err(_) => return Ok(None),
                }
            }
        };
        let state = self.read()?;
        let files = state
            .map
            .match_files(&MatchFilesOptions {
                filter: filter.as_ref(),
                filter_compare_absolute: args.filter_compare_absolute,
                filter_compare_posix: args.filter_compare_posix,
                follow: args.follow,
                recursive: args.recursive,
                root_dir: args.root_dir.as_deref(),
            })
            .map_err(|e| invariant_error(&env, &e))?;
        Ok(Some(files))
    }

    #[napi]
    pub fn hierarchical_lookup(
        &self,
        env: Env,
        start_path: String,
        subpath: String,
        break_on_segment: Option<String>,
        subpath_is_dir: bool,
    ) -> Result<Option<HierarchicalAnswer>> {
        let state = self.read()?;
        Ok(state
            .map
            .store()
            .hierarchical_lookup(
                &start_path,
                &subpath,
                break_on_segment.as_deref(),
                subpath_is_dir,
            )
            .map_err(|e| invariant_error(&env, &e))?
            .map(
                |(absolute_path, container_relative_path)| HierarchicalAnswer {
                    absolute_path,
                    container_relative_path,
                },
            ))
    }

    /// `getSerializableSnapshot` as JSON text. A directory is
    /// `{"e": [[name, node], ...]}`, which keeps the order of its entries
    /// (a JS object would move names that look like array indices first); a
    /// file is its metadata tuple.
    #[napi]
    pub fn get_serializable_snapshot(&self) -> Result<String> {
        Ok(ordered_snapshot(&self.read()?.map.get_serializable_snapshot()).to_string())
    }

    /// `getOrComputeSha1`: the lookup runs now, the hashing on a worker
    /// thread. The promise gives `null`, or `{sha1, content?, id?, version?}`
    /// (`id` and `version` are for `finishSha1`, when the file was hashed), or
    /// `{errorCode, errorMessage}` for an error.
    #[napi]
    pub fn get_or_compute_sha1(&self, env: Env, path: String) -> Result<AsyncTask<Sha1Task>> {
        let job = self
            .read()?
            .map
            .sha1_job(&path)
            .map_err(|e| invariant_error(&env, &e))?;
        Ok(AsyncTask::new(Sha1Task { job }))
    }

    /// Stores a SHA-1 that `getOrComputeSha1` hashed, unless the file has been
    /// changed since. Returns whether it was stored.
    #[napi]
    pub fn finish_sha1(&self, id: u32, version: u32, sha1: String) -> Result<bool> {
        Ok(self
            .write()?
            .map
            .finish_sha1(metro_rs_file_store::FileId(id), version, &sha1))
    }

    // -------------------------------------------------------------- plugins

    /// A plugin's view of `lookup`: what the node is, and the plugin's data
    /// of the file.
    #[napi]
    pub fn plugin_lookup(&self, env: Env, path: String, slot: u32) -> Result<PluginLookupAnswer> {
        let state = self.read()?;
        Ok(
            match state
                .map
                .lookup(&path)
                .map_err(|e| invariant_error(&env, &e))?
            {
                MapLookup::NotFound => PluginLookupAnswer {
                    kind: 0,
                    data: None,
                },
                MapLookup::Dir { .. } => PluginLookupAnswer {
                    kind: 1,
                    data: None,
                },
                MapLookup::File { id, .. } => PluginLookupAnswer {
                    kind: 2,
                    data: state
                        .map
                        .metadata(id)
                        .and_then(|m| m.plugin_slot(slot as usize))
                        .map(str::to_string),
                },
            },
        )
    }

    /// A plugin's view of `metadataIterator`.
    #[napi]
    pub fn plugin_files(
        &self,
        slot: u32,
        include_symlinks: bool,
        include_node_modules: bool,
    ) -> Result<PluginFiles> {
        let state = self.read()?;
        let entries = state
            .map
            .metadata_iterator(include_symlinks, include_node_modules);
        let data = entries
            .iter()
            .map(|entry| {
                state
                    .map
                    .metadata(entry.id)
                    .and_then(|m| m.plugin_slot(slot as usize))
                    .map(str::to_string)
            })
            .collect();
        Ok(PluginFiles {
            base_names: entries.iter().map(|e| e.base_name.clone()).collect(),
            canonical_paths: entries.into_iter().map(|e| e.canonical_path).collect(),
            data,
        })
    }

    // ---------------------------------------------------------------- Haste

    /// `HastePlugin.getModule`. Errors come back in `error`.
    #[napi]
    pub fn haste_get_module(
        &self,
        name: String,
        platform: Option<String>,
        supports_native_platform: bool,
        module_type: Option<u32>,
    ) -> Result<HasteAnswer> {
        let state = self.read()?;
        let Some(haste) = state.haste.as_ref() else {
            return Err(napi::Error::from_reason("metro-rs: there is no Haste map"));
        };
        let module_type = module_type.map(|t| {
            if t == 1 {
                ModuleType::Package
            } else {
                ModuleType::Module
            }
        });
        Ok(
            match haste.map.get_module(
                &name,
                platform.as_deref(),
                supports_native_platform,
                module_type,
            ) {
                Ok(path) => HasteAnswer { path, error: None },
                Err(error) => HasteAnswer {
                    path: None,
                    error: Some(haste_error(error)),
                },
            },
        )
    }

    /// `HastePlugin.getPackage`. Errors come back in `error`.
    #[napi]
    pub fn haste_get_package(&self, name: String, platform: Option<String>) -> Result<HasteAnswer> {
        let state = self.read()?;
        let Some(haste) = state.haste.as_ref() else {
            return Err(napi::Error::from_reason("metro-rs: there is no Haste map"));
        };
        Ok(
            match haste.map.get_package(&name, platform.as_deref(), false) {
                Ok(path) => HasteAnswer { path, error: None },
                Err(error) => HasteAnswer {
                    path: None,
                    error: Some(haste_error(error)),
                },
            },
        )
    }

    /// `HastePlugin.getModuleNameByPath`: the path goes through the store's
    /// `lookup` first (the real path of a file), as the JS reads the file
    /// system's live plugin data.
    #[napi]
    pub fn haste_get_module_name_by_path(&self, env: Env, path: String) -> Result<Option<String>> {
        let state = self.read()?;
        let Some(haste) = state.haste.as_ref() else {
            return Err(napi::Error::from_reason("metro-rs: there is no Haste map"));
        };
        if !haste.initialized {
            return Err(throw(
                &env,
                new_error(
                    &env,
                    None,
                    None,
                    "HastePlugin has not been initialized before getModuleNameByPath",
                ),
            ));
        }
        match state
            .map
            .lookup(&path)
            .map_err(|e| invariant_error(&env, &e))?
        {
            MapLookup::File { real_path, .. } => Ok(haste.map.get_module_name_by_path(&real_path)),
            _ => Ok(None),
        }
    }

    /// `HastePlugin.assertValid`: the conflicts, if it would throw.
    #[napi]
    pub fn haste_assert_valid(&self) -> Result<Option<Vec<HasteConflictJs>>> {
        let state = self.read()?;
        let Some(haste) = state.haste.as_ref() else {
            return Ok(None);
        };
        Ok(match haste.map.assert_valid() {
            Ok(()) => None,
            Err(HasteError::HasteConflicts(e)) => Some(conflicts_js(e.conflicts())),
            Err(HasteError::DuplicateHasteCandidates(_)) => None,
        })
    }

    #[napi]
    pub fn haste_compute_conflicts(&self) -> Result<Vec<HasteConflictJs>> {
        let state = self.read()?;
        Ok(state
            .haste
            .as_ref()
            .map(|h| conflicts_js(&h.map.compute_conflicts()))
            .unwrap_or_default())
    }
}

// -------------------------------------------------------------------- sha-1

pub enum Sha1Outcome {
    Null,
    Cached(String),
    Hashed {
        id: u32,
        version: u32,
        sha1: String,
        content: Vec<u8>,
    },
    Failed(FileMapError),
}

/// Hashing for `getOrComputeSha1`, on a worker thread. It holds the job only:
/// the file map is not touched until JS calls `finishSha1`.
pub struct Sha1Task {
    job: Sha1Job,
}

impl Task for Sha1Task {
    type Output = Sha1Outcome;
    type JsValue = JsUnknown;

    fn compute(&mut self) -> Result<Sha1Outcome> {
        Ok(match &self.job {
            Sha1Job::None => Sha1Outcome::Null,
            Sha1Job::Cached(sha1) => Sha1Outcome::Cached(sha1.clone()),
            Sha1Job::Compute {
                id,
                version,
                absolute_path,
                ..
            } => match metro_rs_file_map::read_and_hash(absolute_path) {
                Ok((sha1, content)) => Sha1Outcome::Hashed {
                    id: id.0,
                    version: *version,
                    sha1,
                    content,
                },
                Err(error) => Sha1Outcome::Failed(error),
            },
        })
    }

    fn resolve(&mut self, env: Env, output: Sha1Outcome) -> Result<JsUnknown> {
        let mut object = env.create_object()?;
        match output {
            Sha1Outcome::Null => return Ok(env.get_null()?.into_unknown()),
            Sha1Outcome::Cached(sha1) => {
                object.set_named_property("sha1", env.create_string(&sha1)?)?;
            }
            Sha1Outcome::Hashed {
                id,
                version,
                sha1,
                content,
            } => {
                object.set_named_property("sha1", env.create_string(&sha1)?)?;
                object.set_named_property("content", Buffer::from(content))?;
                object.set_named_property("id", env.create_uint32(id)?)?;
                object.set_named_property("version", env.create_uint32(version)?)?;
            }
            Sha1Outcome::Failed(error) => {
                if let Some(code) = &error.code {
                    object.set_named_property("errorCode", env.create_string(code)?)?;
                }
                object.set_named_property("errorMessage", env.create_string(&error.message)?)?;
            }
        }
        Ok(object.into_unknown())
    }
}

// ----------------------------------------------------- work on other threads

/// A value that one async call produces and a later sync call consumes: it
/// stays in Rust, in a JS handle.
pub type Handoff<T> = External<Mutex<Option<T>>>;

fn handoff<T>(value: T) -> Handoff<T> {
    External::new(Mutex::new(Some(value)))
}

fn take_handoff<T>(handle: &Handoff<T>) -> Option<T> {
    handle.lock().ok()?.take()
}

/// `find`, off the main thread: a pure function over the file system.
pub struct CrawlTask {
    extensions: Vec<String>,
    ignore_pattern: JsRegExp,
    retain_all_files: bool,
    health_check_file_prefix: String,
    include_symlinks: bool,
    root_dir: String,
    roots: Vec<String>,
}

impl Task for CrawlTask {
    type Output = CrawlOutput;
    type JsValue = Handoff<CrawlOutput>;

    fn compute(&mut self) -> Result<CrawlOutput> {
        Ok(find(&CrawlOptions {
            extensions: &self.extensions,
            ignore: IgnoreRules {
                ignore_pattern: &self.ignore_pattern,
                retain_all_files: self.retain_all_files,
                health_check_file_prefix: Some(&self.health_check_file_prefix),
            },
            include_symlinks: self.include_symlinks,
            root_dir: &self.root_dir,
            roots: &self.roots,
        }))
    }

    fn resolve(&mut self, _env: Env, output: CrawlOutput) -> Result<Self::JsValue> {
        Ok(handoff(output))
    }
}

/// Hashing and reading links for a delta, off the main thread.
pub struct DeltaIoTask {
    io: Option<DeltaIo>,
}

impl Task for DeltaIoTask {
    type Output = DeltaIoResult;
    type JsValue = Handoff<DeltaIoResult>;

    fn compute(&mut self) -> Result<DeltaIoResult> {
        Ok(self.io.take().unwrap_or_default().run())
    }

    fn resolve(&mut self, _env: Env, output: DeltaIoResult) -> Result<Self::JsValue> {
        Ok(handoff(output))
    }
}

/// Reading and decoding the cache file, off the main thread.
pub struct ReadCacheTask {
    path: String,
}

impl Task for ReadCacheTask {
    type Output = Option<CacheData>;
    type JsValue = Option<Handoff<CacheData>>;

    fn compute(&mut self) -> Result<Option<CacheData>> {
        Ok(read_cache_file(Path::new(&self.path)))
    }

    fn resolve(&mut self, _env: Env, output: Option<CacheData>) -> Result<Self::JsValue> {
        Ok(output.map(handoff))
    }
}

/// Serialising a cache snapshot and writing the file, off the main thread.
pub struct WriteCacheTask {
    path: String,
    data: CacheData,
}

impl Task for WriteCacheTask {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> Result<()> {
        write_cache_file(Path::new(&self.path), &self.data)
            .map_err(|e| napi::Error::from_reason(e.message))
    }

    fn resolve(&mut self, _env: Env, _output: ()) -> Result<()> {
        Ok(())
    }
}
