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
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

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

    pub fn clear(&mut self) {
        self.entries.clear();
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

/// Runs `work(i)` for every `i < n` on up to 8 threads.
fn parallel(n: usize, work: impl Fn(usize) + Sync) {
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(8)
        .min(n);
    if threads <= 1 {
        (0..n).for_each(work);
        return;
    }
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= n {
                    break;
                }
                work(i);
            });
        }
    });
}

/// Reads exactly `dst.len()` bytes, then checks that the file ends there.
fn read_into(path: &str, dst: &mut [u8]) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    if file.read_exact(dst).is_err() {
        return false;
    }
    let mut extra = [0u8; 1];
    matches!(file.read(&mut extra), Ok(0))
}

const FAILED_SIZE: u64 = u64::MAX;

/// Reads the files in parallel into ONE buffer (no per-file or per-thread
/// allocations): threads stat the files, the caller sizes the buffer, then
/// threads read into disjoint slices. A file that is not a regular file, is
/// too big for a JavaScript string, cannot be read, or changed size between
/// the stat and the read is failed. Returns the bytes and each file's range,
/// or the indices (into `paths`) of the failed files.
type Ranges = Vec<std::ops::Range<usize>>;

fn read_files(paths: &[&str]) -> Result<(Vec<u8>, Ranges), Vec<usize>> {
    let sizes: Vec<AtomicU64> = (0..paths.len()).map(|_| AtomicU64::new(0)).collect();
    parallel(paths.len(), |i| {
        let size = match std::fs::metadata(paths[i]) {
            Ok(meta) if meta.is_file() && meta.len() <= MAX_FILE_BYTES => meta.len(),
            _ => FAILED_SIZE,
        };
        sizes[i].store(size, Ordering::Relaxed);
    });
    let mut failed: Vec<usize> = Vec::new();
    let mut ranges = Vec::with_capacity(paths.len());
    let mut total = 0usize;
    for (i, size) in sizes.iter().enumerate() {
        let size = size.load(Ordering::Relaxed);
        if size == FAILED_SIZE {
            failed.push(i);
            ranges.push(0..0);
        } else {
            ranges.push(total..total + size as usize);
            total += size as usize;
        }
    }
    if !failed.is_empty() {
        return Err(failed);
    }
    let mut buffer = vec![0u8; total];
    let ok: Vec<AtomicBool> = (0..paths.len()).map(|_| AtomicBool::new(false)).collect();
    {
        let mut rest: &mut [u8] = &mut buffer;
        let mut slices: Vec<Mutex<&mut [u8]>> = Vec::with_capacity(paths.len());
        for range in &ranges {
            let (head, tail) = rest.split_at_mut(range.len());
            slices.push(Mutex::new(head));
            rest = tail;
        }
        parallel(paths.len(), |i| {
            let mut slice = slices[i].lock().expect("slice lock");
            ok[i].store(read_into(paths[i], &mut slice), Ordering::Relaxed);
        });
    }
    let failed: Vec<usize> = (0..paths.len())
        .filter(|i| !ok[*i].load(Ordering::Relaxed))
        .collect();
    if failed.is_empty() {
        Ok((buffer, ranges))
    } else {
        Err(failed)
    }
}

/// Gives freed memory back to the system. glibc keeps what threads freed in
/// their arenas, and the 25 MB allocations of a big map land there, so the
/// resident size would stay high long after a build. Nothing is done on other
/// platforms or libcs.
pub fn trim_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: malloc_trim has no preconditions; it only returns free memory.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// The index map for `modules`. On success the bytes; if files could not be
/// read, the indices (into `modules`) of the modules they belong to.
pub fn build_index_map(
    modules: &[ModuleSpec],
    exclude_source: bool,
) -> Result<Vec<u8>, Vec<usize>> {
    let result = build_inner(modules, exclude_source);
    trim_memory();
    result
}

fn build_inner(modules: &[ModuleSpec], exclude_source: bool) -> Result<Vec<u8>, Vec<usize>> {
    // The files, in one buffer.
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
    let (raw, ranges) = read_files(&file_paths).map_err(|failed| {
        failed
            .into_iter()
            .map(|k| file_modules[k])
            .collect::<Vec<_>>()
    })?;
    // Module index -> its file's range.
    let mut range_of: HashMap<usize, std::ops::Range<usize>> =
        HashMap::with_capacity(file_modules.len());
    for (i, range) in file_modules.iter().zip(ranges) {
        range_of.insert(*i, range);
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
                Source::File(_) => range_of.get(&i).map_or(0, |r| r.len() + r.len() / 16 + 2),
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
                    Source::File(_) => {
                        // Valid UTF-8 is not copied by from_utf8_lossy.
                        let text = String::from_utf8_lossy(&raw[range_of[&i].clone()]);
                        write_json_string(&text, &mut out);
                    }
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
