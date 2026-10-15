//! `serializers` component, bundle part: `BundleAssembler` holds module code
//! in Rust and returns a bundle's bytes as a `Buffer` (see
//! `metro_rs_serializer`). One call per bundle request.

use std::sync::Mutex;

use metro_rs_serializer::{assemble, Entry, ModuleStore};
use napi::bindgen_prelude::{Buffer, Uint32Array};
use napi::{Error, Result};
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
