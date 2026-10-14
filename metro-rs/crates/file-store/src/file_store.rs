//! `FileStore`: a read replica of metro-file-map's `TreeFS`.
//!
//! TODO(windows): POSIX separators only. See `tree.rs` and `root_path_utils.rs`.

use crate::invariant::Invariant;
use crate::jsstr;
use crate::package_cache::{PackageCache, StoreAnswer};
use crate::root_path_utils::RootPathUtils;
use crate::tree::FxHasher;
use crate::tree::{
    lookup_by_normal_path, Ancestor, FileId, LookupOpts, LookupOut, Node, Nodes, ReadOnly,
    ReadWrite,
};
use metro_rs_resolver::path;
use metro_rs_resolver::Lookup;
use metro_rs_resolver::PackageForModule;
use serde_json::Value;
use std::borrow::Cow;
use std::cell::RefCell;
use std::fmt;
use std::hash::BuildHasherDefault;
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

impl From<Invariant> for FileStoreError {
    fn from(e: Invariant) -> Self {
        FileStoreError(e.0)
    }
}

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
    /// Interned directories (boundary rule 4): absolute directory path as the
    /// resolver asks for it, to the `FileId` and canonical normal path of the
    /// directory it reaches (after following symlinks), or `None` if it is not
    /// a directory. Lets `lookup` and `exists` resolve a directory once and
    /// then look the basename up in it. Cleared on every change to the tree.
    dirs: RefCell<FxHashMap<Box<str>, Option<InternedDir>>>,
}

/// An interned directory: its id and canonical normal path.
type InternedDir = (FileId, Box<str>);

type FxHashMap<K, V> = std::collections::HashMap<K, V, BuildHasherDefault<FxHasher>>;

/// Splits an absolute path into its directory and a plain basename when the
/// directory-table fast path applies: absolute, no empty, `.` or `..`
/// segments, and no trailing slash. Anything else takes the full walk.
fn split_plain_absolute(path: &str) -> Option<(&str, &str)> {
    if !path.starts_with('/') || path.ends_with('/') {
        return None;
    }
    if path[1..]
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return None;
    }
    let slash = path.rfind('/')?;
    // The root directory itself is "/" (slash at 0).
    let dir = if slash == 0 { "/" } else { &path[..slash] };
    Some((dir, &path[slash + 1..]))
}

impl FileStore {
    pub fn new(root_dir: &str) -> Self {
        Self {
            root_dir: root_dir.to_string(),
            path_utils: RootPathUtils::new(root_dir),
            nodes: Nodes::new(),
            packages: PackageCache::new(),
            dirs: RefCell::new(FxHashMap::default()),
        }
    }

    /// The directory table entry for `dir` (see `dirs`), filled on first use.
    fn interned_dir(&self, dir: &str) -> Option<(FileId, Box<str>)> {
        let cached = self.dirs.borrow().get(dir).cloned();
        if let Some(entry) = cached {
            return entry;
        }
        let normal_path = self.normalize_path(dir);
        // On an invariant error the full walk reports it.
        let found = self.lookup_ro(&normal_path, LookupOpts::hot()).ok()?;
        let entry = match found {
            LookupOut::Found {
                canonical_path,
                node,
                ..
            } if self.nodes.is_directory(node) && node != FileId::PHANTOM => {
                Some((node, Box::from(canonical_path.as_ref())))
            }
            LookupOut::Found { node, .. } if node == FileId::PHANTOM => {
                // The detached '..' directory: not interned, the caller walks.
                return None;
            }
            _ => None,
        };
        self.dirs.borrow_mut().insert(Box::from(dir), entry.clone());
        entry
    }

    /// The fast path of `lookup` and `exists`: `Some(None)` when the path does
    /// not exist, `Some(Some((node, canonical normal path)))` for a directory
    /// or regular file, and `None` when the full walk is needed (a symlink at
    /// the leaf, the detached '..' directory, or a path that is not plain).
    fn lookup_via_dir(&self, mixed_path: &str) -> Option<Option<(FileId, String)>> {
        let (dir, name) = split_plain_absolute(mixed_path)?;
        // Only inside the root: there the normal path of `dir/name` is the
        // normal path of `dir` plus `/name`. Above the root it is not (an
        // ancestor of the root normalises to '..' segments).
        let inside_root = dir == self.root_dir
            || (dir.starts_with(self.root_dir.as_str())
                && dir.as_bytes().get(self.root_dir.len()) == Some(&b'/')
                && !self.root_dir.ends_with('/'));
        if !inside_root {
            return None;
        }
        let cached = self.dirs.borrow().get(dir).cloned();
        let (dir_id, dir_canonical) = match cached {
            Some(Some(entry)) => entry,
            // A known non-directory: nothing below it exists.
            Some(None) => return Some(None),
            None => match self.interned_dir(dir) {
                Some(entry) => entry,
                None => {
                    return if self.dirs.borrow().contains_key(dir) {
                        Some(None)
                    } else {
                        None
                    };
                }
            },
        };
        let child = match self.nodes.child(dir_id, name) {
            None => return Some(None),
            Some(child) => child,
        };
        if !self.nodes.is_directory(child) && !self.nodes.is_regular_file(child) {
            // A symlink: follow it with the full walk.
            return None;
        }
        let mut canonical = String::with_capacity(dir_canonical.len() + 1 + name.len());
        if !dir_canonical.is_empty() {
            canonical.push_str(&dir_canonical);
            canonical.push('/');
        }
        canonical.push_str(name);
        Some(Some((child, canonical)))
    }

    /// Empties the tree (keeping the package cache), before a `bulk_add` of
    /// the whole file map. Like the JS `PackageCache`, which a reload of the
    /// file map does not reset, cached packages stay until invalidated.
    pub fn clear_tree(&mut self) {
        self.nodes = Nodes::new();
        self.dirs.get_mut().clear();
    }

    pub fn root_dir(&self) -> &str {
        &self.root_dir
    }

    pub fn path_utils(&self) -> &RootPathUtils {
        &self.path_utils
    }

    fn lookup_ro<'a>(
        &self,
        normal_path: &'a str,
        opts: LookupOpts<'_>,
    ) -> Result<LookupOut<'a>, Invariant> {
        lookup_by_normal_path(
            &mut ReadOnly(&self.nodes),
            &self.path_utils,
            normal_path,
            opts,
        )
    }

    fn lookup_rw<'a>(
        &mut self,
        normal_path: &'a str,
        opts: LookupOpts<'_>,
    ) -> Result<LookupOut<'a>, Invariant> {
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
    fn get_file_data(&self, mixed_path: &str) -> Result<Option<FileId>, Invariant> {
        if let Some(found) = self.lookup_via_dir(mixed_path) {
            return Ok(found
                .map(|(node, _)| node)
                .filter(|node| !self.nodes.is_directory(*node)));
        }
        let normal_path = self.normalize_path(mixed_path);
        Ok(match self.lookup_ro(&normal_path, LookupOpts::hot())? {
            LookupOut::Found { node, .. } if !self.nodes.is_directory(node) => Some(node),
            _ => None,
        })
    }

    /// `TreeFS.exists`: whether `mixed_path` is a regular file, after following
    /// symlinks.
    pub fn exists(&self, mixed_path: &str) -> Result<bool, Invariant> {
        Ok(self.get_file_data(mixed_path)?.is_some())
    }

    /// `TreeFS.lookup`. `mixed_path` is absolute or root-relative.
    pub fn lookup(&self, mixed_path: &str) -> Result<Lookup, Invariant> {
        if let Some(found) = self.lookup_via_dir(mixed_path) {
            return Ok(match found {
                None => Lookup::NotFound,
                Some((node, canonical_path)) => {
                    let real_path = self.path_utils.normal_to_absolute(&canonical_path);
                    if self.nodes.is_directory(node) {
                        Lookup::Dir { real_path }
                    } else {
                        Lookup::File { real_path }
                    }
                }
            });
        }
        let normal_path = self.normalize_path(mixed_path);
        Ok(match self.lookup_ro(&normal_path, LookupOpts::hot())? {
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
                    if !self.nodes.is_regular_file(node) {
                        return Err(Invariant(format!(
                            "lookup follows symlinks, so should never return one ({mixed_path} -> {canonical_path})"
                        )));
                    }
                    Lookup::File { real_path }
                }
            }
        })
    }

    /// The kind of the node at `mixed_path` without following a leaf symlink
    /// (`linkStats`): `Some(true)` for a regular file, `Some(false)` for a
    /// symlink, `None` otherwise.
    pub fn link_is_regular_file(&self, mixed_path: &str) -> Result<Option<bool>, Invariant> {
        let normal_path = self.normalize_path(mixed_path);
        let opts = LookupOpts {
            follow_leaf: false,
            ..LookupOpts::hot()
        };
        Ok(match self.lookup_ro(&normal_path, opts)? {
            LookupOut::Found { node, .. } if !self.nodes.is_directory(node) => {
                Some(self.nodes.is_regular_file(node))
            }
            _ => None,
        })
    }

    /// The stable id of the regular file or symlink at `mixed_path`, following
    /// symlinks in directories and, if `follow_leaf`, at the leaf.
    pub fn file_id(
        &self,
        mixed_path: &str,
        follow_leaf: bool,
    ) -> Result<Option<FileId>, Invariant> {
        let normal_path = self.normalize_path(mixed_path);
        let opts = LookupOpts {
            follow_leaf,
            ..LookupOpts::hot()
        };
        Ok(match self.lookup_ro(&normal_path, opts)? {
            LookupOut::Found { node, .. } if !self.nodes.is_directory(node) => Some(node),
            _ => None,
        })
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
        // Any change can move or remove an interned directory.
        self.dirs.get_mut().clear();
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
        )?;
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
        // Any change can move or remove an interned directory.
        self.dirs.get_mut().clear();
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
                )?;
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
        // Any change can move or remove an interned directory.
        self.dirs.get_mut().clear();
        let lookup = self.lookup_rw(
            canonical_path,
            LookupOpts {
                follow_leaf: false,
                make_directories: true,
                ..Default::default()
            },
        )?;
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
    pub fn remove(&mut self, canonical_path: &str) -> Result<(), FileStoreError> {
        // Any change can move or remove an interned directory.
        self.dirs.get_mut().clear();
        let normal_path = self.normalize_path(canonical_path);
        Ok(self.remove_normal_path(&normal_path)?)
    }

    fn remove_normal_path(&mut self, normal_path: &str) -> Result<(), Invariant> {
        let result = self.lookup_ro(
            normal_path,
            LookupOpts {
                follow_leaf: false,
                ..LookupOpts::hot()
            },
        )?;
        let LookupOut::Found {
            parent_node,
            canonical_path,
            node,
            ..
        } = result
        else {
            return Ok(());
        };

        if self.nodes.is_directory(node) && self.nodes.dir_size(node) > 0 {
            for basename in self.nodes.child_names(node) {
                self.remove_normal_path(&format!("{canonical_path}/{basename}"))?;
            }
            // Removing the last file will delete this directory
            return Ok(());
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
                self.remove_normal_path(&path::dirname(&canonical_path))?;
            }
        }
        Ok(())
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
        // Any change can move or remove an interned directory.
        self.dirs.get_mut().clear();
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
            self.remove(p)?;
        }
        for p in &batch.removed_directories {
            self.remove(p)?;
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
    ) -> Result<Option<(String, String)>, Invariant> {
        let mut ancestors_of_input: Vec<Ancestor> = Vec::new();
        let normal_path = self.normalize_path(mixed_start_path);
        let closest_lookup = self.lookup_ro(
            &normal_path,
            LookupOpts {
                collect_ancestors: Some(&mut ancestors_of_input),
                ..Default::default()
            },
        )?;

        match &closest_lookup {
            LookupOut::Found {
                node,
                canonical_path,
                ..
            } if self.nodes.is_directory(*node) => {
                if let Some(abs) =
                    self.check_candidate_has_subpath(canonical_path, subpath, subpath_type_dir)?
                {
                    return Ok(Some((abs, String::new())));
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
                        return Ok(None);
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
                if !self.nodes.is_directory(*node) {
                    return Err(Invariant(
                        "ancestors of the root must be directories".to_string(),
                    ));
                }
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
                return Ok(None);
            }
            // (The JS also passes a `start` here, which `#checkCandidateHasSubpath`
            // never forwards to the lookup.)
            if let Some(abs) =
                self.check_candidate_has_subpath(&candidate.normal_path, subpath, subpath_type_dir)?
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
                return Ok(Some((abs, container_relative_path)));
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
                self.check_candidate_has_subpath(&candidate_normal_path, subpath, subpath_type_dir)?
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
                return Ok(Some((abs, relative_parts.join("/"))));
            }
            depth_below_common_root += 1;
            candidate_normal_path = if candidate_normal_path.is_empty() {
                "..".to_string()
            } else {
                candidate_normal_path + "/.."
            };
            next_node = self.nodes.child(node, "..");
        }
        Ok(None)
    }

    /// `#checkCandidateHasSubpath`.
    fn check_candidate_has_subpath(
        &self,
        normal_candidate_path: &str,
        subpath: &str,
        subpath_type_dir: bool,
    ) -> Result<Option<String>, Invariant> {
        let joined = self
            .path_utils
            .join_normal_to_relative(normal_candidate_path, subpath)?
            .normal_path;
        Ok(match self.lookup_ro(&joined, LookupOpts::hot())? {
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
        })
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
