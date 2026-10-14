//! `crawlers/node/index.js`: `find` and `nodeCrawl`.
//!
//! The walk is parallel (rayon) and deterministic: the result depends only on
//! the file system, never on scheduling. Entries come out in depth-first
//! pre-order of the `readdir` order of every directory, roots in the order
//! given, and warnings in the same order. (The JS orders its result by when
//! the asynchronous callbacks happen to complete, so it has no order to
//! match.)
//!
//! TODO(windows): POSIX separators only.

use crate::js_regex::JsRegExp;
use crate::metadata::FileMetadata;
use crate::FileData;
use crate::FileMapError;
use metro_rs_file_store::RootPathUtils;
use metro_rs_resolver::path;
use rayon::prelude::*;
use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::MetadataExt;

/// `path.sep + 'node_modules' + path.sep`.
const NODE_MODULES: &str = "/node_modules/";

/// The ignore rules of `FileMap.#buildFileDelta` and `Watcher.#crawl`'s
/// `ignoreForCrawl`.
pub struct IgnoreRules<'a> {
    /// The final `ignorePattern` (`FileMap` has appended `|VCS_DIRECTORIES`).
    pub ignore_pattern: &'a JsRegExp,
    pub retain_all_files: bool,
    /// `healthCheck.filePrefix`: a file whose basename starts with it is
    /// ignored (`Watcher`).
    pub health_check_file_prefix: Option<&'a str>,
}

impl IgnoreRules<'_> {
    /// `ignore(filePath)` for an absolute path.
    pub fn ignore(&self, file_path: &str) -> bool {
        let ignore_matched = self.ignore_pattern.test(file_path);
        (ignore_matched || (!self.retain_all_files && file_path.contains(NODE_MODULES)))
            || self
                .health_check_file_prefix
                .is_some_and(|prefix| path::basename(file_path).starts_with(prefix))
    }
}

/// `CrawlerOptions`, minus what the caller handles (abort signal, perf logger,
/// the previous state, `console`).
pub struct CrawlOptions<'a> {
    pub extensions: &'a [String],
    pub ignore: IgnoreRules<'a>,
    pub include_symlinks: bool,
    pub root_dir: &'a str,
    pub roots: &'a [String],
}

pub struct CrawlOutput {
    pub file_data: FileData,
    /// The `console.warn` messages, in walk order.
    pub warnings: Vec<String>,
}

/// `find(options)`.
pub fn find(options: &CrawlOptions<'_>) -> CrawlOutput {
    let path_utils = RootPathUtils::new(options.root_dir);
    let walker = Walker {
        exts: options.extensions.iter().map(String::as_str).collect(),
        ignore: &options.ignore,
        include_symlinks: options.include_symlinks,
        path_utils: &path_utils,
    };

    let per_root: Vec<Found> = options
        .roots
        .par_iter()
        .map(|root| {
            let root_normal = path_utils.absolute_to_normal(root).into_owned();
            let is_within_root = root_normal != ".." && !root_normal.starts_with("../");
            let dir_prefix = if root.ends_with('/') {
                root.clone()
            } else {
                format!("{root}/")
            };
            walker.search(root, &dir_prefix, &root_normal, is_within_root)
        })
        .collect();

    let mut file_data = FileData::new();
    let mut warnings = Vec::new();
    for found in per_root {
        // `result.set(...)`: a later duplicate replaces the value in place.
        for (normal_path, metadata) in found.files {
            file_data.insert(normal_path, metadata);
        }
        warnings.extend(found.warnings);
    }
    CrawlOutput {
        file_data,
        warnings,
    }
}

#[derive(Default)]
struct Found {
    files: Vec<(String, FileMetadata)>,
    warnings: Vec<String>,
}

struct Walker<'a> {
    exts: HashSet<&'a str>,
    ignore: &'a IgnoreRules<'a>,
    include_symlinks: bool,
    path_utils: &'a RootPathUtils,
}

/// What to do with one directory entry, decided in `readdir` order.
enum Task {
    Dir {
        file: String,
        child_normal: String,
        is_within_root: bool,
    },
    File {
        file: String,
        child_normal: String,
        is_symbolic_link: bool,
    },
}

impl Walker<'_> {
    /// `search(directory, dirPrefix, dirNormal, isWithinRoot)`. `dir_prefix` is
    /// `directory` with a trailing separator, which only a root may already
    /// have (a filesystem root).
    fn search(
        &self,
        directory: &str,
        dir_prefix: &str,
        dir_normal: &str,
        is_within_root: bool,
    ) -> Found {
        let read_dir = match fs::read_dir(directory) {
            Ok(rd) => rd,
            Err(err) => {
                return Found {
                    files: Vec::new(),
                    warnings: vec![readdir_warning(&err, directory)],
                }
            }
        };

        // An error from the directory stream fails the whole `readdir` call in
        // JS: nothing from this directory is kept, and it warns like an
        // initial failure.
        let entries = match collect_entries(read_dir) {
            Ok(entries) => entries,
            Err(err) => {
                return Found {
                    files: Vec::new(),
                    warnings: vec![readdir_warning(&err, directory)],
                }
            }
        };

        let mut tasks = Vec::new();
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            let file = format!("{dir_prefix}{name}");

            // A failed type lookup is `UV_DIRENT_UNKNOWN`: not a directory, not
            // a symlink.
            let file_type = entry.file_type().ok();
            let is_symbolic_link = file_type.is_some_and(|t| t.is_symlink());
            if self.ignore.ignore(&file) || (!self.include_symlinks && is_symbolic_link) {
                continue;
            }

            // Within rootDir, a child's normal path is its parent's plus its
            // name. Outside rootDir, normal paths begin with '..' and can
            // collapse into rootDir, so derive them with absoluteToNormal
            // until the walk enters rootDir itself.
            let child_normal = if !is_within_root {
                self.path_utils.absolute_to_normal(&file).into_owned()
            } else if dir_normal.is_empty() {
                name.clone()
            } else {
                format!("{dir_normal}/{name}")
            };

            if file_type.is_some_and(|t| t.is_dir()) {
                let is_within_root = is_within_root || child_normal.is_empty();
                tasks.push(Task::Dir {
                    file,
                    child_normal,
                    is_within_root,
                });
                continue;
            }

            let ext = path::extname(&name);
            let ext = ext.get(1..).unwrap_or("");
            if !is_symbolic_link && !self.exts.contains(ext) {
                continue;
            }
            tasks.push(Task::File {
                file,
                child_normal,
                is_symbolic_link,
            });
        }

        // Directories recurse and files are lstat-ed in parallel; the ordered
        // `collect` keeps the entries in `readdir` order.
        let parts: Vec<Found> = tasks
            .into_par_iter()
            .map(|task| match task {
                Task::Dir {
                    file,
                    child_normal,
                    is_within_root,
                } => self.search(&file, &format!("{file}/"), &child_normal, is_within_root),
                Task::File {
                    file,
                    child_normal,
                    is_symbolic_link,
                } => match lstat(&file) {
                    Some((mtime, size)) => Found {
                        files: vec![(
                            child_normal,
                            FileMetadata::new(Some(mtime), size, is_symbolic_link),
                        )],
                        warnings: Vec::new(),
                    },
                    None => Found::default(),
                },
            })
            .collect();

        let mut found = Found::default();
        for part in parts {
            found.files.extend(part.files);
            found.warnings.extend(part.warnings);
        }
        found
    }
}

/// All the entries of a directory stream, or the first error: `readdir` is
/// all or nothing.
fn collect_entries<T>(
    entries: impl Iterator<Item = std::io::Result<T>>,
) -> std::io::Result<Vec<T>> {
    entries.collect()
}

/// `Error "${err.code ?? err.message}" reading contents of "${directory}", ...`
fn readdir_warning(err: &std::io::Error, directory: &str) -> String {
    let error = FileMapError::from_io(err, "scandir", directory);
    let code_or_message = error.code.as_deref().unwrap_or(&error.message);
    format!(
        "Error \"{code_or_message}\" reading contents of \"{directory}\", skipping. Add this directory to your ignore list to exclude it."
    )
}

/// `fs.lstat`: `(stat.mtime.getTime(), stat.size)`, or `None` on error.
fn lstat(file: &str) -> Option<(f64, f64)> {
    let stat = fs::symlink_metadata(file).ok()?;
    Some((mtime_ms(&stat), stat.len() as f64))
}

/// `stat.mtime.getTime()`: Node computes `mtimeMs = sec * 1000 + nsec / 1e6`
/// and the `Date` truncates it to a whole millisecond (`TimeClip`).
pub fn mtime_ms(stat: &fs::Metadata) -> f64 {
    mtime_ms_from_parts(stat.mtime(), stat.mtime_nsec())
}

/// [`mtime_ms`] from the seconds and nanoseconds of the stat.
pub fn mtime_ms_from_parts(sec: i64, nsec: i64) -> f64 {
    ((sec as f64) * 1000.0 + (nsec as f64) / 1_000_000.0).trunc()
}
