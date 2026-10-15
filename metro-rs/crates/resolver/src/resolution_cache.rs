//! The resolution cache of Metro's `DependencyGraph`, kept natively.
//!
//! It stores the same thing the JavaScript cache does, under the same compound
//! key: the canonical resolver options (a string made once in JS), the origin
//! folder (or "" when resolution does not depend on it), the target name with
//! its module system, and the platform. The value is the `filePath` of a
//! `BundlerResolution`, which always is a `sourceFile`.

use std::collections::HashMap;

/// Written between the parts of an innermost key. A path or a specifier cannot
/// contain NUL.
const SEP: char = '\0';

/// Stands for a null platform, which no platform name can equal.
const NO_PLATFORM: &str = "\u{1}";

#[derive(Debug, Default)]
pub struct ResolutionCache {
    // options key -> origin key -> (name, module system, platform) -> filePath
    by_options: HashMap<String, HashMap<String, HashMap<String, String>>>,
    len: usize,
}

fn target_key(name: &str, is_esm_import: bool, platform: Option<&str>) -> String {
    let platform = platform.unwrap_or(NO_PLATFORM);
    let mut key = String::with_capacity(name.len() + platform.len() + 6);
    key.push_str(name);
    key.push(SEP);
    key.push_str(if is_esm_import { "esm" } else { "cjs" });
    key.push(SEP);
    key.push_str(platform);
    key
}

impl ResolutionCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// `_resolutionCache = new Map()`.
    pub fn clear(&mut self) {
        self.by_options.clear();
        self.len = 0;
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn get(
        &self,
        options_key: &str,
        origin_key: &str,
        name: &str,
        is_esm_import: bool,
        platform: Option<&str>,
    ) -> Option<&str> {
        self.by_options
            .get(options_key)?
            .get(origin_key)?
            .get(&target_key(name, is_esm_import, platform))
            .map(String::as_str)
    }

    pub fn insert(
        &mut self,
        options_key: &str,
        origin_key: &str,
        name: &str,
        is_esm_import: bool,
        platform: Option<&str>,
        file_path: &str,
    ) {
        let by_origin = match self.by_options.get_mut(options_key) {
            Some(by_origin) => by_origin,
            None => self.by_options.entry(options_key.to_string()).or_default(),
        };
        let by_target = match by_origin.get_mut(origin_key) {
            Some(by_target) => by_target,
            None => by_origin.entry(origin_key.to_string()).or_default(),
        };
        if by_target
            .insert(
                target_key(name, is_esm_import, platform),
                file_path.to_string(),
            )
            .is_none()
        {
            self.len += 1;
        }
    }
}

/// The origin part of the key, as `DependencyGraph.resolveDependency` derives
/// it. `origin_dir` is `path.dirname(originModulePath)`.
pub fn origin_key<'a>(
    origin_dir: &'a str,
    origin_is_under_node_modules: bool,
    assume_flat_node_modules: bool,
    name: &str,
) -> &'a str {
    let sensitive = !assume_flat_node_modules
        // Path requests are resolved relative to the origin folder.
        || name.contains('/')
        || name == "."
        || name == ".."
        // Preserve standard assumptions under node_modules.
        || origin_is_under_node_modules;
    if sensitive {
        origin_dir
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_independent() {
        let mut cache = ResolutionCache::new();
        cache.insert("{}", "/a", "x", false, Some("ios"), "/a/x.js");
        assert_eq!(
            cache.get("{}", "/a", "x", false, Some("ios")),
            Some("/a/x.js")
        );
        // Every part of the key matters.
        assert_eq!(
            cache.get("{\"dev\":true}", "/a", "x", false, Some("ios")),
            None
        );
        assert_eq!(cache.get("{}", "/b", "x", false, Some("ios")), None);
        assert_eq!(cache.get("{}", "/a", "y", false, Some("ios")), None);
        assert_eq!(cache.get("{}", "/a", "x", true, Some("ios")), None);
        assert_eq!(cache.get("{}", "/a", "x", false, Some("android")), None);
        assert_eq!(cache.get("{}", "/a", "x", false, None), None);
    }

    #[test]
    fn null_platform_is_not_a_platform_name() {
        let mut cache = ResolutionCache::new();
        cache.insert("{}", "/a", "x", false, None, "/a/x.js");
        assert_eq!(cache.get("{}", "/a", "x", false, None), Some("/a/x.js"));
        assert_eq!(cache.get("{}", "/a", "x", false, Some("")), None);
    }

    #[test]
    fn insert_replaces_and_clear_empties() {
        let mut cache = ResolutionCache::new();
        cache.insert("", "/a", "x", false, None, "/a/x.js");
        cache.insert("", "/a", "x", false, None, "/a/x.ios.js");
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get("", "/a", "x", false, None), Some("/a/x.ios.js"));
        cache.clear();
        assert!(cache.is_empty());
        assert_eq!(cache.get("", "/a", "x", false, None), None);
    }

    #[test]
    fn origin_key_follows_the_js_rules() {
        // Not flat: always the folder.
        assert_eq!(origin_key("/a", false, false, "react"), "/a");
        // Flat: a bare package name does not depend on the folder.
        assert_eq!(origin_key("/a", false, true, "react"), "");
        // Except paths, dots and files under node_modules.
        assert_eq!(origin_key("/a", false, true, "react/x"), "/a");
        assert_eq!(origin_key("/a", false, true, "."), "/a");
        assert_eq!(origin_key("/a", false, true, ".."), "/a");
        assert_eq!(origin_key("/a", false, true, "./x"), "/a");
        assert_eq!(
            origin_key("/a/node_modules/p", true, true, "react"),
            "/a/node_modules/p"
        );
    }
}
