//! `FileStore`: a read replica of metro-file-map's `TreeFS`.
//!
//! TODO(windows): POSIX separators only. See `tree.rs` and `root_path_utils.rs`.

use crate::jsstr;
use crate::package_cache::{PackageCache, StoreAnswer};
use crate::root_path_utils::RootPathUtils;
use crate::tree::{
    lookup_by_normal_path, Ancestor, FileId, LookupOpts, LookupOut, Node, Nodes, ReadOnly,
    ReadWrite,
};
use metro_rs_resolver::path;
use metro_rs_resolver::Lookup;
use metro_rs_resolver::PackageForModule;
use serde_json::Value;
use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

/// What a non-directory node is. A regular file has `metadata[H.SYMLINK] === 0`,
/// a symlink has the lexically resolved target (a *normal* path with POSIX
/// separators, `metadata[H.SYMLINK]` as a string).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind<'a> {
    File,
    Symlink { target: &'a str },
}

/// The throws of `TreeFS.addOrModify` / `bulkAddOrModify` (and creating a
/// directory over a file). The message is the JS error message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStoreError(pub String);

impl fmt::Display for FileStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for FileStoreError {}

/// A batch of file map changes, as the file map reports them.
#[derive(Default, Debug, Clone)]
pub struct ChangeBatch<'a> {
    pub removed_files: Vec<&'a str>,
    pub removed_directories: Vec<&'a str>,
    pub added_directories: Vec<&'a str>,
    pub added_files: Vec<(&'a str, EntryKind<'a>)>,
    pub modified_files: Vec<(&'a str, EntryKind<'a>)>,
}

pub struct FileStore {
    root_dir: String,
    path_utils: RootPathUtils,
    nodes: Nodes,
    packages: PackageCache,
}

impl FileStore {
    pub fn new(root_dir: &str) -> Self {
        Self {
            root_dir: root_dir.to_string(),
            path_utils: RootPathUtils::new(root_dir),
            nodes: Nodes::new(),
            packages: PackageCache::new(),
        }
    }

    pub fn root_dir(&self) -> &str {
        &self.root_dir
    }

    pub fn path_utils(&self) -> &RootPathUtils {
        &self.path_utils
    }

    fn lookup_ro<'a>(&self, normal_path: &'a str, opts: LookupOpts<'_>) -> LookupOut<'a> {
        lookup_by_normal_path(
            &mut ReadOnly(&self.nodes),
            &self.path_utils,
            normal_path,
            opts,
        )
    }

    fn lookup_rw<'a>(&mut self, normal_path: &'a str, opts: LookupOpts<'_>) -> LookupOut<'a> {
        lookup_by_normal_path(
            &mut ReadWrite(&mut self.nodes),
            &self.path_utils,
            normal_path,
            opts,
        )
    }

    /// `#normalizePath`.
    pub fn normalize_path<'a>(&self, relative_or_absolute_path: &'a str) -> Cow<'a, str> {
        if path::is_absolute(relative_or_absolute_path) {
            self.path_utils
                .absolute_to_normal(relative_or_absolute_path)
        } else {
            self.path_utils
                .relative_to_normal(relative_or_absolute_path)
        }
    }

    /// `#getFileData(filePath)` with `followLeaf: true`: the id of the regular
    /// file, if `mixed_path` is one after following symlinks.
    fn get_file_data(&self, mixed_path: &str) -> Option<FileId> {
        let normal_path = self.normalize_path(mixed_path);
        match self.lookup_ro(&normal_path, LookupOpts::hot()) {
            LookupOut::Found { node, .. } if !self.nodes.is_directory(node) => Some(node),
            _ => None,
        }
    }

    /// `TreeFS.exists`: whether `mixed_path` is a regular file, after following
    /// symlinks.
    pub fn exists(&self, mixed_path: &str) -> bool {
        self.get_file_data(mixed_path).is_some()
    }

    /// `TreeFS.lookup`. `mixed_path` is absolute or root-relative.
    pub fn lookup(&self, mixed_path: &str) -> Lookup {
        let normal_path = self.normalize_path(mixed_path);
        match self.lookup_ro(&normal_path, LookupOpts::hot()) {
            LookupOut::Missing { .. } => Lookup::NotFound,
            LookupOut::Found {
                canonical_path,
                node,
                ..
            } => {
                let real_path = self.path_utils.normal_to_absolute(&canonical_path);
                if self.nodes.is_directory(node) {
                    Lookup::Dir { real_path }
                } else {
                    // lookup follows symlinks, so should never return one
                    assert!(
                        self.nodes.is_regular_file(node),
                        "lookup follows symlinks, so should never return one ({mixed_path} -> {canonical_path})"
                    );
                    Lookup::File { real_path }
                }
            }
        }
    }

    /// The kind of the node at `mixed_path` without following a leaf symlink
    /// (`linkStats`): `Some(true)` for a regular file, `Some(false)` for a
    /// symlink, `None` otherwise.
    pub fn link_is_regular_file(&self, mixed_path: &str) -> Option<bool> {
        let normal_path = self.normalize_path(mixed_path);
        let opts = LookupOpts {
            follow_leaf: false,
            ..LookupOpts::hot()
        };
        match self.lookup_ro(&normal_path, opts) {
            LookupOut::Found { node, .. } if !self.nodes.is_directory(node) => {
                Some(self.nodes.is_regular_file(node))
            }
            _ => None,
        }
    }

    /// The stable id of the regular file or symlink at `mixed_path`, following
    /// symlinks in directories and, if `follow_leaf`, at the leaf.
    pub fn file_id(&self, mixed_path: &str, follow_leaf: bool) -> Option<FileId> {
        let normal_path = self.normalize_path(mixed_path);
        let opts = LookupOpts {
            follow_leaf,
            ..LookupOpts::hot()
        };
        match self.lookup_ro(&normal_path, opts) {
            LookupOut::Found { node, .. } if !self.nodes.is_directory(node) => Some(node),
            _ => None,
        }
    }

    // ---------------------------------------------------------------------
    // Mutation
    // ---------------------------------------------------------------------

    fn alloc_entry(&mut self, kind: EntryKind<'_>) -> FileId {
        self.nodes.alloc(match kind {
            EntryKind::File => Node::File,
            EntryKind::Symlink { target } => Node::Symlink(target.into()),
        })
    }

    /// `directoryNode.set(basename, metadata)`. Overwrites whatever is there,
    /// like the JS does when there is no change listener. Modifying a file or
    /// symlink keeps its id.
    fn set_entry(&mut self, dir: FileId, basename: &str, kind: EntryKind<'_>) {
        if let Some(existing) = self.nodes.child(dir, basename) {
            if !self.nodes.is_directory(existing) {
                *self.nodes.get_mut(existing) = match kind {
                    EntryKind::File => Node::File,
                    EntryKind::Symlink { target } => Node::Symlink(target.into()),
                };
                return;
            }
        }
        let id = self.alloc_entry(kind);
        self.nodes.set_child(dir, basename, id);
    }

    /// `TreeFS.addOrModify(mixedPath, metadata)` without a listener.
    pub fn add_or_modify(
        &mut self,
        canonical_path: &str,
        kind: EntryKind<'_>,
    ) -> Result<(), FileStoreError> {
        let mixed_path = canonical_path;
        let normal_path = self.normalize_path(mixed_path);
        // Walk the tree to find the *real* path of the parent node, creating
        // directories as we need.
        let parent_dirname = path::dirname(&normal_path);
        let parent_dir_node = self.lookup_rw(
            &parent_dirname,
            LookupOpts {
                make_directories: true,
                ..Default::default()
            },
        );
        let LookupOut::Found {
            canonical_path: parent_canonical,
            ..
        } = parent_dir_node
        else {
            return Err(FileStoreError(format!(
                "TreeFS: Failed to make parent directory entry for {mixed_path}"
            )));
        };
        // Normalize the resulting path to account for the parent node being root.
        let joined = format!("{}/{}", parent_canonical, path::basename(&normal_path));
        let canonical = self.normalize_path(&joined).into_owned();
        self.bulk_add([(&*canonical, kind)])
    }

    /// `TreeFS.bulkAddOrModify` without a listener. Entries are canonical
    /// paths (`metadataIterator` order or any order).
    pub fn bulk_add<'a>(
        &mut self,
        entries: impl IntoIterator<Item = (&'a str, EntryKind<'a>)>,
    ) -> Result<(), FileStoreError> {
        // Optimisation: Bulk FileData are typically clustered by directory, so we
        // optimise for that case by remembering the last directory we looked up.
        // Experiments with large result sets show this to be significantly (~30%)
        // faster than caching all lookups in a Map, and 70% faster than no cache.
        let mut last_dir: Option<String> = None;
        let mut directory_node = FileId::ROOT;

        for (normal_path, kind) in entries {
            let last_sep_idx = normal_path.rfind('/');
            let dirname = last_sep_idx.map_or("", |i| &normal_path[..i]);
            let basename = last_sep_idx.map_or(normal_path, |i| &normal_path[i + 1..]);

            if last_dir.as_deref() != Some(dirname) {
                let lookup = self.lookup_rw(
                    dirname,
                    LookupOpts {
                        follow_leaf: false,
                        make_directories: true,
                        ..Default::default()
                    },
                );
                match lookup {
                    LookupOut::Missing {
                        canonical_missing_path,
                        ..
                    } => {
                        // This should only be possible if the input is non-real and
                        // lookup hits a broken symlink.
                        return Err(FileStoreError(format!(
                            "TreeFS: Unexpected error adding {normal_path}.\nMissing: {canonical_missing_path}"
                        )));
                    }
                    LookupOut::Found { node, .. } => {
                        if !self.nodes.is_directory(node) {
                            return Err(FileStoreError(format!(
                                "TreeFS: Could not add directory {dirname}, adding {normal_path}. {dirname} already exists in the file map as a file."
                            )));
                        }
                        last_dir = Some(dirname.to_string());
                        directory_node = node;
                    }
                }
            }
            self.set_entry(directory_node, basename, kind);
        }
        Ok(())
    }

    /// A directory reported by a change event (`changeListener.directoryAdded`):
    /// creates it and its missing ancestors, as `makeDirectories` does.
    pub fn add_directory(&mut self, canonical_path: &str) -> Result<(), FileStoreError> {
        let lookup = self.lookup_rw(
            canonical_path,
            LookupOpts {
                follow_leaf: false,
                make_directories: true,
                ..Default::default()
            },
        );
        match lookup {
            LookupOut::Found { node, .. } if self.nodes.is_directory(node) => Ok(()),
            LookupOut::Found { .. } => Err(FileStoreError(format!(
                "TreeFS: Could not add directory {canonical_path}. It already exists in the file map as a file."
            ))),
            LookupOut::Missing {
                canonical_missing_path,
                ..
            } => Err(FileStoreError(format!(
                "TreeFS: Unexpected error adding directory {canonical_path}.\nMissing: {canonical_missing_path}"
            ))),
        }
    }

    /// `TreeFS.remove(mixedPath)` without a listener. Also used for removed
    /// directories: removing a directory removes everything under it.
    pub fn remove(&mut self, canonical_path: &str) {
        let normal_path = self.normalize_path(canonical_path);
        self.remove_normal_path(&normal_path);
    }

    fn remove_normal_path(&mut self, normal_path: &str) {
        let result = self.lookup_ro(
            normal_path,
            LookupOpts {
                follow_leaf: false,
                ..LookupOpts::hot()
            },
        );
        let LookupOut::Found {
            parent_node,
            canonical_path,
            node,
            ..
        } = result
        else {
            return;
        };

        if self.nodes.is_directory(node) && self.nodes.dir_size(node) > 0 {
            for basename in self.nodes.child_names(node) {
                self.remove_normal_path(&format!("{canonical_path}/{basename}"));
            }
            // Removing the last file will delete this directory
            return;
        }
        if let Some(parent_node) = parent_node {
            self.nodes
                .delete_child(parent_node, &path::basename(&canonical_path));
            if self.nodes.dir_size(parent_node) == 0 && parent_node != FileId::ROOT {
                // NB: This isn't the most efficient algorithm - in the case of
                // removing the last file in a deep hierarchy it's O(depth^2), but
                // that's not expected to be a case common enough to justify
                // implementation complexity, or slowing down more common uses of
                // lookup_by_normal_path.
                self.remove_normal_path(&path::dirname(&canonical_path));
            }
        }
    }

    /// Test hook: replaces how package.json files are read.
    pub fn set_package_reader(&mut self, reader: crate::package_cache::ReadPackageJsonFn) {
        self.packages.set_reader(reader);
    }

    /// `packageCache.getPackage(p).packageJson ?? null` with the catch of
    /// createModuleResolver's `getPackage`. Anything Rust cannot be sure
    /// matches `JSON.parse(readFileSync(p, 'utf8'))` (unreadable file, invalid
    /// UTF-8, any serde error) is `Defer`, so the caller asks the JS callback.
    pub fn get_package(&self, package_json_path: &str) -> StoreAnswer<Option<Arc<Value>>> {
        self.packages.get_package(package_json_path)
    }

    /// `packageCache.getPackageForModule`. A parse error propagates in JS
    /// (it throws), so it is `Defer` here.
    pub fn get_package_for_module(
        &self,
        absolute_module_path: &str,
    ) -> StoreAnswer<Option<PackageForModule>> {
        self.packages
            .get_package_for_module(self, absolute_module_path)
    }

    /// `PackageCache.invalidate`.
    pub fn invalidate_package(&self, absolute_file_path: &str) {
        self.packages.invalidate(absolute_file_path);
    }

    /// Slice form of [`FileStore::apply_change_batch_full`]: `changed` holds
    /// added and modified files together.
    pub fn apply_change_batch(
        &mut self,
        removed_files: &[String],
        removed_directories: &[String],
        added_directories: &[String],
        changed: &[(&str, EntryKind<'_>)],
    ) -> Result<(), FileStoreError> {
        let batch = ChangeBatch {
            removed_files: removed_files.iter().map(String::as_str).collect(),
            removed_directories: removed_directories.iter().map(String::as_str).collect(),
            added_directories: added_directories.iter().map(String::as_str).collect(),
            added_files: changed.to_vec(),
            modified_files: Vec::new(),
        };
        self.apply_change_batch_full(&batch)
    }

    /// Applies a change batch and invalidates the package cache the way
    /// `DependencyGraph._onHasteChange` does.
    ///
    /// Order (every step is a no-op when it has nothing to do):
    ///
    /// 1. `removed_files`: `remove`. Pruning of directories left empty happens
    ///    here.
    /// 2. `removed_directories`: `remove`, which removes everything still
    ///    under them. Removals come first so that a path that changes kind in
    ///    the batch (directory to file, file to directory) is gone before its
    ///    replacement is added, and a directory that is removed and recreated
    ///    does not keep stale children.
    /// 3. `added_directories`: `add_directory`, shallowest first (sorted by
    ///    depth, stable), so that parents exist before children are reported.
    /// 4. `added_files`, then `modified_files`: in the order given, creating
    ///    directories as needed.
    /// 5. Package cache invalidation, after the tree is up to date, in the
    ///    order `[...addedFiles, ...modifiedFiles, ...removedFiles]` with
    ///    `path.join(rootDir, canonicalPath)`. Directories do not invalidate.
    ///
    /// An error stops the tree updates at the failing step, but the
    /// invalidations still run (the tree may have changed partially), and the
    /// first error is returned.
    pub fn apply_change_batch_full(
        &mut self,
        batch: &ChangeBatch<'_>,
    ) -> Result<(), FileStoreError> {
        let result = self.apply_tree_changes(batch);
        for (canonical_path, _) in batch.added_files.iter().chain(&batch.modified_files) {
            self.packages
                .invalidate(&path::join(&[&self.root_dir, canonical_path]));
        }
        for canonical_path in &batch.removed_files {
            self.packages
                .invalidate(&path::join(&[&self.root_dir, canonical_path]));
        }
        result
    }

    fn apply_tree_changes(&mut self, batch: &ChangeBatch<'_>) -> Result<(), FileStoreError> {
        for p in &batch.removed_files {
            self.remove(p);
        }
        for p in &batch.removed_directories {
            self.remove(p);
        }
        let mut dirs: Vec<&str> = batch.added_directories.clone();
        dirs.sort_by_key(|d| d.matches('/').count());
        for d in dirs {
            self.add_directory(d)?;
        }
        self.bulk_add(batch.added_files.iter().copied())?;
        self.bulk_add(batch.modified_files.iter().copied())?;
        Ok(())
    }

    // ---------------------------------------------------------------------
    // hierarchicalLookup
    // ---------------------------------------------------------------------

    /// Given a start path (which need not exist), a subpath and type, and
    /// optionally a 'breakOnSegment', performs the following:
    ///
    /// ```text
    /// X = mixedStartPath
    /// do
    ///   if basename(X) === opts.breakOnSegment
    ///     return null
    ///   if X + subpath exists and has type opts.subpathType
    ///     return {
    ///       absolutePath: realpath(X + subpath)
    ///       containerRelativePath: relative(mixedStartPath, X)
    ///     }
    ///   X = dirname(X)
    /// while X !== dirname(X)
    /// ```
    ///
    /// Useful for finding the closest package scope (subpath: package.json,
    /// type f, breakOnSegment: node_modules) or closest potential package root
    /// (subpath: node_modules/pkg, type: d) in Node.js resolution.
    ///
    /// Returns `(absolutePath, containerRelativePath)`.
    pub fn hierarchical_lookup(
        &self,
        mixed_start_path: &str,
        subpath: &str,
        break_on_segment: Option<&str>,
        subpath_type_dir: bool,
    ) -> Option<(String, String)> {
        let mut ancestors_of_input: Vec<Ancestor> = Vec::new();
        let normal_path = self.normalize_path(mixed_start_path);
        let closest_lookup = self.lookup_ro(
            &normal_path,
            LookupOpts {
                collect_ancestors: Some(&mut ancestors_of_input),
                ..Default::default()
            },
        );

        match &closest_lookup {
            LookupOut::Found {
                node,
                canonical_path,
                ..
            } if self.nodes.is_directory(*node) => {
                if let Some(abs) =
                    self.check_candidate_has_subpath(canonical_path, subpath, subpath_type_dir)
                {
                    return Some((abs, String::new()));
                }
            }
            other => {
                if let (
                    Some(b),
                    LookupOut::Missing {
                        missing_segment_name,
                        ..
                    },
                ) = (break_on_segment, other)
                {
                    if missing_segment_name == b {
                        return None;
                    }
                }
            }
        }

        // Let the "common root" be the nearest common ancestor of this.rootDir
        // and the input path. We'll look for a match in two stages:
        // 1. Every collected ancestor of the input path, from nearest to furthest,
        //    that is a descendent of the common root
        // 2. The common root, and its ancestors.
        let mut common_root = FileId::ROOT;
        let mut common_root_depth = 0usize;

        // Collected ancestors do not include the lookup result itself, so go one
        // further if the input path is itself a root ancestor.
        match &closest_lookup {
            LookupOut::Found {
                ancestor_of_root_idx: Some(idx),
                node,
                ..
            } => {
                common_root_depth = *idx;
                assert!(
                    self.nodes.is_directory(*node),
                    "ancestors of the root must be directories"
                );
                common_root = *node;
            }
            _ => {
                // Establish the common root by counting the '..' segments at the start
                // of the collected ancestors.
                for ancestor in &ancestors_of_input {
                    let Some(idx) = ancestor.ancestor_of_root_idx else {
                        break;
                    };
                    common_root_depth = idx;
                    common_root = ancestor.node;
                }
            }
        }

        // Phase 1: Consider descendenants of the common root, from deepest to
        // shallowest.
        let mut candidate_idx = ancestors_of_input.len() as isize - 1;
        while candidate_idx >= common_root_depth as isize {
            let candidate = &ancestors_of_input[candidate_idx as usize];
            if Some(candidate.segment_name()) == break_on_segment {
                return None;
            }
            // (The JS also passes a `start` here, which `#checkCandidateHasSubpath`
            // never forwards to the lookup.)
            if let Some(abs) =
                self.check_candidate_has_subpath(&candidate.normal_path, subpath, subpath_type_dir)
            {
                // Determine the input path relative to the current candidate. Note
                // that the candidate path will always be canonical (real), whereas the
                // input may contain symlinks, so the candidate is not necessarily a
                // prefix of the input. Use the fact that each remaining candidate
                // corresponds to a leading segment of the input normal path, and
                // discard the first candidateIdx + 1 segments of the input path.
                //
                // The next 5 lines are equivalent to (but faster than)
                // normalPath.split('/').slice(candidateIdx + 1).join('/').
                let mut prefix_length = (common_root_depth * 3) as isize; // Leading '../'
                for _ in common_root_depth as isize..=candidate_idx {
                    prefix_length = jsstr::index_of(&normal_path, b'/', prefix_length + 1);
                }
                let container_relative_path = jsstr::slice(&normal_path, prefix_length + 1, None);
                return Some((abs, container_relative_path));
            }
            candidate_idx -= 1;
        }

        // Phase 2: Consider the common root and its ancestors

        // This will be '', '..', '../..', etc.
        let mut candidate_normal_path = if common_root_depth > 0 {
            jsstr::slice(&normal_path, 0, Some(3 * common_root_depth as isize - 1))
        } else {
            String::new()
        };
        let remaining_normal_path =
            jsstr::slice(&normal_path, common_root_depth as isize * 3, None);

        let mut next_node: Option<FileId> = Some(common_root);
        let mut depth_below_common_root = 0usize;

        while let Some(node) = next_node.filter(|n| self.nodes.is_directory(*n)) {
            if let Some(abs) =
                self.check_candidate_has_subpath(&candidate_normal_path, subpath, subpath_type_dir)
            {
                let root_dir_parts = self.path_utils.get_parts();
                let mut relative_parts: Vec<&str> = if depth_below_common_root > 0 {
                    // rootDirParts.slice(
                    //   -(depthBelowCommonRoot + commonRootDepth),
                    //   commonRootDepth > 0 ? -commonRootDepth : undefined)
                    let len = root_dir_parts.len() as isize;
                    let start =
                        (len - (depth_below_common_root + common_root_depth) as isize).max(0);
                    let end = if common_root_depth > 0 {
                        (len - common_root_depth as isize).max(0)
                    } else {
                        len
                    };
                    if start < end {
                        root_dir_parts[start as usize..end as usize]
                            .iter()
                            .map(String::as_str)
                            .collect()
                    } else {
                        Vec::new()
                    }
                } else {
                    Vec::new()
                };
                if !remaining_normal_path.is_empty() {
                    relative_parts.push(&remaining_normal_path);
                }
                return Some((abs, relative_parts.join("/")));
            }
            depth_below_common_root += 1;
            candidate_normal_path = if candidate_normal_path.is_empty() {
                "..".to_string()
            } else {
                candidate_normal_path + "/.."
            };
            next_node = self.nodes.child(node, "..");
        }
        None
    }

    /// `#checkCandidateHasSubpath`.
    fn check_candidate_has_subpath(
        &self,
        normal_candidate_path: &str,
        subpath: &str,
        subpath_type_dir: bool,
    ) -> Option<String> {
        let joined = self
            .path_utils
            .join_normal_to_relative(normal_candidate_path, subpath)
            .normal_path;
        match self.lookup_ro(&joined, LookupOpts::hot()) {
            LookupOut::Found {
                node,
                canonical_path,
                ..
            } if
            // Should be a Map iff subpathType is directory
            self.nodes.is_directory(node) == subpath_type_dir => {
                Some(self.path_utils.normal_to_absolute(&canonical_path))
            }
            _ => None,
        }
    }

    /// Every regular file and symlink as `path|f` or `path|l:<target>`, sorted.
    /// Mirrors `metadataIterator({includeSymlinks, includeNodeModules})`; for
    /// tests and debugging.
    pub fn metadata_iterator(&self) -> Vec<String> {
        fn walk(nodes: &Nodes, dir: FileId, prefix: &str, out: &mut Vec<String>) {
            let Node::Dir(children) = nodes.get(dir) else {
                return;
            };
            for (name, id) in children {
                let p = if prefix.is_empty() {
                    name.to_string()
                } else {
                    format!("{prefix}/{name}")
                };
                match nodes.get(*id) {
                    Node::Dir(_) => walk(nodes, *id, &p, out),
                    Node::File => out.push(format!("{p}|f")),
                    Node::Symlink(t) => out.push(format!("{p}|l:{t}")),
                    Node::Removed => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.nodes, FileId::ROOT, "", &mut out);
        out.sort();
        out
    }
}
