//! Source maps: VLQ encoding and decoding, mapping generation, lookups,
//! composition. Port of `metro-source-map`; no Node bindings here, see the
//! `binding` crate.

pub mod compose;
pub mod consumer;
pub mod decode;
pub mod encode;
pub mod generator;
pub mod vlq;
