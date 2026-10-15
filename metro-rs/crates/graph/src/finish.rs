//! The end of a traversal: encode what changed (see `wire`), then make the
//! state the one the next traversal starts from.

use crate::wire;
use crate::{Core, CtxRef, Fail, Id, Phase, Src, R};
use std::collections::HashSet;

fn push_list(out: &mut Vec<u32>, ids: &[Id]) {
    out.push(ids.len() as u32);
    out.extend_from_slice(ids);
}

fn dedupe(ids: &[Id]) -> Vec<Id> {
    let mut seen = HashSet::new();
    ids.iter().copied().filter(|i| seen.insert(*i)).collect()
}

fn push_counts(out: &mut Vec<u32>, content: &[(Id, u32)]) {
    out.push(content.len() as u32);
    for (p, c) in content {
        out.push(*p);
        out.push(*c);
    }
}

fn src_word(s: Src) -> u32 {
    (s == Src::Upd) as u32
}

impl Core {
    pub(crate) fn finish(&mut self, result: R<()>, ph: &Phase, modified: &[Id]) -> Vec<u32> {
        let (status, arg) = match result {
            Ok(()) => (wire::STATUS_OK, 0),
            Err(Fail::Module(id)) => (wire::STATUS_MODULE, id),
            Err(Fail::NullThrows) => (wire::STATUS_NULL_THROWS, 0),
            Err(Fail::Invariant(kind)) => (wire::STATUS_INVARIANT, kind),
            Err(Fail::Depth) => (wire::STATUS_DEPTH, 0),
        };
        // After an error that was not rolled back, the JavaScript graph is
        // left half way through; this one cannot reproduce that.
        let has_state = result.is_ok() || self.t.rolled_back;
        let mut out = vec![status, arg, has_state as u32];
        if has_state {
            self.encode_state(&mut out, ph, modified);
            self.cleanup();
        }
        out
    }

    fn descriptor(&self, out: &mut Vec<u32>, id: Id, o: usize, in_place: bool) -> bool {
        let ob = self.obj(o);
        let kind = if ob.orig {
            if ob.dirty {
                1
            } else {
                0
            }
        } else {
            2
        };
        if in_place && kind == 0 {
            return false;
        }
        out.push(id);
        out.push(kind);
        let edges = |out: &mut Vec<u32>| {
            out.push(ob.edges.len() as u32);
            for (k, e) in ob.edges.iter() {
                out.push(k);
                out.push(src_word(e.src));
            }
        };
        match kind {
            0 => {}
            1 => edges(out),
            _ => {
                let inv = self.inv(ob.inv);
                out.push(src_word(ob.fields));
                out.push(inv.pre as u32);
                edges(out);
                if !inv.pre {
                    push_counts(out, &inv.content());
                }
            }
        }
        true
    }

    fn encode_state(&self, out: &mut Vec<u32>, ph: &Phase, modified: &[Id]) {
        // graph.dependencies
        push_list(out, &dedupe(&self.t.removed_modules));
        let mut in_place: Vec<(Id, usize)> = Vec::new();
        let mut fresh: Vec<(u64, Id, usize)> = Vec::new();
        for id in dedupe(&self.t.touched_mod_ids) {
            let (Some(&o), Some(seq)) = (self.modules.get(&id), self.modules.seq(&id)) else {
                continue;
            };
            if seq >= self.t.mod_seq0 {
                fresh.push((seq, id, o));
            } else {
                in_place.push((id, o));
            }
        }
        fresh.sort_by_key(|f| f.0);
        let mut body = Vec::new();
        let mut n = 0;
        for (id, o) in &in_place {
            if self.descriptor(&mut body, *id, *o, true) {
                n += 1;
            }
        }
        out.push(n);
        out.extend(body);
        out.push(fresh.len() as u32);
        for (_, id, o) in &fresh {
            self.descriptor(out, *id, *o, false);
        }

        // Inverse dependencies of modules that were already in the graph.
        let mut body = Vec::new();
        let mut n = 0;
        let mut seen = HashSet::new();
        for &i in &self.t.touched_invs {
            if !seen.insert(i) {
                continue;
            }
            let Some(inv) = self.invs[i].as_ref() else {
                continue;
            };
            let attached = self
                .modules
                .get(&inv.owner)
                .is_some_and(|&o| self.obj(o).inv == i);
            let now = inv.content();
            if attached && inv.initial.as_ref() != Some(&now) {
                body.push(inv.owner);
                push_counts(&mut body, &now);
                n += 1;
            }
        }
        out.push(n);
        out.extend(body);

        // Import bundle nodes
        out.push(self.t.import_cleared as u32);
        let touched = dedupe(&self.t.import_touched);
        out.push(touched.len() as u32);
        for id in touched {
            out.push(id);
            match self.import_nodes.get(&id) {
                Some(&i) => {
                    out.push(1);
                    push_counts(out, &self.inv(i).content());
                }
                None => out.push(0),
            }
        }

        // Resolved contexts
        push_list(out, &dedupe(&self.t.removed_ctx));
        let mut in_place = Vec::new();
        let mut appended = Vec::new();
        for id in dedupe(&self.t.touched_ctx_ids) {
            let (Some(&r), Some(seq)) = (self.ctxs.get(&id), self.ctxs.seq(&id)) else {
                continue;
            };
            if seq >= self.t.ctx_seq0 {
                appended.push((seq, id, r));
            } else if r != CtxRef::Pre {
                in_place.push((0, id, r));
            }
        }
        appended.sort_by_key(|a| a.0);
        for list in [&in_place, &appended] {
            out.push(list.len() as u32);
            for (_, id, r) in list.iter() {
                out.push(*id);
                match r {
                    CtxRef::Pre => out.extend([0, 0, 0]),
                    CtxRef::Upd(m, k) => out.extend([1, *m, *k]),
                }
            }
        }

        // The result
        let added: Vec<Id> = self.added.iter().map(|(k, _)| k).collect();
        let deleted: Vec<Id> = self.deleted.iter().map(|(k, _)| k).collect();
        let modified: Vec<Id> = modified
            .iter()
            .copied()
            .filter(|m| {
                ph.touched.contains(m) && !self.deleted.contains(m) && !self.added.contains(m)
            })
            .collect();
        push_list(out, &added);
        push_list(out, &deleted);
        push_list(out, &modified);
    }

    /// Makes the state the one the next traversal starts from: everything that
    /// is in the graph now has a JavaScript object behind it.
    fn cleanup(&mut self) {
        for i in std::mem::take(&mut self.t.touched_invs) {
            if let Some(inv) = self.invs[i].as_mut() {
                inv.initial = None;
            }
        }
        let mut seen = HashSet::new();
        for o in std::mem::take(&mut self.t.cand_objs) {
            if !seen.insert(o) {
                continue;
            }
            let Some(ob) = self.objs[o].as_ref() else {
                continue;
            };
            let attached = self.modules.get(&ob.path) == Some(&o);
            if attached {
                let ob = self.obj_mut(o);
                ob.orig = true;
                ob.dirty = false;
                ob.fields = Src::Pre;
                ob.edges.for_each_mut(|_, e| e.src = Src::Pre);
                let inv = ob.inv;
                self.inv_mut(inv).pre = true;
            } else {
                self.free_obj(o);
            }
        }
        for id in std::mem::take(&mut self.t.touched_ctx_ids) {
            if let Some(seq) = self.ctxs.seq(&id) {
                let _ = seq;
                self.ctxs.insert(id, CtxRef::Pre);
            }
        }
        self.modules.compact();
        self.roots.compact();
        self.ctxs.compact();
        self.t.removed_modules.clear();
        self.t.touched_mod_ids.clear();
        self.t.removed_ctx.clear();
        self.t.import_touched.clear();
        self.t.key_eq.clear();
    }
}
