//! The bytes of `JSON.stringify(fromRawMappingsIndexed(infos).toMap(undefined,
//! {excludeSource}))` of packages/metro-source-map, built without making
//! JavaScript objects or strings of the map.
//!
//! What does not change between requests is held per module, keyed by a token
//! the JavaScript side chooses (one per module output): the encoded
//! `mappings`, the `names`, the function map and the number of lines of the
//! code. Per request JavaScript sends a token per module, its path as JSON, its
//! ignore flag and where its source comes from:
//!
//! - nothing (`""`: asset modules; or `excludeSource`, which drops the key);
//! - a file, which is read here (in parallel) and decoded like Node's
//!   `Buffer.toString()` (UTF-8, invalid sequences become U+FFFD, a BOM stays);
//! - a JSON literal that JavaScript made (virtual modules).
//!
//! JSON is written like `JSON.stringify` does: `"` `\` and the control
//! characters below U+0020 are escaped (`\b \f \n \r \t`, else `\u00xx` in
//! lowercase) and nothing else. Text that JavaScript supplies arrives as a JSON
//! literal already, because a JavaScript string may hold lone surrogates that
//! cannot cross as UTF-8, and `JSON.stringify` writes those as `\udxxx`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Files larger than this are reported as unreadable: Node cannot make a
/// string of them (`buffer.constants.MAX_STRING_LENGTH` is 2^29 - 24 UTF-16
/// code units), so JavaScript does the read and throws its own error.
pub const MAX_FILE_BYTES: u64 = (1 << 29) - 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// No map: no section, the lines still count.
    None,
    /// A tuple map, encoded by the Rust encoder. Its section has `names` and
    /// `mappings` last, after the metadata.
    Tuples,
    /// A VLQ map, passed through. Its section has the metadata last.
    Vlq,
}

/// What is held per module.
#[derive(Debug)]
pub struct ModuleEntry {
    pub kind: Kind,
    /// `countLines(code)`.
    pub lines: u32,
    /// `"names":<names json>,"mappings":<mappings json>`
    names_mappings: Vec<u8>,
    /// `[[<function map json>]]`
    function_map: Option<Vec<u8>>,
}

impl ModuleEntry {
    /// `mappings_json` is a JSON string literal (quotes included), `names_json`
    /// a JSON array, `function_map_json` a JSON value.
    pub fn new(
        kind: Kind,
        lines: u32,
        mappings_json: &str,
        names_json: &str,
        function_map_json: Option<&str>,
    ) -> Self {
        let mut names_mappings = Vec::with_capacity(names_json.len() + mappings_json.len() + 24);
        names_mappings.extend_from_slice(b"\"names\":");
        names_mappings.extend_from_slice(names_json.as_bytes());
        names_mappings.extend_from_slice(b",\"mappings\":");
        names_mappings.extend_from_slice(mappings_json.as_bytes());
        let function_map = function_map_json.map(|fm| {
            let mut v = Vec::with_capacity(fm.len() + 4);
            v.extend_from_slice(b"[[");
            v.extend_from_slice(fm.as_bytes());
            v.extend_from_slice(b"]]");
            v
        });
        ModuleEntry {
            kind,
            lines,
            names_mappings,
            function_map,
        }
    }
}

/// The entries held, by token.
#[derive(Default)]
pub struct Cache {
    entries: HashMap<u32, Arc<ModuleEntry>>,
}

impl Cache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, token: u32, entry: ModuleEntry) {
        self.entries.insert(token, Arc::new(entry));
    }

    pub fn remove(&mut self, token: u32) {
        self.entries.remove(&token);
    }

    pub fn get(&self, token: u32) -> Option<Arc<ModuleEntry>> {
        self.entries.get(&token).cloned()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Where a module's `sourcesContent` entry comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// `""`.
    Empty,
    /// The contents of this file.
    File(String),
    /// A JSON string literal, quotes included.
    Literal(String),
}

/// One module of the request, in bundle order.
pub struct ModuleSpec {
    pub entry: Arc<ModuleEntry>,
    pub ignored: bool,
    /// JSON string literal of the source path.
    pub path_json: String,
    pub source: Source,
}

/// Escapes `text` as a JSON string literal into `out`, like `JSON.stringify`.
pub fn write_json_string(text: &str, out: &mut Vec<u8>) {
    out.push(b'"');
    write_json_escaped(text.as_bytes(), out);
    out.push(b'"');
}

/// The body of `write_json_string`. `bytes` must be valid UTF-8 (every byte
/// that needs escaping is ASCII, so the scan works on bytes).
fn write_json_escaped(bytes: &[u8], out: &mut Vec<u8>) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if b >= 0x20 && b != b'"' && b != b'\\' {
            continue;
        }
        out.extend_from_slice(&bytes[start..i]);
        start = i + 1;
        match b {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            0x08 => out.extend_from_slice(b"\\b"),
            0x0c => out.extend_from_slice(b"\\f"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            _ => {
                out.extend_from_slice(b"\\u00");
                out.push(HEX[(b >> 4) as usize]);
                out.push(HEX[(b & 15) as usize]);
            }
        }
    }
    out.extend_from_slice(&bytes[start..]);
}

/// A JSON string literal of the file's contents decoded like
/// `Buffer.toString()` (`String::from_utf8_lossy` follows the same "maximal
/// subpart" replacement rule as V8), or `None` if it cannot be read like
/// `fs.readFileSync` followed by `toString()` would.
fn read_file_json(path: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize + 1);
    file.read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return None;
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut out = Vec::with_capacity(text.len() + text.len() / 16 + 2);
    write_json_string(&text, &mut out);
    Some(out)
}

/// Reads the files in parallel. `None` for a file that could not be read.
pub fn read_files_json(paths: &[&str]) -> Vec<Option<Vec<u8>>> {
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(8)
        .min(paths.len());
    if threads <= 1 {
        return paths.iter().map(|p| read_file_json(p)).collect();
    }
    let next = AtomicUsize::new(0);
    let mut results: Vec<Option<Vec<u8>>> = vec![None; paths.len()];
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut done: Vec<(usize, Option<Vec<u8>>)> = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= paths.len() {
                            break;
                        }
                        done.push((i, read_file_json(paths[i])));
                    }
                    done
                })
            })
            .collect();
        for handle in handles {
            // A panic in a reader is a bug, not an unreadable file.
            for (i, bytes) in handle.join().expect("source reader panicked") {
                results[i] = bytes;
            }
        }
    });
    results
}

/// The index map for `modules`. On success the bytes; if files could not be
/// read, the indices (into `modules`) of the modules they belong to.
pub fn build_index_map(
    modules: &[ModuleSpec],
    exclude_source: bool,
) -> Result<Vec<u8>, Vec<usize>> {
    // Read and escape the files first.
    let mut file_modules: Vec<usize> = Vec::new();
    let mut file_paths: Vec<&str> = Vec::new();
    if !exclude_source {
        for (i, m) in modules.iter().enumerate() {
            // Modules without a map are read too: `getSource()` is called for
            // every module today, and an unreadable file throws.
            if let Source::File(path) = &m.source {
                file_modules.push(i);
                file_paths.push(path);
            }
        }
    }
    let files = read_files_json(&file_paths);
    let failed: Vec<usize> = file_modules
        .iter()
        .zip(&files)
        .filter(|(_, f)| f.is_none())
        .map(|(i, _)| *i)
        .collect();
    if !failed.is_empty() {
        return Err(failed);
    }
    let mut file_json: HashMap<usize, Vec<u8>> = HashMap::with_capacity(files.len());
    for (i, bytes) in file_modules.iter().zip(files) {
        file_json.insert(*i, bytes.expect("checked above"));
    }

    let mut estimate = 32;
    for (i, m) in modules.iter().enumerate() {
        if m.entry.kind == Kind::None {
            continue;
        }
        estimate += 150 + m.path_json.len() + m.entry.names_mappings.len();
        if let Some(fm) = &m.entry.function_map {
            estimate += fm.len() + 24;
        }
        if !exclude_source {
            estimate += match &m.source {
                Source::Empty => 2,
                Source::File(_) => file_json.get(&i).map_or(0, |f| f.len()),
                Source::Literal(l) => l.len(),
            };
        }
    }
    let mut out: Vec<u8> = Vec::with_capacity(estimate);
    out.extend_from_slice(b"{\"version\":3,\"sections\":[");
    let mut line: u64 = 0;
    let mut first = true;
    for (i, m) in modules.iter().enumerate() {
        let entry = &*m.entry;
        if entry.kind != Kind::None {
            if !first {
                out.push(b',');
            }
            first = false;
            out.extend_from_slice(b"{\"offset\":{\"line\":");
            out.extend_from_slice(line.to_string().as_bytes());
            out.extend_from_slice(b",\"column\":0},\"map\":{\"version\":3,\"sources\":[");
            out.extend_from_slice(m.path_json.as_bytes());
            out.extend_from_slice(b"],");
            if !exclude_source {
                out.extend_from_slice(b"\"sourcesContent\":[");
                match &m.source {
                    Source::Empty => out.extend_from_slice(b"\"\""),
                    Source::File(_) => out.extend_from_slice(&file_json[&i]),
                    Source::Literal(l) => out.extend_from_slice(l.as_bytes()),
                }
                out.extend_from_slice(b"],");
            }
            match entry.kind {
                Kind::Tuples => {
                    // Generator.toMap: metadata, then names and mappings.
                    if let Some(fm) = &entry.function_map {
                        out.extend_from_slice(b"\"x_facebook_sources\":");
                        out.extend_from_slice(fm);
                        out.push(b',');
                    }
                    if m.ignored {
                        out.extend_from_slice(b"\"x_google_ignoreList\":[0],");
                    }
                    out.extend_from_slice(&entry.names_mappings);
                }
                Kind::Vlq => {
                    // toIndexMapSection: names and mappings, then metadata.
                    out.extend_from_slice(&entry.names_mappings);
                    if let Some(fm) = &entry.function_map {
                        out.extend_from_slice(b",\"x_facebook_sources\":");
                        out.extend_from_slice(fm);
                    }
                    if m.ignored {
                        out.extend_from_slice(b",\"x_google_ignoreList\":[0]");
                    }
                }
                Kind::None => unreachable!(),
            }
            out.extend_from_slice(b"}}");
        }
        line += entry.lines as u64;
    }
    out.extend_from_slice(b"]}");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn escape(text: &str) -> String {
        let mut out = Vec::new();
        write_json_string(text, &mut out);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn escapes_like_json_stringify() {
        assert_eq!(escape("abc"), "\"abc\"");
        assert_eq!(escape("a\"b\\c"), r#""a\"b\\c""#);
        assert_eq!(escape("\u{8}\u{c}\n\r\t"), r#""\b\f\n\r\t""#);
        assert_eq!(
            escape("\u{0}\u{1}\u{b}\u{1f}"),
            r#""\u0000\u0001\u000b\u001f""#
        );
        // Not escaped: DEL, U+2028/2029, non-ASCII, a BOM.
        assert_eq!(
            escape("\u{7f}\u{2028}\u{2029}é\u{feff}\u{1F600}"),
            "\"\u{7f}\u{2028}\u{2029}é\u{feff}\u{1F600}\""
        );
        assert_eq!(escape("/"), "\"/\"");
    }

    fn entry(kind: Kind, lines: u32, fm: Option<&str>) -> Arc<ModuleEntry> {
        Arc::new(ModuleEntry::new(kind, lines, "\"AAAA\"", "[\"n\"]", fm))
    }

    fn spec(entry: Arc<ModuleEntry>, ignored: bool, source: Source) -> ModuleSpec {
        ModuleSpec {
            entry,
            ignored,
            path_json: "\"/a.js\"".to_string(),
            source,
        }
    }

    fn build(modules: &[ModuleSpec], exclude: bool) -> String {
        String::from_utf8(build_index_map(modules, exclude).unwrap()).unwrap()
    }

    #[test]
    fn empty() {
        assert_eq!(build(&[], false), r#"{"version":3,"sections":[]}"#);
    }

    #[test]
    fn key_orders_and_offsets() {
        let modules = vec![
            spec(
                entry(Kind::Tuples, 3, Some("{\"names\":[],\"mappings\":\"\"}")),
                true,
                Source::Literal("\"x\"".into()),
            ),
            spec(entry(Kind::None, 5, None), false, Source::Empty),
            spec(entry(Kind::Vlq, 2, Some("{\"f\":1}")), true, Source::Empty),
            spec(
                entry(Kind::Vlq, 1, None),
                false,
                Source::Literal("\"y\"".into()),
            ),
            spec(entry(Kind::Tuples, 1, None), false, Source::Empty),
        ];
        let expected = concat!(
            r#"{"version":3,"sections":["#,
            r#"{"offset":{"line":0,"column":0},"map":{"version":3,"sources":["/a.js"],"sourcesContent":["x"],"x_facebook_sources":[[{"names":[],"mappings":""}]],"x_google_ignoreList":[0],"names":["n"],"mappings":"AAAA"}},"#,
            r#"{"offset":{"line":8,"column":0},"map":{"version":3,"sources":["/a.js"],"sourcesContent":[""],"names":["n"],"mappings":"AAAA","x_facebook_sources":[[{"f":1}]],"x_google_ignoreList":[0]}},"#,
            r#"{"offset":{"line":10,"column":0},"map":{"version":3,"sources":["/a.js"],"sourcesContent":["y"],"names":["n"],"mappings":"AAAA"}},"#,
            r#"{"offset":{"line":11,"column":0},"map":{"version":3,"sources":["/a.js"],"sourcesContent":[""],"names":["n"],"mappings":"AAAA"}}"#,
            r#"]}"#
        );
        assert_eq!(build(&modules, false), expected);
        // excludeSource drops sourcesContent and nothing else.
        let without = expected
            .replace(r#""sourcesContent":["x"],"#, "")
            .replace(r#""sourcesContent":[""],"#, "")
            .replace(r#""sourcesContent":["y"],"#, "");
        assert_eq!(build(&modules, true), without);
    }

    #[test]
    fn reads_files_and_reports_failures() {
        let dir = std::env::temp_dir().join(format!("metro-rs-index-map-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("good.js");
        std::fs::write(&good, b"a\"\n\xEF\xBB\xBFb\xFFc").unwrap();
        let missing = dir.join("missing.js");
        let modules = vec![
            spec(
                entry(Kind::Vlq, 1, None),
                false,
                Source::File(good.to_string_lossy().into()),
            ),
            spec(
                entry(Kind::Vlq, 1, None),
                false,
                Source::File(missing.to_string_lossy().into()),
            ),
            // A directory cannot be read as a file.
            spec(
                entry(Kind::Vlq, 1, None),
                false,
                Source::File(dir.to_string_lossy().into()),
            ),
        ];
        assert_eq!(build_index_map(&modules, false).unwrap_err(), vec![1, 2]);
        // Nothing is read with excludeSource.
        assert!(build_index_map(&modules, true).is_ok());
        let text = build(&modules[..1], false);
        assert!(
            text.contains("\"sourcesContent\":[\"a\\\"\\n\u{feff}b\u{fffd}c\"]"),
            "{text}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn decoding_follows_the_replacement_rules_of_node() {
        // Pinned against Node in packages/metro/src/DeltaBundler/Serializers/
        // __tests__/sourceMapRust-test.js (random and hand-made sequences).
        let cases: &[(&[u8], &str)] = &[
            (b"\xFF", "\u{fffd}"),
            (b"\xE2\x82", "\u{fffd}"),         // truncated 3-byte
            (b"\xF0\x9F\x98", "\u{fffd}"),     // truncated 4-byte
            (b"\xC0\x80", "\u{fffd}\u{fffd}"), // overlong NUL
            (b"\xED\xA0\x80", "\u{fffd}\u{fffd}\u{fffd}"), // encoded surrogate
            (b"\xEF\xBB\xBFx", "\u{feff}x"),
        ];
        for (bytes, expected) in cases {
            assert_eq!(String::from_utf8_lossy(bytes), *expected);
        }
    }
}
