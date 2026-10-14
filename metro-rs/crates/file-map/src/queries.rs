//! The `TreeFS` queries: `getAllFiles`, `getSha1`, `getSize`, `linkStats`,
//! `lookup`, `matchFiles`, `metadataIterator`, `getOrComputeSha1` and
//! `getSerializableSnapshot`.
//!
//! TODO(windows): POSIX separators only.

use crate::file_map::{FileMap, FileStats, MapLookup, MetadataEntry, Slot};
use crate::js_regex::JsRegExp;
use crate::metadata::FileMetadata;
use crate::process::{self, get_worker_input};
use crate::FileMapError;
use metro_rs_file_store::{FileId, Invariant, NodeInfo, NodeLookup};
use metro_rs_resolver::path;
use serde_json::{Map, Value};

/// `MatchFilesOptions`.
#[derive(Clone, Copy)]
pub struct MatchFilesOptions<'a> {
    /// Filter relative paths against a pattern.
    pub filter: Option<&'a JsRegExp>,
    /// `filter` is applied against absolute paths, vs rootDir-relative. (default: false)
    pub filter_compare_absolute: bool,
    /// `filter` is applied against posix-delimited paths, even on Windows. (default: false)
    pub filter_compare_posix: bool,
    /// Follow symlinks when enumerating paths. (default: false)
    pub follow: bool,
    /// Should search for files recursively. (default: true)
    pub recursive: bool,
    /// Match files under a given root, or `None` for all files.
    pub root_dir: Option<&'a str>,
}

impl Default for MatchFilesOptions<'_> {
    fn default() -> Self {
        Self {
            filter: None,
            filter_compare_absolute: false,
            filter_compare_posix: false,
            follow: false,
            recursive: true,
            root_dir: None,
        }
    }
}

/// What `getOrComputeSha1` found, or what to compute. See
/// [`FileMap::sha1_job`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sha1Job {
    /// Not a file: `null`.
    None,
    /// `{sha1: existing}`.
    Cached(String),
    /// Read `absolute_path` and hash it, then [`FileMap::finish_sha1`].
    Compute {
        id: FileId,
        version: u32,
        canonical_path: String,
        absolute_path: String,
        /// Plugins whose worker `processRegularFile` would also run.
        plugins_to_run: Vec<usize>,
    },
}

/// `{sha1, content?}` of `getOrComputeSha1`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sha1Result {
    pub sha1: String,
    /// The contents, when the file had to be read.
    pub content: Option<Vec<u8>>,
    /// Plugins whose worker `processRegularFile` would also have run on this
    /// file (the caller can re-run them; the data is the same unless the file
    /// changed).
    pub plugins_to_run: Vec<usize>,
}

impl FileMap {
    /// `TreeFS.getSerializableSnapshot()` as the JS tree of `CacheData.fileSystemData`:
    /// a directory is a JSON object (keys in `Map` order) and a file is the
    /// metadata tuple `[mtime, size, visited, sha1, symlink, ...pluginData]`
    /// as an array. A plugin slot that is `undefined` in JS is `null` here.
    pub fn get_serializable_snapshot(&self) -> Value {
        self.clone_tree(FileId::ROOT)
    }

    fn clone_tree(&self, dir: FileId) -> Value {
        let mut map = Map::new();
        for (name, id) in self.store.ordered_children(dir) {
            match self.store.node_info(id) {
                NodeInfo::Dir => {
                    map.insert(name.to_string(), self.clone_tree(id));
                }
                NodeInfo::Removed => {}
                _ => {
                    if let Some(m) = self.metadata(id) {
                        map.insert(name.to_string(), m.to_tuple());
                    }
                }
            }
        }
        Value::Object(map)
    }

    fn normalize(&self, mixed_path: &str) -> String {
        self.store.normalize_path(mixed_path).into_owned()
    }

    /// `#getFileData(mixedPath, {followLeaf})`.
    fn get_file_data(
        &self,
        mixed_path: &str,
        follow_leaf: bool,
    ) -> Result<Option<&FileMetadata>, Invariant> {
        Ok(self
            .store
            .file_id(mixed_path, follow_leaf)?
            .and_then(|id| self.metadata(id)))
    }

    /// `TreeFS.getSize`.
    pub fn get_size(&self, mixed_path: &str) -> Result<Option<f64>, Invariant> {
        Ok(self.get_file_data(mixed_path, true)?.map(|m| m.size))
    }

    /// `TreeFS.getSha1`.
    pub fn get_sha1(&self, mixed_path: &str) -> Result<Option<String>, Invariant> {
        Ok(self
            .get_file_data(mixed_path, true)?
            .and_then(|m| m.sha1.clone()))
    }

    /// `TreeFS.exists`.
    pub fn exists(&self, mixed_path: &str) -> Result<bool, Invariant> {
        self.store.exists(mixed_path)
    }

    /// `TreeFS.lookup` (without `observations`).
    pub fn lookup(&self, mixed_path: &str) -> Result<MapLookup, Invariant> {
        let normal_path = self.normalize(mixed_path);
        match self.store.lookup_node(&normal_path, true)? {
            NodeLookup::Missing { .. } => Ok(MapLookup::NotFound),
            NodeLookup::Found {
                canonical_path,
                node,
                ..
            } => {
                let real_path = self.path_utils().normal_to_absolute(&canonical_path);
                match self.store.node_info(node) {
                    NodeInfo::Dir => Ok(MapLookup::Dir { real_path }),
                    NodeInfo::File => Ok(MapLookup::File { real_path, id: node }),
                    _ => Err(Invariant(format!(
                        "lookup follows symlinks, so should never return one ({mixed_path} -> {canonical_path})"
                    ))),
                }
            }
        }
    }

    /// `TreeFS.getAllFiles`.
    pub fn get_all_files(&self) -> Vec<String> {
        self.metadata_iterator(false, true)
            .into_iter()
            .map(|e| self.path_utils().normal_to_absolute(&e.canonical_path))
            .collect()
    }

    /// `TreeFS.linkStats`.
    pub fn link_stats(&self, mixed_path: &str) -> Result<Option<FileStats>, Invariant> {
        let Some(file_metadata) = self.get_file_data(mixed_path, false)? else {
            return Ok(None);
        };
        Ok(Some(FileStats {
            file_type: if file_metadata.is_regular_file() {
                'f'
            } else {
                'l'
            },
            modified_time: file_metadata.mtime,
            size: file_metadata.size,
        }))
    }

    /// `TreeFS.metadataIterator({includeSymlinks, includeNodeModules})`.
    pub fn metadata_iterator(
        &self,
        include_symlinks: bool,
        include_node_modules: bool,
    ) -> Vec<MetadataEntry> {
        let mut out = Vec::new();
        self.walk_files(
            FileId::ROOT,
            &mut String::new(),
            include_symlinks,
            include_node_modules,
            &mut |base_name, canonical_path, id| {
                out.push(MetadataEntry {
                    base_name: base_name.to_string(),
                    canonical_path: canonical_path.to_string(),
                    id,
                })
            },
        );
        out
    }

    // ---------------------------------------------------------------------
    // getOrComputeSha1
    // ---------------------------------------------------------------------

    /// The lookup half of `TreeFS.getOrComputeSha1`, so that the caller can
    /// hash on another thread.
    pub fn sha1_job(&self, mixed_path: &str) -> Result<Sha1Job, Invariant> {
        let normal_path = self.normalize(mixed_path);
        let NodeLookup::Found {
            canonical_path,
            node,
            ..
        } = self.store.lookup_node(&normal_path, true)?
        else {
            return Ok(Sha1Job::None);
        };
        if matches!(self.store.node_info(node), NodeInfo::Dir) {
            return Ok(Sha1Job::None);
        }
        let Some(file_metadata) = self.metadata(node) else {
            return Ok(Sha1Job::None);
        };

        // Empty strings
        if let Some(existing) = &file_metadata.sha1 {
            if !existing.is_empty() {
                return Ok(Sha1Job::Cached(existing.clone()));
            }
        }

        // `processFile(canonicalPath, fileMetadata, {computeSha1: true})`
        let Some(input) =
            get_worker_input(&canonical_path, file_metadata, true, &self.config.plugins)
        else {
            // No worker ran, so the SHA-1 stays empty.
            return Err(Invariant(format!(
                "File processing didn't populate a SHA-1 hash for {canonical_path}"
            )));
        };
        let version = self.slots[node.0 as usize].version;
        Ok(Sha1Job::Compute {
            id: node,
            version,
            absolute_path: self.path_utils().normal_to_absolute(&canonical_path),
            canonical_path,
            plugins_to_run: input.plugins_to_run,
        })
    }

    /// The storing half of `TreeFS.getOrComputeSha1`: sets the SHA-1 (and the
    /// visited flag) of the metadata the job was made from. Returns `false`
    /// and does nothing when the file has been added, modified or removed
    /// since: the JS mutates the metadata it first retrieved, which is then
    /// orphaned, "so we don't risk caching a stale SHA-1 after a change event".
    pub fn finish_sha1(&mut self, id: FileId, version: u32, sha1: &str) -> bool {
        match self.slots.get_mut(id.0 as usize) {
            Some(Slot {
                meta: Some(meta),
                version: v,
            }) if *v == version => {
                meta.sha1 = Some(sha1.to_string());
                meta.visited = true;
                true
            }
            _ => false,
        }
    }

    /// `TreeFS.getOrComputeSha1`, synchronously.
    pub fn get_or_compute_sha1(
        &mut self,
        mixed_path: &str,
    ) -> Result<Option<Sha1Result>, FileMapError> {
        match self.sha1_job(mixed_path)? {
            Sha1Job::None => Ok(None),
            Sha1Job::Cached(sha1) => Ok(Some(Sha1Result {
                sha1,
                content: None,
                plugins_to_run: Vec::new(),
            })),
            Sha1Job::Compute {
                id,
                version,
                absolute_path,
                plugins_to_run,
                ..
            } => {
                let (sha1, content) = process::read_and_hash(&absolute_path)?;
                self.finish_sha1(id, version, &sha1);
                Ok(Some(Sha1Result {
                    sha1,
                    content: Some(content),
                    plugins_to_run,
                }))
            }
        }
    }

    // ---------------------------------------------------------------------
    // matchFiles
    // ---------------------------------------------------------------------

    /// Given a search context, return a list of file paths matching the query.
    /// The query matches against normalized paths which start with `./`,
    /// for example: `a/b.js` -> `./a/b.js`
    pub fn match_files(&self, opts: &MatchFilesOptions<'_>) -> Result<Vec<String>, Invariant> {
        let MatchFilesOptions {
            filter,
            filter_compare_absolute,
            filter_compare_posix,
            follow,
            recursive,
            root_dir,
        } = *opts;
        let normal_root = match root_dir {
            None => String::new(),
            Some(r) => self.normalize(r),
        };
        let NodeLookup::Found {
            ancestor_of_root_idx,
            canonical_path: root_real_path,
            node: context_root,
            parent_node: context_root_parent,
        } = self.store.lookup_node(&normal_root, true)?
        else {
            return Ok(Vec::new());
        };
        if !matches!(self.store.node_info(context_root), NodeInfo::Dir) {
            return Ok(Vec::new());
        }
        let context_root_absolute_path = if root_real_path.is_empty() {
            self.root_dir().to_string()
        } else {
            path::join(&[self.root_dir(), &root_real_path])
        };

        // (`path.sep` is '/', so the prefix is './' either way.)
        let prefix = "./";

        // (`replaceAll(path.sep, '/')` is the identity on POSIX.)
        let context_root_absolute_path_for_comparison = &context_root_absolute_path;

        let it_opts = PathIteratorOptions {
            canonical_path_of_root: &root_real_path,
            follow,
            recursive,
            subtree_only: root_dir.is_some(),
        };
        let mut relative_paths = Vec::new();
        self.path_iterator(
            context_root,
            context_root_parent,
            ancestor_of_root_idx,
            &it_opts,
            "",
            &[],
            &mut relative_paths,
        )?;

        let mut out = Vec::new();
        for relative_path_for_comparison in relative_paths {
            let matches = match filter {
                None => true,
                Some(filter) => {
                    // NOTE(EvanBacon): Ensure files start with `./` for matching purposes
                    // this ensures packages work across Metro and Webpack (ex: Storybook for React DOM / React Native).
                    // `a/b.js` -> `./a/b.js`
                    let subject = if filter_compare_absolute {
                        path::join(&[
                            context_root_absolute_path_for_comparison,
                            &relative_path_for_comparison,
                        ])
                    } else {
                        format!("{prefix}{relative_path_for_comparison}")
                    };
                    filter.test(&subject)
                }
            };
            if matches {
                // (`filterComparePosix` only changes separators on Windows.)
                let _ = filter_compare_posix;
                out.push(path::join(&[
                    &context_root_absolute_path,
                    &relative_path_for_comparison,
                ]));
            }
        }
        Ok(out)
    }

    /// `#directoryNodeIterator`.
    fn directory_node_iterator(
        &self,
        node: FileId,
        parent: Option<FileId>,
        ancestor_of_root_idx: Option<usize>,
    ) -> Vec<(String, FileId)> {
        let mut out = Vec::new();
        if let (Some(idx), Some(parent)) = (ancestor_of_root_idx, parent) {
            if idx > 0 {
                out.push((
                    self.path_utils()
                        .get_basename_of_nth_ancestor(idx - 1)
                        .to_string(),
                    parent,
                ));
            }
        }
        out.extend(
            self.store
                .ordered_children(node)
                .into_iter()
                .map(|(n, id)| (n.to_string(), id)),
        );
        out
    }

    /// Enumerate paths under a given node, including symlinks and through
    /// symlinks (if `follow` is enabled).
    #[allow(clippy::too_many_arguments)]
    fn path_iterator(
        &self,
        iteration_root_node: FileId,
        iteration_root_parent_node: Option<FileId>,
        ancestor_of_root_idx: Option<usize>,
        opts: &PathIteratorOptions<'_>,
        path_prefix: &str,
        followed_links: &[FileId],
        out: &mut Vec<String>,
    ) -> Result<(), Invariant> {
        let prefix_with_sep = if path_prefix.is_empty() {
            String::new()
        } else {
            format!("{path_prefix}/")
        };
        for (name, node) in self.directory_node_iterator(
            iteration_root_node,
            iteration_root_parent_node,
            ancestor_of_root_idx,
        ) {
            if opts.subtree_only && name == ".." {
                continue;
            }

            let node_path = format!("{prefix_with_sep}{name}");
            match self.store.node_info(node) {
                NodeInfo::Dir => {
                    if opts.recursive {
                        self.path_iterator(
                            node,
                            iteration_root_parent_node,
                            match ancestor_of_root_idx {
                                Some(idx) if idx > 0 => Some(idx - 1),
                                _ => None,
                            },
                            opts,
                            &node_path,
                            followed_links,
                            out,
                        )?;
                    }
                }
                NodeInfo::File => {
                    // regular file
                    out.push(node_path);
                }
                NodeInfo::Removed => {}
                NodeInfo::Symlink { .. } | NodeInfo::UnresolvedSymlink => {
                    // symlink

                    // Although both paths are normal, the node path may begin '..' so we
                    // can't simply concatenate.
                    let normal_path_of_symlink =
                        path::join(&[opts.canonical_path_of_root, &node_path]);

                    // We can't resolve the symlink directly here because we only have
                    // its normal path, and we need a canonical path for resolution
                    // (imagine our normal path contains a symlink 'bar' -> '.', and we
                    // are at /foo/bar/baz where baz -> '..' - that should resolve to
                    // /foo, not /foo/bar). We *can* use _lookupByNormalPath to walk to
                    // the canonical symlink, and then to its target.
                    let NodeLookup::Found {
                        node: target,
                        parent_node,
                        ancestor_of_root_idx: target_ancestor_idx,
                        ..
                    } = self.store.lookup_node(&normal_path_of_symlink, true)?
                    else {
                        // Symlink goes nowhere, nothing to report.
                        continue;
                    };
                    if !matches!(self.store.node_info(target), NodeInfo::Dir) {
                        // Symlink points to a file, just yield the path of the symlink.
                        out.push(node_path);
                    } else if opts.recursive && opts.follow && !followed_links.contains(&node) {
                        // Symlink points to a directory - iterate over its contents using
                        // the path where we found the symlink as a prefix.
                        let mut followed = followed_links.to_vec();
                        followed.push(node);
                        self.path_iterator(
                            target,
                            parent_node,
                            target_ancestor_idx,
                            opts,
                            &node_path,
                            &followed,
                            out,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }
}

struct PathIteratorOptions<'a> {
    canonical_path_of_root: &'a str,
    follow: bool,
    recursive: bool,
    subtree_only: bool,
}
