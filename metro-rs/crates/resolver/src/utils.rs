//! Port of `utils/*.js`: `isAssetFile`, `paths`, `matchSubpathPattern`,
//! `isSubpathDefinedInExportsLike`, `matchSubpathFromExportsLike` and
//! `reduceExportsLikeMap`.

use crate::config::Ctx;
use crate::error::ResolveError;
use crate::path;
use serde_json::Value;
use std::borrow::Cow;
use std::collections::HashSet;

/// JS `string.length` (UTF-16 code units).
pub fn js_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// `utils/paths.js` `systemToPosixPath`. TODO(windows): identity on POSIX.
pub fn system_to_posix_path(p: &str) -> String {
    p.to_string()
}

/// `utils/paths.js` `posixToSystemPath`. TODO(windows): identity on POSIX.
pub fn posix_to_system_path(p: &str) -> String {
    p.to_string()
}

/// Determine if a file path should be considered an asset file based on the
/// given `assetExts`.
pub fn is_asset_file(file_path: &str, asset_exts: &HashSet<String>) -> bool {
    let base_name = path::basename(file_path);
    let bytes = base_name.as_bytes();
    for i in (0..bytes.len()).rev() {
        if bytes[i] == b'.' {
            let ext = &base_name[i + 1..];
            if asset_exts.contains(ext) {
                return true;
            }
        }
    }
    false
}

/// If a subpath pattern expands to the passed subpath, return the subpath match
/// (value to substitute for '*'). Otherwise, return `None`.
///
/// See https://nodejs.org/docs/latest-v19.x/api/packages.html#subpath-patterns.
pub fn match_subpath_pattern(subpath_pattern: &str, subpath: &str) -> Option<String> {
    let mut parts = subpath_pattern.split('*');
    let pattern_base = parts.next().unwrap_or("");
    // JS would throw on a pattern without '*'; callers guarantee at least one.
    let pattern_trailer = parts.next().unwrap_or("");

    if subpath.starts_with(pattern_base)
        && (pattern_trailer.is_empty()
            || (subpath.ends_with(pattern_trailer) && js_len(subpath) >= js_len(subpath_pattern)))
    {
        return Some(
            subpath[pattern_base.len()..subpath.len() - pattern_trailer.len()].to_string(),
        );
    }

    None
}

/// An "exports"-like map after normalisation (types.js
/// `NormalizedExportsLikeMap`). Entries are in key order. Values borrow from
/// the package.json unless normalisation had to build a new object.
pub type NormalizedExportsLikeMap<'a> = Vec<(&'a str, Cow<'a, Value>)>;

/// `types.js` `FlattenedExportMap`: subpath to target, in key order.
pub type FlattenedExportMap<'a> = Vec<(&'a str, Option<&'a str>)>;

/// Identifies whether the given subpath is defined in the given "exports"-like
/// mapping. Does not reduce exports conditions (therefore does not identify
/// whether the subpath is mapped to a value).
pub fn is_subpath_defined_in_exports_like(
    exports_like_map: &NormalizedExportsLikeMap<'_>,
    subpath: &str,
) -> bool {
    if exports_like_map.iter().any(|(k, _)| *k == subpath) {
        return true;
    }

    // Attempt to match after expanding any subpath pattern keys
    for (key, _) in exports_like_map {
        if key.split('*').count() == 2 && match_subpath_pattern(key, subpath).is_some() {
            return true;
        }
    }

    false
}

/// Get the mapped replacement for the given subpath.
///
/// Implements modern package resolution behaviour based on the [Package Entry
/// Points spec](https://nodejs.org/docs/latest-v19.x/api/packages.html#package-entry-points).
///
/// Returns `(target, pattern_match)`.
pub fn match_subpath_from_exports_like<'a>(
    ctx: &Ctx<'_>,
    // The package-relative subpath (beginning with '.') to match against either
    // an exact subpath key or subpath pattern key in "exports".
    subpath: &str,
    exports_like_map: &'a NormalizedExportsLikeMap<'a>,
    platform: Option<&str>,
    create_config_error: &dyn Fn(String) -> ResolveError,
) -> Result<(Option<&'a str>, Option<String>), ResolveError> {
    let mut condition_names: HashSet<&str> = HashSet::new();
    condition_names.insert("default");
    condition_names.insert(if ctx.request.is_esm_import {
        "import"
    } else {
        "require"
    });
    for name in &ctx.config.unstable_condition_names {
        condition_names.insert(name);
    }
    if let Some(platform) = platform {
        if let Some(names) = ctx.config.unstable_conditions_by_platform.get(platform) {
            for name in names {
                condition_names.insert(name);
            }
        }
    }

    let exports_like_map_after_conditions =
        reduce_exports_like_map(exports_like_map, &condition_names, create_config_error)?;

    let mut target: Option<&str> = None;
    let mut pattern_match: Option<String> = None;

    // Attempt to match subpath pattern keys only if there is no exact key. An
    // exact key takes precedence even when its target is null. See step 2 of
    // `PACKAGE_IMPORTS_EXPORTS_RESOLVE` in:
    // https://nodejs.org/api/esm.html#resolution-algorithm-specification
    if let Some((_, value)) = exports_like_map_after_conditions
        .iter()
        .find(|(k, _)| *k == subpath)
    {
        target = *value;
    } else {
        // Gather keys which are subpath patterns in descending order of
        // specificity. For ordering, see `PATTERN_KEY_COMPARE` in:
        // https://nodejs.org/api/esm.html#resolution-algorithm-specification
        let mut expansion_keys: Vec<(&str, usize)> = exports_like_map_after_conditions
            .iter()
            .filter_map(|(key, _)| {
                key.find('*')
                    .map(|byte_idx| (*key, js_len(&key[..byte_idx])))
            })
            .collect();
        // `sort_by` is stable, like `Array.prototype.sort`.
        expansion_keys.sort_by(|a, b| {
            if a.1 == b.1 {
                // If wildcards are in equal positions, the longer key is more
                // specific
                return js_len(b.0).cmp(&js_len(a.0));
            }
            b.1.cmp(&a.1)
        });

        for (key, _) in expansion_keys {
            let value = exports_like_map_after_conditions
                .iter()
                .find(|(k, _)| *k == key)
                .and_then(|(_, v)| *v);

            pattern_match = match_subpath_pattern(key, subpath);

            if pattern_match.is_some() {
                target = value;
                break;
            }
        }
    }

    Ok((target, pattern_match))
}

/// Reduce an "exports"-like mapping to a flat subpath mapping after resolving
/// conditional exports.
pub fn reduce_exports_like_map<'a>(
    exports_like_map: &'a NormalizedExportsLikeMap<'a>,
    condition_names: &HashSet<&str>,
    create_config_error: &dyn Fn(String) -> ResolveError,
) -> Result<FlattenedExportMap<'a>, ResolveError> {
    let mut result: FlattenedExportMap<'a> = Vec::new();

    for (subpath, value) in exports_like_map {
        match reduce_conditional_export(value, condition_names) {
            // If a subpath has no resolution for the passed `conditionNames`,
            // do not include it in the result. (This includes only explicit
            // `null` values, which may conditionally hide higher-specificity
            // subpath patterns.)
            Reduced::NoMatch => {}
            Reduced::Null => result.push((subpath, None)),
            Reduced::Str(s) => result.push((subpath, Some(s))),
        }
    }

    for (_, value) in &result {
        if let Some(value) = value {
            if !value.starts_with("./") {
                return Err(create_config_error(
                    "One or more mappings for subpaths defined in \"exports\" are invalid. \
                     All values must begin with \"./\"."
                        .to_string(),
                ));
            }
        }
    }

    Ok(result)
}

enum Reduced<'a> {
    NoMatch,
    Null,
    Str(&'a str),
}

static NULL: Value = Value::Null;

/// Reduce an "exports"-like subpath value after asserting the passed
/// `conditionNames` in any nested conditions.
///
/// See https://nodejs.org/docs/latest-v19.x/api/packages.html#conditional-exports.
fn reduce_conditional_export<'a>(
    subpath_value: &'a Value,
    condition_names: &HashSet<&str>,
) -> Reduced<'a> {
    let mut reduced_value = subpath_value;

    loop {
        match reduced_value {
            Value::Null => return Reduced::Null,
            Value::String(s) => {
                // JS uses the string 'no-match' as the sentinel, so a target
                // spelled exactly "no-match" is also treated as no match.
                return if s == "no-match" {
                    Reduced::NoMatch
                } else {
                    Reduced::Str(s)
                };
            }
            Value::Object(map) => {
                // when conditions are present and default is not specified
                // the default condition is implicitly set to null, to allow
                // for restricting access to unexported internals of a package.
                let mut no_match = map.contains_key("default");
                let mut next: &Value = &NULL;

                for (condition_name, value) in map {
                    if condition_names.contains(condition_name.as_str()) {
                        next = value;
                        no_match = false;
                        break;
                    }
                }

                if no_match {
                    return Reduced::NoMatch;
                }
                reduced_value = next;
            }
            // Arrays have no matching condition names in JS (their keys are
            // indices) and reduce to null. Numbers and booleans make JS throw
            // a TypeError; we treat them as null as well.
            _ => return Reduced::Null,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn match_subpath_pattern_basics() {
        assert_eq!(
            match_subpath_pattern("./features/*.js", "./features/x/y.js"),
            Some("x/y".to_string())
        );
        assert_eq!(
            match_subpath_pattern("./a/*", "./a/b"),
            Some("b".to_string())
        );
        assert_eq!(match_subpath_pattern("./a/*", "./b/c"), None);
        // The subpath must be at least as long as the pattern.
        assert_eq!(match_subpath_pattern("./a/*/x", "./a/x"), None);
        assert_eq!(match_subpath_pattern("./a/*x", "./a/x"), None);
        assert_eq!(
            match_subpath_pattern("./a/*x", "./a/bx"),
            Some("b".to_string())
        );
    }

    #[test]
    fn reduce_follows_key_order_and_default() {
        let names: HashSet<&str> = ["default", "require"].into_iter().collect();
        let err = |r: String| ResolveError::invalid_package_configuration(r, "/p");
        let v1 = json!({"import": "./esm.js", "require": "./cjs.js", "default": "./d.js"});
        let v2 = json!({"default": "./d.js", "require": "./cjs.js"});
        let v3 = json!({"import": "./esm.js", "default": "./d.js"});
        let v4 = json!({"import": "./esm.js"});
        let map: NormalizedExportsLikeMap = vec![
            ("./a", Cow::Borrowed(&v1)),
            ("./b", Cow::Borrowed(&v2)),
            ("./c", Cow::Borrowed(&v3)),
            ("./d", Cow::Borrowed(&v4)),
        ];
        let reduced = reduce_exports_like_map(&map, &names, &err).unwrap();
        // First matching key in object order wins: "require" comes before
        // "default" in v1, "default" before "require" in v2.
        assert_eq!(
            reduced,
            vec![
                ("./a", Some("./cjs.js")),
                ("./b", Some("./d.js")),
                ("./c", Some("./d.js")),
                // "default" absent and nothing matched: explicit null.
                ("./d", None),
            ]
        );
    }

    #[test]
    fn reduce_rejects_non_relative_targets() {
        let names: HashSet<&str> = ["default"].into_iter().collect();
        let err = |r: String| ResolveError::invalid_package_configuration(r, "/p");
        let v = json!("foo.js");
        let map: NormalizedExportsLikeMap = vec![(".", Cow::Borrowed(&v))];
        let e = reduce_exports_like_map(&map, &names, &err).unwrap_err();
        assert_eq!(e.class_name(), "InvalidPackageConfigurationError");
    }
}
