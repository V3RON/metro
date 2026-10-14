//! Port of `PackageImportsResolve.js`: the package "imports" field.

use crate::config::Ctx;
use crate::error::ResolveError;
use crate::package_exports::object_keys;
use crate::path;
use crate::resolve_asset::resolve_asset;
use crate::utils::{
    is_asset_file, is_subpath_defined_in_exports_like, match_subpath_from_exports_like,
    NormalizedExportsLikeMap,
};
use crate::{Lookup, Resolution};
use serde_json::Value;
use std::borrow::Cow;

/// Resolve a package subpath based on the entry points defined in the package's
/// "imports" field. If there is no match for the given subpath (which may be
/// augmented by resolution of conditional exports for the passed `context`),
/// returns a `PackageImportNotResolvedError`.
///
/// Implementation of PACKAGE_IMPORTS_RESOLVE described in https://nodejs.org/api/esm.html
///
/// Errors: `InvalidPackageConfigurationError` if configuration specified by
/// `imports_map` is invalid.
pub fn resolve_package_target_from_imports(
    ctx: &Ctx<'_>,
    // The absolute path to the package root.
    package_path: &str,
    import_path: &str,
    imports_map: &Value,
    platform: Option<&str>,
) -> Result<Resolution, ResolveError> {
    let create_config_error =
        |reason: String| ResolveError::invalid_package_configuration(reason, package_path);

    let first_level_keys = object_keys(imports_map);
    let keys_without_prefix = first_level_keys
        .iter()
        .filter(|k| !k.starts_with('#'))
        .count();
    if first_level_keys.is_empty() {
        return Err(create_config_error(
            "The \"imports\" field cannot be empty".to_string(),
        ));
    } else if keys_without_prefix != 0 {
        return Err(create_config_error(
            "The \"imports\" field cannot have keys which do not start with #".to_string(),
        ));
    }

    // `Object.entries(importsMap)`; only objects get here with keys that all
    // start with '#' (string and array values fail the check above).
    let normalized_map: NormalizedExportsLikeMap = match imports_map {
        Value::Object(map) => map
            .iter()
            .map(|(k, v)| (k.as_str(), Cow::Borrowed(v)))
            .collect(),
        _ => Vec::new(),
    };
    if !is_subpath_defined_in_exports_like(&normalized_map, import_path) {
        return Err(ResolveError::PackageImportNotResolvedError {
            import_specifier: import_path.to_string(),
            reason: format!(
                "\"{import_path}\" could not be matched using \"imports\" of {package_path}"
            ),
        });
    }

    let (target, pattern_match) = match_subpath_from_exports_like(
        ctx,
        import_path,
        &normalized_map,
        platform,
        &create_config_error,
    )?;

    let Some(target) = target else {
        return Err(ResolveError::PackageImportNotResolvedError {
            import_specifier: import_path.to_string(),
            // The missing space before "however" is in the JS message.
            reason: format!(
                "\"{import_path}\" which matches a subpath \"imports\" in {package_path}\
                 however no match was resolved for this request (platform = {}).",
                platform.unwrap_or("null")
            ),
        });
    };

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

    Err(create_config_error(format!(
        "The resolved path for \"{import_path}\" defined in \"imports\" is {file_path}, \
         however this file does not exist."
    )))
}
