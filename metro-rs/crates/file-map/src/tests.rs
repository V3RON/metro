//! Tests against expected values produced by the real JS metro-file-map
//! (`oracle/gen.js` writes `oracle_data.rs`), plus tests of internals that have
//! no JS equivalent.

use crate::cache::{write_file, CacheFile};
use crate::crawl::{CrawlOptions, IgnoreRules};
use crate::file_map::{FileMap, FileMapConfig, MapLookup, WatchEvent};
use crate::metadata::number;
use crate::oracle_data as o;
use crate::{
    get_worker_input, mtime_ms_from_parts, sha1_hex, CacheExtras, Changes, Difference, FileData,
    FileMapError, FileMetadata, JsRegExp, MatchFilesOptions, PluginFilter, PluginJobResult,
    RegExpError, Sha1Job, Symlink, FORMAT_VERSION,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

const VCS_DIRECTORIES: &str = r"[/\\]\.(git|hg)[/\\]";

// ---------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "mfm-rs-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // The JS resolved the real path of its temporary directory too.
        Self(std::fs::canonicalize(dir).unwrap())
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn base(&self) -> &str {
        self.0.to_str().unwrap()
    }

    /// Replaces the `$B` of the oracle data.
    fn sub(&self, s: &str) -> String {
        s.replace("$B", self.base())
    }

    /// The reverse, for what Rust produced.
    fn unsub(&self, s: &str) -> String {
        s.replace(self.base(), "$B")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            self.0.join("root/unreadable_dir"),
            std::fs::Permissions::from_mode(0o755),
        );
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Builds the oracle's fixture directory.
fn materialize(dir: &TempDir) {
    use std::os::unix::fs::PermissionsExt;
    for (kind, p, a, b) in o::FIXTURE {
        let full = dir.path().join(p);
        match *kind {
            "dir" => std::fs::create_dir_all(&full).unwrap(),
            "file" => {
                let size: usize = a.parse().unwrap();
                std::fs::write(&full, vec![b'x'; size]).unwrap();
                let secs: f64 = o::BASE_SEC as f64 + b.parse::<f64>().unwrap();
                let file = std::fs::OpenOptions::new().write(true).open(&full).unwrap();
                file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs_f64(secs))
                    .unwrap();
            }
            "link" => std::os::unix::fs::symlink(dir.sub(a), &full).unwrap(),
            "chmod" => {
                let mode: u32 = a.parse().unwrap();
                std::fs::set_permissions(&full, std::fs::Permissions::from_mode(mode)).unwrap();
            }
            other => panic!("unknown fixture kind {other}"),
        }
    }
}

/// Whether the unreadable directory of the fixture is unreadable (it is not
/// when the tests run as root).
fn unreadable_dir_is_unreadable(dir: &TempDir) -> bool {
    std::fs::read_dir(dir.path().join("root/unreadable_dir")).is_err()
}

fn parse(json: &str) -> Value {
    serde_json::from_str(json).unwrap()
}

fn meta(tuple: &Value) -> FileMetadata {
    FileMetadata::from_tuple(tuple).unwrap_or_else(|| panic!("bad tuple {tuple}"))
}

fn entries(json: &str) -> Vec<(String, FileMetadata)> {
    parse(json)
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e[0].as_str().unwrap().to_string(), meta(&e[1])))
        .collect()
}

fn file_data(json: &str) -> FileData {
    entries(json).into_iter().collect()
}

fn map_with(root: &str, files: Vec<(String, FileMetadata)>) -> FileMap {
    let mut map = FileMap::new(root, FileMapConfig::default());
    map.load(files).unwrap();
    map
}

/// `[[canonicalPath, tuple]]` in `metadataIterator` order.
fn listing(map: &FileMap) -> Value {
    Value::Array(
        map.metadata_iterator(true, true)
            .into_iter()
            .map(|e| json!([e.canonical_path, map.metadata(e.id).unwrap().to_tuple()]))
            .collect(),
    )
}

fn files_json(files: &[(String, FileMetadata)]) -> Value {
    Value::Array(
        files
            .iter()
            .map(|(p, m)| json!([p, m.to_tuple()]))
            .collect(),
    )
}

fn view(changes: &Changes) -> Value {
    json!({
        "ad": changes.added_directories,
        "rd": changes.removed_directories,
        "af": files_json(&changes.added_files),
        "mf": files_json(&changes.modified_files),
        "rf": files_json(&changes.removed_files),
    })
}

fn file_data_json(files: &FileData) -> Value {
    Value::Array(
        files
            .iter()
            .map(|(p, m)| json!([p, m.to_tuple()]))
            .collect(),
    )
}

/// The snapshot as `["d", [[name, node]]]` / `["f", tuple]`.
fn snapshot_json(snapshot: &Value, sort: bool, normalize: bool) -> Value {
    snapshot_node(snapshot, "", sort, normalize)
}

/// `normalize`: the mtime of a symlink is the time the fixture was made, and
/// the size of an absolute symlink is the length of the fixture's path.
fn snapshot_node(snapshot: &Value, name: &str, sort: bool, normalize: bool) -> Value {
    match snapshot {
        Value::Object(map) => {
            let mut children: Vec<(String, Value)> = map
                .iter()
                .map(|(k, v)| (k.clone(), snapshot_node(v, k, sort, normalize)))
                .collect();
            if sort {
                children.sort_by(|a, b| a.0.cmp(&b.0));
            }
            json!([
                "d",
                children
                    .into_iter()
                    .map(|(k, v)| json!([k, v]))
                    .collect::<Vec<_>>()
            ])
        }
        Value::Array(tuple) => {
            let mut tuple = tuple.clone();
            if normalize && tuple[4] != json!(0) {
                tuple[0] = Value::Null;
            }
            if normalize && name == "link_abs" {
                tuple[1] = Value::Null;
            }
            json!(["f", tuple])
        }
        other => panic!("bad snapshot node {other}"),
    }
}

// ---------------------------------------------------------------------
// Small pieces
// ---------------------------------------------------------------------

#[test]
fn extname_matches_node() {
    for (name, expected) in o::EXTNAMES {
        assert_eq!(
            &metro_rs_resolver::path::extname(name),
            expected,
            "{name:?}"
        );
    }
}

#[test]
fn mtime_is_truncated_like_a_date() {
    for (sec, nsec, expected) in o::MTIMES {
        assert_eq!(
            mtime_ms_from_parts(*sec, *nsec),
            *expected as f64,
            "{sec} {nsec}"
        );
    }
}

#[test]
fn sha1_matches_node() {
    for (content, expected) in o::SHA1S {
        assert_eq!(&sha1_hex(content.as_bytes()), expected, "{content:?}");
    }
}

#[test]
fn regexp_test_matches_js() {
    for (source, flags, subjects) in o::REGEX {
        let re = JsRegExp::new(source, flags).unwrap_or_else(|e| panic!("/{source}/{flags}: {e}"));
        for (subject, expected) in *subjects {
            assert_eq!(
                re.test(subject),
                *expected,
                "/{source}/{flags}.test({subject:?})"
            );
        }
    }
}

#[test]
fn regexp_rejects_invalid_and_stateful() {
    for (source, flags) in o::REGEX_INVALID {
        assert!(
            matches!(JsRegExp::new(source, flags), Err(RegExpError::Invalid(_))),
            "/{source}/{flags}"
        );
    }
    // Stateful in JS (`lastIndex`): the caller falls back to JS.
    for flags in ["g", "y", "gi", "iy"] {
        assert!(
            matches!(JsRegExp::new("a", flags), Err(RegExpError::Unsupported(_))),
            "{flags}"
        );
    }
    // Flags that do not change `test` are fine.
    for flags in ["", "i", "m", "s", "u", "d", "ims"] {
        assert!(JsRegExp::new("a", flags).is_ok(), "{flags}");
    }
}

#[test]
fn metadata_tuple_round_trips() {
    let tuple = json!([1700000000500i64, 3, 1, "abc", "../x", [1, 2], null, "s"]);
    let m = meta(&tuple);
    assert_eq!(m.symlink, Symlink::Target("../x".into()));
    assert_eq!(m.plugin_data.len(), 3);
    assert_eq!(m.to_tuple(), tuple);
    let regular = meta(&json!([null, 0, 0, null, 0]));
    assert!(regular.is_regular_file() && regular.mtime.is_none());
    assert!(FileMetadata::from_tuple(&json!([1, 2])).is_none());
    assert!(FileMetadata::from_tuple(&json!([1, 2, 0, null, 2])).is_none());
}

// ---------------------------------------------------------------------
// getDifference, the aggregator, change events, deltas
// ---------------------------------------------------------------------

#[test]
fn get_difference_matches_js() {
    assert!(o::DIFFS.len() > 100);
    for (i, c) in o::DIFFS.iter().enumerate() {
        let map = map_with("/proj/root", entries(c.prev));
        let files = file_data(c.files);
        let diff = map.get_difference(&files, c.subpath).unwrap();
        assert_eq!(
            file_data_json(&diff.changed_files),
            parse(c.changed),
            "case {i}: changed (prev {} files {} subpath {:?})",
            c.prev,
            c.files,
            c.subpath
        );
        assert_eq!(
            json!(diff.removed_files),
            parse(c.removed),
            "case {i}: removed"
        );
    }
}

#[test]
fn aggregator_matches_js() {
    use crate::FileSystemChangeAggregator;
    assert!(o::AGGREGATOR.len() > 100);
    for (i, (ops, expected_view, expected_size)) in o::AGGREGATOR.iter().enumerate() {
        let mut agg = FileSystemChangeAggregator::new();
        for op in parse(ops).as_array().unwrap() {
            let p = op[1].as_str().unwrap();
            match op[0].as_str().unwrap() {
                "da" => agg.directory_added(p),
                "dr" => agg.directory_removed(p),
                "fa" => agg.file_added(p, meta(&op[2])),
                "fm" => agg.file_modified(p, meta(&op[2]), meta(&op[3])),
                "fr" => agg.file_removed(p, meta(&op[2])),
                other => panic!("op {other}"),
            }
        }
        assert_eq!(agg.get_size(), *expected_size, "case {i}: size");
        let changes = agg.changes();
        assert_eq!(changes.size(), *expected_size, "case {i}: Changes::size");
        assert_eq!(view(&changes), parse(expected_view), "case {i} ({ops})");
        assert_eq!(view(&agg.into_changes()), parse(expected_view), "case {i}");
    }
}

fn events_of(json: &str) -> Vec<WatchEvent> {
    parse(json)
        .as_array()
        .unwrap()
        .iter()
        .map(|e| match e[0].as_str().unwrap() {
            "delete" => WatchEvent::Delete {
                normal_path: e[1].as_str().unwrap().to_string(),
            },
            "touch" => WatchEvent::Touch {
                normal_path: e[1].as_str().unwrap().to_string(),
                metadata: meta(&e[2]),
            },
            other => panic!("event {other}"),
        })
        .collect()
}

#[test]
fn watch_events_match_js() {
    assert!(o::EVENTS.len() > 100);
    let mut threw = 0;
    for (i, (initial, events, expected_view, expected_listing, expected_err)) in
        o::EVENTS.iter().enumerate()
    {
        let mut map = map_with("/proj/root", entries(initial));
        let result = map.apply_events(events_of(events));
        match result {
            Ok(changes) => {
                assert_eq!(*expected_err, "", "case {i}: JS threw, Rust did not");
                assert_eq!(view(&changes), parse(expected_view), "case {i} ({events})");
            }
            Err(e) => {
                threw += 1;
                assert_eq!(e.message, *expected_err, "case {i}");
            }
        }
        assert_eq!(
            listing(&map),
            parse(expected_listing),
            "case {i}: tree ({events})"
        );
    }
    assert!(threw > 0, "the data should include throwing sequences");
}

#[test]
fn applying_a_delta_matches_js() {
    assert!(o::DELTAS.len() > 100);
    let mut threw = 0;
    for (i, (initial, removed, changed, expected_view, expected_listing, expected_err)) in
        o::DELTAS.iter().enumerate()
    {
        let mut map = map_with("/proj/root", entries(initial));
        let delta = Difference {
            changed_files: file_data(changed),
            removed_files: parse(removed)
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p.as_str().unwrap().to_string())
                .collect(),
        };
        match map.apply_delta(delta) {
            Ok(changes) => {
                assert_eq!(*expected_err, "", "case {i}: JS threw, Rust did not");
                assert_eq!(view(&changes), parse(expected_view), "case {i}");
            }
            Err(e) => {
                threw += 1;
                assert_eq!(e.message, *expected_err, "case {i}");
            }
        }
        assert_eq!(listing(&map), parse(expected_listing), "case {i}: tree");
    }
    assert!(threw > 0, "the data should include throwing deltas");
}

// ---------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------

fn lookup_json(map: &FileMap, p: &str) -> Value {
    match map.lookup(p).unwrap() {
        MapLookup::NotFound => Value::Null,
        MapLookup::Dir { real_path } => json!(["d", real_path, null]),
        MapLookup::File { real_path, id } => {
            json!(["f", real_path, map.metadata(id).unwrap().to_tuple()])
        }
    }
}

#[test]
fn point_queries_match_js() {
    let map = map_with(o::Q_ROOT, entries(o::Q_TREE_LOOPS));
    for (p, sha1, size, link_stats, lookup) in o::Q_POINT {
        assert_eq!(
            json!(map.get_sha1(p).unwrap()),
            parse(sha1),
            "getSha1 {p:?}"
        );
        assert_eq!(
            json!(map.get_size(p).unwrap().map(number)),
            parse(size),
            "getSize {p:?}"
        );
        let stats = map.link_stats(p).unwrap().map(|s| {
            json!({"fileType": s.file_type.to_string(), "modifiedTime": s.modified_time.map(number), "size": number(s.size)})
        });
        assert_eq!(json!(stats), parse(link_stats), "linkStats {p:?}");
        assert_eq!(lookup_json(&map, p), parse(lookup), "lookup {p:?}");
    }
    assert_eq!(json!(map.get_all_files()), parse(o::Q_ALL_FILES));
}

#[test]
fn metadata_iterator_and_snapshot_match_js() {
    let map = map_with(o::Q_ROOT, entries(o::Q_TREE_MAIN));
    for (include_symlinks, include_node_modules, expected) in o::Q_ITER {
        let actual: Vec<Value> = map
            .metadata_iterator(*include_symlinks, *include_node_modules)
            .into_iter()
            .map(|e| {
                json!([
                    e.base_name,
                    e.canonical_path,
                    map.metadata(e.id).unwrap().to_tuple()
                ])
            })
            .collect();
        assert_eq!(
            Value::Array(actual),
            parse(expected),
            "symlinks {include_symlinks} node_modules {include_node_modules}"
        );
    }
    assert_eq!(
        snapshot_json(&map.get_serializable_snapshot(), false, false),
        parse(o::Q_SNAPSHOT)
    );
}

#[test]
fn match_files_matches_js() {
    assert!(o::Q_MATCH.len() > 400);
    let main = map_with(o::Q_ROOT, entries(o::Q_TREE_MAIN));
    let small = map_with(o::Q_ROOT, entries(o::Q_TREE_SMALL_LOOPS));
    for (tree, filter, absolute, follow, recursive, root_dir, expected) in o::Q_MATCH {
        let map = if *tree == 0 { &main } else { &small };
        let regexp = filter.map(|(s, f)| JsRegExp::new(s, f).unwrap());
        let opts = MatchFilesOptions {
            filter: regexp.as_ref(),
            filter_compare_absolute: *absolute,
            filter_compare_posix: false,
            follow: *follow,
            recursive: *recursive,
            root_dir: *root_dir,
        };
        let actual = map.match_files(&opts).unwrap();
        assert_eq!(
            json!(actual),
            parse(expected),
            "tree {tree} filter {filter:?} absolute {absolute} follow {follow} recursive {recursive} root {root_dir:?}"
        );
    }
}

#[test]
fn unresolved_symlinks_fail_like_js() {
    let map = map_with(o::Q_ROOT, entries(o::Q_TREE_UNRESOLVED));
    let err = map.match_files(&MatchFilesOptions::default()).unwrap_err();
    assert_eq!(err.0, o::Q_UNRESOLVED_THROW);
}

#[test]
fn get_or_compute_sha1() {
    let dir = TempDir::new("sha1");
    let root = dir.base().to_string();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.js"), "hello\n").unwrap();
    std::fs::write(dir.path().join("src/b.js"), "x").unwrap();
    let expected = o::SHA1S.iter().find(|(c, _)| *c == "hello\n").unwrap().1;
    let mut map = FileMap::new(
        &root,
        FileMapConfig {
            compute_sha1: false,
            plugins: vec![PluginFilter {
                other: true,
                ..PluginFilter::default()
            }],
        },
    );
    map.load(vec![
        ("src/a.js".into(), meta(&json!([1, 6, 0, null, 0]))),
        ("src/b.js".into(), meta(&json!([1, 1, 1, "cached", 0]))),
        ("src/c.js".into(), meta(&json!([1, 1, 1, "", 0]))),
        ("src/gone.js".into(), meta(&json!([1, 1, 1, null, 0]))),
    ])
    .unwrap();

    // Not a file
    assert_eq!(map.get_or_compute_sha1("src").unwrap(), None);
    assert_eq!(map.get_or_compute_sha1("nope.js").unwrap(), None);
    // Cached
    let cached = map.get_or_compute_sha1("src/b.js").unwrap().unwrap();
    assert_eq!((cached.sha1.as_str(), cached.content), ("cached", None));
    // Computed: the content comes back, the plugins the JS would also run too
    let first = map.get_or_compute_sha1("src/a.js").unwrap().unwrap();
    assert_eq!(first.sha1, expected);
    assert_eq!(first.content.as_deref(), Some(&b"hello\n"[..]));
    assert_eq!(first.plugins_to_run, vec![0]);
    // ... and stored, with the visited flag
    let id = map.store().file_id("src/a.js", true).unwrap().unwrap();
    assert_eq!(map.metadata(id).unwrap().sha1.as_deref(), Some(expected));
    assert!(map.metadata(id).unwrap().visited);
    assert_eq!(
        map.get_sha1(&format!("{root}/src/a.js"))
            .unwrap()
            .as_deref(),
        Some(expected)
    );
    let second = map.get_or_compute_sha1("src/a.js").unwrap().unwrap();
    assert_eq!((second.sha1.as_str(), second.content), (expected, None));
    // An empty SHA-1 is not computed again: the JS invariant fires
    let err = map.get_or_compute_sha1("src/c.js").unwrap_err();
    assert_eq!(
        err.message,
        "File processing didn't populate a SHA-1 hash for src/c.js"
    );
    // A missing file is the fs error
    let err = map.get_or_compute_sha1("src/gone.js").unwrap_err();
    assert_eq!(err.code.as_deref(), Some("ENOENT"));
    assert_eq!(
        err.message,
        format!("ENOENT: no such file or directory, open '{root}/src/gone.js'")
    );
}

#[test]
fn a_stale_sha1_is_not_stored() {
    let dir = TempDir::new("stale");
    std::fs::write(dir.path().join("a.js"), "hello\n").unwrap();
    let mut map = FileMap::new(dir.base(), FileMapConfig::default());
    map.load(vec![("a.js".into(), meta(&json!([1, 6, 0, null, 0])))])
        .unwrap();
    let Sha1Job::Compute { id, version, .. } = map.sha1_job("a.js").unwrap() else {
        panic!("expected a job");
    };
    // The file is modified (a change event) while the SHA-1 is being computed.
    map.apply_events(vec![WatchEvent::Touch {
        normal_path: "a.js".into(),
        metadata: meta(&json!([2, 6, 1, null, 0])),
    }])
    .unwrap();
    assert!(!map.finish_sha1(id, version, "stale"));
    assert_eq!(map.metadata(id).unwrap().sha1, None);
    // A job made after the change stores.
    let Sha1Job::Compute { id, version, .. } = map.sha1_job("a.js").unwrap() else {
        panic!("expected a job");
    };
    assert!(map.finish_sha1(id, version, "fresh"));
    assert_eq!(map.metadata(id).unwrap().sha1.as_deref(), Some("fresh"));
}

// ---------------------------------------------------------------------
// Plugin workers
// ---------------------------------------------------------------------

fn named_filter(name: &str) -> PluginFilter {
    let excluded: Vec<String> = o::EXCLUDED_EXTENSIONS
        .iter()
        .map(|s| s.to_string())
        .collect();
    match name {
        "haste_impl_pkgs" => PluginFilter {
            other: true,
            package_json: Some(true),
            ..PluginFilter::default()
        },
        "haste_impl_nopkgs" => PluginFilter {
            other: true,
            package_json: Some(false),
            ..PluginFilter::default()
        },
        "haste_noimpl_pkgs" => PluginFilter {
            other: false,
            package_json: Some(true),
            ..PluginFilter::default()
        },
        "haste_noimpl_nopkgs" => PluginFilter {
            other: false,
            package_json: Some(false),
            ..PluginFilter::default()
        },
        "deps_on" => PluginFilter {
            other: true,
            excluded_extensions: excluded,
            ..PluginFilter::default()
        },
        "deps_off" => PluginFilter {
            other: false,
            excluded_extensions: excluded,
            ..PluginFilter::default()
        },
        other => panic!("filter {other}"),
    }
}

#[test]
fn plugin_filters_match_the_real_plugins() {
    for (name, rows) in o::FILTERS {
        let filter = named_filter(name);
        for (path, is_node_modules, expected) in *rows {
            assert_eq!(
                filter.passes(*is_node_modules, path),
                *expected,
                "{name} {path:?} node_modules {is_node_modules}"
            );
        }
    }
}

#[test]
fn worker_input_matches_file_processor() {
    assert!(o::WORKER_INPUT.len() > 300);
    for (plugins, lazy, compute_sha1, existing, path, kind, expected) in o::WORKER_INPUT {
        let names: Vec<String> = serde_json::from_str(plugins).unwrap();
        let lazy: bool = serde_json::from_str(lazy).unwrap();
        let existing: Option<String> = serde_json::from_str(existing).unwrap();
        let filters: Vec<PluginFilter> = names
            .iter()
            .enumerate()
            .map(|(i, n)| PluginFilter {
                lazy: lazy && i == 0,
                ..named_filter(n)
            })
            .collect();
        let mut metadata = FileMetadata::new(Some(1.0), 7.0, *kind == "symlink");
        metadata.sha1 = existing;
        let actual = match get_worker_input(path, &metadata, *compute_sha1, &filters) {
            None => Value::Null,
            Some(input) => {
                json!({"sha1": input.compute_sha1, "plugins": input.plugins_to_run, "visited": true})
            }
        };
        assert_eq!(
            actual,
            parse(expected),
            "{plugins} lazy {lazy} computeSha1 {compute_sha1} existing {:?} {path} {kind}",
            metadata.sha1
        );
    }
}

fn deps_config(compute_sha1: bool) -> FileMapConfig {
    FileMapConfig {
        compute_sha1,
        plugins: vec![named_filter("deps_on")],
    }
}

#[test]
fn plugin_results_are_stored_and_failures_drop_the_file() {
    let dir = TempDir::new("plugin");
    materialize(&dir);
    let root = dir.sub("$B/root");
    let delta = || Difference {
        changed_files: file_data(
            r#"[["a.js",[1700000000500,3,0,null,0]],["src/x.js",[1700000000500,10,0,null,0]],["b.json",[1700000000250,4,0,null,0]],["gone.js",[1,1,0,null,0]]]"#,
        ),
        removed_files: vec![],
    };

    let mut map = FileMap::new(&root, deps_config(true));
    let session = map.begin_delta(delta()).unwrap();
    // JS: only files whose filter passes get plugin work; .json is excluded.
    let jobs: Vec<(&str, &[usize])> = session
        .plugin_jobs()
        .iter()
        .map(|j| (j.normal_path.as_str(), j.plugins_to_run.as_slice()))
        .collect();
    assert_eq!(
        jobs,
        vec![
            ("a.js", &[0usize][..]),
            ("src/x.js", &[0]),
            ("gone.js", &[0])
        ]
    );
    let changes = map
        .finish_delta(
            session,
            vec![
                PluginJobResult {
                    normal_path: "a.js".into(),
                    outcome: Ok(vec![Some(r#"["dep"]"#.into())]),
                },
                PluginJobResult {
                    normal_path: "src/x.js".into(),
                    outcome: Ok(vec![None]),
                },
                PluginJobResult {
                    normal_path: "gone.js".into(),
                    outcome: Err(FileMapError {
                        code: Some("ENOENT".into()),
                        message: "ENOENT: gone".into(),
                    }),
                },
                // Not a job: ignored
                PluginJobResult {
                    normal_path: "b.json".into(),
                    outcome: Ok(vec![Some("1".into())]),
                },
            ],
        )
        .unwrap();
    let added: Vec<_> = changes
        .added_files
        .iter()
        .map(|(p, _)| p.as_str())
        .collect();
    assert_eq!(added, vec!["a.js", "src/x.js", "b.json"]);
    let a = map
        .metadata(map.store().file_id("a.js", true).unwrap().unwrap())
        .unwrap();
    assert_eq!(a.plugin_slot(0), Some(r#"["dep"]"#));
    assert!(a.visited);
    assert_eq!(a.sha1.as_deref(), Some(sha1_hex(b"xxx").as_str()));
    // `pluginData[i] ?? null`
    let x = map
        .metadata(map.store().file_id("src/x.js", true).unwrap().unwrap())
        .unwrap();
    assert_eq!(x.plugin_slot(0), Some("null"));
    // No worker ran for the .json file (excluded), but its SHA-1 was computed.
    let b = map
        .metadata(map.store().file_id("b.json", true).unwrap().unwrap())
        .unwrap();
    assert_eq!((b.plugin_slot(0), b.visited), (None, true));
    assert!(b.sha1.is_some());
    assert!(map.store().file_id("gone.js", true).unwrap().is_none());

    // Any other error is fatal.
    let mut map = FileMap::new(&root, deps_config(false));
    let session = map.begin_delta(delta()).unwrap();
    let err = map
        .finish_delta(
            session,
            vec![PluginJobResult {
                normal_path: "a.js".into(),
                outcome: Err(FileMapError {
                    code: Some("EIO".into()),
                    message: "EIO: boom".into(),
                }),
            }],
        )
        .unwrap_err();
    assert_eq!(err.message, "EIO: boom");

    // EACCES is fatal as well: the JS list of tolerated codes has the typo
    // 'EACCESS'.
    let mut map = FileMap::new(&root, deps_config(false));
    let session = map.begin_delta(delta()).unwrap();
    let err = map
        .finish_delta(
            session,
            vec![PluginJobResult {
                normal_path: "a.js".into(),
                outcome: Err(FileMapError {
                    code: Some("EACCES".into()),
                    message: "EACCES: nope".into(),
                }),
            }],
        )
        .unwrap_err();
    assert_eq!(err.code.as_deref(), Some("EACCES"));
    // ... while the JS 'EACCESS' (not a real code) is dropped.
    let mut map = FileMap::new(&root, deps_config(false));
    let session = map.begin_delta(delta()).unwrap();
    map.finish_delta(
        session,
        vec![PluginJobResult {
            normal_path: "a.js".into(),
            outcome: Err(FileMapError {
                code: Some("EACCESS".into()),
                message: "".into(),
            }),
        }],
    )
    .unwrap();
    assert!(map.store().file_id("a.js", true).unwrap().is_none());
}

#[test]
fn visited_files_are_not_processed_and_removals_come_first() {
    let dir = TempDir::new("visited");
    materialize(&dir);
    let root = dir.sub("$B/root");
    let mut map = FileMap::new(&root, deps_config(true));
    map.load(vec![
        ("a.js".into(), meta(&json!([1, 3, 1, "old", 0]))),
        ("b.json".into(), meta(&json!([1, 4, 1, "old", 0]))),
    ])
    .unwrap();
    // a.js is re-added visited (the crawler found the same content): kept as
    // is. b.json is removed and re-added unvisited: a remove, then an add, is
    // a modification.
    let changes = map
        .apply_delta(Difference {
            changed_files: file_data(
                r#"[["a.js",[2,3,1,"old",0]],["b.json",[1700000000250,4,0,null,0]]]"#,
            ),
            removed_files: vec!["b.json".into()],
        })
        .unwrap();
    assert_eq!(changes.added_files.len(), 0);
    let modified: Vec<_> = changes
        .modified_files
        .iter()
        .map(|(p, _)| p.as_str())
        .collect();
    // (b.json was removed, then added again: a modification, reported after
    // the plain modification of a.js)
    assert_eq!(modified, vec!["a.js", "b.json"]);
    let a = map
        .metadata(map.store().file_id("a.js", true).unwrap().unwrap())
        .unwrap();
    assert_eq!((a.sha1.as_deref(), a.mtime), (Some("old"), Some(2.0)));
    let b = map
        .metadata(map.store().file_id("b.json", true).unwrap().unwrap())
        .unwrap();
    assert_eq!(
        b.sha1.as_deref(),
        Some("4ad583af22c2e7d40c1c916b2920299155a46464")
    );
}

#[test]
fn touch_processing() {
    let dir = TempDir::new("touch");
    materialize(&dir);
    let root = dir.sub("$B/root");
    // src/x.js is 10 x
    let map = FileMap::new(&root, deps_config(true));
    let input = map.lstat_touch("src/x.js").unwrap();
    assert_eq!(input.size, 10.0);
    assert_eq!(input.mtime, Some((o::BASE_SEC * 1000 + 500) as f64));
    assert!(!input.is_symlink);
    let out = map.process_touch(&input).unwrap();
    assert_eq!(out.plugins_to_run, vec![0]);
    assert!(out.metadata.visited);
    assert_eq!(
        out.metadata.sha1.as_deref(),
        Some(sha1_hex(&[b'x'; 10]).as_str())
    );

    // Without computeSha1 and without plugins there is nothing to do.
    let map = FileMap::new(&root, FileMapConfig::default());
    let out = map.process_touch(&input).unwrap();
    assert!(!out.metadata.visited && out.metadata.sha1.is_none());

    // A symlink is read (resolved against its own directory).
    let input = map.lstat_touch("link_to_file").unwrap();
    assert!(input.is_symlink);
    let out = map.process_touch(&input).unwrap();
    assert_eq!(out.metadata.symlink, Symlink::Target("src/x.js".into()));
    assert!(out.metadata.visited);
    let input = map.lstat_touch("link_abs").unwrap();
    let out = map.process_touch(&input).unwrap();
    assert_eq!(
        out.metadata.symlink,
        Symlink::Target("../outside/o.js".into())
    );

    // Errors carry the Node code and message.
    let err = map.lstat_touch("nope.js").unwrap_err();
    assert_eq!(err.code.as_deref(), Some("ENOENT"));
    assert_eq!(
        err.message,
        format!("ENOENT: no such file or directory, lstat '{root}/nope.js'")
    );
    let mut missing = input.clone();
    missing.normal_path = "nope".into();
    let err = map.process_touch(&missing).unwrap_err();
    assert_eq!(err.code.as_deref(), Some("ENOENT"));
}

// ---------------------------------------------------------------------
// The crawler
// ---------------------------------------------------------------------

fn crawl_case(dir: &TempDir, c: &o::CrawlCase) -> (Difference, Vec<String>) {
    let ignore_pattern = match c.ignore {
        Some((source, flags)) => JsRegExp::new(&format!("{source}|{VCS_DIRECTORIES}"), flags),
        None => JsRegExp::new(VCS_DIRECTORIES, ""),
    }
    .unwrap();
    let extensions: Vec<String> = c.extensions.iter().map(|s| s.to_string()).collect();
    let roots: Vec<String> = c.roots.iter().map(|r| dir.sub(r)).collect();
    let root_dir = dir.sub(c.root_dir);
    let options = CrawlOptions {
        extensions: &extensions,
        ignore: IgnoreRules {
            ignore_pattern: &ignore_pattern,
            retain_all_files: c.retain_all_files,
            health_check_file_prefix: c.health_check_file_prefix,
        },
        include_symlinks: c.include_symlinks,
        root_dir: &root_dir,
        roots: &roots,
    };
    let prev = o::PREV_STATES.iter().find(|(n, _)| *n == c.prev).unwrap().1;
    let mut map = FileMap::new(&root_dir, FileMapConfig::default());
    map.load(
        prev.iter()
            .map(|(p, t)| (p.to_string(), meta(&parse(t))))
            .collect(),
    )
    .unwrap();
    let (diff, warnings) = map.node_crawl(&options, c.subpath).unwrap();
    (diff, warnings.into_iter().map(|w| dir.unsub(&w)).collect())
}

#[test]
fn node_crawl_matches_js() {
    let dir = TempDir::new("crawl");
    materialize(&dir);
    let can_check_unreadable = unreadable_dir_is_unreadable(&dir);
    for c in o::CRAWLS {
        let expects_unreadable = c.warnings.iter().any(|w| w.contains("unreadable_dir"));
        if expects_unreadable && !can_check_unreadable {
            eprintln!("skipping crawl case {} (running as root?)", c.name);
            continue;
        }
        let (diff, warnings) = crawl_case(&dir, c);

        let mut changed: Vec<(String, Value)> = diff
            .changed_files
            .iter()
            .map(|(p, m)| {
                // The mtime of a symlink the crawl just found is the time the
                // fixture was made: check it against the file system instead.
                let mut tuple = m.to_tuple();
                if p.ends_with("link_abs") {
                    tuple[1] = Value::Null;
                }
                let fresh_symlink = m.symlink != Symlink::Regular && !m.visited;
                if fresh_symlink {
                    assert!(m.mtime.is_some(), "{}: {p}", c.name);
                    tuple[0] = Value::Null;
                }
                (p.to_string(), tuple)
            })
            .collect();
        changed.sort_by(|a, b| a.0.cmp(&b.0));
        let expected_changed: Vec<(String, Value)> = c
            .changed
            .iter()
            .map(|(p, t)| (p.to_string(), parse(t)))
            .collect();
        assert_eq!(
            changed.iter().map(|(p, _)| p).collect::<Vec<_>>(),
            expected_changed.iter().map(|(p, _)| p).collect::<Vec<_>>(),
            "{}: changed paths",
            c.name
        );
        for ((p, actual), (_, expected)) in changed.iter().zip(&expected_changed) {
            assert_eq!(actual, expected, "{}: {p}", c.name);
        }

        let mut removed = diff.removed_files.clone();
        removed.sort();
        assert_eq!(
            removed,
            c.removed.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "{}: removed",
            c.name
        );

        let mut warnings = warnings;
        warnings.sort();
        assert_eq!(
            warnings,
            c.warnings.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "{}: warnings",
            c.name
        );
    }
}

/// The mtime of a crawled symlink is its `lstat` mtime.
#[test]
fn crawled_symlinks_carry_their_lstat_mtime() {
    use std::os::unix::fs::MetadataExt;
    let dir = TempDir::new("lstat");
    materialize(&dir);
    let c = o::CRAWLS.iter().find(|c| c.name == "default").unwrap();
    let (diff, _) = crawl_case(&dir, c);
    let mut checked = 0;
    for (p, m) in diff.changed_files.iter() {
        let stat = std::fs::symlink_metadata(dir.path().join("root").join(p)).unwrap();
        assert_eq!(
            m.mtime,
            Some(mtime_ms_from_parts(stat.mtime(), stat.mtime_nsec())),
            "{p}"
        );
        assert_eq!(m.size, stat.len() as f64, "{p}");
        assert_eq!(
            m.symlink == Symlink::Unresolved,
            stat.file_type().is_symlink(),
            "{p}"
        );
        checked += 1;
    }
    assert!(checked > 10);
}

/// The result must not depend on scheduling: the same map, in the same order,
/// with the same warnings, whatever the number of threads.
#[test]
fn node_crawl_is_deterministic() {
    let dir = TempDir::new("determinism");
    materialize(&dir);
    // A wide tree, so that scheduling has something to reorder.
    for d in 0..12 {
        for sub in 0..6 {
            let p = dir.path().join(format!("root/wide/d{d}/s{sub}"));
            std::fs::create_dir_all(&p).unwrap();
            for f in 0..8 {
                std::fs::write(p.join(format!("f{f}.js")), "x").unwrap();
            }
            std::os::unix::fs::symlink("f0.js", p.join("link")).unwrap();
        }
    }
    let c = o::CRAWLS
        .iter()
        .find(|c| c.name == "parent_root_symlinks")
        .unwrap();
    let run = |threads: usize| {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        pool.install(|| {
            let (diff, warnings) = crawl_case(&dir, c);
            (
                file_data_json(&diff.changed_files),
                diff.removed_files,
                warnings,
            )
        })
    };
    let reference = run(1);
    assert!(reference.0.as_array().unwrap().len() > 500);
    for threads in [2, 3, 8, 16, 1, 8] {
        assert_eq!(run(threads), reference, "{threads} threads");
    }
}

#[test]
fn crawl_warns_in_walk_order_with_the_js_text() {
    if cfg!(target_os = "linux") && std::env::var("USER").as_deref() == Ok("root") {
        return;
    }
    let dir = TempDir::new("warn");
    materialize(&dir);
    if !unreadable_dir_is_unreadable(&dir) {
        return;
    }
    let c = o::CRAWLS.iter().find(|c| c.name == "missing_root").unwrap();
    let (_, warnings) = crawl_case(&dir, c);
    // The roots are walked in order: the readable one has no warning.
    assert_eq!(warnings.len(), 2);
    assert!(
        warnings[0].starts_with("Error \"EACCES\" reading contents of \"$B/root/unreadable_dir\"")
    );
    assert_eq!(
        warnings[1],
        "Error \"ENOENT\" reading contents of \"$B/does/not/exist\", skipping. Add this directory to your ignore list to exclude it."
    );
}

// ---------------------------------------------------------------------
// FileMap.build() end to end
// ---------------------------------------------------------------------

fn expected_plugin_data(expected: &Value) -> std::collections::HashMap<String, Value> {
    fn walk(prefix: &str, node: &Value, out: &mut std::collections::HashMap<String, Value>) {
        if node[0] == json!("d") {
            for child in node[1].as_array().unwrap() {
                let name = child[0].as_str().unwrap();
                let path = if prefix.is_empty() {
                    name.to_string()
                } else {
                    format!("{prefix}/{name}")
                };
                walk(&path, &child[1], out);
            }
        } else if let Some(slot) = node[1].as_array().and_then(|t| t.get(5)) {
            out.insert(prefix.to_string(), slot.clone());
        }
    }
    let mut out = std::collections::HashMap::new();
    walk("", expected, &mut out);
    out
}

#[test]
fn building_the_fixture_matches_the_js_file_map() {
    let dir = TempDir::new("e2e");
    materialize(&dir);
    for (name, compute_sha1, enable_symlinks, retain_all_files, roots, deps, expected) in o::E2E {
        let expected = parse(expected);
        let ignore_pattern = JsRegExp::new(VCS_DIRECTORIES, "").unwrap();
        let extensions = vec!["js".to_string(), "json".to_string()];
        let roots: Vec<String> = roots.iter().map(|r| dir.sub(r)).collect();
        let root_dir = dir.sub("$B/root");
        let options = CrawlOptions {
            extensions: &extensions,
            ignore: IgnoreRules {
                ignore_pattern: &ignore_pattern,
                retain_all_files: *retain_all_files,
                health_check_file_prefix: Some(".metro-health-check"),
            },
            include_symlinks: *enable_symlinks,
            root_dir: &root_dir,
            roots: &roots,
        };
        let config = FileMapConfig {
            compute_sha1: *compute_sha1,
            plugins: if *deps {
                vec![named_filter("deps_on")]
            } else {
                vec![]
            },
        };
        let mut map = FileMap::new(&root_dir, config);
        let (diff, warnings) = map.node_crawl(&options, None).unwrap();
        if unreadable_dir_is_unreadable(&dir) {
            assert!(warnings.len() <= 1, "{name}: {warnings:?}");
        }
        let session = map.begin_delta(diff).unwrap();
        // The plugin workers' answers are the JS ones: the test checks which
        // files the jobs are for, and where the data lands.
        let plugin_data = expected_plugin_data(&expected);
        let results: Vec<PluginJobResult> = session
            .plugin_jobs()
            .iter()
            .map(|job| PluginJobResult {
                normal_path: job.normal_path.clone(),
                outcome: Ok(vec![Some(
                    plugin_data
                        .get(&job.normal_path)
                        .unwrap_or_else(|| panic!("{name}: unexpected job {}", job.normal_path))
                        .to_string(),
                )]),
            })
            .collect();
        assert_eq!(results.len(), plugin_data.len(), "{name}: jobs");
        let changes = map.finish_delta(session, results).unwrap();
        assert!(changes.added_files.len() > 5, "{name}");
        assert_eq!(
            snapshot_json(&map.get_serializable_snapshot(), true, true),
            expected,
            "{name}"
        );
    }
}

#[test]
fn processing_errors_match_the_js_file_map() {
    let dir = TempDir::new("e2e-errors");
    materialize(&dir);
    for (name, compute_sha1, files, expected) in o::E2E_ERRORS {
        if *name == "unreadable_file" && !unreadable_dir_is_unreadable(&dir) {
            continue;
        }
        let expected = parse(expected);
        let root_dir = dir.sub("$B/root");
        let mut map = FileMap::new(
            &root_dir,
            FileMapConfig {
                compute_sha1: *compute_sha1,
                plugins: vec![],
            },
        );
        let result = map.apply_delta(Difference {
            changed_files: file_data(files),
            removed_files: vec![],
        });
        match result {
            Ok(_) => assert_eq!(
                json!({"ok": snapshot_json(&map.get_serializable_snapshot(), false, false)}),
                expected,
                "{name}"
            ),
            Err(e) => assert_eq!(
                json!({"err": [e.code, dir.unsub(&e.message)]}),
                expected,
                "{name}"
            ),
        }
    }
}

// ---------------------------------------------------------------------
// The disk cache
// ---------------------------------------------------------------------

fn cache_dir_entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn extras() -> CacheExtras {
    CacheExtras {
        clocks: vec![
            ("".into(), r#""c:123:456""#.into()),
            (
                "sub".into(),
                r#"{"scm":{"mergebase":"abc"},"clock":"c:1"}"#.into(),
            ),
        ],
        plugins: vec![
            ("haste".into(), None),
            ("other".into(), Some("[1,{\"a\":2}]".into())),
        ],
    }
}

#[test]
fn the_cache_round_trips() {
    let dir = TempDir::new("cache");
    let mut trees: Vec<(String, Vec<(String, FileMetadata)>)> = o::EVENTS
        .iter()
        .take(60)
        .map(|(initial, ..)| ("/proj/root".to_string(), entries(initial)))
        .collect();
    trees.push((o::Q_ROOT.to_string(), entries(o::Q_TREE_LOOPS)));
    trees.push((o::Q_ROOT.to_string(), entries(o::Q_TREE_UNRESOLVED)));
    trees.push(("/r".to_string(), vec![]));
    for (i, (root, files)) in trees.into_iter().enumerate() {
        let map = map_with(&root, files);
        let path = dir.path().join(format!("cache-{i}"));
        map.write_cache(&path, &extras()).unwrap();
        let (read, read_extras) =
            FileMap::read_cache(&path, &root, FileMapConfig::default()).unwrap();
        assert_eq!(read_extras, extras(), "{i}");
        // The same tree, in the same order, with the same metadata.
        assert_eq!(listing(&read), listing(&map), "{i}");
        assert_eq!(
            read.get_serializable_snapshot(),
            map.get_serializable_snapshot(),
            "{i}"
        );
        // ... and it can be changed like any other.
        let mut read = read;
        read.apply_events(vec![WatchEvent::Touch {
            normal_path: "added/file.js".into(),
            metadata: meta(&json!([1, 1, 1, null, 0])),
        }])
        .unwrap();
    }
    // Only the cache files are left behind (no temporary files).
    assert!(cache_dir_entries(dir.path())
        .iter()
        .all(|n| n.starts_with("cache-") && !n.ends_with(".tmp")));
}

#[test]
fn the_cache_keeps_a_real_file_map() {
    let dir = TempDir::new("cache-real");
    materialize(&dir);
    let root_dir = dir.sub("$B/root");
    let ignore_pattern = JsRegExp::new(VCS_DIRECTORIES, "").unwrap();
    let extensions = vec!["js".to_string(), "json".to_string()];
    let roots = vec![root_dir.clone()];
    let options = CrawlOptions {
        extensions: &extensions,
        ignore: IgnoreRules {
            ignore_pattern: &ignore_pattern,
            retain_all_files: false,
            health_check_file_prefix: None,
        },
        include_symlinks: true,
        root_dir: &root_dir,
        roots: &roots,
    };
    let mut map = FileMap::new(&root_dir, deps_config(true));
    let (diff, _) = map.node_crawl(&options, None).unwrap();
    let session = map.begin_delta(diff).unwrap();
    let results: Vec<PluginJobResult> = session
        .plugin_jobs()
        .iter()
        .map(|j| PluginJobResult {
            normal_path: j.normal_path.clone(),
            outcome: Ok(vec![Some(format!(r#"["dep of {}"]"#, j.normal_path))]),
        })
        .collect();
    map.finish_delta(session, results).unwrap();

    let path = dir.path().join("cache");
    map.write_cache(&path, &extras()).unwrap();
    let (mut read, _) = FileMap::read_cache(&path, &root_dir, deps_config(true)).unwrap();
    assert_eq!(listing(&read), listing(&map));

    // Crawling again against the cache finds nothing to do.
    let (diff, _) = read.node_crawl(&options, None).unwrap();
    assert_eq!(diff.removed_files, Vec::<String>::new());
    // (symlink mtimes are re-read; regular files are unchanged)
    assert!(
        diff.changed_files
            .iter()
            .all(|(_, m)| m.symlink != Symlink::Regular),
        "{:?}",
        diff.changed_files.keys().collect::<Vec<_>>()
    );
    let changes = read.apply_delta(diff).unwrap();
    assert_eq!(changes.added_files.len(), 0);
}

#[test]
fn a_bad_cache_is_a_miss() {
    let dir = TempDir::new("cache-bad");
    let map = map_with("/proj/root", entries(o::Q_TREE_MAIN));
    let path = dir.path().join("cache");
    map.write_cache(&path, &extras()).unwrap();
    let good = std::fs::read(&path).unwrap();
    let read = |bytes: &[u8], root: &str| {
        let p = dir.path().join("probe");
        std::fs::write(&p, bytes).unwrap();
        FileMap::read_cache(&p, root, FileMapConfig::default()).map(|_| ())
    };
    assert!(read(&good, "/proj/root").is_some());

    // missing, empty, other root
    assert!(FileMap::read_cache(
        &dir.path().join("none"),
        "/proj/root",
        FileMapConfig::default()
    )
    .is_none());
    assert!(read(&[], "/proj/root").is_none());
    assert!(read(&good, "/proj/other").is_none());
    // truncated at every length
    for len in (0..good.len()).step_by(7).chain([good.len() - 1]) {
        assert!(
            read(&good[..len], "/proj/root").is_none(),
            "truncated to {len}"
        );
    }
    // wrong magic
    let mut bad = good.clone();
    bad[0] ^= 0xff;
    assert!(read(&bad, "/proj/root").is_none());
    // garbage after the magic, including huge length prefixes
    for fill in [0xffu8, 0x7f, 0x00, 0x41] {
        let mut garbage = good[..8].to_vec();
        garbage.extend(std::iter::repeat_n(fill, 200));
        assert!(read(&garbage, "/proj/root").is_none(), "fill {fill:#x}");
    }
    // a flipped byte somewhere in the middle never panics
    for at in (8..good.len()).step_by(11) {
        let mut flipped = good.clone();
        flipped[at] ^= 0xff;
        let _ = read(&flipped, "/proj/root");
    }
    // another format version
    let other_version = CacheFile {
        format_version: FORMAT_VERSION + 1,
        root_dir: "/proj/root".into(),
        extras: CacheExtras::default(),
        files: vec![],
    };
    let p = dir.path().join("other-version");
    write_file(&p, &other_version).unwrap();
    assert!(FileMap::read_cache(&p, "/proj/root", FileMapConfig::default()).is_none());
    let same_version = CacheFile {
        format_version: FORMAT_VERSION,
        ..other_version
    };
    write_file(&p, &same_version).unwrap();
    assert!(FileMap::read_cache(&p, "/proj/root", FileMapConfig::default()).is_some());
}

#[test]
fn a_cache_write_is_atomic() {
    let dir = TempDir::new("cache-atomic");
    let map = map_with("/proj/root", entries(o::Q_TREE_MAIN));
    let path = dir.path().join("cache");
    map.write_cache(&path, &extras()).unwrap();
    let before = std::fs::read(&path).unwrap();

    // A write that fails leaves the old file alone and no temporary file.
    let tmp_blocker = dir.path().join("sub");
    std::fs::write(&tmp_blocker, "a file, not a directory").unwrap();
    assert!(map
        .write_cache(&tmp_blocker.join("cache"), &extras())
        .is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(cache_dir_entries(dir.path()), vec!["cache", "sub"]);

    // Overwriting replaces the whole content.
    let other = map_with("/proj/root", vec![]);
    other.write_cache(&path, &CacheExtras::default()).unwrap();
    let (read, read_extras) =
        FileMap::read_cache(&path, "/proj/root", FileMapConfig::default()).unwrap();
    assert_eq!(read.get_all_files(), Vec::<String>::new());
    assert_eq!(read_extras, CacheExtras::default());
    assert_eq!(cache_dir_entries(dir.path()), vec!["cache", "sub"]);
}
