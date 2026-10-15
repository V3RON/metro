//! Topology of the Delta Bundler's module graph: the bookkeeping of
//! packages/metro/src/DeltaBundler/Graph.js (the `delta-core` component of
//! the Rust port).
//!
//! This is a step-by-step port of that file's commit and garbage collection
//! code, not a redesign: the order in which it inserts, deletes and visits
//! modules decides the order of `graph.dependencies` and of the inverse
//! dependency sets, and the JavaScript side must end up with exactly the same
//! objects as the JavaScript algorithm makes. To keep that, the state here
//! mirrors the JavaScript objects one for one:
//!
//! - a module object (`Obj`) has its own ordered `dependencies` map; a commit
//!   makes a new object that replaces the old one in the graph, the way the
//!   JavaScript code makes `nextModule`, and an object that was released
//!   stays alive until the traversal ends (code may still hold it);
//! - an inverse dependency set (`Inv`, a `CountingSet`) is shared by the
//!   objects a module goes through, and is new only when the module is added;
//! - `Core::modules` mirrors `graph.dependencies` including insertion order.
//!
//! Everything is ids. JavaScript owns the real objects; after a traversal this
//! crate returns what changed (see `wire`): which objects to make, with which
//! dependencies, from which source (the transform result of this traversal, or
//! the object that was already in the graph), the final inverse dependency
//! sets of the modules that changed, and the added, modified and deleted sets.

mod finish;
mod ord;
mod paths;
#[cfg(test)]
mod tests;
pub mod wire;

pub use paths::{intern_paths, path_of};

use ord::OrdMap;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use wire::{Input, UpdatedModule};

pub type Id = u32;
pub type KeyId = u32;

/// "No path": an optional dependency that did not resolve.
pub const NONE: u32 = u32::MAX;
/// `asyncType` codes of an edge.
pub const ASY_NONE: u8 = 0;
pub const ASY_WEAK: u8 = 1;

/// Deepest recursion (modules above each other in a chain) this crate
/// attempts, with `BIG_STACK` bytes of stack for it (a frame is at most about
/// 1.2 KB). JavaScript runs out of stack at a few thousand modules; here a
/// deeper graph fails with `Fail::Depth` instead of overflowing the stack.
const MAX_DEPTH: u32 = 50_000;
const BIG_STACK: usize = 256 << 20;
/// When no thread for the big stack can be made, traverse on the caller's
/// stack, which may be as small as 1 MB.
const SMALL_DEPTH: u32 = 500;

/// Where the JavaScript object behind a value comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Src {
    /// The module object that was in the graph when the traversal started.
    Pre,
    /// The transform result of this traversal (`updatedModuleData`).
    Upd,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Edge {
    /// Absolute path of the target, `NONE` if unresolved.
    pub target: Id,
    pub asy: u8,
    /// Identity of the `require.context` parameters, 0 for none.
    pub sig: u32,
    /// Where the `Dependency` object of this edge comes from.
    pub src: Src,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CtxRef {
    /// The value in `#resolvedContexts` at the start of the traversal.
    Pre,
    /// `updatedModuleData.get(module).resolvedContexts.get(key)`.
    Upd(Id, KeyId),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Color {
    Black,
    Gray,
    White,
    Purple,
}

/// Why a traversal stopped.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Fail {
    /// `delta.errors.get(path)` is thrown (after the graph was rolled back).
    Module(Id),
    /// `nullthrows` on a missing value.
    NullThrows,
    /// `invariant(false, message)`; one of the `INV_*` constants.
    Invariant(u32),
    /// Recursion deeper than `MAX_DEPTH`.
    Depth,
}

type R<T> = Result<T, Fail>;

pub const INV_ROLLBACK: u32 = 0;
pub const INV_DEPENDENCIES: u32 = 1;
pub const INV_LAZY_IMPORT: u32 = 2;

/// A module's data in a `Phase`: from the transform result, or a snapshot.
pub(crate) struct ModData {
    fields: Src,
    key_null: bool,
    edges: Vec<(KeyId, Edge)>,
    index: HashMap<KeyId, usize>,
    ctxs: HashMap<KeyId, CtxRef>,
}

impl ModData {
    fn new(
        fields: Src,
        key_null: bool,
        edges: Vec<(KeyId, Edge)>,
        ctxs: HashMap<KeyId, CtxRef>,
    ) -> Self {
        let index = edges.iter().enumerate().map(|(i, e)| (e.0, i)).collect();
        ModData {
            fields,
            key_null,
            edges,
            index,
            ctxs,
        }
    }
    fn get(&self, key: KeyId) -> Option<Edge> {
        self.index.get(&key).map(|&i| self.edges[i].1)
    }
}

/// The JavaScript `Delta` object of one commit pass.
pub(crate) struct Phase {
    updated: HashMap<Id, Rc<ModData>>,
    base: HashMap<Id, Rc<ModData>>,
    touched: HashSet<Id>,
    errors: HashSet<Id>,
}

pub(crate) struct Obj {
    path: Id,
    edges: OrdMap<KeyId, Edge>,
    inv: usize,
    fields: Src,
    key_null: bool,
    /// A JavaScript object for it exists (from before this traversal).
    orig: bool,
    /// An original object whose `dependencies` changed.
    dirty: bool,
}

pub(crate) struct Inv {
    map: OrdMap<Id, u32>,
    /// A JavaScript `CountingSet` for it exists (from before this traversal).
    pre: bool,
    owner: Id,
    refs: u32,
    /// The content at the first change in this traversal (`pre` sets only).
    initial: Option<Vec<(Id, u32)>>,
}

impl Inv {
    fn content(&self) -> Vec<(Id, u32)> {
        self.map.iter().map(|(k, v)| (k, *v)).collect()
    }
}

#[derive(Default)]
pub(crate) struct Trav {
    lazy: bool,
    shallow: bool,
    depth: u32,
    max_depth: u32,
    rolled_back: bool,
    mod_seq0: u64,
    ctx_seq0: u64,
    removed_modules: Vec<Id>,
    removed_ctx: Vec<Id>,
    touched_mod_ids: Vec<Id>,
    touched_ctx_ids: Vec<Id>,
    cand_objs: Vec<usize>,
    touched_invs: Vec<usize>,
    import_touched: Vec<Id>,
    import_cleared: bool,
    key_eq: HashMap<Id, bool>,
}

pub struct Core {
    entries: Vec<Id>,
    entry_set: HashSet<Id>,
    objs: Vec<Option<Obj>>,
    free_objs: Vec<usize>,
    invs: Vec<Option<Inv>>,
    free_invs: Vec<usize>,
    /// `graph.dependencies`.
    modules: OrdMap<Id, usize>,
    color: HashMap<Id, Color>,
    roots: OrdMap<Id, ()>,
    /// `#resolvedContexts`.
    ctxs: OrdMap<Id, CtxRef>,
    /// `#importBundleNodes`: the inverse dependencies of each.
    import_nodes: HashMap<Id, usize>,
    /// `delta.added` and `delta.deleted` of the traversal in progress.
    added: OrdMap<Id, ()>,
    deleted: OrdMap<Id, ()>,
    t: Trav,
    max_depth: u32,
}

impl Core {
    pub fn new(entries: Vec<Id>) -> Self {
        Core {
            entry_set: entries.iter().copied().collect(),
            entries,
            objs: Vec::new(),
            free_objs: Vec::new(),
            invs: Vec::new(),
            free_invs: Vec::new(),
            modules: OrdMap::new(),
            color: HashMap::new(),
            roots: OrdMap::new(),
            ctxs: OrdMap::new(),
            import_nodes: HashMap::new(),
            added: OrdMap::new(),
            deleted: OrdMap::new(),
            t: Trav::default(),
            max_depth: MAX_DEPTH,
        }
    }

    /// Number of modules in the graph.
    pub fn module_count(&self) -> usize {
        self.modules.len()
    }

    // ------------------------------------------------------------ arenas

    fn obj(&self, o: usize) -> &Obj {
        self.objs[o].as_ref().expect("live object")
    }
    fn obj_mut(&mut self, o: usize) -> &mut Obj {
        self.objs[o].as_mut().expect("live object")
    }
    fn inv(&self, i: usize) -> &Inv {
        self.invs[i].as_ref().expect("live inverse set")
    }
    fn inv_mut(&mut self, i: usize) -> &mut Inv {
        self.invs[i].as_mut().expect("live inverse set")
    }

    fn alloc_inv(&mut self, owner: Id) -> usize {
        let inv = Inv {
            map: OrdMap::new(),
            pre: false,
            owner,
            refs: 0,
            initial: None,
        };
        if let Some(i) = self.free_invs.pop() {
            self.invs[i] = Some(inv);
            i
        } else {
            self.invs.push(Some(inv));
            self.invs.len() - 1
        }
    }

    fn alloc_obj(&mut self, obj: Obj) -> usize {
        self.inv_mut(obj.inv).refs += 1;
        let o = if let Some(o) = self.free_objs.pop() {
            self.objs[o] = Some(obj);
            o
        } else {
            self.objs.push(Some(obj));
            self.objs.len() - 1
        };
        self.t.cand_objs.push(o);
        o
    }

    fn free_obj(&mut self, o: usize) {
        if let Some(obj) = self.objs[o].take() {
            let inv = self.inv_mut(obj.inv);
            inv.refs -= 1;
            if inv.refs == 0 {
                self.invs[obj.inv] = None;
                self.free_invs.push(obj.inv);
            }
            self.free_objs.push(o);
        }
    }

    // ------------------------------------------------- mirrored JS state

    fn mods_set(&mut self, path: Id, o: usize) {
        if let Some(&old) = self.modules.get(&path) {
            if old != o {
                self.t.cand_objs.push(old);
            }
        }
        self.modules.insert(path, o);
        self.t.touched_mod_ids.push(path);
        self.t.cand_objs.push(o);
    }

    fn mods_remove(&mut self, path: Id) {
        if let Some(seq) = self.modules.seq(&path) {
            if seq < self.t.mod_seq0 {
                self.t.removed_modules.push(path);
            }
            if let Some(&old) = self.modules.get(&path) {
                self.t.cand_objs.push(old);
            }
            self.modules.remove(&path);
        }
    }

    fn ctx_set(&mut self, path: Id, r: CtxRef) {
        self.ctxs.insert(path, r);
        self.t.touched_ctx_ids.push(path);
    }

    fn ctx_remove(&mut self, path: Id) {
        if let Some(seq) = self.ctxs.seq(&path) {
            if seq < self.t.ctx_seq0 {
                self.t.removed_ctx.push(path);
            }
            self.ctxs.remove(&path);
        }
    }

    fn edges_remove(&mut self, o: usize, key: KeyId) {
        let ob = self.obj_mut(o);
        ob.edges.remove(&key);
        if ob.orig {
            ob.dirty = true;
            self.t.cand_objs.push(o);
        }
    }

    fn edges_set(&mut self, o: usize, key: KeyId, edge: Edge) {
        let ob = self.obj_mut(o);
        ob.edges.insert(key, edge);
        if ob.orig {
            ob.dirty = true;
            self.t.cand_objs.push(o);
        }
    }

    fn inv_touch(&mut self, i: usize) {
        let inv = self.inv_mut(i);
        if inv.pre && inv.initial.is_none() {
            inv.initial = Some(inv.content());
            self.t.touched_invs.push(i);
        }
    }

    fn inv_add(&mut self, i: usize, parent: Id) {
        self.inv_touch(i);
        let inv = self.inv_mut(i);
        let count = inv.map.get(&parent).copied().unwrap_or(0) + 1;
        inv.map.insert(parent, count);
    }

    fn inv_delete(&mut self, i: usize, parent: Id) {
        self.inv_touch(i);
        let inv = self.inv_mut(i);
        let count = inv.map.get(&parent).copied().unwrap_or(0) as i64 - 1;
        if count <= 0 {
            inv.map.remove(&parent);
        } else {
            inv.map.insert(parent, count as u32);
        }
    }

    fn inv_len(&self, o: usize) -> usize {
        self.inv(self.obj(o).inv).map.len()
    }

    fn nullthrows_module(&self, path: Id) -> R<usize> {
        self.modules.get(&path).copied().ok_or(Fail::NullThrows)
    }

    fn nullthrows_color(&self, path: Id) -> R<Color> {
        self.color.get(&path).copied().ok_or(Fail::NullThrows)
    }

    // ------------------------------------------------------ option checks

    fn is_weak_or_lazy(&self, e: &Edge) -> bool {
        e.asy == ASY_WEAK || (e.asy != ASY_NONE && self.t.lazy)
    }

    fn deps_equal(&self, a: &Edge, b: &Edge) -> bool {
        a.target == b.target && (!self.t.lazy || a.asy == b.asy) && a.sig == b.sig
    }

    /// `transformOutputMayDiffer(a, b)` for two objects of `path`.
    fn output_may_differ(&self, a: usize, b: usize, path: Id) -> bool {
        let (a, b) = (self.obj(a), self.obj(b));
        if a.key_null {
            return true;
        }
        if a.fields == b.fields {
            return false;
        }
        !self.t.key_eq.get(&path).copied().unwrap_or(false)
    }

    fn enter(&mut self) -> R<()> {
        self.t.depth += 1;
        if self.t.depth > self.t.max_depth {
            self.t.depth -= 1;
            return Err(Fail::Depth);
        }
        Ok(())
    }

    // ------------------------------------------------------ commit

    fn snapshot(&self, o: usize) -> ModData {
        let ob = self.obj(o);
        let edges: Vec<(KeyId, Edge)> = ob.edges.iter().map(|(k, e)| (k, *e)).collect();
        let mut ctxs = HashMap::new();
        for (key, edge) in &edges {
            if edge.target != NONE {
                if let Some(c) = self.ctxs.get(&edge.target) {
                    ctxs.insert(*key, *c);
                }
            }
        }
        ModData::new(ob.fields, ob.key_null, edges, ctxs)
    }

    fn commit_module(&mut self, ph: &mut Phase, path: Id, only_remove: bool) -> R<usize> {
        self.enter()?;
        let r = self.commit_module_inner(ph, path, only_remove);
        self.t.depth -= 1;
        r
    }

    fn commit_module_inner(&mut self, ph: &mut Phase, path: Id, only_remove: bool) -> R<usize> {
        if ph.errors.contains(&path) {
            return Err(Fail::Module(path));
        }
        let prev = self.modules.get(&path).copied();
        let cur = ph
            .updated
            .get(&path)
            .or_else(|| ph.base.get(&path))
            .cloned()
            .ok_or(Fail::NullThrows)?;

        let prev_edges: Vec<(KeyId, Edge)> = match prev {
            Some(p) => self.obj(p).edges.iter().map(|(k, e)| (k, *e)).collect(),
            None => Vec::new(),
        };
        let prev_index: HashMap<KeyId, Edge> = prev_edges.iter().copied().collect();

        let inv = match prev {
            Some(p) => self.obj(p).inv,
            None => self.alloc_inv(path),
        };
        let mut edges = OrdMap::new();
        for (k, e) in &prev_edges {
            edges.insert(*k, *e);
        }
        let next = self.alloc_obj(Obj {
            path,
            edges,
            inv,
            fields: cur.fields,
            key_null: cur.key_null,
            orig: false,
            dirty: false,
        });

        // Update the module information.
        self.mods_set(path, next);

        if prev.is_none() {
            if self.deleted.contains(&path) {
                self.deleted.remove(&path);
            } else {
                self.added.insert(path, ());
            }
        }

        // Diff dependencies (1/3): remove dependencies that have changed or
        // been removed.
        let mut dependencies_removed = false;
        for (key, prev_dep) in &prev_edges {
            let removed = match cur.get(*key) {
                None => true,
                Some(c) => !self.deps_equal(prev_dep, &c),
            };
            if removed {
                dependencies_removed = true;
                self.remove_dependency(ph, next, *key, *prev_dep)?;
            }
        }

        // Diff dependencies (2/3): add dependencies that have changed or been
        // added.
        let mut dependencies_added = false;
        if !only_remove {
            for (key, cur_dep) in &cur.edges {
                let added = match prev_index.get(key) {
                    None => true,
                    Some(p) => !self.deps_equal(p, cur_dep),
                };
                if added {
                    dependencies_added = true;
                    let ctx = cur.ctxs.get(key).copied();
                    self.add_dependency(ph, next, *key, *cur_dep, ctx)?;
                }
            }
        }

        // Diff dependencies (3/3): detect changes in the ordering of keys.
        let keys_changed = cur.edges.len() != prev_edges.len()
            || cur
                .edges
                .iter()
                .zip(prev_edges.iter())
                .any(|(c, p)| c.0 != p.0);

        if let Some(p) = prev {
            if !self.output_may_differ(p, next, path)
                && !dependencies_removed
                && !dependencies_added
                && !keys_changed
            {
                // We have not operated on next, so restore the previous one.
                self.mods_set(path, p);
                return Ok(p);
            }
        }

        ph.touched.insert(path);

        if only_remove {
            return Ok(next);
        }

        if self.obj(next).edges.len() != cur.edges.len() {
            return Err(Fail::Invariant(INV_DEPENDENCIES));
        }

        // Replace dependencies with the correctly-ordered version.
        let mut ordered = OrdMap::new();
        for (k, e) in &cur.edges {
            ordered.insert(*k, *e);
        }
        self.obj_mut(next).edges = ordered;
        Ok(next)
    }

    fn add_dependency(
        &mut self,
        ph: &mut Phase,
        parent: usize,
        key: KeyId,
        dep: Edge,
        ctx: Option<CtxRef>,
    ) -> R<()> {
        let parent_path = self.obj(parent).path;
        let resolved = dep.target != NONE;
        if self.t.shallow {
            // Don't add a node for the module if the graph is shallow.
        } else if !resolved {
            // A missing optional dependency has no node of its own.
        } else if dep.asy == ASY_WEAK {
            // Exclude weak dependencies from the bundle.
        } else if self.t.lazy && dep.asy != ASY_NONE {
            self.incr_import_ref(dep.target, parent_path);
        } else {
            let path = dep.target;
            let module = match self.modules.get(&path).copied() {
                Some(m) => m,
                None => match self.commit_module(ph, path, false) {
                    Ok(m) => m,
                    Err(error) => {
                        // If we couldn't add this module but it was added to
                        // the graph before failing on a sub-dependency, it
                        // may be orphaned.
                        if let Some(m) = self.modules.get(&path).copied() {
                            if self.inv_len(m) > 0 {
                                self.mark_possible_cycle_root(m);
                            } else {
                                self.release_module(ph, m)?;
                            }
                        }
                        return Err(error);
                    }
                },
            };
            let inv = self.obj(module).inv;
            self.inv_add(inv, parent_path);
            let module_path = self.obj(module).path;
            self.color.insert(module_path, Color::Black);
        }

        if resolved {
            match ctx {
                Some(c) => self.ctx_set(dep.target, c),
                None => self.ctx_remove(dep.target),
            }
        }

        // Update the parent's dependency map.
        self.edges_set(parent, key, dep);
        Ok(())
    }

    fn remove_dependency(&mut self, ph: &mut Phase, parent: usize, key: KeyId, dep: Edge) -> R<()> {
        self.enter()?;
        let r = self.remove_dependency_inner(ph, parent, key, dep);
        self.t.depth -= 1;
        r
    }

    fn remove_dependency_inner(
        &mut self,
        ph: &mut Phase,
        parent: usize,
        key: KeyId,
        dep: Edge,
    ) -> R<()> {
        let parent_path = self.obj(parent).path;
        self.edges_remove(parent, key);

        if dep.target == NONE || dep.asy == ASY_WEAK {
            return Ok(());
        }
        let abs = dep.target;
        let module = self.modules.get(&abs).copied();

        if self.t.lazy && dep.asy != ASY_NONE {
            self.decr_import_ref(abs, parent_path)?;
        } else if let Some(m) = module {
            let inv = self.obj(m).inv;
            self.inv_delete(inv, parent_path);
        }

        let Some(m) = module else {
            return Ok(());
        };
        if self.inv_len(m) > 0 || self.entry_set.contains(&abs) {
            self.mark_possible_cycle_root(m);
        } else {
            self.release_module(ph, m)?;
        }
        Ok(())
    }

    fn incr_import_ref(&mut self, target: Id, parent_path: Id) {
        let inv = match self.import_nodes.get(&target) {
            Some(&i) => i,
            None => {
                let i = self.alloc_inv(target);
                self.inv_mut(i).refs = 1;
                self.import_nodes.insert(target, i);
                i
            }
        };
        self.t.import_touched.push(target);
        self.inv_add(inv, parent_path);
    }

    fn decr_import_ref(&mut self, target: Id, parent_path: Id) -> R<()> {
        let inv = *self.import_nodes.get(&target).ok_or(Fail::NullThrows)?;
        if !self.inv(inv).map.contains(&parent_path) {
            return Err(Fail::Invariant(INV_LAZY_IMPORT));
        }
        self.t.import_touched.push(target);
        self.inv_delete(inv, parent_path);
        if self.inv(inv).map.is_empty() {
            self.import_nodes.remove(&target);
            self.invs[inv] = None;
            self.free_invs.push(inv);
        }
        Ok(())
    }

    // ------------------------------------------------ garbage collection

    fn mark_possible_cycle_root(&mut self, m: usize) {
        let path = self.obj(m).path;
        if self.color.get(&path) != Some(&Color::Purple) {
            self.color.insert(path, Color::Purple);
            self.roots.insert(path, ());
        }
    }

    fn release_module(&mut self, ph: &mut Phase, m: usize) -> R<()> {
        self.enter()?;
        let r = self.release_module_inner(ph, m);
        self.t.depth -= 1;
        r
    }

    fn release_module_inner(&mut self, ph: &mut Phase, m: usize) -> R<()> {
        let path = self.obj(m).path;
        if !ph.updated.contains_key(&path) && !ph.base.contains_key(&path) {
            // Before releasing a module, take a snapshot of the data we might
            // need to reintroduce it to the graph later in this commit.
            let snapshot = self.snapshot(m);
            ph.base.insert(path, Rc::new(snapshot));
        }
        let mut pos = 0;
        loop {
            let next = self.obj(m).edges.next_live(&mut pos).map(|(k, e)| (k, *e));
            let Some((key, dep)) = next else { break };
            if dep.target == NONE {
                continue;
            }
            self.remove_dependency(ph, m, key, dep)?;
        }
        self.color.insert(path, Color::Black);
        self.free_module(m);
        Ok(())
    }

    fn free_module(&mut self, m: usize) {
        let path = self.obj(m).path;
        if self.added.contains(&path) {
            self.added.remove(&path);
        } else {
            self.deleted.insert(path, ());
        }
        self.mods_remove(path);
        self.roots.remove(&path);
        self.color.remove(&path);
        self.ctx_remove(path);
    }

    /// `module.dependencies` targets that count as children for the cycle
    /// collector, in order.
    fn child_targets(&self, m: usize) -> Vec<Id> {
        self.obj(m)
            .edges
            .iter()
            .filter(|(_, e)| e.target != NONE && !self.is_weak_or_lazy(e))
            .map(|(_, e)| e.target)
            .collect()
    }

    fn collect_cycles(&mut self) -> R<()> {
        // Mark recursively from roots (trial deletion).
        let mut pos = 0;
        while let Some((path, _)) = self.roots.next_live(&mut pos) {
            let module = self.nullthrows_module(path)?;
            let color = self.nullthrows_color(path)?;
            if color == Color::Purple {
                self.mark_gray(module)?;
            } else {
                self.roots.remove(&path);
                if color == Color::Black
                    && self.inv_len(module) == 0
                    && !self.entry_set.contains(&path)
                {
                    self.free_module(module);
                }
            }
        }
        // Scan recursively from roots (undo unsuccessful trial deletions).
        let mut pos = 0;
        while let Some((path, _)) = self.roots.next_live(&mut pos) {
            let module = self.nullthrows_module(path)?;
            self.scan(module)?;
        }
        // Collect recursively from roots (free unreachable cycles).
        let mut pos = 0;
        while let Some((path, _)) = self.roots.next_live(&mut pos) {
            self.roots.remove(&path);
            let module = self.nullthrows_module(path)?;
            self.collect_white(module)?;
        }
        Ok(())
    }

    fn mark_gray(&mut self, m: usize) -> R<()> {
        self.enter()?;
        let r = self.mark_gray_inner(m);
        self.t.depth -= 1;
        r
    }

    fn mark_gray_inner(&mut self, m: usize) -> R<()> {
        let path = self.obj(m).path;
        if self.nullthrows_color(path)? != Color::Gray {
            self.color.insert(path, Color::Gray);
            for t in self.child_targets(m) {
                let child = self.nullthrows_module(t)?;
                let inv = self.obj(child).inv;
                self.inv_delete(inv, path);
                self.mark_gray(child)?;
            }
        }
        Ok(())
    }

    fn scan(&mut self, m: usize) -> R<()> {
        self.enter()?;
        let r = self.scan_inner(m);
        self.t.depth -= 1;
        r
    }

    fn scan_inner(&mut self, m: usize) -> R<()> {
        let path = self.obj(m).path;
        if self.nullthrows_color(path)? == Color::Gray {
            if self.inv_len(m) > 0 || self.entry_set.contains(&path) {
                self.scan_black(m)?;
            } else {
                self.color.insert(path, Color::White);
                for t in self.child_targets(m) {
                    let child = self.nullthrows_module(t)?;
                    self.scan(child)?;
                }
            }
        }
        Ok(())
    }

    fn scan_black(&mut self, m: usize) -> R<()> {
        self.enter()?;
        let r = self.scan_black_inner(m);
        self.t.depth -= 1;
        r
    }

    fn scan_black_inner(&mut self, m: usize) -> R<()> {
        let path = self.obj(m).path;
        self.color.insert(path, Color::Black);
        for t in self.child_targets(m) {
            let child = self.nullthrows_module(t)?;
            let inv = self.obj(child).inv;
            self.inv_add(inv, path);
            let child_path = self.obj(child).path;
            if self.nullthrows_color(child_path)? != Color::Black {
                self.scan_black(child)?;
            }
        }
        Ok(())
    }

    fn collect_white(&mut self, m: usize) -> R<()> {
        self.enter()?;
        let r = self.collect_white_inner(m);
        self.t.depth -= 1;
        r
    }

    fn collect_white_inner(&mut self, m: usize) -> R<()> {
        let path = self.obj(m).path;
        let color = self.nullthrows_color(path)?;
        if color == Color::White && !self.roots.contains(&path) {
            self.color.insert(path, Color::Black);
            let targets: Vec<Id> = self
                .obj(m)
                .edges
                .iter()
                .filter(|(_, e)| e.target != NONE)
                .map(|(_, e)| e.target)
                .collect();
            for t in targets {
                // The child may already have been collected.
                if let Some(child) = self.modules.get(&t).copied() {
                    self.collect_white(child)?;
                }
            }
            self.free_module(m);
        }
        Ok(())
    }

    // -------------------------------------------------------- traversals

    /// Runs a traversal on a thread with a large stack: the recursion is as
    /// deep as the longest chain of imports.
    fn on_big_stack(&mut self, input: &Input, initial: bool) -> Vec<u32> {
        let run = |core: &mut Core| {
            if initial {
                core.initial_traversal(input)
            } else {
                core.incremental_traversal(input)
            }
        };
        let spawned = std::thread::scope(|scope| {
            let core = &mut *self;
            std::thread::Builder::new()
                .stack_size(BIG_STACK)
                .spawn_scoped(scope, move || run(core))
                .map(|handle| handle.join())
        });
        match spawned {
            Ok(Ok(out)) => out,
            Ok(Err(panic)) => std::panic::resume_unwind(panic),
            Err(_) => {
                self.max_depth = SMALL_DEPTH;
                let out = run(self);
                self.max_depth = MAX_DEPTH;
                out
            }
        }
    }

    fn begin(&mut self, input: &Input) -> Phase {
        self.t = Trav {
            lazy: input.lazy,
            shallow: input.shallow,
            max_depth: self.max_depth,
            mod_seq0: self.modules.mark(),
            ctx_seq0: self.ctxs.mark(),
            ..Trav::default()
        };
        self.added = OrdMap::new();
        self.deleted = OrdMap::new();
        let mut updated = HashMap::new();
        for u in &input.updated {
            self.t.key_eq.insert(u.id, u.key_eq);
            updated.insert(u.id, Rc::new(mod_data(u)));
        }
        Phase {
            updated,
            base: HashMap::new(),
            touched: HashSet::new(),
            errors: input.errors.iter().copied().collect(),
        }
    }

    /// `initialTraverseDependencies` after `_buildDelta` and the check for
    /// errors. The caller (JavaScript) has checked the graph is empty.
    pub fn initial(&mut self, input: &Input) -> Vec<u32> {
        self.on_big_stack(input, true)
    }

    fn initial_traversal(&mut self, input: &Input) -> Vec<u32> {
        let mut ph = self.begin(input);
        self.color.clear();
        self.roots.clear();
        let nodes: Vec<usize> = self.import_nodes.drain().map(|(_, i)| i).collect();
        for i in nodes {
            self.invs[i] = None;
            self.free_invs.push(i);
        }
        self.t.import_cleared = true;
        for p in self.entries.clone() {
            // Each entry point implicitly has a refcount of 1.
            self.color.insert(p, Color::Black);
        }
        let mut result = Ok(());
        for p in self.entries.clone() {
            if let Err(e) = self.commit_module(&mut ph, p, false) {
                result = Err(e);
                break;
            }
        }
        self.finish(result, &ph, &[])
    }

    /// `traverseDependencies` after `_buildDelta`.
    pub fn incremental(&mut self, input: &Input) -> Vec<u32> {
        self.on_big_stack(input, false)
    }

    fn incremental_traversal(&mut self, input: &Input) -> Vec<u32> {
        let mut ph = self.begin(input);
        let result = self.incremental_inner(&mut ph, &input.modified);
        self.finish(result, &ph, &input.modified)
    }

    fn incremental_inner(&mut self, ph: &mut Phase, modified: &[Id]) -> R<()> {
        // If we have errors we might need to roll back any changes - take
        // snapshots of all modified modules at the base state.
        if !ph.errors.is_empty() {
            for &m in modified {
                let o = self.nullthrows_module(m)?;
                let snapshot = self.snapshot(o);
                ph.base.insert(m, Rc::new(snapshot));
            }
        }

        // Commit changes in a subtractive pass and then an additive pass.
        for &m in modified {
            if ph.errors.contains(&m) {
                continue;
            }
            if self.modules.get(&m).is_none() {
                continue;
            }
            self.commit_module(ph, m, true)?;
        }

        // Ensure we have released any unreachable modules before the additive
        // pass.
        self.collect_cycles()?;

        // Additive pass - any errors we encounter here are rethrown after
        // rolling back the commit.
        let mut additive = Ok(());
        for &m in modified {
            if self.modules.get(&m).is_none() {
                continue;
            }
            if let Err(e) = self.commit_module(ph, m, false) {
                additive = Err(e);
                break;
            }
        }
        if let Err(error) = additive {
            // Roll back to base before re-throwing.
            let mut rollback = Phase {
                updated: std::mem::take(&mut ph.base),
                base: HashMap::new(),
                touched: HashSet::new(),
                errors: HashSet::new(),
            };
            for &m in modified {
                if self.modules.get(&m).is_none() {
                    continue;
                }
                self.commit_module(&mut rollback, m, false)?;
            }
            // Collect cycles again after rolling back.
            self.collect_cycles()?;
            // Cheap check to validate the rollback.
            if !self.added.is_empty() || !self.deleted.is_empty() {
                return Err(Fail::Invariant(INV_ROLLBACK));
            }
            self.t.rolled_back = true;
            return Err(error);
        }
        Ok(())
    }
}

fn mod_data(u: &UpdatedModule) -> ModData {
    let mut edges = Vec::with_capacity(u.edges.len());
    let mut ctxs = HashMap::new();
    for e in &u.edges {
        edges.push((
            e.key,
            Edge {
                target: e.target,
                asy: e.asy,
                sig: e.sig,
                src: Src::Upd,
            },
        ));
        if e.has_ctx {
            ctxs.insert(e.key, CtxRef::Upd(u.id, e.key));
        }
    }
    ModData::new(Src::Upd, u.key_null, edges, ctxs)
}
