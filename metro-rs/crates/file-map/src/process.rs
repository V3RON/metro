//! `lib/FileProcessor.js`'s `#getWorkerInput`, the SHA-1 of the worker
//! (`worker.js`) and `FileMap.#maybeReadLink`.

use crate::metadata::{FileMetadata, Symlink};
use crate::FileMapError;
use metro_rs_file_store::RootPathUtils;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::fs::File;
use std::io::Read;

/// Which files a plugin's worker runs on: the data form of a
/// `FileMapPluginWorker`'s `filter` and `lazy`.
///
/// The three filters Metro has:
///
/// - `FileDataPlugin` with a custom `filter`: not modelled; use JS.
/// - `HastePlugin`: `node_modules: false`, `other: hasteImplModulePath != null`,
///   `package_json: Some(enableHastePackages)`.
/// - `DependencyPlugin`: `node_modules: false`, `other: computeDependencies`,
///   `excluded_extensions: [...excludedExtensions]`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginFilter {
    /// `plugin.lazy === true`: never run in batches.
    pub lazy: bool,
    /// The filter passes for a file in `node_modules`.
    pub node_modules: bool,
    /// The filter passes for any other file, unless `package_json` or
    /// `excluded_extensions` says otherwise.
    pub other: bool,
    /// For a file outside `node_modules` whose path matches
    /// `/(?:[/\\]|^)package\.json$/`: the answer, replacing `other`.
    pub package_json: Option<bool>,
    /// A file outside `node_modules` does not pass if
    /// `normalPath.substr(normalPath.lastIndexOf('.'))` is in this list. Only
    /// consulted when the file would otherwise pass through `other`.
    pub excluded_extensions: Vec<String>,
}

impl PluginFilter {
    /// `plugin.filter({isNodeModules, normalPath})`.
    pub fn passes(&self, is_node_modules: bool, normal_path: &str) -> bool {
        if is_node_modules {
            return self.node_modules;
        }
        if let Some(answer) = self.package_json {
            if matches_package_json(normal_path) {
                return answer;
            }
        }
        if !self.other {
            return false;
        }
        if self.excluded_extensions.is_empty() {
            return true;
        }
        // `normalPath.substr(normalPath.lastIndexOf('.'))`: with no '.' the
        // index is -1, and `substr(-1)` is the last code unit.
        let ext = match normal_path.rfind('.') {
            Some(i) => &normal_path[i..],
            None => match normal_path.char_indices().next_back() {
                Some((i, _)) => &normal_path[i..],
                None => "",
            },
        };
        !self.excluded_extensions.iter().any(|e| e == ext)
    }
}

/// `/(?:[/\\]|^)package\.json$/`.
fn matches_package_json(p: &str) -> bool {
    match p.strip_suffix("package.json") {
        Some("") => true,
        Some(head) => head.ends_with('/') || head.ends_with('\\'),
        None => false,
    }
}

/// What `#getWorkerInput` decided for one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerInput {
    pub compute_sha1: bool,
    /// Indices of the plugins whose worker runs.
    pub plugins_to_run: Vec<usize>,
}

/// `FileProcessor.#getWorkerInput`. `None` means nothing to process.
pub fn get_worker_input(
    normal_path: &str,
    metadata: &FileMetadata,
    req_compute_sha1: bool,
    plugins: &[PluginFilter],
) -> Option<WorkerInput> {
    if metadata.symlink != Symlink::Regular {
        // Only process regular files
        return None;
    }

    let compute_sha1 = req_compute_sha1 && metadata.sha1.is_none();

    let node_modules_idx = normal_path.find("node_modules/");
    // Path may begin 'node_modules/' or contain '/node_modules/'.
    let is_node_modules = match node_modules_idx {
        Some(0) => true,
        Some(i) => normal_path.as_bytes()[i - 1] == b'/',
        None => false,
    };

    // Indices of plugins with a passing filter
    let plugins_to_run: Vec<usize> = plugins
        .iter()
        .enumerate()
        .filter(|(_, p)| !p.lazy && p.passes(is_node_modules, normal_path))
        .map(|(i, _)| i)
        .collect();

    if !compute_sha1 && plugins_to_run.is_empty() {
        // Nothing to process
        return None;
    }

    // Use a cheaper worker configuration for node_modules files, because
    // they may never be Haste modules or packages.
    //
    // Note that we'd only expect node_modules files to reach this point if
    // retainAllFiles is true, or they're touched during watch mode.
    if is_node_modules {
        if compute_sha1 {
            return Some(WorkerInput {
                compute_sha1: true,
                plugins_to_run,
            });
        }
        return None;
    }

    Some(WorkerInput {
        compute_sha1,
        plugins_to_run,
    })
}

/// `sha1hex(fs.readFileSync(filePath))`; also returns the content.
pub fn read_and_hash(absolute_path: &str) -> Result<(String, Vec<u8>), FileMapError> {
    let mut file =
        File::open(absolute_path).map_err(|e| FileMapError::from_io(&e, "open", absolute_path))?;
    let mut content = Vec::new();
    file.read_to_end(&mut content)
        .map_err(|e| FileMapError::from_io(&e, "read", absolute_path))?;
    Ok((sha1_hex(&content), content))
}

/// `createHash('sha1').update(content).digest('hex')`.
pub fn sha1_hex(content: &[u8]) -> String {
    let digest = Sha1::digest(content);
    let mut out = String::with_capacity(40);
    for b in digest {
        out.push(char::from_digit(u32::from(b >> 4), 16).expect("nibble"));
        out.push(char::from_digit(u32::from(b & 15), 16).expect("nibble"));
    }
    out
}

/// The SHA-1 half of the worker, without keeping the content.
pub fn hash_file(absolute_path: &str) -> Result<String, FileMapError> {
    read_and_hash(absolute_path).map(|(sha1, _)| sha1)
}

/// `FileMap.#maybeReadLink` for a symlink whose target has not been read:
/// `readlink`, then `resolveSymlinkToNormal`. Returns the stored target.
pub fn read_link(path_utils: &RootPathUtils, normal_path: &str) -> Result<String, FileMapError> {
    let absolute = path_utils.normal_to_absolute(normal_path);
    let target = std::fs::read_link(&absolute)
        .map_err(|e| FileMapError::from_io(&e, "readlink", &absolute))?;
    // `normalizePathSeparatorsToPosix` is the identity on POSIX.
    Ok(path_utils.resolve_symlink_to_normal(normal_path, &target.to_string_lossy()))
}
