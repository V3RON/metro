//! `JSON.parse(readFileSync(p, 'utf8'))` as closely as Rust can promise.
//!
//! Anything uncertain is an `Err(())`; the caller defers to the JS callback.
//! That covers: unreadable file, invalid UTF-8, any serde_json error (this
//! includes lone-surrogate escapes, a BOM, nesting deeper than serde's limit,
//! numbers out of f64 range).

use crate::package_cache::Defer;
use serde_json::{Map, Number, Value};

pub fn read_package_json(path: &str) -> Result<Value, Defer> {
    let bytes = std::fs::read(path).map_err(|_| Defer)?;
    let text = String::from_utf8(bytes).map_err(|_| Defer)?;
    parse_like_js(&text)
}

pub fn parse_like_js(text: &str) -> Result<Value, Defer> {
    let mut value: Value = serde_json::from_str(text).map_err(|_| Defer)?;
    normalize(&mut value);
    Ok(value)
}

/// An array index: the canonical decimal form of an integer 0 <= n < 2^32 - 1.
pub fn array_index(key: &str) -> Option<u32> {
    let b = key.as_bytes();
    if b.is_empty() || b.len() > 10 || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    if b.len() > 1 && b[0] == b'0' {
        return None;
    }
    let n: u64 = key.parse().ok()?;
    if n < u32::MAX as u64 {
        Some(n as u32)
    } else {
        None
    }
}

/// Reorders object keys as a JS object orders them (array-index keys first,
/// ascending; then the others in insertion order), recursively, and rounds
/// integers beyond 2^53 to doubles as `JSON.parse` does.
fn normalize(v: &mut Value) {
    match v {
        Value::Array(items) => items.iter_mut().for_each(normalize),
        Value::Object(map) => {
            let old = std::mem::take(map);
            let mut indexed: Vec<(u32, String, Value)> = Vec::new();
            let mut others: Vec<(String, Value)> = Vec::new();
            for (k, mut val) in old {
                normalize(&mut val);
                match array_index(&k) {
                    Some(i) => indexed.push((i, k, val)),
                    None => others.push((k, val)),
                }
            }
            indexed.sort_by_key(|(i, _, _)| *i);
            let mut out = Map::new();
            for (_, k, val) in indexed {
                out.insert(k, val);
            }
            for (k, val) in others {
                out.insert(k, val);
            }
            *map = out;
        }
        Value::Number(n) => {
            const MAX_SAFE: u64 = 1 << 53;
            let big = n.as_u64().is_some_and(|u| u > MAX_SAFE)
                || n.as_i64()
                    .is_some_and(|i| i < 0 && i.unsigned_abs() > MAX_SAFE);
            if big {
                if let Some(f) = n.as_f64().and_then(Number::from_f64) {
                    *n = f;
                }
            }
        }
        _ => {}
    }
}
