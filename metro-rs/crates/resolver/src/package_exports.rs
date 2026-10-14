//! Port of `PackageExportsResolve.js`: modern package resolution based on the
//! "exports" field.

use crate::config::Ctx;
use crate::error::ResolveError;
use crate::path;
use crate::resolve_asset::resolve_asset;
use crate::utils::{
    is_asset_file, is_subpath_defined_in_exports_like, match_subpath_from_exports_like,
    system_to_posix_path, NormalizedExportsLikeMap,
};
use crate::{Lookup, Resolution};
use serde_json::{Map, Value};
use std::borrow::Cow;

/// Resolve a package subpath based on the entry points defined in the package's
/// "exports" field. If there is no match for the given subpath (which may be
/// augmented by resolution of conditional exports for the passed `context`),
/// returns a `PackagePathNotExportedError`.
///
/// Implements modern package resolution behaviour based on the [Package Entry
/// Points spec](https://nodejs.org/docs/latest-v19.x/api/packages.html#package-entry-points).
///
/// Errors: `InvalidPackageConfigurationError` if configuration specified by
/// `exports_field` is invalid; `PackagePathNotExportedError` when the requested
/// subpath is not exported.
pub fn resolve_package_target_from_exports(
    ctx: &Ctx<'_>,
    // The path to the containing npm package directory.
    package_path: &str,
    // The unresolved absolute path to the target module. This will be converted
    // to a package-relative subpath for comparison.
    module_path: &str,
    package_relative_path: &str,
    exports_field: &Value,
    platform: Option<&str>,
) -> Result<Resolution, ResolveError> {
    let create_config_error =
        |reason: String| ResolveError::invalid_package_configuration(reason, package_path);

    let subpath = get_exports_subpath(package_relative_path);
    let export_map = normalize_exports_field(exports_field, &create_config_error)?;

    if !is_subpath_defined_in_exports_like(&export_map, &subpath) {
        return Err(ResolveError::PackagePathNotExportedError {
            message: format!(
                "Attempted to import the module \"{module_path}\" which is not listed \
                 in the \"exports\" of \"{package_path}\" under the requested subpath \
                 \"{subpath}\"."
            ),
        });
    }

    let (target, pattern_match) = match_subpath_from_exports_like(
        ctx,
        &subpath,
        &export_map,
        platform,
        &create_config_error,
    )?;

    if let Some(target) = target {
        // `target.slice(2)`: reduceExportsLikeMap guarantees a "./" prefix.
        let invalid_segment_in_target = find_invalid_path_segment(&target[2..]);

        if let Some(invalid_segment) = invalid_segment_in_target {
            return Err(create_config_error(format!(
                "The target for \"{subpath}\" defined in \"exports\" is \"{target}\", \
                 however this value is an invalid subpath or subpath pattern \
                 because it includes \"{invalid_segment}\"."
            )));
        }

        let file_path = path::join(&[
            package_path,
            &match &pattern_match {
                Some(m) => target.replace('*', m),
                None => target.to_string(),
            },
        ]);

        if is_asset_file(&file_path, &ctx.config.asset_exts) {
            if let Some(asset_result) = resolve_asset(ctx, &file_path) {
                return Ok(asset_result);
            }
        }

        if let Lookup::File { real_path } = ctx.host().file_system_lookup(&file_path) {
            return Ok(Resolution::SourceFile {
                file_path: real_path,
            });
        }

        return Err(create_config_error(format!(
            "The resolution for \"{module_path}\" defined in \"exports\" is {file_path}, \
             however this file does not exist."
        )));
    }

    Err(ResolveError::PackagePathNotExportedError {
        message: format!(
            "Attempted to import the module \"{module_path}\" which is listed in the \
             \"exports\" of \"{package_path}\", however no match was resolved for this \
             request (platform = {}).",
            platform.unwrap_or("null")
        ),
    })
}

/// Convert a module path to the package-relative subpath key to attempt for
/// "exports" field lookup.
fn get_exports_subpath(package_subpath: &str) -> String {
    if package_subpath.is_empty() {
        ".".to_string()
    } else {
        format!("./{}", system_to_posix_path(package_subpath))
    }
}

/// JS `Object.keys` on an arbitrary JSON value (strings and arrays have index
/// keys).
pub(crate) fn object_keys(value: &Value) -> Vec<String> {
    match value {
        Value::Object(map) => map.keys().cloned().collect(),
        Value::Array(a) => (0..a.len()).map(|i| i.to_string()).collect(),
        Value::String(s) => (0..crate::utils::js_len(s))
            .map(|i| i.to_string())
            .collect(),
        _ => Vec::new(),
    }
}

/// Normalise an "exports"-like field by parsing string shorthand and conditions
/// shorthand at root, and flattening any legacy Node.js <13.7 array values.
///
/// See https://nodejs.org/docs/latest-v19.x/api/packages.html#exports-sugar.
///
/// (The JS caches the result per package.json object in a WeakMap; this port
/// recomputes it, which is cheap because values are borrowed.)
fn normalize_exports_field<'a>(
    exports_field: &'a Value,
    create_config_error: &dyn Fn(String) -> ResolveError,
) -> Result<NormalizedExportsLikeMap<'a>, ResolveError> {
    let root_value: Option<&Value>;

    if let Value::String(_) = exports_field {
        return Ok(vec![(".", Cow::Borrowed(exports_field))]);
    }

    if let Value::Array(arr) = exports_field {
        // If an array of strings, use first value with valid specifier (root shorthand)
        if arr.iter().all(Value::is_string) {
            root_value = arr
                .iter()
                .find(|v| v.as_str().is_some_and(|s| s.starts_with("./")));
        } else {
            // Otherwise, should be a condition map and fallback string (Node.js <13.7)
            root_value = arr.first();
        }
    } else {
        root_value = Some(exports_field);
    }

    let root_value = match root_value {
        None | Some(Value::Null) | Some(Value::Array(_)) => {
            return Err(create_config_error(
                "Could not parse non-standard array value at root of \"exports\" field."
                    .to_string(),
            ));
        }
        Some(v) => v,
    };

    if let Value::String(_) = root_value {
        return Ok(vec![(".", Cow::Borrowed(root_value))]);
    }

    let first_level_keys = object_keys(root_value);
    let subpath_keys = first_level_keys
        .iter()
        .filter(|k| k.starts_with('.'))
        .count();
    let import_keys = first_level_keys
        .iter()
        .filter(|k| k.starts_with('#'))
        .count();

    if import_keys + subpath_keys == first_level_keys.len() {
        return flatten_legacy_subpath_values(root_value, create_config_error);
    }

    if subpath_keys != 0 {
        return Err(create_config_error(
            "The \"exports\" field cannot have keys which are both subpaths and \
             condition names at the same level."
                .to_string(),
        ));
    }

    // Conditions shorthand: the whole object is the value of ".".
    let flattened = flatten_legacy_subpath_values(root_value, create_config_error)?;
    let mut map = Map::new();
    for (k, v) in flattened {
        map.insert(k.to_string(), v.into_owned());
    }
    Ok(vec![(".", Cow::Owned(Value::Object(map)))])
}

/// Flatten legacy Node.js <13.7 array subpath values in an exports mapping.
fn flatten_legacy_subpath_values<'a>(
    export_map: &'a Value,
    create_config_error: &dyn Fn(String) -> ResolveError,
) -> Result<NormalizedExportsLikeMap<'a>, ResolveError> {
    let mut result: NormalizedExportsLikeMap<'a> = Vec::new();
    // `Object.entries` of a non-object yields nothing (strings and arrays
    // would yield index entries, which never match a real exports map).
    if let Value::Object(map) = export_map {
        for (subpath, value) in map {
            // We do not support empty or nested arrays (non-standard)
            if let Value::Array(arr) = value {
                if arr.is_empty() || arr[0].is_array() {
                    return Err(create_config_error(
                        "Could not parse non-standard array value in \"exports\" field."
                            .to_string(),
                    ));
                }
                result.push((subpath, Cow::Borrowed(&arr[0])));
            } else {
                result.push((subpath, Cow::Borrowed(value)));
            }
        }
    }
    Ok(result)
}

fn find_invalid_path_segment(subpath: &str) -> Option<&str> {
    subpath.split(['\\', '/']).find(|segment| {
        segment.is_empty() || *segment == "." || *segment == ".." || *segment == "node_modules"
    })
}
