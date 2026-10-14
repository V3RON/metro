//! `FileMap`: the file store plus the metadata of every file, and the
//! operations of `index.js` / `TreeFS.js` built on both.
//!
//! The tree (`metro_rs_file_store::FileStore`) is the single source of truth
//! for paths and for whether a node is a directory, a regular file or a
//! symlink. The metadata is a side table indexed by `FileId`, kept in sync by
//! the tree's change sink on every add, modify and remove.
//!
//! TODO(windows): POSIX separators only.

use crate::aggregator::{Changes, FileSystemChangeAggregator};
use crate::crawl::{self, CrawlOptions};
use crate::metadata::{FileMetadata, Symlink};
use crate::process::{self, get_worker_input, PluginFilter};
use crate::{FileData, FileMapError};
use metro_rs_file_store::{ChangeSink, FileId, FileStore, NodeInfo, NodeLookup, RootPathUtils};
use rayon::prelude::*;
use std::collections::HashMap;

/// What the file map needs to know about the build: `computeSha1` and the
/// plugin workers' filters.
#[derive(Clone, Debug, Default)]
pub struct FileMapConfig {
    pub compute_sha1: bool,
    /// One entry per plugin that has a worker, in plugin order. The index is
    /// the plugin's data slot.
    pub plugins: Vec<PluginFilter>,
}

/// The result of `TreeFS.getDifference` and of a crawl.
#[derive(Clone, Debug, Default)]
pub struct Difference {
    pub changed_files: FileData,
    pub removed_files: Vec<String>,
}

pub(crate) struct Slot {
    pub(crate) meta: Option<FileMetadata>,
    /// Bumped whenever the node's metadata is replaced, so that a result
    /// computed off-thread for an older version can be dropped.
    pub(crate) version: u32,
}

pub struct FileMap {
    pub(crate) store: FileStore,
    pub(crate) slots: Vec<Slot>,
    pub(crate) config: FileMapConfig,
}

/// The tree's `FileSystemListener`: keeps the metadata table in step with the
/// tree and forwards to the aggregator, if any.
pub(crate) struct Recorder<'a> {
    pub(crate) slots: &'a mut Vec<Slot>,
    pub(crate) aggregator: Option<&'a mut FileSystemChangeAggregator>,
}

impl Recorder<'_> {
    fn slot(&mut self, id: FileId) -> &mut Slot {
        let idx = id.0 as usize;
        if self.slots.len() <= idx {
            self.slots.resize_with(idx + 1, || Slot {
                meta: None,
                version: 0,
            });
        }
        &mut self.slots[idx]
    }
}

impl ChangeSink<FileMetadata> for Recorder<'_> {
    fn directory_added(&mut self, canonical_path: &str) {
        if let Some(a) = self.aggregator.as_deref_mut() {
            a.directory_added(canonical_path);
        }
    }

    fn directory_removed(&mut self, canonical_path: &str) {
        if let Some(a) = self.aggregator.as_deref_mut() {
            a.directory_removed(canonical_path);
        }
    }

    fn file_added(&mut self, normal_path: &str, id: FileId, new: FileMetadata) {
        let has_aggregator = self.aggregator.is_some();
        let for_aggregator = has_aggregator.then(|| new.clone());
        let slot = self.slot(id);
        slot.meta = Some(new);
        slot.version = slot.version.wrapping_add(1);
        if let (Some(a), Some(data)) = (self.aggregator.as_deref_mut(), for_aggregator) {
            a.file_added(normal_path, data);
        }
    }

    fn file_modified(&mut self, normal_path: &str, id: FileId, new: FileMetadata) {
        let has_aggregator = self.aggregator.is_some();
        let for_aggregator = has_aggregator.then(|| new.clone());
        let slot = self.slot(id);
        let old = slot.meta.replace(new);
        slot.version = slot.version.wrapping_add(1);
        if let (Some(a), Some(new_data), Some(old_data)) =
            (self.aggregator.as_deref_mut(), for_aggregator, old)
        {
            a.file_modified(normal_path, old_data, new_data);
        }
    }

    fn file_removed(&mut self, canonical_path: &str, id: FileId) {
        let data = self.slot(id).meta.take();
        if let (Some(a), Some(data)) = (self.aggregator.as_deref_mut(), data) {
            a.file_removed(canonical_path, data);
        }
    }
}

/// Watch-mode events (`InternalEnqueuedEvent`) applied by [`FileMap::apply_events`].
#[derive(Clone, Debug)]
pub enum WatchEvent {
    Delete {
        normal_path: String,
    },
    /// `metadata` as `index.js` `onChange` builds it, after processing.
    Touch {
        normal_path: String,
        metadata: FileMetadata,
    },
}

/// One regular file that needs plugin workers (`FileProcessor`'s
/// `workerInput.pluginsToRun`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginJob {
    pub normal_path: String,
    pub plugins_to_run: Vec<usize>,
}

/// A plugin worker's answer for a job.
#[derive(Clone, Debug)]
pub struct PluginJobResult {
    pub normal_path: String,
    /// `Ok`: one value (JSON text, `None` for null/undefined) per entry of the
    /// job's `plugins_to_run`. `Err`: the worker threw.
    pub outcome: Result<Vec<Option<String>>, FileMapError>,
}

/// A delta whose removals are applied and whose files are hashed and
/// readlinked: waiting for the plugin workers. See [`FileMap::begin_delta`].
pub struct DeltaSession {
    changed_files: FileData,
    aggregator: FileSystemChangeAggregator,
    batch_errors: Vec<(String, FileMapError)>,
    readlink_errors: Vec<(String, FileMapError)>,
    plugin_jobs: Vec<PluginJob>,
}

impl DeltaSession {
    /// The regular files that need plugin-worker processing, in delta order.
    pub fn plugin_jobs(&self) -> &[PluginJob] {
        &self.plugin_jobs
    }
}

/// `touch` input of watch mode: what a watcher reports about a file.
#[derive(Clone, Debug, PartialEq)]
pub struct TouchInput {
    pub normal_path: String,
    pub mtime: Option<f64>,
    pub size: f64,
    /// `change.metadata.type === 'l'`.
    pub is_symlink: bool,
}

/// A processed touch: the metadata to enqueue and the plugins to run on it.
#[derive(Clone, Debug)]
pub struct TouchOutput {
    pub metadata: FileMetadata,
    pub plugins_to_run: Vec<usize>,
}

/// `TreeFS.linkStats`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FileStats {
    /// `'f'` or `'l'`.
    pub file_type: char,
    pub modified_time: Option<f64>,
    pub size: f64,
}

/// `TreeFS.lookup`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MapLookup {
    NotFound,
    Dir { real_path: String },
    File { real_path: String, id: FileId },
}

/// One `TreeFS.metadataIterator` item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataEntry {
    pub base_name: String,
    pub canonical_path: String,
    pub id: FileId,
}

impl FileMap {
    pub fn new(root_dir: &str, config: FileMapConfig) -> Self {
        Self {
            store: FileStore::new(root_dir),
            slots: Vec::new(),
            config,
        }
    }

    pub fn root_dir(&self) -> &str {
        self.store.root_dir()
    }

    pub fn path_utils(&self) -> &RootPathUtils {
        self.store.path_utils()
    }

    /// The tree. Read-only: all mutation goes through the file map so that the
    /// metadata stays in sync.
    pub fn store(&self) -> &FileStore {
        &self.store
    }

    pub fn config(&self) -> &FileMapConfig {
        &self.config
    }

    pub fn set_config(&mut self, config: FileMapConfig) {
        self.config = config;
    }

    /// The metadata of a file or symlink node.
    pub fn metadata(&self, id: FileId) -> Option<&FileMetadata> {
        self.slots.get(id.0 as usize).and_then(|s| s.meta.as_ref())
    }

    fn recorder<'a>(
        slots: &'a mut Vec<Slot>,
        aggregator: Option<&'a mut FileSystemChangeAggregator>,
    ) -> Recorder<'a> {
        Recorder { slots, aggregator }
    }

    // ---------------------------------------------------------------------
    // Crawl and difference
    // ---------------------------------------------------------------------

    /// `nodeCrawl`: `find`, then `getDifference` against this file system.
    /// Returns the difference and the crawl's warnings. (`subpath` is the
    /// normal subdirectory of a scoped re-crawl.)
    pub fn node_crawl(
        &self,
        options: &CrawlOptions<'_>,
        subpath: Option<&str>,
    ) -> Result<(Difference, Vec<String>), FileMapError> {
        let found = crawl::find(options);
        let difference = self.get_difference(&found.file_data, subpath)?;
        Ok((difference, found.warnings))
    }

    /// `TreeFS.getDifference`.
    pub fn get_difference(
        &self,
        files: &FileData,
        subpath: Option<&str>,
    ) -> Result<Difference, FileMapError> {
        let mut changed_files: FileData = files.clone();
        let mut removed_files: Vec<String> = Vec::new();

        // If a subpath is specified, start iteration from that node
        let mut root_node = FileId::ROOT;
        let mut prefix = String::new();
        if let Some(subpath) = subpath.filter(|s| !s.is_empty()) {
            match self.store.lookup_node(subpath, false)? {
                NodeLookup::Missing { .. } => {
                    // Directory doesn't exist, nothing to compare - all files are new
                    return Ok(Difference {
                        changed_files,
                        removed_files,
                    });
                }
                NodeLookup::Found {
                    node,
                    canonical_path,
                    ..
                } => {
                    if !matches!(self.store.node_info(node), NodeInfo::Dir) {
                        // A file or symlink has been replaced by this directory, so it is
                        // removed and everything under the directory is new.
                        removed_files.push(canonical_path);
                        return Ok(Difference {
                            changed_files,
                            removed_files,
                        });
                    }
                    root_node = node;
                    prefix = canonical_path;
                }
            }
        }

        self.walk_files(
            root_node,
            &mut prefix,
            true,
            true,
            &mut |_, canonical, id| {
                let Some(metadata) = self.metadata(id) else {
                    return;
                };
                match files.get(canonical) {
                    Some(new_metadata) => {
                        if new_metadata.is_regular_file() != metadata.is_regular_file() {
                            // Types differ, file has changed
                            return;
                        }
                        if new_metadata.mtime.is_some()
                        // TODO: Remove when mtime is null if not populated
                        && new_metadata.mtime != Some(0.0)
                        && new_metadata.mtime == metadata.mtime
                        {
                            // Types and modified time match - not changed.
                            changed_files.remove(canonical);
                        } else if new_metadata.sha1.is_some()
                            && new_metadata.sha1 == metadata.sha1
                            && metadata.visited
                        {
                            // Content matches - update modified time but don't revisit
                            let mut updated_metadata = metadata.clone();
                            updated_metadata.mtime = new_metadata.mtime;
                            changed_files.insert(canonical.to_string(), updated_metadata);
                        }
                    }
                    None => removed_files.push(canonical.to_string()),
                }
            },
        );
        Ok(Difference {
            changed_files,
            removed_files,
        })
    }

    /// `#metadataIterator(rootNode, opts, prefix)`: calls `f(baseName,
    /// canonicalPath, id)` for each file in `Map` order. `prefix` is restored
    /// on return.
    pub(crate) fn walk_files(
        &self,
        dir: FileId,
        prefix: &mut String,
        include_symlinks: bool,
        include_node_modules: bool,
        f: &mut dyn FnMut(&str, &str, FileId),
    ) {
        for (name, id) in self.store.ordered_children(dir) {
            let info = self.store.node_info(id);
            let is_dir = matches!(info, NodeInfo::Dir);
            if !include_node_modules && is_dir && name == "node_modules" {
                continue;
            }
            let saved = prefix.len();
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(name);
            match info {
                NodeInfo::Dir => {
                    self.walk_files(id, prefix, include_symlinks, include_node_modules, f)
                }
                NodeInfo::File => f(name, prefix, id),
                NodeInfo::Symlink { .. } | NodeInfo::UnresolvedSymlink => {
                    if include_symlinks {
                        f(name, prefix, id);
                    }
                }
                NodeInfo::Removed => {}
            }
            prefix.truncate(saved);
        }
    }

    // ---------------------------------------------------------------------
    // Applying a delta
    // ---------------------------------------------------------------------

    /// First half of `FileMap.#applyFileDelta`: removes the removed files,
    /// then (in parallel) hashes the regular files that need it and reads the
    /// symlinks. The returned session lists the files that need plugin
    /// workers; run them (in JS) and call [`FileMap::finish_delta`].
    ///
    /// Nothing but the removals touches the tree here.
    pub fn begin_delta(&mut self, delta: Difference) -> Result<DeltaSession, FileMapError> {
        let Difference {
            mut changed_files,
            removed_files,
        } = delta;
        // Remove files first so that we don't mistake moved modules
        // modules as duplicates.
        let mut aggregator = FileSystemChangeAggregator::new();
        for relative_file_path in &removed_files {
            let mut recorder = Self::recorder(&mut self.slots, Some(&mut aggregator));
            self.store
                .remove_with(relative_file_path, Some(&mut recorder))?;
        }

        struct HashJob {
            normal_path: String,
            absolute_path: String,
        }
        let mut hash_jobs: Vec<HashJob> = Vec::new();
        let mut link_jobs: Vec<String> = Vec::new();
        let mut plugin_jobs: Vec<PluginJob> = Vec::new();
        let mut processed: Vec<String> = Vec::new();
        let path_utils = self.store.path_utils();

        for (normal_file_path, file_data) in changed_files.iter() {
            // A crawler may preserve the H.VISITED flag to indicate that the file
            // contents are unchanged and it doesn't need visiting again.
            if file_data.visited {
                continue;
            }
            if file_data.symlink == Symlink::Regular {
                if let Some(input) = get_worker_input(
                    normal_file_path,
                    file_data,
                    self.config.compute_sha1,
                    &self.config.plugins,
                ) {
                    processed.push(normal_file_path.to_string());
                    if input.compute_sha1 {
                        hash_jobs.push(HashJob {
                            normal_path: normal_file_path.to_string(),
                            absolute_path: path_utils.normal_to_absolute(normal_file_path),
                        });
                    }
                    if !input.plugins_to_run.is_empty() {
                        plugin_jobs.push(PluginJob {
                            normal_path: normal_file_path.to_string(),
                            plugins_to_run: input.plugins_to_run,
                        });
                    }
                }
            } else if file_data.symlink == Symlink::Unresolved {
                link_jobs.push(normal_file_path.to_string());
            }
        }

        let hashes: Vec<Result<String, FileMapError>> = hash_jobs
            .par_iter()
            .map(|job| process::hash_file(&job.absolute_path))
            .collect();
        let links: Vec<Result<String, FileMapError>> = link_jobs
            .par_iter()
            .map(|p| process::read_link(path_utils, p))
            .collect();

        let mut batch_errors = Vec::new();
        for (job, hash) in hash_jobs.iter().zip(hashes) {
            match hash {
                Ok(sha1) => {
                    if let Some(m) = changed_files.get_mut(&job.normal_path) {
                        m.sha1 = Some(sha1);
                    }
                }
                Err(e) => batch_errors.push((job.normal_path.clone(), e)),
            }
        }
        // `processWorkerReply`: the file is visited once its worker has run.
        for normal_path in &processed {
            if let Some(m) = changed_files.get_mut(normal_path) {
                m.visited = true;
            }
        }
        let mut readlink_errors = Vec::new();
        for (normal_path, link) in link_jobs.iter().zip(links) {
            match link {
                Ok(target) => {
                    if let Some(m) = changed_files.get_mut(normal_path) {
                        m.visited = true;
                        m.symlink = Symlink::Target(target);
                    }
                }
                Err(e) => readlink_errors.push((normal_path.clone(), e)),
            }
        }

        Ok(DeltaSession {
            changed_files,
            aggregator,
            batch_errors,
            readlink_errors,
            plugin_jobs,
        })
    }

    /// Second half of `FileMap.#applyFileDelta`: stores the plugin data,
    /// drops files that vanished (`ENOENT`, `EACCESS`) or fails on any other
    /// error, then `bulkAddOrModify`s. Returns the aggregated change.
    ///
    /// The JS list of tolerated codes is `['ENOENT', 'EACCESS']`; `EACCESS` is
    /// not a real code (`EACCES` is), so a permission error is fatal, exactly
    /// as in JS.
    pub fn finish_delta(
        &mut self,
        session: DeltaSession,
        plugin_results: Vec<PluginJobResult>,
    ) -> Result<Changes, FileMapError> {
        let DeltaSession {
            mut changed_files,
            mut aggregator,
            batch_errors,
            readlink_errors,
            plugin_jobs,
        } = session;

        let plugins_by_path: HashMap<&str, &[usize]> = plugin_jobs
            .iter()
            .map(|j| (j.normal_path.as_str(), j.plugins_to_run.as_slice()))
            .collect();
        let mut plugin_errors: Vec<(String, FileMapError)> = Vec::new();
        for result in plugin_results {
            match result.outcome {
                Ok(data) => {
                    let Some(plugins_run) = plugins_by_path.get(result.normal_path.as_str()) else {
                        continue;
                    };
                    if let Some(metadata) = changed_files.get_mut(&result.normal_path) {
                        for (i, plugin_idx) in plugins_run.iter().enumerate() {
                            // `pluginData[i] ?? null`
                            let value = data.get(i).cloned().flatten();
                            metadata
                                .set_plugin_slot(*plugin_idx, Some(value.unwrap_or("null".into())));
                        }
                        metadata.visited = true;
                    }
                }
                Err(e) => plugin_errors.push((result.normal_path, e)),
            }
        }

        // A worker runs the plugins before it hashes, so a plugin error wins.
        let mut errors: Vec<(String, FileMapError)> = batch_errors
            .into_iter()
            .filter(|(p, _)| !plugin_errors.iter().any(|(q, _)| q == p))
            .collect();
        errors.extend(plugin_errors);
        errors.extend(readlink_errors);

        // It's possible that a file could be deleted between being seen by the
        // crawler and our attempt to process it. For our purposes, this is
        // equivalent to the file being deleted before the crawl, being absent
        // from `changedFiles`, and (if we loaded from cache, and the file
        // existed previously) possibly being reported in `removedFiles`.
        //
        // Treat the file accordingly - don't add it to `FileSystem`, and remove
        // it if it already exists. We're not emitting events at this point in
        // startup, so there's nothing more to do.
        for (normal_file_path, error) in errors {
            if matches!(error.code.as_deref(), Some("ENOENT" | "EACCESS")) {
                changed_files.remove(&normal_file_path);
                let mut recorder = Self::recorder(&mut self.slots, Some(&mut aggregator));
                self.store
                    .remove_with(&normal_file_path, Some(&mut recorder))?;
            } else {
                // Anything else is fatal.
                return Err(error);
            }
        }

        let mut recorder = Self::recorder(&mut self.slots, Some(&mut aggregator));
        self.store.bulk_add_with(
            changed_files
                .iter()
                .map(|(p, m)| (p, m.entry_kind(), m.clone())),
            &mut recorder,
        )?;
        Ok(aggregator.into_changes())
    }

    /// [`begin_delta`](FileMap::begin_delta) and
    /// [`finish_delta`](FileMap::finish_delta) for a build without plugin
    /// workers (or one that ignores their jobs).
    pub fn apply_delta(&mut self, delta: Difference) -> Result<Changes, FileMapError> {
        let session = self.begin_delta(delta)?;
        self.finish_delta(session, Vec::new())
    }

    // ---------------------------------------------------------------------
    // Watch mode
    // ---------------------------------------------------------------------

    /// The core of `emitChange`: applies the events in order through a fresh
    /// aggregator and returns the net change. A result with `size() == 0`
    /// means the events cancelled out and nothing is to be emitted.
    pub fn apply_events(&mut self, events: Vec<WatchEvent>) -> Result<Changes, FileMapError> {
        let mut aggregator = FileSystemChangeAggregator::new();
        // Process a sequence of events. Note that preserving ordering is
        // important here - a file may be both removed and added in the same
        // batch.
        for event in events {
            let mut recorder = Self::recorder(&mut self.slots, Some(&mut aggregator));
            match event {
                WatchEvent::Delete { normal_path } => {
                    self.store.remove_with(&normal_path, Some(&mut recorder))?;
                }
                WatchEvent::Touch {
                    normal_path,
                    metadata,
                } => {
                    let kind = metadata.entry_kind();
                    // `kind` borrows `metadata`; the sink gets its own copy.
                    let payload = metadata.clone();
                    self.store
                        .add_or_modify_with(&normal_path, kind, payload, &mut recorder)?;
                }
            }
        }
        Ok(aggregator.into_changes())
    }

    /// What `onChange` does for a `touch` before it enqueues the event: a
    /// symlink is read (`#maybeReadLink`), a regular file is processed
    /// (`processRegularFile` with `computeSha1`, plugins aside). Plugin
    /// workers are the caller's: set their data on `metadata` with
    /// [`FileMetadata::set_plugin_slot`] before enqueuing the event.
    ///
    /// The caller swallows `ENOENT` and `EACCESS` errors as the JS does.
    pub fn process_touch(&self, input: &TouchInput) -> Result<TouchOutput, FileMapError> {
        let mut metadata = FileMetadata::new(input.mtime, input.size, input.is_symlink);
        if input.is_symlink {
            let target = process::read_link(self.path_utils(), &input.normal_path)?;
            metadata.visited = true;
            metadata.symlink = Symlink::Target(target);
            return Ok(TouchOutput {
                metadata,
                plugins_to_run: Vec::new(),
            });
        }
        let Some(worker_input) = get_worker_input(
            &input.normal_path,
            &metadata,
            self.config.compute_sha1,
            &self.config.plugins,
        ) else {
            return Ok(TouchOutput {
                metadata,
                plugins_to_run: Vec::new(),
            });
        };
        if worker_input.compute_sha1 {
            let absolute = self.path_utils().normal_to_absolute(&input.normal_path);
            metadata.sha1 = Some(process::hash_file(&absolute)?);
        }
        metadata.visited = true;
        Ok(TouchOutput {
            metadata,
            plugins_to_run: worker_input.plugins_to_run,
        })
    }

    /// `fs.lstat` of a normal path as a [`TouchInput`], for a caller whose
    /// watcher did not report the stats.
    pub fn lstat_touch(&self, normal_path: &str) -> Result<TouchInput, FileMapError> {
        let absolute = self.path_utils().normal_to_absolute(normal_path);
        let stat = std::fs::symlink_metadata(&absolute)
            .map_err(|e| FileMapError::from_io(&e, "lstat", &absolute))?;
        Ok(TouchInput {
            normal_path: normal_path.to_string(),
            mtime: Some(crawl::mtime_ms(&stat)),
            size: stat.len() as f64,
            is_symlink: stat.file_type().is_symlink(),
        })
    }

    /// Replaces the whole contents (`bulkAddOrModify` of everything, without a
    /// listener), for loading a cache.
    pub(crate) fn load(&mut self, files: Vec<(String, FileMetadata)>) -> Result<(), FileMapError> {
        let mut recorder = Self::recorder(&mut self.slots, None);
        self.store.bulk_add_with(
            files
                .iter()
                .map(|(p, m)| (p.as_str(), m.entry_kind(), m.clone())),
            &mut recorder,
        )?;
        Ok(())
    }
}
