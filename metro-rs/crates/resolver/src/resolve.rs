//! Port of `resolve.js`.

use crate::config::{Ctx, ResolveRequest, ResolverConfig};
use crate::error::{FileAndDirCandidates, FileCandidates, ResolveError};
use crate::host::{Lookup, ResolverHost};
use crate::package_exports::resolve_package_target_from_exports;
use crate::package_imports::resolve_package_target_from_imports;
use crate::package_resolve::{
    get_package_entry_point, match_subpath_from_main_fields, redirect_module_path, Redirect,
    Subpath,
};
use crate::path;
use crate::resolve_asset::resolve_asset;
use crate::utils::{is_asset_file, posix_to_system_path};
use crate::Resolution;
use serde_json::Value;
use std::collections::HashSet;

/// The outcome of one resolution attempt that did not raise: JS `Result`.
enum Attempt<C> {
    Resolved(Resolution),
    Failed(C),
}

type AttemptResult<C> = Result<Attempt<C>, ResolveError>;

struct ParsedBareSpecifier {
    is_single_part: bool,
    is_valid_package_name: bool,
    first_part: String,
    normalized_specifier: String,
    package_name: String,
    posix_subpath: String,
}

/// Resolve `specifier` as `metro-resolver`'s `resolve` does: if the host has a
/// custom `resolveRequest`, it is used instead of the algorithm.
pub fn resolve(
    config: &ResolverConfig,
    host: &dyn ResolverHost,
    request: &ResolveRequest<'_>,
    specifier: &str,
    platform: Option<&str>,
) -> Result<Resolution, ResolveError> {
    // Prevent infinite recursion in the trivial case: a custom resolver that
    // is the default `resolve` is reported by the host as no custom resolver.
    if let Some(result) = host.custom_resolve_request(config, request, specifier, platform) {
        return result;
    }
    resolve_default(config, host, request, specifier, platform)
}

/// The resolution algorithm without the custom `resolveRequest` hook. This is
/// what `context.resolveRequest` is set to when a custom resolver or scheme
/// resolver is invoked, so delegating hosts call this.
pub fn resolve_default(
    config: &ResolverConfig,
    host: &dyn ResolverHost,
    request: &ResolveRequest<'_>,
    specifier: &str,
    platform: Option<&str>,
) -> Result<Resolution, ResolveError> {
    let ctx = &Ctx {
        config,
        host,
        request,
    };
    let context = ctx;
    let mut scheme_error: Option<ResolveError> = None;

    if is_relative_import(specifier) || path::is_absolute(specifier) {
        return match resolve_module_path(context, specifier, platform)? {
            Attempt::Failed(candidates) => Err(ResolveError::FailedToResolvePathError {
                candidates: Box::new(candidates),
            }),
            Attempt::Resolved(resolution) => Ok(resolution),
        };
    } else if is_subpath_import(specifier) {
        let pkg = context
            .host()
            .get_package_for_module(request.origin_module_path)?;
        let imports_field = pkg
            .as_ref()
            .and_then(|p| non_null(p.package_json.get("imports")));

        match (&pkg, imports_field) {
            (None, _) => {
                return Err(ResolveError::PackageImportNotResolvedError {
                    import_specifier: specifier.to_string(),
                    reason: format!(
                        "Could not find a package.json file relative to module {}",
                        request.origin_module_path
                    ),
                });
            }
            (Some(pkg), None) => {
                return Err(ResolveError::PackageImportNotResolvedError {
                    import_specifier: specifier.to_string(),
                    reason: format!(
                        "Missing field \"imports\" in package.json. Check package.json at: {}",
                        pkg.root_path
                    ),
                });
            }
            (Some(pkg), Some(imports_field)) => {
                match resolve_package_target_from_imports(
                    context,
                    &pkg.root_path,
                    specifier,
                    imports_field,
                    platform,
                ) {
                    Ok(package_imports_result) => return Ok(package_imports_result),
                    Err(e @ ResolveError::PackageImportNotResolvedError { .. }) => {
                        context.host().log_warning(&format!(
                            "{} Falling back to file-based resolution. Consider updating the \
                             call site or checking there is a matching subpath inside \"imports\" of package.json.",
                            e.message()
                        ));
                    }
                    Err(e @ ResolveError::InvalidPackageConfigurationError { .. }) => {
                        context.host().log_warning(&format!(
                            "{} Falling back to file-based resolution.",
                            e.message()
                        ));
                    }
                    Err(e) => return Err(e),
                }
            }
        }
    } else if specifier.find(':').is_some_and(|i| i > 0) && host.can_parse_url(specifier) {
        let scheme_end = specifier.find(':').unwrap_or(0);
        let scheme = specifier[..scheme_end].to_lowercase();
        if let Some(result) = context
            .host()
            .scheme_resolver(&scheme, config, request, specifier, platform)
        {
            return match result {
                Ok(resolution) => Ok(resolution),
                Err(error) => {
                    // A scheme resolver that delegates back into default
                    // resolution (via `context.resolveRequest`) may surface any
                    // resolution error. Preserve those, so callers keep the
                    // structured failure (candidate paths, etc).
                    if is_resolution_error(&error) {
                        return Err(error);
                    }
                    // Otherwise a scheme resolver may throw a plain error to
                    // signal an unsupported specifier (they need not depend on
                    // metro-resolver); surface it as the resolver's typed
                    // error.
                    Err(ResolveError::FailedToResolveUnsupportedError {
                        message: error.message(),
                        cause: Some(Box::new(error)),
                    })
                }
            };
        }

        // TODO: In a breaking change, we should throw this immediately.
        // For now, fall through in case the user is using scheme-like specifiers
        // for Haste, or in extraNodeModules, etc. Throw a scheme-specific error
        // if nothing else works.
        scheme_error = Some(ResolveError::FailedToResolveUnsupportedError {
            message: format!("No resolver is registered for the '{scheme}:' URI scheme."),
            cause: None,
        });
    }

    let origin_module_path = request.origin_module_path;

    let closest_package_to_origin = context.host().get_package_for_module(origin_module_path)?;

    let maybe_redirected_specifier = match &closest_package_to_origin {
        Some(p) => match_subpath_from_main_fields(
            Subpath::One(specifier),
            &p.package_json,
            &config.main_fields,
        ),
        None => None,
    };

    // exclude
    if maybe_redirected_specifier == Some(Redirect::Ignored) {
        return Ok(Resolution::Empty);
    }
    let maybe_redirected_specifier: Option<String> = match maybe_redirected_specifier {
        Some(Redirect::Path(p)) => Some(p),
        _ => None,
    };

    if let (Some(redirected), Some(closest)) =
        (&maybe_redirected_specifier, &closest_package_to_origin)
    {
        // If the specifier was redirected to a relative path
        if is_relative_import(redirected) {
            // Per the "browser" spec: "All paths for browser fields are relative
            // to the package.json file location". `closestPackageToOrigin` is
            // the package that provided the redirect, so join relative paths to
            // its `rootPath`.
            let abs_path = path::resolve(&[&closest.root_path, redirected]);
            return match resolve_module_path(context, &abs_path, platform)? {
                Attempt::Failed(candidates) => Err(ResolveError::FailedToResolvePathError {
                    candidates: Box::new(candidates),
                }),
                Attempt::Resolved(resolution) => Ok(resolution),
            };
        } else if
        // An absolute path inside the file may not match the system's path separator
        path::is_absolute(redirected) || path::win32_is_absolute(redirected) {
            return Err(ResolveError::InvalidPackageConfigurationError {
                package_path: closest.root_path.to_string(),
                reason: format!(
                    "Attempted to redirect import to an absolute path. \
                     This is not allowed by the \"browser\" spec.\
                     \n  From: {origin_module_path}\
                     \n  Import: {specifier}\
                     \n  Attempted redirect: {redirected}"
                ),
            });
        }
    }

    // At this point, maybeRedirectedSpecifier is either null (not redirected),
    // and `specifier` is still bare, or `maybeRedirectedSpecifier` is a
    // redirected bare specifier. Supported bare specifiers are either Haste
    // names, package names, or (rarely) non-package modules under node_modules.
    let parsed_specifier =
        parse_bare_specifier(maybe_redirected_specifier.as_deref().unwrap_or(specifier));

    if request.allow_haste {
        if parsed_specifier.is_single_part {
            let result = context
                .host()
                .resolve_haste_module(&parsed_specifier.first_part)?;
            if let Some(result) = result {
                return Ok(Resolution::SourceFile { file_path: result });
            }
        }
        if parsed_specifier.is_valid_package_name {
            if let Attempt::Resolved(resolution) =
                resolve_haste_package(context, &parsed_specifier, platform)?
            {
                return Ok(resolution);
            }
        }
    }

    // parsedSpecifier is now a non-Haste bare specifier.

    // PACKAGE_SELF_RESOLVE: if the origin lives inside a package whose `name`
    // matches the bare specifier and which declares an `exports` field, resolve
    // the request via that package's `exports`. Per Node.js ESM spec section 6.5
    // (https://nodejs.org/api/esm.html#resolution-algorithm-specification).
    if config.unstable_enable_package_exports {
        if let Some(closest) = &closest_package_to_origin {
            let exports_field = non_null(closest.package_json.get("exports"));
            if let Some(exports_field) = exports_field {
                if closest.package_json.get("name").and_then(Value::as_str)
                    == Some(parsed_specifier.package_name.as_str())
                {
                    match resolve_package_target_from_exports(
                        context,
                        &closest.root_path,
                        &path::join(&[&closest.root_path, &parsed_specifier.posix_subpath]),
                        // packageRelativePath for the requested subpath,
                        // stripping the leading './' so the empty string
                        // represents the package root (matching how
                        // `getPackageForModule` reports `packageRelativePath`).
                        if parsed_specifier.posix_subpath == "." {
                            ""
                        } else {
                            &parsed_specifier.posix_subpath[2..]
                        },
                        exports_field,
                        platform,
                    ) {
                        Ok(package_exports_result) => return Ok(package_exports_result),
                        // NB: Falling back is a departure from the spec, but
                        // retained for backwards compatibility. Remove this in
                        // a breaking change.
                        Err(
                            e @ (ResolveError::PackagePathNotExportedError { .. }
                            | ResolveError::InvalidPackageConfigurationError { .. }),
                        ) => {
                            context.host().log_warning(&format!(
                                "{} Falling back to hierarchical resolution for backwards compatibility.",
                                e.message()
                            ));
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
        }
    }

    if !config.disable_hierarchical_lookup {
        let mut visited: HashSet<String> = HashSet::new();
        let mut next = path::dirname(origin_module_path);
        let mut candidate;
        loop {
            candidate = next;
            let node_modules_path = if candidate.ends_with(path::SEP) {
                format!("{candidate}node_modules")
            } else {
                format!("{candidate}{}node_modules", path::SEP)
            };

            let resolution = resolve_from_node_modules_path(
                context,
                &parsed_specifier,
                platform,
                &node_modules_path,
            )?;
            if let Some(resolution) = resolution {
                return Ok(resolution);
            }

            visited.insert(node_modules_path);
            next = path::dirname(&candidate);
            if candidate == next {
                break;
            }
        }

        // Fall back to `nodeModulesPaths` after hierarchical lookup, similar to $NODE_PATH
        // This is done separately from the else branch below to save an allocation and check `visited`
        for node_modules_path in &config.node_modules_paths {
            // Skip already checked paths, since this could contain duplicates that we already checked
            if visited.contains(node_modules_path) {
                continue;
            }
            let resolution = resolve_from_node_modules_path(
                context,
                &parsed_specifier,
                platform,
                node_modules_path,
            )?;
            if let Some(resolution) = resolution {
                return Ok(resolution);
            }
        }
    } else {
        // Only visit `nodeModulesPaths` when hierarchical lookup is disabled
        for node_modules_path in &config.node_modules_paths {
            let resolution = resolve_from_node_modules_path(
                context,
                &parsed_specifier,
                platform,
                node_modules_path,
            )?;
            if let Some(resolution) = resolution {
                return Ok(resolution);
            }
        }
    }

    let mut extra_node_module_path: Option<String> = None;
    if let Some(extra_node_modules) = &config.extra_node_modules {
        if let Some(new_package_name) = extra_node_modules
            .get(&parsed_specifier.package_name)
            .filter(|s| !s.is_empty())
        {
            let p = path::join(&[new_package_name, &parsed_specifier.posix_subpath]);
            extra_node_module_path = Some(p.clone());
            let resolution = resolve_module_from_target_path(context, platform, &p)?;
            if let Some(resolution) = resolution {
                return Ok(resolution);
            }
        }
    }

    if let Some(scheme_error) = scheme_error {
        // The specifier is a scheme we don't recognise and every other
        // resolution strategy has been exhausted, so fail with a
        // scheme-specific error.
        return Err(scheme_error);
    }

    Err(build_failed_to_resolve_name_error(
        context,
        extra_node_module_path.into_iter().collect(),
    ))
}

/// Treat JSON `null` like a missing field (JS `!= null`).
fn non_null(v: Option<&Value>) -> Option<&Value> {
    v.filter(|v| !v.is_null())
}

fn resolve_from_node_modules_path(
    ctx: &Ctx<'_>,
    parsed_specifier: &ParsedBareSpecifier,
    platform: Option<&str>,
    node_modules_path: &str,
) -> Result<Option<Resolution>, ResolveError> {
    // Insight: The module can only exist if there is a `node_modules` at
    // this path. Redirections cannot succeed, because we will never look
    // beyond a node_modules path segment for finding the closest
    // package.json. Moreover, if the specifier contains a '/' separator,
    // the first part *must* be a real directory, because it is the
    // shallowest path that can possibly contain a redirecting package.json.
    let must_be_directory = if parsed_specifier.posix_subpath != "."
        || parsed_specifier.package_name.len() > parsed_specifier.first_part.len()
    {
        format!(
            "{node_modules_path}{}{}",
            path::SEP,
            parsed_specifier.first_part
        )
    } else {
        node_modules_path.to_string()
    };
    let lookup_result = ctx.host().file_system_lookup(&must_be_directory)?;
    if !matches!(lookup_result, Lookup::Dir { .. }) {
        return Ok(None);
    }
    resolve_module_from_target_path(
        ctx,
        platform,
        &format!(
            "{node_modules_path}{}{}",
            path::SEP,
            posix_to_system_path(&parsed_specifier.normalized_specifier)
        ),
    )
}

fn resolve_module_from_target_path(
    ctx: &Ctx<'_>,
    platform: Option<&str>,
    target_path: &str,
) -> Result<Option<Resolution>, ResolveError> {
    let candidate = match redirect_module_path(ctx, target_path)? {
        Redirect::Ignored => return Ok(Some(Resolution::Empty)),
        Redirect::Path(p) => p,
        Redirect::Unchanged => target_path.to_string(),
    };

    // candidate should be absolute here - we assume that redirectModulePath
    // always returns an absolute path when given an absolute path.
    match resolve_package(ctx, &candidate, platform)? {
        Attempt::Resolved(resolution) => Ok(Some(resolution)),
        Attempt::Failed(_) => Ok(None),
    }
}

fn build_failed_to_resolve_name_error(ctx: &Ctx<'_>, extra_paths: Vec<String>) -> ResolveError {
    let mut node_modules_paths: Vec<String> = Vec::new();

    if !ctx.config.disable_hierarchical_lookup {
        let mut next = path::dirname(ctx.request.origin_module_path);
        let mut candidate;
        loop {
            candidate = next;
            let node_modules_path = if candidate.ends_with(path::SEP) {
                format!("{candidate}node_modules")
            } else {
                format!("{candidate}{}node_modules", path::SEP)
            };
            node_modules_paths.push(node_modules_path);
            next = path::dirname(&candidate);
            if candidate == next {
                break;
            }
        }
    }

    node_modules_paths.extend(ctx.config.node_modules_paths.iter().cloned());
    ResolveError::failed_to_resolve_name(node_modules_paths, extra_paths)
}

fn parse_bare_specifier(specifier: &str) -> ParsedBareSpecifier {
    // TODO(windows): the JS replaces '\' with '/' when `path.sep` is not '/'.
    let normalized = specifier;
    let first_sep_idx = normalized.find('/');
    if let (true, Some(first_sep_idx)) = (normalized.starts_with('@'), first_sep_idx) {
        let second_sep_idx = normalized[first_sep_idx + 1..]
            .find('/')
            .map(|i| i + first_sep_idx + 1);
        return match second_sep_idx {
            None => {
                // @foo/bar (valid scoped, no subpath)
                ParsedBareSpecifier {
                    is_single_part: false,
                    is_valid_package_name: true,
                    first_part: normalized[..first_sep_idx].to_string(),
                    normalized_specifier: normalized.to_string(),
                    package_name: normalized.to_string(),
                    posix_subpath: ".".to_string(),
                }
            }
            Some(second_sep_idx) => {
                // @foo/bar[/subpath] (valid scoped with subpath)
                ParsedBareSpecifier {
                    is_single_part: false,
                    is_valid_package_name: true,
                    first_part: normalized[..first_sep_idx].to_string(),
                    normalized_specifier: normalized.to_string(),
                    package_name: normalized[..second_sep_idx].to_string(),
                    posix_subpath: format!(".{}", &normalized[second_sep_idx..]),
                }
            }
        };
    }
    // foo or @foo, no subpath. Valid if doesn't start with '@'.
    let Some(first_sep_idx) = first_sep_idx else {
        return ParsedBareSpecifier {
            is_single_part: true,
            is_valid_package_name: !normalized.starts_with('@'),
            first_part: normalized.to_string(),
            normalized_specifier: normalized.to_string(),
            package_name: normalized.to_string(),
            posix_subpath: ".".to_string(),
        };
    };
    let package_name = normalized[..first_sep_idx].to_string();
    // foo/subpath, valid, not scoped, with subpath
    ParsedBareSpecifier {
        is_single_part: false,
        is_valid_package_name: true,
        first_part: package_name.clone(),
        normalized_specifier: normalized.to_string(),
        package_name,
        posix_subpath: format!(".{}", &normalized[first_sep_idx..]),
    }
}

/// Resolve any kind of module path, whether it's a file or a directory.
/// For example we may want to resolve './foobar'. The closest
/// `package.json` may define a redirection for this path, for example
/// `/smth/lib/foobar`, that may be further resolved to
/// `/smth/lib/foobar/index.ios.js`.
fn resolve_module_path(
    ctx: &Ctx<'_>,
    to_module_name: &str,
    platform: Option<&str>,
) -> AttemptResult<FileAndDirCandidates> {
    // System-separated absolute path
    let module_path = if path::is_absolute(to_module_name) {
        // TODO(windows): the JS converts '/' to '\' when `path.sep` is '\'.
        to_module_name.to_string()
    } else {
        path::join(&[
            &path::dirname(ctx.request.origin_module_path),
            to_module_name,
        ])
    };
    let redirected_path = match redirect_module_path(ctx, &module_path)? {
        Redirect::Ignored => return Ok(Attempt::Resolved(Resolution::Empty)),
        Redirect::Path(p) => p,
        Redirect::Unchanged => module_path,
    };

    let dir_path = path::dirname(&redirected_path);
    let file_name = path::basename(&redirected_path);

    let file_result: Option<Attempt<FileCandidates>> =
        // require('./foo/') should never resolve to ./foo.js - a trailing slash
        // implies we should resolve as a directory only.
        if redirected_path.ends_with(path::SEP) {
            None
        } else {
            Some(resolve_file(ctx, &dir_path, &file_name, platform)?)
        };

    let file_candidates = match file_result {
        Some(Attempt::Resolved(resolution)) => return Ok(Attempt::Resolved(resolution)),
        Some(Attempt::Failed(candidates)) => Some(candidates),
        None => None,
    };
    let dir_result = resolve_package_entry_point(ctx, &redirected_path, platform)?;
    match dir_result {
        Attempt::Resolved(resolution) => Ok(Attempt::Resolved(resolution)),
        Attempt::Failed(dir_candidates) => Ok(Attempt::Failed(FileAndDirCandidates {
            file: file_candidates,
            dir: Some(dir_candidates),
        })),
    }
}

/// Resolve a specifier as a Haste package.
fn resolve_haste_package(
    ctx: &Ctx<'_>,
    parsed: &ParsedBareSpecifier,
    platform: Option<&str>,
) -> AttemptResult<()> {
    let package_json_path = match ctx.host().resolve_haste_package(&parsed.package_name)? {
        None => return Ok(Attempt::Failed(())),
        Some(p) => p,
    };
    let potential_module_path = path::join(&[&package_json_path, "..", &parsed.posix_subpath]);
    match resolve_package(ctx, &potential_module_path, platform)? {
        Attempt::Resolved(resolution) => Ok(Attempt::Resolved(resolution)),
        Attempt::Failed(candidates) => Err(ResolveError::MissingFileInHastePackageError {
            candidates: Box::new(candidates),
            module_name: parsed.normalized_specifier.clone(),
            package_name: parsed.package_name.clone(),
            path_in_module: parsed.posix_subpath.clone(),
        }),
    }
}

/// Resolve a package entry point or subpath target.
///
/// This should be used when resolving a bare import specifier prefixed with the
/// package name. Use `resolve_module_path` instead to scope to legacy "browser"
/// spec behaviour, which is also applicable to relative and absolute imports.
fn resolve_package(
    ctx: &Ctx<'_>,
    // The absolute path to a file or directory that may be contained within an
    // npm package, e.g. from being joined with `context.extraNodeModules`.
    absolute_candidate_path: &str,
    platform: Option<&str>,
) -> AttemptResult<FileAndDirCandidates> {
    if ctx.config.unstable_enable_package_exports {
        let pkg = ctx.host().get_package_for_module(absolute_candidate_path)?;
        let exports_field = pkg
            .as_ref()
            .and_then(|p| non_null(p.package_json.get("exports")));

        if let (Some(pkg), Some(exports_field)) = (&pkg, exports_field) {
            match resolve_package_target_from_exports(
                ctx,
                &pkg.root_path,
                absolute_candidate_path,
                &pkg.package_relative_path,
                exports_field,
                platform,
            ) {
                Ok(package_exports_result) => return Ok(Attempt::Resolved(package_exports_result)),
                Err(e @ ResolveError::PackagePathNotExportedError { .. }) => {
                    ctx.host().log_warning(&format!(
                        "{} Falling back to file-based resolution. Consider updating the \
                         call site or asking the package maintainer(s) to expose this API.",
                        e.message()
                    ));
                }
                Err(e @ ResolveError::InvalidPackageConfigurationError { .. }) => {
                    ctx.host().log_warning(&format!(
                        "{} Falling back to file-based resolution.",
                        e.message()
                    ));
                }
                Err(e) => return Err(e),
            }
        }
    }

    resolve_module_path(ctx, absolute_candidate_path, platform)
}

/// Attempt to resolve a module path as an npm package entry point, or resolve as
/// a file if no `package.json` file is present.
///
/// Implements legacy (non-exports) package resolution behaviour based on the
/// ["browser" field spec](https://github.com/defunctzombie/package-browser-field-spec):
/// - Looks for a "main" entry point based on `context.mainFields`.
/// - Considers any "main" subpaths after expending source and platform-specific
///   extensions, e.g. `./lib/index` -> `./lib/index.ios.js`.
/// - Falls back to a child `index.js` file, e.g. `./lib` -> `./lib/index.js`.
fn resolve_package_entry_point(
    ctx: &Ctx<'_>,
    package_path: &str,
    platform: Option<&str>,
) -> AttemptResult<FileCandidates> {
    let dir_lookup = ctx.host().file_system_lookup(package_path)?;
    if !matches!(dir_lookup, Lookup::Dir { .. }) {
        return Ok(Attempt::Failed(FileCandidates::SourceFile {
            file_path_prefix: package_path.to_string(),
            candidate_exts: Vec::new(),
        }));
    }

    let package_json_path = path::join(&[package_path, "package.json"]);

    if !ctx.host().does_file_exist(&package_json_path)? {
        return resolve_file(ctx, package_path, "index", platform);
    }

    let root_path = path::dirname(&package_json_path);
    let package_json = ctx
        .host()
        .get_package(&package_json_path)?
        .unwrap_or_else(|| std::sync::Arc::new(Value::Object(Default::default())));

    let main_module_path = path::join(&[&root_path, &get_package_entry_point(ctx, &package_json)]);

    let file_result = resolve_file(
        ctx,
        &path::dirname(&main_module_path),
        &path::basename(&main_module_path),
        platform,
    )?;

    let file_candidates = match file_result {
        Attempt::Resolved(resolution) => return Ok(Attempt::Resolved(resolution)),
        Attempt::Failed(c) => c,
    };

    // Fallback: Attempt to resolve any file at <subpath>/index.js
    let index_result = resolve_file(ctx, &main_module_path, "index", platform)?;

    match index_result {
        Attempt::Resolved(resolution) => Ok(Attempt::Resolved(resolution)),
        Attempt::Failed(index_candidates) => Err(ResolveError::InvalidPackageError {
            package_json_path,
            main_module_path,
            file_candidates: Box::new(file_candidates),
            index_candidates: Box::new(index_candidates),
        }),
    }
}

/// Given a file name for a particular directory, return a resolution result
/// depending on whether or not we found the corresponding module as a file. For
/// example, we might ask for `foo.png`, that resolves to
/// `['/js/beep/foo.ios.png']`. Or we may ask for `boop`, that resolves to
/// `/js/boop.android.ts`. On the other hand this function does not resolve
/// directory-based module names: for example `boop` will not resolve to
/// `/js/boop/index.js` (see `_loadAsDir` for that).
fn resolve_file(
    ctx: &Ctx<'_>,
    dir_path: &str,
    file_name: &str,
    platform: Option<&str>,
) -> AttemptResult<FileCandidates> {
    if is_asset_file(file_name, &ctx.config.asset_exts) {
        return Ok(
            match resolve_asset(ctx, &path::join(&[dir_path, file_name])) {
                None => Attempt::Failed(FileCandidates::Asset {
                    name: file_name.to_string(),
                }),
                Some(asset_resolutions) => Attempt::Resolved(asset_resolutions),
            },
        );
    }

    let mut candidate_exts: Vec<String> = Vec::new();
    let file_path_prefix = path::join(&[dir_path, file_name]);
    let sf_context = SourceFileContext {
        ctx,
        candidate_exts: &mut candidate_exts,
        file_path_prefix: &file_path_prefix,
    };
    let source_file_resolution = resolve_source_file(sf_context, platform)?;
    Ok(match source_file_resolution {
        Some(SourceFileResolution::Path(p)) => {
            Attempt::Resolved(Resolution::SourceFile { file_path: p })
        }
        Some(SourceFileResolution::Empty) => Attempt::Resolved(Resolution::Empty),
        None => Attempt::Failed(FileCandidates::SourceFile {
            file_path_prefix,
            candidate_exts,
        }),
    })
}

struct SourceFileContext<'a, 'b> {
    ctx: &'a Ctx<'a>,
    candidate_exts: &'b mut Vec<String>,
    file_path_prefix: &'b str,
}

/// Either a full path, or `{type: 'empty'}`.
enum SourceFileResolution {
    Path(String),
    Empty,
}

impl SourceFileResolution {
    /// JS truthiness of the value: an empty path string is falsy.
    fn is_truthy(&self) -> bool {
        match self {
            SourceFileResolution::Path(p) => !p.is_empty(),
            SourceFileResolution::Empty => true,
        }
    }
}

/// A particular 'base path' can resolve to a number of possibilities depending
/// on the context. For example `foo/bar` could resolve to `foo/bar.ios.js`, or
/// to `foo/bar.js`. If can also resolve to the bare path `foo/bar` itself, as
/// supported by Node.js resolution. On the other hand it doesn't support
/// `foo/bar.ios`, for historical reasons.
///
/// Return the full path of the resolved module, `None` if no resolution could
/// be found, or `Empty` if redirected to an empty module.
fn resolve_source_file(
    mut context: SourceFileContext<'_, '_>,
    platform: Option<&str>,
) -> Result<Option<SourceFileResolution>, ResolveError> {
    let file_path = resolve_source_file_for_all_exts(&mut context, "", None)?;
    if let Some(fp) = file_path {
        if fp.is_truthy() {
            return Ok(Some(fp));
        }
    }
    let source_exts = &context.ctx.config.source_exts;
    for source_ext in source_exts {
        let ext = format!(".{source_ext}");
        let file_path = resolve_source_file_for_all_exts(&mut context, &ext, platform)?;
        if file_path.is_some() {
            return Ok(file_path);
        }
    }
    Ok(None)
}

/// For a particular extension, ex. `js`, we want to try a few possibilities,
/// such as `foo.ios.js`, `foo.native.js`, and of course `foo.js`. Return the
/// full path of the resolved module, `None` if no resolution could be found, or
/// `Empty` if redirected to an empty module.
fn resolve_source_file_for_all_exts(
    context: &mut SourceFileContext<'_, '_>,
    source_ext: &str,
    platform: Option<&str>,
) -> Result<Option<SourceFileResolution>, ResolveError> {
    if let Some(platform) = platform {
        let ext = format!(".{platform}{source_ext}");
        let file_path = resolve_source_file_for_ext(context, &ext)?;
        if let Some(fp) = file_path {
            if fp.is_truthy() {
                return Ok(Some(fp));
            }
        }
    }
    if context.ctx.config.prefer_native_platform && !source_ext.is_empty() {
        let file_path = resolve_source_file_for_ext(context, &format!(".native{source_ext}"))?;
        if let Some(fp) = file_path {
            if fp.is_truthy() {
                return Ok(Some(fp));
            }
        }
    }
    resolve_source_file_for_ext(context, source_ext)
}

/// We try to resolve a single possible extension. If it doesn't exist, then
/// we make sure to add the extension to a list of candidates for reporting.
fn resolve_source_file_for_ext(
    context: &mut SourceFileContext<'_, '_>,
    extension: &str,
) -> Result<Option<SourceFileResolution>, ResolveError> {
    let mut file_path = String::with_capacity(context.file_path_prefix.len() + extension.len());
    file_path.push_str(context.file_path_prefix);
    file_path.push_str(extension);
    let redirected_path =
        // Any redirections for the bare path have already happened
        if !extension.is_empty() {
            redirect_module_path(context.ctx, &file_path)?
        } else {
            Redirect::Unchanged
        };
    let redirected_path = match redirected_path {
        Redirect::Ignored => return Ok(Some(SourceFileResolution::Empty)),
        Redirect::Path(p) => p,
        Redirect::Unchanged => file_path,
    };
    if let Lookup::File { real_path } = context.ctx.host().file_system_lookup(&redirected_path)? {
        return Ok(Some(SourceFileResolution::Path(real_path)));
    }
    context.candidate_exts.push(extension.to_string());
    Ok(None)
}

/// `/^[.][.]?(?:[/]|$)/.test(filePath)`.
fn is_relative_import(file_path: &str) -> bool {
    let rest = match file_path.strip_prefix('.') {
        None => return false,
        Some(r) => r,
    };
    let rest = rest.strip_prefix('.').unwrap_or(rest);
    rest.is_empty() || rest.starts_with('/')
}

fn is_subpath_import(file_path: &str) -> bool {
    file_path.starts_with('#')
}

/// Whether an error is one of metro-resolver's own resolution failures, which
/// carry structured detail that callers rely on for diagnostics. (Not
/// `MissingFileInHastePackageError`, which the JS list leaves out.)
fn is_resolution_error(error: &ResolveError) -> bool {
    matches!(
        error,
        ResolveError::FailedToResolveNameError { .. }
            | ResolveError::FailedToResolvePathError { .. }
            | ResolveError::FailedToResolveUnsupportedError { .. }
            | ResolveError::InvalidPackageConfigurationError { .. }
            | ResolveError::InvalidPackageError { .. }
            | ResolveError::PackageImportNotResolvedError { .. }
            | ResolveError::PackagePathNotExportedError { .. }
    )
}

/// An approximation of `URL.canParse(input)` (WHATWG URL, no base): the
/// scheme must be syntactically valid, and for authority-based URLs the host
/// and port must be acceptable. It does not do IDNA or IPv4/IPv6 validation.
pub(crate) fn url_can_parse(input: &str) -> bool {
    // Strip leading/trailing C0 control or space, remove tab and newlines.
    let trimmed = input.trim_matches(|c: char| c <= ' ');
    let cleaned: String = trimmed
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    let Some(colon) = cleaned.find(':') else {
        return false;
    };
    let scheme = &cleaned[..colon];
    let mut chars = scheme.chars();
    let valid_scheme = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if !valid_scheme {
        return false;
    }
    let scheme = scheme.to_ascii_lowercase();
    let rest = &cleaned[colon + 1..];
    let is_special = matches!(scheme.as_str(), "http" | "https" | "ws" | "wss" | "ftp");
    if scheme == "file" {
        return true;
    }
    if is_special {
        let after = rest.trim_start_matches(['/', '\\']);
        let end = after.find(['/', '\\', '?', '#']).unwrap_or(after.len());
        return authority_is_valid(&after[..end], true);
    }
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        return authority_is_valid(&after[..end], false);
    }
    true
}

fn authority_is_valid(authority: &str, is_special: bool) -> bool {
    let host_port = match authority.rfind('@') {
        Some(i) => &authority[i + 1..],
        None => authority,
    };
    let (host, port) = if host_port.starts_with('[') {
        match host_port.find(']') {
            None => return false,
            Some(i) => (&host_port[..=i], host_port[i + 1..].strip_prefix(':')),
        }
    } else {
        match host_port.find(':') {
            Some(i) => (&host_port[..i], Some(&host_port[i + 1..])),
            None => (host_port, None),
        }
    };
    if let Some(port) = port {
        if !port.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
        if !port.is_empty() && port.parse::<u32>().map_or(true, |p| p > 65535) {
            return false;
        }
    }
    if is_special && host.is_empty() {
        return false;
    }
    if host.starts_with('[') {
        return true;
    }
    !host.chars().any(|c| {
        c <= ' '
            || matches!(
                c,
                '#' | '/' | '<' | '>' | '?' | '@' | '[' | '\\' | ']' | '^' | '|'
            )
    })
}
