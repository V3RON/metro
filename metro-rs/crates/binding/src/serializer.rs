//! `serializers` component, bundle part: `BundleAssembler` holds module code
//! in Rust and returns a bundle's bytes as a `Buffer` (see
//! `metro_rs_serializer`). One call per bundle request.

use std::sync::Mutex;

use metro_rs_serializer::index_map::{
    build_index_map, Cache, Kind, ModuleEntry, ModuleSpec, Source,
};
use metro_rs_serializer::{assemble, Entry, ModuleStore};
use napi::bindgen_prelude::{AsyncTask, Buffer, Uint32Array, Uint8Array};
use napi::{Env, Error, JsUnknown, Result, Task};
use napi_derive::napi;

/// In `tokens`: "the next literal".
const LITERAL: u32 = 0xFFFF_FFFF;

#[napi(object)]
pub struct AssembledBundle {
    pub buffer: Buffer,
    /// Length of the bundle as a JavaScript string, in UTF-16 code units.
    pub utf16_length: f64,
}

#[napi]
pub struct BundleAssembler {
    store: Mutex<ModuleStore>,
}

#[napi]
impl BundleAssembler {
    #[napi(constructor)]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        BundleAssembler {
            store: Mutex::new(ModuleStore::new()),
        }
    }

    /// Assembles one bundle.
    ///
    /// - `new_tokens[i]` / `new_codes[i]`: code that is not held yet.
    /// - `tokens[k]`: the k-th entry, a token or `0xFFFFFFFF` for the next of
    ///   `literals`.
    /// - `params`: the parameters of all entries, joined by NUL (one part per
    ///   entry; an empty part for literals and for code used as it is).
    ///
    /// Afterwards, code that this call did not use is dropped.
    #[napi]
    #[allow(clippy::too_many_arguments)]
    pub fn assemble(
        &self,
        pre: String,
        post: String,
        tokens: Uint32Array,
        params: String,
        new_tokens: Uint32Array,
        new_codes: Vec<String>,
        literals: Vec<String>,
    ) -> Result<AssembledBundle> {
        if new_tokens.len() != new_codes.len() {
            return Err(Error::from_reason(
                "newTokens and newCodes differ in length",
            ));
        }
        let mut store = self
            .store
            .lock()
            .map_err(|_| Error::from_reason("BundleAssembler is poisoned"))?;
        for (token, code) in new_tokens.iter().zip(new_codes) {
            store.insert(*token, code.into_bytes());
        }
        let parts: Vec<&str> = if tokens.is_empty() {
            Vec::new()
        } else {
            params.split('\0').collect()
        };
        if parts.len() != tokens.len() {
            return Err(Error::from_reason(format!(
                "{} parameter parts for {} entries",
                parts.len(),
                tokens.len()
            )));
        }
        let mut next_literal = literals.iter();
        let mut entries = Vec::with_capacity(tokens.len());
        for (token, part) in tokens.iter().zip(parts) {
            if *token == LITERAL {
                let literal = next_literal
                    .next()
                    .ok_or_else(|| Error::from_reason("fewer literals than entries"))?;
                entries.push(Entry::Literal(literal.as_bytes()));
            } else {
                entries.push(Entry::Token {
                    token: *token,
                    params: part.as_bytes(),
                });
            }
        }
        let result = assemble(&store, pre.as_bytes(), &entries, post.as_bytes());
        store.retain_entries(&entries);
        let (bytes, utf16) = result.map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(AssembledBundle {
            buffer: Buffer::from(bytes),
            utf16_length: utf16 as f64,
        })
    }

    /// Number of module codes held (for tests).
    #[napi]
    pub fn held(&self) -> u32 {
        self.store.lock().map(|s| s.len() as u32).unwrap_or(0)
    }
}

// ------------------------------------------------------------ source maps

/// One request for an index source map, see `metro_rs_serializer::index_map`.
///
/// Per module `i` (in bundle order):
/// - `tokens[i]`: the module output's token;
/// - `flags[i]`: bit 0 the module is on the ignore list, bits 1 and 2 where
///   the source comes from (0 none, 1 a file: the next of `files`, 2 a JSON
///   literal: the next of `literals`);
/// - `paths[i]`: the source path as a JSON string literal.
///
/// Entries not held yet come in `new_*` (a kind of 0 none, 1 tuples, 2 VLQ);
/// `evict` lists tokens to drop.
#[napi(object)]
pub struct IndexMapRequest {
    pub tokens: Uint32Array,
    pub flags: Uint8Array,
    pub paths: Vec<String>,
    pub files: Vec<String>,
    pub literals: Vec<String>,
    pub exclude_source: bool,
    pub new_tokens: Uint32Array,
    pub new_kinds: Uint8Array,
    pub new_lines: Uint32Array,
    pub new_mappings: Vec<String>,
    pub new_names: Vec<String>,
    /// The function map as JSON, or an empty string for none.
    pub new_function_maps: Vec<String>,
    pub evict: Uint32Array,
}

#[napi(object)]
pub struct IndexMapResult {
    /// The map's JSON as UTF-8. Empty when `failed` is not.
    pub buffer: Buffer,
    /// Whether every byte of `buffer` is ASCII.
    pub ascii: bool,
    /// Indices of modules whose file could not be read.
    pub failed: Vec<u32>,
}

#[napi]
pub struct IndexMapBuilder {
    cache: Mutex<Cache>,
}

#[napi]
impl IndexMapBuilder {
    #[napi(constructor)]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        IndexMapBuilder {
            cache: Mutex::new(Cache::new()),
        }
    }

    /// Builds the map on the calling thread.
    #[napi]
    pub fn build(&self, request: IndexMapRequest) -> Result<IndexMapResult> {
        let (modules, exclude_source) = self.prepare(request)?;
        Ok(finish(build_index_map(&modules, exclude_source)))
    }

    /// Builds the map on a worker thread.
    #[napi]
    pub fn build_async(&self, request: IndexMapRequest) -> Result<AsyncTask<IndexMapTask>> {
        let (modules, exclude_source) = self.prepare(request)?;
        Ok(AsyncTask::new(IndexMapTask {
            modules: Some(modules),
            exclude_source,
        }))
    }

    /// Builds the map on the calling thread and returns it as a JavaScript
    /// string made here (latin1 when all ASCII), or the indices of the
    /// modules whose file could not be read, as an array.
    #[napi]
    pub fn build_string(&self, env: Env, request: IndexMapRequest) -> Result<JsUnknown> {
        let (modules, exclude_source) = self.prepare(request)?;
        string_or_failed(env, build_index_map(&modules, exclude_source))
    }

    /// `build_string` on a worker thread.
    #[napi]
    pub fn build_string_async(
        &self,
        request: IndexMapRequest,
    ) -> Result<AsyncTask<IndexMapStringTask>> {
        let (modules, exclude_source) = self.prepare(request)?;
        Ok(AsyncTask::new(IndexMapStringTask {
            modules: Some(modules),
            exclude_source,
        }))
    }

    /// Drops every entry held, now, and returns memory to the system.
    #[napi]
    pub fn clear(&self) -> Result<()> {
        self.cache
            .lock()
            .map_err(|_| Error::from_reason("IndexMapBuilder is poisoned"))?
            .clear();
        metro_rs_serializer::index_map::trim_memory();
        Ok(())
    }

    /// Number of entries held (for tests).
    #[napi]
    pub fn held(&self) -> u32 {
        self.cache.lock().map(|c| c.len() as u32).unwrap_or(0)
    }

    fn prepare(&self, request: IndexMapRequest) -> Result<(Vec<ModuleSpec>, bool)> {
        let n = request.tokens.len();
        if request.flags.len() != n || request.paths.len() != n {
            return Err(Error::from_reason(
                "tokens, flags and paths differ in length",
            ));
        }
        let new = request.new_tokens.len();
        if request.new_kinds.len() != new
            || request.new_lines.len() != new
            || request.new_mappings.len() != new
            || request.new_names.len() != new
            || request.new_function_maps.len() != new
        {
            return Err(Error::from_reason("the new entries differ in length"));
        }
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| Error::from_reason("IndexMapBuilder is poisoned"))?;
        for token in request.evict.iter() {
            cache.remove(*token);
        }
        for k in 0..new {
            let kind = match request.new_kinds[k] {
                0 => Kind::None,
                1 => Kind::Tuples,
                2 => Kind::Vlq,
                other => {
                    return Err(Error::from_reason(format!("unknown map kind {other}")));
                }
            };
            let function_map = &request.new_function_maps[k];
            cache.insert(
                request.new_tokens[k],
                ModuleEntry::new(
                    kind,
                    request.new_lines[k],
                    &request.new_mappings[k],
                    &request.new_names[k],
                    if function_map.is_empty() {
                        None
                    } else {
                        Some(function_map)
                    },
                ),
            );
        }
        let mut files = request.files.into_iter();
        let mut literals = request.literals.into_iter();
        let mut modules = Vec::with_capacity(n);
        for (i, path_json) in request.paths.into_iter().enumerate() {
            let token = request.tokens[i];
            let entry = cache
                .get(token)
                .ok_or_else(|| Error::from_reason(format!("module token {token} is not held")))?;
            let flags = request.flags[i];
            let source = match (flags >> 1) & 3 {
                0 => Source::Empty,
                1 => Source::File(
                    files
                        .next()
                        .ok_or_else(|| Error::from_reason("fewer files than modules"))?,
                ),
                2 => Source::Literal(
                    literals
                        .next()
                        .ok_or_else(|| Error::from_reason("fewer literals than modules"))?,
                ),
                other => {
                    return Err(Error::from_reason(format!("unknown source mode {other}")));
                }
            };
            modules.push(ModuleSpec {
                entry,
                ignored: flags & 1 == 1,
                path_json,
                source,
            });
        }
        Ok((modules, request.exclude_source))
    }
}

fn finish(result: std::result::Result<Vec<u8>, Vec<usize>>) -> IndexMapResult {
    match result {
        Ok(bytes) => {
            let ascii = bytes.is_ascii();
            IndexMapResult {
                buffer: Buffer::from(bytes),
                ascii,
                failed: Vec::new(),
            }
        }
        Err(failed) => IndexMapResult {
            buffer: Buffer::from(Vec::new()),
            ascii: true,
            failed: failed.into_iter().map(|i| i as u32).collect(),
        },
    }
}

pub struct IndexMapTask {
    modules: Option<Vec<ModuleSpec>>,
    exclude_source: bool,
}

impl Task for IndexMapTask {
    type Output = std::result::Result<Vec<u8>, Vec<usize>>;
    type JsValue = IndexMapResult;

    fn compute(&mut self) -> Result<Self::Output> {
        let modules = self.modules.take().unwrap_or_default();
        Ok(build_index_map(&modules, self.exclude_source))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<IndexMapResult> {
        Ok(finish(output))
    }
}

fn string_or_failed(
    env: Env,
    result: std::result::Result<Vec<u8>, Vec<usize>>,
) -> Result<JsUnknown> {
    match result {
        Ok(bytes) => {
            let string = if bytes.is_ascii() {
                env.create_string_latin1(&bytes)?
            } else {
                // Valid UTF-8 by construction (JSON of decoded text).
                env.create_string_from_std(
                    String::from_utf8(bytes).map_err(|e| Error::from_reason(e.to_string()))?,
                )?
            };
            metro_rs_serializer::index_map::trim_memory();
            Ok(string.into_unknown())
        }
        Err(failed) => {
            let mut array = env.create_array_with_length(failed.len())?;
            for (k, i) in failed.into_iter().enumerate() {
                array.set_element(k as u32, env.create_uint32(i as u32)?)?;
            }
            Ok(array.into_unknown())
        }
    }
}

pub struct IndexMapStringTask {
    modules: Option<Vec<ModuleSpec>>,
    exclude_source: bool,
}

impl Task for IndexMapStringTask {
    type Output = std::result::Result<Vec<u8>, Vec<usize>>;
    type JsValue = JsUnknown;

    fn compute(&mut self) -> Result<Self::Output> {
        let modules = self.modules.take().unwrap_or_default();
        Ok(build_index_map(&modules, self.exclude_source))
    }

    fn resolve(&mut self, env: Env, output: Self::Output) -> Result<JsUnknown> {
        string_or_failed(env, output)
    }
}
