//! Port of `PackageResolve.js`: legacy (non-exports) package resolution based
//! on the "browser" field spec.

use crate::config::Ctx;
use crate::path;
use crate::utils::system_to_posix_path;
use serde_json::Value;

/// A replacement found in a `mainFields` map: either a path/specifier, or
/// `false` (the module should be ignored, i.e. resolve to the empty module).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Redirect {
    Path(String),
    Ignored,
}

/// The subpath, or set of subpath variants, to match.
pub enum Subpath<'a> {
    /// A single subpath, expanded to its "browser"-spec variants.
    One(&'a str),
    /// A pre-expanded list, matched as-is.
    Variants(&'a [String]),
}

/// Resolve the main entry point subpath for a package.
///
/// Implements legacy (non-exports) package resolution behaviour based on the
/// ["browser" field spec](https://github.com/defunctzombie/package-browser-field-spec).
pub fn get_package_entry_point(ctx: &Ctx<'_>, pkg: &Value) -> String {
    let main_fields = &ctx.config.main_fields;

    let mut main = "index".to_string();

    for name in main_fields {
        if let Some(Value::String(s)) = pkg.get(name.as_str()) {
            if !s.is_empty() {
                main = s.clone();
                break;
            }
        }
    }

    // NOTE: Additional variants are used when checking for subpath replacements
    // against the main entry point. This inconsistent with those matched by
    // `redirectModulePath`, but we are preserving this long-standing behaviour.
    let other = match main.strip_prefix("./") {
        Some(rest) => rest.to_string(),
        None => format!("./{main}"),
    };
    let mut variants: Vec<String> = Vec::new();
    for variant in [main.clone(), other] {
        // `variant.replace(/(\.js|\.json)$/, '')`
        let stripped = if let Some(s) = variant.strip_suffix(".json") {
            s.to_string()
        } else if let Some(s) = variant.strip_suffix(".js") {
            s.to_string()
        } else {
            variant.clone()
        };
        variants.push(variant.clone());
        variants.push(format!("{variant}.js"));
        variants.push(format!("{variant}.json"));
        variants.push(stripped);
    }

    let replacement =
        match_subpath_from_main_fields(Subpath::Variants(&variants), pkg, main_fields);

    if let Some(Redirect::Path(replacement)) = replacement {
        return replacement;
    }

    main
}

/// Get the resolved file path for the given import specifier based on any
/// `package.json` rules. Returns `Redirect::Ignored` if the module should be
/// [ignored](https://github.com/defunctzombie/package-browser-field-spec#ignore-a-module),
/// and returns the original path if no `package.json` mapping is matched. Does
/// not test file existence.
///
/// `module_path` may be:
///
/// - A relative specifier (beginning with '.'), which may be redirected by a
///   `package.json` file local to `context.originModulePath`.
///     - Note: A path beginning with '/' is treated as an absolute specifier
///       (non-standard).
/// - A bare specifier (e.g. 'some-pkg', 'some-pkg/foo'), which may be
///   redirected by `package.json` rules in the containing package.
/// - An absolute specifier, which may be redirected by `package.json` rules
///   in the containing package (non-standard, "browser" spec only).
pub fn redirect_module_path(ctx: &Ctx<'_>, module_path: &str) -> Redirect {
    let main_fields = &ctx.config.main_fields;
    let is_module_path_absolute = path::is_absolute(module_path);

    let containing_package = ctx
        .host()
        .get_package_for_module(if is_module_path_absolute {
            module_path
        } else {
            ctx.request.origin_module_path
        });

    let containing_package = match containing_package {
        // No package.json rules apply
        None => return Redirect::Path(module_path.to_string()),
        Some(p) => p,
    };

    let mut redirected_path: Option<Redirect>;

    if module_path.starts_with('.') || is_module_path_absolute {
        let package_relative_module_path = if is_module_path_absolute {
            // If the module path is absolute, containingPackage is relative to
            // it (see above).
            containing_package.package_relative_path.clone()
        } else {
            // Otherwise containingPackage is relative to the origin module.
            // Origin's package-relative directory joined with the target
            // module's origin-relative path gives us the module's
            // package-relative path.
            path::join(&[
                &path::dirname(&containing_package.package_relative_path),
                module_path,
            ])
        };

        redirected_path = match_subpath_from_main_fields(
            // Use prefixed POSIX path for lookup in package.json
            Subpath::One(&format!(
                "./{}",
                system_to_posix_path(&package_relative_module_path)
            )),
            &containing_package.package_json,
            main_fields,
        );

        if let Some(Redirect::Path(p)) = &redirected_path {
            // BRITTLE ASSUMPTION: This is always treated as a package-relative
            // path and is converted back, even if the redirected path is a
            // specifier referring to another package.
            redirected_path = Some(Redirect::Path(if path::is_absolute(p) {
                path::normalize(p)
            } else {
                path::join(&[&containing_package.root_path, p])
            }));
        }
    } else {
        // Otherwise, `modulePath` may be an unprefixed relative path or a bare
        // specifier (can also be an absolute specifier prefixed with a URL
        // scheme). This is used only by the "browser" spec.
        redirected_path = match_subpath_from_main_fields(
            Subpath::One(module_path),
            &containing_package.package_json,
            main_fields,
        );
    }

    redirected_path.unwrap_or_else(|| Redirect::Path(module_path.to_string()))
}

/// Get the mapped replacement for the given subpath defined by matching
/// `mainFields` entries in the passed `package.json`
/// (https://github.com/defunctzombie/package-browser-field-spec#replace-specific-files---advanced).
///
/// Returns either:
/// - `Some(Redirect::Path)` with the matched replacement subpath.
/// - `Some(Redirect::Ignored)`, indicating the module should be ignored.
/// - `None` when there is no entry for the subpath.
///
/// The JS merges all object-valued main fields into one map in which earlier
/// `mainFields` win on key conflicts. Looking up each variant in the fields in
/// order, stopping at the first field that has the key, is equivalent.
pub fn match_subpath_from_main_fields(
    subpath: Subpath<'_>,
    pkg: &Value,
    main_fields: &[String],
) -> Option<Redirect> {
    let expanded;
    let variants: &[String] = match subpath {
        Subpath::Variants(v) => v,
        Subpath::One(s) => {
            expanded = expand_subpath_variants(s);
            &expanded
        }
    };
    for variant in variants {
        for field in main_fields {
            if let Some(Value::Object(map)) = pkg.get(field.as_str()) {
                if let Some(replacement) = map.get(variant.as_str()) {
                    // `replacement != null`, else try the next variant.
                    match replacement {
                        Value::Null => {}
                        Value::String(s) => return Some(Redirect::Path(s.clone())),
                        Value::Bool(false) => return Some(Redirect::Ignored),
                        // Any other JSON type (`true`, numbers, objects) would
                        // be returned as-is by JS and fail later. Not
                        // modelled; treated as no replacement.
                        _ => {}
                    }
                    break;
                }
            }
        }
    }

    None
}

/// Get the expanded variants for a given subpath to try against mappings in
/// `package.json`. This is unique to "main" and the "browser" spec.
fn expand_subpath_variants(subpath: &str) -> Vec<String> {
    vec![
        subpath.to_string(),
        format!("{subpath}.js"),
        format!("{subpath}.json"),
    ]
}
