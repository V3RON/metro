//! The `delta-core` component: the topology of Metro's module graph
//! (`metro_rs_graph`). The JS side is
//! packages/metro/src/DeltaBundler/rustGraph.js. One call per traversal, with
//! flat `u32` arrays both ways (the layout is `metro_rs_graph::wire`).

use metro_rs_graph::{intern_paths, wire, Core};
use napi::bindgen_prelude::Uint32Array;
use napi::{Error, Result};
use napi_derive::napi;
use std::cell::RefCell;

/// Interns one path and returns its id. A path that was interned before keeps
/// its id. Ids are never reused. Any string is a valid path: virtual modules
/// (Expo's `\0polyfill:…`) contain NUL, so paths are never joined with it.
#[napi]
pub fn graph_intern_path(path: String) -> u32 {
    intern_paths(std::iter::once(path.as_str()))[0]
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
