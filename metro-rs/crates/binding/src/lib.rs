//! napi-rs binding. Each ported component adds its exports here.
//!
//! The binding only converts between JavaScript values and the plain Rust
//! crates; anything with logic lives in those crates and is tested there.
//! Everything crosses in bulk, as typed arrays, strings and arrays of strings:
//! a call per mapping would cost more than the work it saves.

use metro_rs_source_map::compose::{self, ComposeError, ComposeMap};
use metro_rs_source_map::consumer::ColumnsRef;
use metro_rs_source_map::decode;
use metro_rs_source_map::encode::{Batch, BatchError, Encoder};
use metro_rs_source_map::generator::AddError;
use napi::bindgen_prelude::{Buffer, Int32Array};
use napi_derive::napi;

pub mod file_map;
pub mod resolver;

/// Version of the Rust core.
#[napi]
pub fn version() -> String {
    metro_rs_core::version().to_string()
}

// ---------------------------------------------------------------- cache-key

#[napi(object)]
pub struct CacheKeyResult {
    /// The key, empty when a file could not be read.
    pub key: String,
    /// Index of the file that could not be read, or -1.
    pub error_index: i32,
}

/// `getCacheKey` of metro-cache-key. On a read error the JavaScript side reads
/// the file again, so that the exception is Node's own.
#[napi]
pub fn get_cache_key(files: Vec<String>) -> CacheKeyResult {
    match metro_rs_cache_key::get_cache_key(&files) {
        Ok(key) => CacheKeyResult {
            key,
            error_index: -1,
        },
        Err(error) => CacheKeyResult {
            key: String::new(),
            error_index: error.index as i32,
        },
    }
}

// --------------------------------------------------------------- source-map

/// Modules flattened for one call, see `metro_rs_source_map::encode::Batch`.
#[napi(object)]
pub struct ModuleBatch {
    pub tuples: Int32Array,
    pub names: Vec<String>,
    pub modules: Int32Array,
    pub vlq_mappings: Vec<String>,
    pub vlq_names: Vec<Vec<String>>,
}

impl ModuleBatch {
    fn with<R>(&self, f: impl FnOnce(&Batch) -> R) -> R {
        f(&Batch {
            tuples: &self.tuples,
            names: &self.names,
            modules: &self.modules,
            vlq_mappings: &self.vlq_mappings,
            vlq_names: &self.vlq_names,
        })
    }
}

pub const STATUS_OK: i32 = 0;
pub const STATUS_PRECEDING_MAPPING: i32 = 1;
pub const STATUS_NO_FILE: i32 = 2;
pub const STATUS_INVALID_MAPPING: i32 = 3;
pub const STATUS_UNEXPECTED_MODULE: i32 = 4;
pub const STATUS_FALLBACK: i32 = 5;

/// Result of adding modules. `code` is one of the `STATUS_*` values (also
/// exported to JavaScript); the other fields say where it happened.
#[napi(object)]
pub struct BatchStatus {
    pub code: i32,
    /// Index of the module in the batch.
    pub module: i32,
    /// Index of the mapping in the module, for an invalid mapping.
    pub index: i32,
}

fn status_of(result: Result<(), BatchError>) -> BatchStatus {
    let (code, module, index) = match result {
        Ok(()) => (STATUS_OK, 0, 0),
        Err(BatchError::Add {
            module,
            error: AddError::PrecedingMapping,
        }) => (STATUS_PRECEDING_MAPPING, module, 0),
        Err(BatchError::Add {
            module,
            error: AddError::NoFile,
        }) => (STATUS_NO_FILE, module, 0),
        Err(BatchError::InvalidMapping { module, index }) => {
            (STATUS_INVALID_MAPPING, module, index)
        }
        Err(BatchError::UnexpectedModule { module }) => (STATUS_UNEXPECTED_MODULE, module, 0),
        Err(BatchError::Fallback { module }) => (STATUS_FALLBACK, module, 0),
    };
    BatchStatus {
        code,
        module: module as i32,
        index: index as i32,
    }
}

#[napi(object)]
pub struct EncodedMappings {
    /// The `mappings` string, as ASCII bytes.
    pub mappings: Buffer,
    /// The names in order of first use.
    pub names: Vec<String>,
    /// What a JavaScript `Generator` needs to continue from here:
    /// generatedColumn, generatedLine, name, source, sourceColumn, sourceLine
    /// of `last`, then 1 if the current line has a segment, else 0.
    pub state: Int32Array,
}

/// The flat `Generator` behind `fromRawMappings`: modules are added in batches
/// so that the non-blocking variant can yield to the event loop in between.
#[napi]
pub struct SourceMapEncoder {
    encoder: Encoder,
}

impl Default for SourceMapEncoder {
    fn default() -> Self {
        Self::new()
    }
}

#[napi]
impl SourceMapEncoder {
    #[napi(constructor)]
    pub fn new() -> Self {
        SourceMapEncoder {
            encoder: Encoder::new(),
        }
    }

    #[napi]
    pub fn add_modules(&mut self, batch: ModuleBatch) -> BatchStatus {
        status_of(batch.with(|b| self.encoder.add_batch(b)))
    }

    #[napi]
    pub fn finish(&mut self) -> EncodedMappings {
        let generator = self.encoder.take_generator();
        let last = generator.last();
        let state = vec![
            last.generated_column,
            last.generated_line,
            last.name,
            last.source,
            last.source_column,
            last.source_line,
            generator.has_segment() as i32,
        ];
        let (mappings, names) = generator.into_parts();
        EncodedMappings {
            mappings: Buffer::from(mappings),
            names,
            state: Int32Array::new(state),
        }
    }
}

#[napi(object)]
pub struct EncodedSections {
    pub status: BatchStatus,
    /// The `mappings` string of every module.
    pub mappings: Vec<String>,
    /// The names of every module.
    pub names: Vec<Vec<String>>,
}

/// One generator per module, as for the sections of an indexed source map and
/// for `vlqMapFromTuples`.
#[napi]
pub fn encode_sections(batch: ModuleBatch) -> EncodedSections {
    let mut encoder = Encoder::new();
    match batch.with(|b| encoder.encode_each(b)) {
        Ok(each) => {
            let (mappings, names) = each
                .into_iter()
                .map(|(mappings, names)| {
                    // The encoder writes base64 digits, separators only: ASCII.
                    (String::from_utf8_lossy(&mappings).into_owned(), names)
                })
                .unzip();
            EncodedSections {
                status: status_of(Ok(())),
                mappings,
                names,
            }
        }
        Err(error) => EncodedSections {
            status: status_of(Err(error)),
            mappings: Vec::new(),
            names: Vec::new(),
        },
    }
}

#[napi(object)]
pub struct DecodedMappings {
    /// 5 values per mapping, see `metro_rs_source_map::decode::Columns`.
    pub data: Int32Array,
    pub line_starts: Int32Array,
}

/// `MappingsConsumer._decodeMappings`. `None` when the JavaScript decoder has
/// to run (it throws, or its result depends on 32-bit wrap-around).
#[napi]
pub fn decode_mappings(mappings: String) -> Option<DecodedMappings> {
    let columns = decode::decode_columns(mappings.as_bytes()).ok()?;
    Some(DecodedMappings {
        data: Int32Array::new(columns.data),
        line_starts: Int32Array::new(columns.line_starts),
    })
}

/// How many mappings and lines were found by [`decode_mappings_into`].
#[napi(object)]
pub struct DecodedCounts {
    pub mappings: u32,
    pub lines: u32,
}

/// Like [`decode_mappings`], into buffers that JavaScript reuses from one call
/// to the next. The buffers are filled as far as they reach; when the counts
/// are bigger than the buffers, JavaScript calls again with bigger ones.
#[napi]
pub fn decode_mappings_into(
    mappings: String,
    mut data: Int32Array,
    mut line_starts: Int32Array,
) -> Option<DecodedCounts> {
    let mut sink = decode::SliceSink {
        data: &mut data,
        line_starts: &mut line_starts,
        mappings: 0,
        lines: 0,
    };
    decode::decode(mappings.as_bytes(), &mut sink).ok()?;
    Some(DecodedCounts {
        mappings: sink.mappings as u32,
        lines: sink.lines as u32,
    })
}

#[napi(object)]
pub struct ComposeInput {
    pub data: Int32Array,
    pub line_starts: Int32Array,
    pub sources: Int32Array,
    pub names: Int32Array,
}

#[napi(object)]
pub struct ComposedMappings {
    pub mappings: String,
    pub sources: Int32Array,
    pub names: Int32Array,
}

/// The mapping loop of `composeSourceMaps`, or `None` when JavaScript has to
/// run it (see `ComposeError`). `maps[0]` is the last map.
#[napi]
pub fn compose_mappings(maps: Vec<ComposeInput>) -> Option<ComposedMappings> {
    let inputs: Vec<ComposeMap> = maps
        .iter()
        .map(|m| ComposeMap {
            columns: ColumnsRef {
                data: &m.data,
                line_starts: &m.line_starts,
            },
            sources: &m.sources,
            names: &m.names,
        })
        .collect();
    match compose::compose(&inputs) {
        Ok(composed) => Some(ComposedMappings {
            mappings: String::from_utf8_lossy(&composed.mappings).into_owned(),
            sources: Int32Array::new(composed.sources),
            names: Int32Array::new(composed.names),
        }),
        Err(ComposeError::Unsorted | ComposeError::Invalid) => None,
    }
}

/// Exports the `STATUS_*` values so that JavaScript does not repeat them.
#[napi(object)]
pub struct Statuses {
    pub ok: i32,
    pub preceding_mapping: i32,
    pub no_file: i32,
    pub invalid_mapping: i32,
    pub unexpected_module: i32,
    pub fallback: i32,
}

#[napi]
pub fn source_map_statuses() -> Statuses {
    Statuses {
        ok: STATUS_OK,
        preceding_mapping: STATUS_PRECEDING_MAPPING,
        no_file: STATUS_NO_FILE,
        invalid_mapping: STATUS_INVALID_MAPPING,
        unexpected_module: STATUS_UNEXPECTED_MODULE,
        fallback: STATUS_FALLBACK,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_core() {
        assert_eq!(version(), metro_rs_core::version());
    }

    #[test]
    fn statuses_map_batch_errors() {
        assert_eq!(status_of(Ok(())).code, STATUS_OK);
        let status = status_of(Err(BatchError::InvalidMapping {
            module: 2,
            index: 5,
        }));
        assert_eq!(
            (status.code, status.module, status.index),
            (STATUS_INVALID_MAPPING, 2, 5)
        );
        assert_eq!(
            status_of(Err(BatchError::Add {
                module: 0,
                error: AddError::PrecedingMapping
            }))
            .code,
            STATUS_PRECEDING_MAPPING
        );
        assert_eq!(
            status_of(Err(BatchError::Add {
                module: 0,
                error: AddError::NoFile
            }))
            .code,
            STATUS_NO_FILE
        );
        assert_eq!(
            status_of(Err(BatchError::UnexpectedModule { module: 1 })).code,
            STATUS_UNEXPECTED_MODULE
        );
        assert_eq!(
            status_of(Err(BatchError::Fallback { module: 1 })).code,
            STATUS_FALLBACK
        );
    }
}
