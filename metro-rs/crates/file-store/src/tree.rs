//! The node arena and the core traversal, `#lookupByNormalPath`.
//!
//! TODO(windows): POSIX separators only (`path.sep` is `/`).
//!
//! OVERVIEW (from TreeFS.js):
//!
//! TreeFS is Metro's in-memory representation of the file system. It is
//! structured as a tree of non-empty maps and leaves (tuples), with the root
//! node representing the given `rootDir`, typically Metro's _project root_
//! (not a filesystem root). Map keys are path segments, and branches outside
//! the project root are accessed via `'..'`.
//!
//! For a root dir '/data/project', the file '/data/other/app/index.js' would
//! have metadata at root.get('..').get('other').get('app').get('index.js')
//!
//! SYMLINKS:
//!
//! Symlinks are represented as nodes whose metadata contains their target,
//! lexically resolved to a normal path with POSIX separators when the node was
//! populated. Lexical resolution only interprets the literal target relative to
//! the symlink's directory: it does not follow symlinks in the target, and the
//! target need not exist, so it is a normalPath, not a canonicalPath. If a
//! symlink is encountered during traversal, we restart traversal at the root
//! node targeting join(normal symlink target, remaining path suffix).
//!
//! TERMINOLOGY:
//!
//! - mixedPath: a root-relative or absolute path
//! - relativePath: a root-relative path
//! - normalPath: a root-relative, normalised path (no extraneous '.' or '..'),
//!   may have a single trailing slash
//! - canonicalPath: a root-relative, normalised, real path (no symlinks in
//!   dirname), never has a trailing slash
//!
//! Rust representation: nodes live in an arena and are addressed by [`FileId`].
//! A JS `Map` directory is `Node::Dir`; a regular file (`metadata[H.SYMLINK] ===
//! 0`) is `Node::File`; a symlink (`typeof metadata[H.SYMLINK] === 'string'`) is
//! `Node::Symlink`. Ids are never reused: a removed node becomes
//! `Node::Removed`.
//!
//! JS quirk preserved on purpose: when a lookup walks through a `'..'` segment
//! that does not exist (and does not create it), the JS makes a fresh empty
//! `Map` that is not attached to the tree. That temporary, empty, detached
//! directory can be *returned* as the result of the lookup (so `lookup('..')`
//! reports a directory even if nothing outside the root is tracked). It is
//! modelled by the special id [`FileId::PHANTOM`].

use crate::invariant::Invariant;
use crate::jsstr;
use crate::root_path_utils::RootPathUtils;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

/// Stable identifier of a node. Never reused after removal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileId(pub u32);

impl FileId {
    pub const ROOT: FileId = FileId(0);
    /// The detached empty directory described in the module docs. Never stored.
    pub(crate) const PHANTOM: FileId = FileId(u32::MAX);
}

/// FxHash, as used by rustc. Path segments are short; SipHash is wasteful here.
#[derive(Default)]
pub(crate) struct FxHasher(u64);

impl Hasher for FxHasher {
    fn write(&mut self, bytes: &[u8]) {
        const K: u64 = 0x517c_c1b7_2722_0a95;
        for chunk in bytes.chunks(8) {
            let mut v = 0u64;
            for (i, b) in chunk.iter().enumerate() {
                v |= (*b as u64) << (i * 8);
            }
            self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(K);
        }
    }
    fn write_u8(&mut self, i: u8) {
        self.write(&[i]);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

/// The children of a directory: a JS `Map<string, MixedNode>`. Iteration
/// order is insertion order, like a `Map`: setting an existing key keeps its
/// position, deleting a key and setting it again moves it to the end. Every
/// observable listing (`getAllFiles`, `matchFiles`, `metadataIterator`, the
/// order of `removedFiles`) depends on this order.
#[derive(Default)]
pub(crate) struct Children {
    map: HashMap<Box<str>, (FileId, u32), BuildHasherDefault<FxHasher>>,
    next_seq: u32,
}

impl Children {
    pub(crate) fn get(&self, name: &str) -> Option<FileId> {
        self.map.get(name).map(|(id, _)| *id)
    }

    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }

    /// `map.set(name, id)`: returns the replaced id. A replaced key keeps its
    /// position.
    pub(crate) fn insert(&mut self, name: &str, id: FileId) -> Option<FileId> {
        if let Some(entry) = self.map.get_mut(name) {
            return Some(std::mem::replace(&mut entry.0, id));
        }
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        self.map.insert(name.into(), (id, seq));
        None
    }

    pub(crate) fn remove(&mut self, name: &str) -> Option<FileId> {
        self.map.remove(name).map(|(id, _)| id)
    }

    /// `map.entries()`, in insertion order.
    pub(crate) fn ordered(&self) -> Vec<(&str, FileId)> {
        let mut entries: Vec<(&str, FileId, u32)> = self
            .map
            .iter()
            .map(|(k, (id, seq))| (&**k, *id, *seq))
            .collect();
        entries.sort_unstable_by_key(|e| e.2);
        entries.into_iter().map(|(k, id, _)| (k, id)).collect()
    }

    pub(crate) fn into_ids(self) -> impl Iterator<Item = FileId> {
        self.map.into_values().map(|(id, _)| id)
    }
}

pub(crate) enum Node {
    Dir(Children),
    File,
    /// The stored target: a normal path with POSIX separators.
    Symlink(Box<str>),
    /// A symlink whose target has not been read yet (`metadata[H.SYMLINK] ===
    /// 1`). Traversing through it fails the JS `invariant`.
    UnresolvedSymlink,
    Removed,
}

pub(crate) struct Nodes {
    arena: Vec<Node>,
}

impl Nodes {
    pub(crate) fn new() -> Self {
        Self {
            arena: vec![Node::Dir(Children::default())],
        }
    }

    pub(crate) fn get(&self, id: FileId) -> &Node {
        &self.arena[id.0 as usize]
    }

    pub(crate) fn get_mut(&mut self, id: FileId) -> &mut Node {
        &mut self.arena[id.0 as usize]
    }

    pub(crate) fn alloc(&mut self, node: Node) -> FileId {
        // Unreachable in practice: it takes 4 billion nodes. Not a JS
        // invariant, and only mutations allocate, so it stays a panic.
        let id = FileId(u32::try_from(self.arena.len()).expect("FileId space exhausted"));
        assert!(id != FileId::PHANTOM, "FileId space exhausted");
        self.arena.push(node);
        id
    }

    /// `isDirectory(node)`.
    pub(crate) fn is_directory(&self, id: FileId) -> bool {
        id == FileId::PHANTOM || matches!(self.get(id), Node::Dir(_))
    }

    /// `isRegularFile(node)`.
    pub(crate) fn is_regular_file(&self, id: FileId) -> bool {
        id != FileId::PHANTOM && matches!(self.get(id), Node::File)
    }

    /// `directory.get(name)`.
    pub(crate) fn child(&self, dir: FileId, name: &str) -> Option<FileId> {
        if dir == FileId::PHANTOM {
            return None;
        }
        match self.get(dir) {
            Node::Dir(c) => c.get(name),
            _ => None,
        }
    }

    /// `directory.size`.
    pub(crate) fn dir_size(&self, dir: FileId) -> usize {
        if dir == FileId::PHANTOM {
            return 0;
        }
        match self.get(dir) {
            Node::Dir(c) => c.len(),
            _ => 0,
        }
    }

    /// `directory.keys()`, collected.
    pub(crate) fn child_names(&self, dir: FileId) -> Vec<String> {
        match self.get(dir) {
            Node::Dir(c) => c
                .ordered()
                .into_iter()
                .map(|(k, _)| k.to_string())
                .collect(),
            _ => Vec::new(),
        }
    }

    /// `directory.entries()`, collected, in insertion order. Empty for the
    /// detached directory and for non-directories.
    pub(crate) fn ordered_children(&self, dir: FileId) -> Vec<(&str, FileId)> {
        if dir == FileId::PHANTOM {
            return Vec::new();
        }
        match self.get(dir) {
            Node::Dir(c) => c.ordered(),
            _ => Vec::new(),
        }
    }

    /// `directory.delete(name)`; the removed node is tombstoned.
    pub(crate) fn delete_child(&mut self, dir: FileId, name: &str) {
        if dir == FileId::PHANTOM {
            return;
        }
        let removed = match self.get_mut(dir) {
            Node::Dir(c) => c.remove(name),
            _ => None,
        };
        if let Some(id) = removed {
            self.drop_subtree(id);
        }
    }

    /// `directory.set(name, node)`: replaces whatever was there (its subtree,
    /// if any, becomes unreachable).
    pub(crate) fn set_child(&mut self, dir: FileId, name: &str, id: FileId) {
        if dir == FileId::PHANTOM {
            return;
        }
        let replaced = match self.get_mut(dir) {
            Node::Dir(c) => c.insert(name, id),
            _ => None,
        };
        if let Some(old) = replaced {
            if old != id {
                self.drop_subtree(old);
            }
        }
    }

    pub(crate) fn drop_subtree(&mut self, root: FileId) {
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            if let Node::Dir(c) = std::mem::replace(self.get_mut(id), Node::Removed) {
                stack.extend(c.into_ids());
            }
        }
    }
}

/// Mutable or read-only access to the arena, so that one traversal serves
/// both `lookup` (`&self`) and the `makeDirectories` mutation paths.
pub(crate) trait NodeAccess {
    fn nodes(&self) -> &Nodes;
    /// Creates (and attaches) an empty directory. Only called when
    /// `make_directories` is set.
    fn make_dir(&mut self, parent: FileId, name: &str) -> FileId;
    /// `changeListener.directoryAdded(canonicalPath)`, called before the
    /// directory is created. `None`: no listener, the path is not built.
    fn directory_listener(&mut self) -> Option<&mut dyn FnMut(&str)> {
        None
    }
}

pub(crate) struct ReadOnly<'a>(pub(crate) &'a Nodes);

impl NodeAccess for ReadOnly<'_> {
    fn nodes(&self) -> &Nodes {
        self.0
    }
    fn make_dir(&mut self, _parent: FileId, _name: &str) -> FileId {
        unreachable!("make_directories requires a mutable file store")
    }
}

pub(crate) struct ReadWrite<'a>(pub(crate) &'a mut Nodes);

/// `ReadWrite` with a `changeListener.directoryAdded`.
pub(crate) struct ReadWriteListen<'a, 'l>(
    pub(crate) &'a mut Nodes,
    pub(crate) &'l mut dyn FnMut(&str),
);

impl NodeAccess for ReadWriteListen<'_, '_> {
    fn nodes(&self) -> &Nodes {
        self.0
    }
    fn make_dir(&mut self, parent: FileId, name: &str) -> FileId {
        let id = self.0.alloc(Node::Dir(Children::default()));
        self.0.set_child(parent, name, id);
        id
    }
    fn directory_listener(&mut self) -> Option<&mut dyn FnMut(&str)> {
        Some(&mut *self.1)
    }
}

impl NodeAccess for ReadWrite<'_> {
    fn nodes(&self) -> &Nodes {
        self.0
    }
    fn make_dir(&mut self, parent: FileId, name: &str) -> FileId {
        let id = self.0.alloc(Node::Dir(Children::default()));
        // `parent.set(segmentName, segmentNode)`
        self.0.set_child(parent, name, id);
        id
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Ancestor {
    pub ancestor_of_root_idx: Option<usize>,
    pub node: FileId,
    pub normal_path: String,
    /// `None`: the last segment of `normal_path` (traversed ancestors). Set for
    /// ancestors synthesised from the root's own path.
    segment_name: Option<String>,
}

impl Ancestor {
    pub(crate) fn segment_name(&self) -> &str {
        match &self.segment_name {
            Some(s) => s,
            None => self
                .normal_path
                .rsplit_once('/')
                .map_or(&self.normal_path, |(_, tail)| tail),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Start {
    pub ancestor_of_root_idx: Option<usize>,
    pub node: FileId,
    pub path_idx: usize,
}

pub(crate) struct LookupOpts<'a> {
    pub collect_ancestors: Option<&'a mut Vec<Ancestor>>,
    /// Like lstat vs stat, whether to follow a symlink at the basename of
    /// the given path, or return the details of the symlink itself.
    /// (`undefined` in the JS means follow.)
    pub follow_leaf: bool,
    /// Whether to (recursively) create missing directory nodes during
    /// traversal, useful when adding files.
    pub make_directories: bool,
    pub start: Option<Start>,
    /// Whether `LookupOut::Missing` carries `canonical_missing_path` and
    /// `missing_segment_name`. Hot callers that only test existence turn this
    /// off to skip building the strings.
    pub missing_details: bool,
}

impl LookupOpts<'_> {
    /// Defaults for callers that ignore what is missing.
    pub(crate) fn hot() -> Self {
        Self {
            missing_details: false,
            ..Self::default()
        }
    }
}

impl Default for LookupOpts<'_> {
    fn default() -> Self {
        Self {
            collect_ancestors: None,
            follow_leaf: true,
            make_directories: false,
            start: None,
            missing_details: true,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum LookupOut<'a> {
    Found {
        ancestor_of_root_idx: Option<usize>,
        /// Borrowed from the requested path unless a symlink was followed.
        canonical_path: Cow<'a, str>,
        node: FileId,
        /// `None` only for the root.
        parent_node: Option<FileId>,
    },
    Missing {
        /// Empty unless `LookupOpts::missing_details`.
        canonical_missing_path: String,
        /// Empty unless `LookupOpts::missing_details`.
        missing_segment_name: String,
    },
}

/// `target[..end]`, reusing the owned target's buffer if there is one.
fn prefix_of<'a>(owned: Option<String>, requested: &'a str, end: usize) -> Cow<'a, str> {
    match owned {
        Some(mut s) => {
            s.truncate(end);
            Cow::Owned(s)
        }
        None => Cow::Borrowed(&requested[..end]),
    }
}

/// The core traversal algorithm of TreeFS - takes a normal path and traverses
/// through a tree of maps keyed on path segments, returning the node,
/// canonical path, and other metadata if successful, or the first missing
/// segment otherwise.
///
/// When a symlink is encountered, we set a new target of the symlink's
/// normalised target path plus the remainder of the original target path. In
/// this way, the eventual target path in a successful lookup has all symlinks
/// resolved, and gives us the real path "for free". Similarly if a traversal
/// fails, we automatically have the real path of the first non-existent node.
///
/// Note that this code is extremely hot during resolution, being the most
/// expensive part of a file existence check. Until a symlink is hit it only
/// borrows the requested path: no allocation happens on the common path.
///
/// Returns `Err(Invariant)` where the JS `invariant`s throw: a symlink node
/// without a target cannot exist here, 'Failed to collapse' and 'Unexpectedly
/// escaped traversal' need a non-normal input path.
pub(crate) fn lookup_by_normal_path<'a, A: NodeAccess>(
    acc: &mut A,
    utils: &RootPathUtils,
    requested_normal_path: &'a str,
    mut opts: LookupOpts<'_>,
) -> Result<LookupOut<'a>, Invariant> {
    // We'll update the target if we hit a symlink; until then it is the
    // requested path itself.
    let mut owned_target: Option<String> = None;
    // Lazy-initialised set of seen target paths, to detect symlink cycles.
    let mut seen: Option<HashSet<String>> = None;
    // Pointer to the first character of the current path segment in
    // targetNormalPath.
    let mut from_idx = opts.start.map_or(0, |s| s.path_idx);
    // The parent of the current segment.
    let mut parent_node = opts.start.map_or(FileId::ROOT, |s| s.node);
    // If a returned node is (an ancestor of) the root, this is the number of
    // levels below the root, i.e. '' is 0, '..' is 1, '../..' is 2, otherwise
    // None. (`start?.ancestorOfRootIdx ?? 0`: a null start index becomes 0.)
    let mut ancestor_of_root_idx: Option<usize> =
        Some(opts.start.and_then(|s| s.ancestor_of_root_idx).unwrap_or(0));

    // Used only when collecting ancestors, to avoid double-counting nodes and
    // paths when traversing a symlink takes us back to rootNode and out again.
    // This tracks the first character of the first segment not already
    // collected, in UTF-16 units like the JS (compared against `fromIdx`).
    let mut unseen_path_from_idx = 0usize;

    loop {
        let target_normal_path: &str = owned_target.as_deref().unwrap_or(requested_normal_path);
        if target_normal_path.len() <= from_idx {
            break;
        }
        let next_sep_idx = target_normal_path[from_idx..]
            .find('/')
            .map(|i| i + from_idx);
        let is_last_segment = next_sep_idx.is_none();
        let seg_start = from_idx;
        let seg_end = next_sep_idx.unwrap_or(target_normal_path.len());
        let is_unseen = opts.collect_ancestors.is_none()
            || unseen_path_from_idx == 0
            || jsstr::utf16_len(&target_normal_path[..from_idx]) >= unseen_path_from_idx;
        from_idx = if is_last_segment {
            target_normal_path.len()
        } else {
            seg_end + 1
        };
        let segment_name = &target_normal_path[seg_start..seg_end];

        if segment_name == "." {
            continue;
        }

        let segment_node = match acc.nodes().child(parent_node, segment_name) {
            Some(n) => {
                // In normal paths all indirections are at the prefix, so we are at the
                // nth ancestor of the root iff the path so far is n '..' segments.
                if segment_name == ".." && ancestor_of_root_idx.is_some() {
                    ancestor_of_root_idx = ancestor_of_root_idx.map(|n| n + 1);
                } else {
                    ancestor_of_root_idx = None;
                }
                n
            }
            None => {
                if segment_name == ".." && ancestor_of_root_idx.is_some() {
                    ancestor_of_root_idx = ancestor_of_root_idx.map(|n| n + 1);
                }
                if !opts.make_directories && segment_name != ".." {
                    if !opts.missing_details {
                        return Ok(LookupOut::Missing {
                            canonical_missing_path: String::new(),
                            missing_segment_name: String::new(),
                        });
                    }
                    return Ok(LookupOut::Missing {
                        canonical_missing_path: if is_last_segment {
                            target_normal_path.to_string()
                        } else {
                            target_normal_path[..from_idx - 1].to_string()
                        },
                        missing_segment_name: segment_name.to_string(),
                    });
                }
                if opts.make_directories {
                    if let Some(listener) = acc.directory_listener() {
                        let canonical_path = if is_last_segment {
                            target_normal_path
                        } else {
                            &target_normal_path[..from_idx - 1]
                        };
                        listener(canonical_path);
                    }
                    acc.make_dir(parent_node, segment_name)
                } else {
                    // A fresh `new Map()` that is never attached to the tree.
                    FileId::PHANTOM
                }
            }
        };

        let nodes = acc.nodes();
        // We are done if...
        if
        // ...at a directory node and the only subsequent character is `/`, or
        (next_sep_idx == Some(target_normal_path.len() - 1) && nodes.is_directory(segment_node))
            // there are no subsequent `/`, and this node is anything but a symlink
            // we're required to resolve due to followLeaf.
            || (is_last_segment
                && (nodes.is_directory(segment_node)
                    || nodes.is_regular_file(segment_node)
                    || !opts.follow_leaf))
        {
            let end = if is_last_segment {
                target_normal_path.len()
            } else {
                // remove trailing `/`
                target_normal_path.len() - 1
            };
            return Ok(LookupOut::Found {
                ancestor_of_root_idx,
                canonical_path: prefix_of(owned_target, requested_normal_path, end),
                node: segment_node,
                parent_node: Some(parent_node),
            });
        }

        // If the next node is a directory, go into it
        if nodes.is_directory(segment_node) {
            parent_node = segment_node;
            if let Some(collect) = opts.collect_ancestors.as_deref_mut() {
                if is_unseen {
                    let current_path = if is_last_segment {
                        target_normal_path.to_string()
                    } else {
                        target_normal_path[..from_idx - 1].to_string()
                    };
                    collect.push(Ancestor {
                        ancestor_of_root_idx,
                        node: segment_node,
                        normal_path: current_path,
                        segment_name: None,
                    });
                }
            }
        } else {
            if nodes.is_regular_file(segment_node) {
                // Regular file in a directory path
                if !opts.missing_details {
                    return Ok(LookupOut::Missing {
                        canonical_missing_path: String::new(),
                        missing_segment_name: String::new(),
                    });
                }
                let current_path = if is_last_segment {
                    target_normal_path.to_string()
                } else {
                    target_normal_path[..from_idx - 1].to_string()
                };
                return Ok(LookupOut::Missing {
                    canonical_missing_path: current_path,
                    missing_segment_name: segment_name.to_string(),
                });
            }

            // Symlink in a directory path. Targets are stored already lexically
            // resolved to a normal path, with POSIX separators so that the
            // snapshot is portable between operating systems.
            let normal_symlink_target: String = match nodes.get(segment_node) {
                Node::Symlink(t) => t.to_string(),
                _ => {
                    return Err(Invariant(
                        "Expected symlink target to be populated.".to_string(),
                    ))
                }
            };
            let segment_name = segment_name.to_string();

            let remaining_target_path = if is_last_segment {
                ""
            } else {
                &target_normal_path[from_idx..]
            };

            // Append any subsequent path segments to the symlink target, and reset
            // with our new target.
            let joined_result =
                utils.join_normal_to_relative(&normal_symlink_target, remaining_target_path)?;

            // Two special cases (covered by unit tests):
            //
            // If the symlink target is the root, the root should be a counted as
            // an ancestor. We'd otherwise miss counting it because we normally
            // push new ancestors only when entering a directory.
            //
            // If the symlink target is an ancestor of the root *and* joining it
            // with the remaining path results in collapsing segments, e.g:
            // '../..' + 'parentofroot/root/foo.js' = 'foo.js', then we must add
            // parentofroot and root as ancestors.
            if let Some(collect) = opts.collect_ancestors.as_deref_mut() {
                if !is_last_segment
                    // No-op optimisation to bail out the common case of nothing to do.
                    && (normal_symlink_target.is_empty() || joined_result.collapsed_segments > 0)
                {
                    let mut node = FileId::ROOT;
                    let mut collapsed_path = String::new();
                    let mut reverse_ancestors = Vec::new();
                    let mut i = 0;
                    while i <= joined_result.collapsed_segments && acc.nodes().is_directory(node) {
                        reverse_ancestors.push(Ancestor {
                            ancestor_of_root_idx: Some(i),
                            node,
                            normal_path: collapsed_path.clone(),
                            segment_name: Some(utils.get_basename_of_nth_ancestor(i).to_string()),
                        });
                        // `node.get('..') ?? new Map()`
                        node = acc.nodes().child(node, "..").unwrap_or(FileId::PHANTOM);
                        collapsed_path = if collapsed_path.is_empty() {
                            "..".to_string()
                        } else {
                            collapsed_path + "/.."
                        };
                        i += 1;
                    }
                    reverse_ancestors.reverse();
                    collect.extend(reverse_ancestors);
                }
            }

            // For the purpose of collecting ancestors: Ignore the traversal to
            // the symlink target, and start collecting ancestors only
            // from the target itself (ie, the basename of the normal target path)
            // onwards.
            unseen_path_from_idx = match normal_symlink_target.rfind('/') {
                Some(i) => jsstr::utf16_len(&normal_symlink_target[..i]) + 1,
                None => 0,
            };

            let new_target = joined_result.normal_path;
            // Optimisation: set this lazily only when we've encountered a symlink
            let seen = seen.get_or_insert_with(|| {
                let mut s = HashSet::new();
                s.insert(requested_normal_path.to_string());
                s
            });
            if seen.contains(&new_target) {
                // TODO: Warn `Symlink cycle detected: ${[...seen, node].join(' -> ')}`
                return Ok(LookupOut::Missing {
                    canonical_missing_path: new_target,
                    missing_segment_name: segment_name,
                });
            }
            seen.insert(new_target.clone());
            owned_target = Some(new_target);
            from_idx = 0;
            parent_node = FileId::ROOT;
            ancestor_of_root_idx = Some(0);
        }
    }
    if parent_node != FileId::ROOT {
        return Err(Invariant("Unexpectedly escaped traversal".to_string()));
    }
    let len = owned_target
        .as_deref()
        .unwrap_or(requested_normal_path)
        .len();
    Ok(LookupOut::Found {
        ancestor_of_root_idx: Some(0),
        canonical_path: prefix_of(owned_target, requested_normal_path, len),
        node: FileId::ROOT,
        parent_node: None,
    })
}
