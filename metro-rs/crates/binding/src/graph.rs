//! The `delta-core` component: the topology of Metro's module graph
//! (`metro_rs_graph`). The JS side is
//! packages/metro/src/DeltaBundler/rustGraph.js. One call per traversal, with
//! flat `u32` arrays both ways (the layout is `metro_rs_graph::wire`).

use metro_rs_graph::{intern_paths, wire, Core};
use napi::bindgen_prelude::Uint32Array;
use napi::{Error, Result};
use napi_derive::napi;
use std::cell::RefCell;

/// Interns paths (joined with `\0`) and returns their ids, in order. Paths
/// that were interned before keep their id. Ids are never reused.
#[napi]
pub fn graph_intern_paths(joined: String) -> Uint32Array {
    if joined.is_empty() {
        return Uint32Array::new(Vec::new());
    }
    Uint32Array::new(intern_paths(joined.split('\0')))
}

/// The topology of one graph.
#[napi]
pub struct DeltaGraph {
    core: RefCell<Core>,
}

#[napi]
impl DeltaGraph {
    #[napi(constructor)]
    pub fn new(entry_points: Uint32Array) -> Self {
        DeltaGraph {
            core: RefCell::new(Core::new(entry_points.to_vec())),
        }
    }

    /// `initialTraverseDependencies`, after the transform and resolution
    /// phase.
    #[napi]
    pub fn initial(&self, input: Uint32Array) -> Result<Uint32Array> {
        let input = decode(&input)?;
        let out = self.core.borrow_mut().initial(&input);
        Ok(Uint32Array::new(out))
    }

    /// `traverseDependencies`, after the transform and resolution phase.
    #[napi]
    pub fn incremental(&self, input: Uint32Array) -> Result<Uint32Array> {
        let input = decode(&input)?;
        let out = self.core.borrow_mut().incremental(&input);
        Ok(Uint32Array::new(out))
    }

    /// The number of modules in the graph.
    #[napi]
    pub fn module_count(&self) -> u32 {
        self.core.borrow().module_count() as u32
    }
}

fn decode(words: &Uint32Array) -> Result<wire::Input> {
    wire::decode_input(words).ok_or_else(|| Error::from_reason("malformed graph traversal input"))
}
