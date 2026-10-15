//! The flat `u32` layout in which a traversal crosses the JavaScript/Rust
//! boundary. Keep in sync with packages/metro/src/DeltaBundler/rustGraph.js.
//!
//! Input:
//! ```text
//! flags            bit 0: lazy, bit 1: shallow
//! n, n x id        modifiedPathsInBaseGraph (the entry points on the first traversal)
//! n, n x id        modules with errors
//! n, then per updated module:
//!   id, flags (bit 0: key is null, bit 1: key equals the one in the graph),
//!   m, m x (key, target or NONE, asyncType code, context signature, has context)
//! ```
//!
//! Output:
//! ```text
//! status, arg, has_state
//! if has_state:
//!   n, n x id                      modules to delete from graph.dependencies
//!   n, n x descriptor              objects that replace one at its place
//!   n, n x descriptor              objects to append, in order
//!   n, n x (owner, m, m x (parent, count))      inverse dependencies that changed
//!   cleared, n, n x (id, present, [m, m x (parent, count)])    import bundle nodes
//!   n, n x id                      resolved contexts to delete
//!   n, n x (id, kind, module, key) resolved contexts to replace in place
//!   n, n x (id, kind, module, key) resolved contexts to append, in order
//!   n, n x id                      added
//!   n, n x id                      deleted
//!   n, n x id                      modified
//! descriptor:
//!   id, 0                          the original object, unchanged
//!   id, 1, m, m x (key, src)       the original object with new dependencies
//!   id, 2, fields src, inverse set is original (0/1), m, m x (key, src),
//!     and, if the inverse set is not original: k, k x (parent, count)
//! src / kind: 0 = from the graph as it was, 1 = from the transform results
//! ```

use crate::{Id, KeyId};

pub const STATUS_OK: u32 = 0;
/// The error of this module is thrown (the graph was rolled back).
pub const STATUS_MODULE: u32 = 1;
pub const STATUS_NULL_THROWS: u32 = 2;
/// `arg` is one of the `INV_*` constants.
pub const STATUS_INVARIANT: u32 = 3;
pub const STATUS_DEPTH: u32 = 4;

pub struct InEdge {
    pub key: KeyId,
    pub target: Id,
    pub asy: u8,
    pub sig: u32,
    pub has_ctx: bool,
}

pub struct UpdatedModule {
    pub id: Id,
    pub key_null: bool,
    pub key_eq: bool,
    pub edges: Vec<InEdge>,
}

pub struct Input {
    pub lazy: bool,
    pub shallow: bool,
    pub modified: Vec<Id>,
    pub errors: Vec<Id>,
    pub updated: Vec<UpdatedModule>,
}

/// Decodes the input words; `None` if they are malformed.
pub fn decode_input(w: &[u32]) -> Option<Input> {
    let mut r = Reader { w, pos: 0 };
    let flags = r.next()?;
    let modified = r.list()?;
    let errors = r.list()?;
    let n = r.next()? as usize;
    let mut updated = Vec::with_capacity(n);
    for _ in 0..n {
        let id = r.next()?;
        let f = r.next()?;
        let m = r.next()? as usize;
        let mut edges = Vec::with_capacity(m);
        for _ in 0..m {
            edges.push(InEdge {
                key: r.next()?,
                target: r.next()?,
                asy: r.next()? as u8,
                sig: r.next()?,
                has_ctx: r.next()? != 0,
            });
        }
        updated.push(UpdatedModule {
            id,
            key_null: f & 1 != 0,
            key_eq: f & 2 != 0,
            edges,
        });
    }
    Some(Input {
        lazy: flags & 1 != 0,
        shallow: flags & 2 != 0,
        modified,
        errors,
        updated,
    })
}

struct Reader<'a> {
    w: &'a [u32],
    pos: usize,
}

impl Reader<'_> {
    fn next(&mut self) -> Option<u32> {
        let v = *self.w.get(self.pos)?;
        self.pos += 1;
        Some(v)
    }
    fn list(&mut self) -> Option<Vec<u32>> {
        let n = self.next()? as usize;
        let s = self.w.get(self.pos..self.pos + n)?;
        self.pos += n;
        Some(s.to_vec())
    }
}
