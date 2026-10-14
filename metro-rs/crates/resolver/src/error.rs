//! Port of `errors/*.js` and `MissingFileInHastePackageError` from
//! `resolve.js`. One enum variant per JS error class, with the same fields and
//! the same `message` text.

use std::fmt;

/// `types.js` `FileCandidates`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileCandidates {
    /// We only tried to resolve a specific asset.
    Asset { name: String },
    /// We attempted to resolve a name as being a source file (ex. JavaScript,
    /// JSON...), in which case there can be several extensions we tried, for
    /// example `/js/foo.ios.js`, `/js/foo.js`, etc. for a single prefix
    /// '/js/foo'.
    SourceFile {
        file_path_prefix: String,
        candidate_exts: Vec<String>,
    },
}

/// `types.js` `FileAndDirCandidates`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileAndDirCandidates {
    pub dir: Option<FileCandidates>,
    pub file: Option<FileCandidates>,
}

/// `errors/formatFileCandidates.js`.
pub fn format_file_candidates(candidates: &FileCandidates) -> String {
    match candidates {
        FileCandidates::Asset { name } => name.clone(),
        FileCandidates::SourceFile {
            file_path_prefix,
            candidate_exts,
        } => {
            let mut formatted = file_path_prefix.clone();
            if !candidate_exts.is_empty() {
                // `.filter(Boolean)` drops the empty extension.
                let exts: Vec<&str> = candidate_exts
                    .iter()
                    .map(String::as_str)
                    .filter(|e| !e.is_empty())
                    .collect();
                formatted.push('(');
                formatted.push_str(&exts.join("|"));
                formatted.push(')');
            }
            formatted
        }
    }
}

/// An error raised by a host callback (a user's `resolveRequest` or a scheme
/// resolver throwing). The host decides `name` (the JS `constructor.name`) and
/// `message`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostError {
    pub name: String,
    pub message: String,
}

/// Every error `resolve` can raise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    FailedToResolveNameError {
        dir_paths: Vec<String>,
        extra_paths: Vec<String>,
    },
    FailedToResolvePathError {
        candidates: Box<FileAndDirCandidates>,
    },
    FailedToResolveUnsupportedError {
        message: String,
        cause: Option<Box<ResolveError>>,
    },
    InvalidPackageError {
        /// The file candidates we tried to find to resolve the `main` field.
        file_candidates: Box<FileCandidates>,
        /// The 'index' file candidates we tried for the `main` field.
        index_candidates: Box<FileCandidates>,
        /// The full path to the main module that was attempted.
        main_module_path: String,
        /// Full path the package we were trying to resolve.
        package_json_path: String,
    },
    InvalidPackageConfigurationError {
        /// The description of the error cause.
        reason: String,
        /// Absolute path of the package being resolved.
        package_path: String,
    },
    PackagePathNotExportedError {
        message: String,
    },
    PackageImportNotResolvedError {
        import_specifier: String,
        reason: String,
    },
    MissingFileInHastePackageError {
        candidates: Box<FileAndDirCandidates>,
        module_name: String,
        package_name: String,
        path_in_module: String,
    },
    /// An error raised by a host callback.
    Host(HostError),
}

impl ResolveError {
    pub fn failed_to_resolve_name(dir_paths: Vec<String>, extra_paths: Vec<String>) -> Self {
        ResolveError::FailedToResolveNameError {
            dir_paths,
            extra_paths,
        }
    }

    pub fn invalid_package_configuration(reason: String, package_path: &str) -> Self {
        ResolveError::InvalidPackageConfigurationError {
            reason,
            package_path: package_path.to_string(),
        }
    }

    /// The JS class name (`error.constructor.name`).
    pub fn class_name(&self) -> &str {
        match self {
            ResolveError::FailedToResolveNameError { .. } => "FailedToResolveNameError",
            ResolveError::FailedToResolvePathError { .. } => "FailedToResolvePathError",
            ResolveError::FailedToResolveUnsupportedError { .. } => {
                "FailedToResolveUnsupportedError"
            }
            ResolveError::InvalidPackageError { .. } => "InvalidPackageError",
            ResolveError::InvalidPackageConfigurationError { .. } => {
                "InvalidPackageConfigurationError"
            }
            ResolveError::PackagePathNotExportedError { .. } => "PackagePathNotExportedError",
            ResolveError::PackageImportNotResolvedError { .. } => "PackageImportNotResolvedError",
            ResolveError::MissingFileInHastePackageError { .. } => "MissingFileInHastePackageError",
            ResolveError::Host(e) => &e.name,
        }
    }

    /// The JS `error.message`.
    pub fn message(&self) -> String {
        match self {
            ResolveError::FailedToResolveNameError {
                dir_paths,
                extra_paths,
            } => {
                let display: Vec<&String> = dir_paths.iter().chain(extra_paths.iter()).collect();
                let hint = if display.is_empty() {
                    ""
                } else {
                    " or in these directories:"
                };
                let lines: Vec<String> = display.iter().map(|p| format!("  {p}")).collect();
                format!(
                    "Module does not exist in the Haste module map{hint}\n{}\n",
                    lines.join("\n")
                )
            }
            ResolveError::FailedToResolvePathError { candidates } => {
                format!(
                    "The module could not be resolved because none of these files exist:\n\n{}",
                    format_candidate_lines(candidates, "  * ", "")
                )
            }
            ResolveError::FailedToResolveUnsupportedError { message, .. } => message.clone(),
            ResolveError::InvalidPackageError {
                file_candidates,
                index_candidates,
                main_module_path,
                package_json_path,
            } => format!(
                "The package `{package_json_path}` is invalid because it specifies a `main` \
                 module field that could not be resolved (`{main_module_path}`. None of these \
                 files exist:\n\n  * {}\n  * {}",
                format_file_candidates(file_candidates),
                format_file_candidates(index_candidates)
            ),
            ResolveError::InvalidPackageConfigurationError {
                reason,
                package_path,
            } => format!(
                "The package {package_path} contains an invalid package.json configuration. \
                 Consider raising this issue with the package maintainer(s).\nReason: {reason}"
            ),
            ResolveError::PackagePathNotExportedError { message } => message.clone(),
            ResolveError::PackageImportNotResolvedError {
                import_specifier,
                reason,
            } => {
                format!("The path for {import_specifier} could not be resolved.\nReason: {reason}")
            }
            ResolveError::MissingFileInHastePackageError {
                candidates,
                module_name,
                package_name,
                path_in_module,
            } => format!(
                "While resolving module `{module_name}`, the Haste package `{package_name}` was \
                 found. However the subpath `{path_in_module}` could not be found within the \
                 package. Indeed, none of these files exist:\n\n{}",
                format_candidate_lines(candidates, "  * `", "`")
            ),
            ResolveError::Host(e) => e.message.clone(),
        }
    }
}

/// `[candidates.file, candidates.dir].filter(Boolean).map(...).join('\n')`.
fn format_candidate_lines(candidates: &FileAndDirCandidates, before: &str, after: &str) -> String {
    [&candidates.file, &candidates.dir]
        .into_iter()
        .flatten()
        .map(|c| format!("{before}{}{after}", format_file_candidates(c)))
        .collect::<Vec<_>>()
        .join("\n")
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for ResolveError {}
