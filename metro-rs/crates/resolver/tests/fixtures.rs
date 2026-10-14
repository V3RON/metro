//! Runs every case in `port/blackbox/fixtures/resolver.json` through the Rust
//! `resolve`. The Jest runner `port/blackbox/__tests__/resolver-fixtures-test.js`
//! reads the same file; the expectations were recorded from the JavaScript
//! implementation.
//!
//! `MemoryHost` reproduces what that runner builds through
//! `createResolutionContext` and `createPackageAccessors` in
//! `packages/metro-resolver/src/__tests__/utils.js`.

use metro_rs_resolver::path;
use metro_rs_resolver::{
    resolve, HostError, Lookup, PackageForModule, PackageJson, Resolution, ResolveRequest,
    ResolverConfig, ResolverHost,
};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

enum Entry {
    /// A file with these contents (package.json objects are stringified).
    File(String),
    /// `{realPath}`: a symlink.
    Link(Option<String>),
}

struct MemoryHost {
    files: HashMap<String, Entry>,
    directory_set: HashSet<String>,
    haste_modules: HashMap<String, String>,
    /// `"<dir>/<base><ext>"` to files.
    assets: HashMap<String, Vec<String>>,
    has_asset_override: bool,
    has_haste_override: bool,
}

impl MemoryHost {
    fn new(files: HashMap<String, Entry>) -> Self {
        let mut directory_set = HashSet::new();
        for file_path in files.keys() {
            let mut current_dir = file_path.clone();
            loop {
                let prev_dir = current_dir.clone();
                current_dir = path::dirname(&current_dir);
                directory_set.insert(current_dir.clone());
                if current_dir == prev_dir {
                    break;
                }
            }
        }
        MemoryHost {
            files,
            directory_set,
            haste_modules: HashMap::new(),
            assets: HashMap::new(),
            has_asset_override: false,
            has_haste_override: false,
        }
    }
}

impl MemoryHost {
    fn lookup(&self, input_path: &str) -> Lookup {
        // Normalise and remove any trailing slash.
        let file_path = path::resolve(&[input_path]);
        match self.files.get(&file_path) {
            Some(Entry::File(_)) => Lookup::File {
                real_path: file_path,
            },
            None => {
                if self.directory_set.contains(&file_path) {
                    Lookup::Dir {
                        real_path: file_path,
                    }
                } else {
                    Lookup::NotFound
                }
            }
            Some(Entry::Link(None)) => Lookup::NotFound,
            Some(Entry::Link(Some(real_path))) => Lookup::File {
                real_path: real_path.clone(),
            },
        }
    }

    fn file_exists(&self, file_path: &str) -> bool {
        // Should return false unless realpath(filePath) exists. We mock
        // shallow dereferencing.
        matches!(
            self.files.get(file_path),
            Some(Entry::File(_)) | Some(Entry::Link(Some(_)))
        )
    }

    fn package(&self, package_json_path: &str) -> Option<PackageJson> {
        match self.files.get(package_json_path) {
            Some(Entry::File(contents)) => Some(Arc::new(
                serde_json::from_str(contents).expect("JSON.parse would throw here too"),
            )),
            _ => None,
        }
    }

    fn package_for_module(&self, module_path: &str) -> Option<PackageForModule> {
        let parsed_path = path::parse(module_path);
        let root = parsed_path.root;
        let mut dir = path::join(&[&parsed_path.dir, &parsed_path.base]);

        loop {
            if path::basename(&dir) == "node_modules" {
                return None;
            }
            let candidate = path::join(&[&dir, "package.json"]);
            if let Some(package_json) = self.package(&candidate) {
                return Some(PackageForModule {
                    root_path: dir.clone().into(),
                    package_json,
                    package_relative_path: path::relative(&dir, module_path).into(),
                });
            }

            let prev_dir = dir.clone();
            dir = path::dirname(&dir);
            // `root` may use different separators to `dir`, so also stop once
            // `dirname` no longer changes the path.
            if dir == "." || dir == root || dir == prev_dir {
                return None;
            }
        }
    }

    fn haste_module(&self, name: &str) -> Option<String> {
        if !self.has_haste_override {
            return None;
        }
        // `c.haste.modules[name] ? p(...) : null`
        self.haste_modules
            .get(name)
            .filter(|p| !p.is_empty())
            .cloned()
    }
}

impl ResolverHost for MemoryHost {
    fn file_system_lookup(&self, path: &str) -> Result<Lookup, HostError> {
        Ok(self.lookup(path))
    }

    fn does_file_exist(&self, path: &str) -> Result<bool, HostError> {
        Ok(self.file_exists(path))
    }

    fn get_package(&self, package_json_path: &str) -> Result<Option<PackageJson>, HostError> {
        Ok(self.package(package_json_path))
    }

    fn get_package_for_module(&self, path: &str) -> Result<Option<PackageForModule>, HostError> {
        Ok(self.package_for_module(path))
    }

    fn resolve_haste_module(&self, name: &str) -> Result<Option<String>, HostError> {
        Ok(self.haste_module(name))
    }

    fn resolve_haste_package(&self, _name: &str) -> Result<Option<String>, HostError> {
        Ok(None)
    }

    fn resolve_asset(
        &self,
        dir_path: &str,
        basename: &str,
        extension: &str,
    ) -> Option<Vec<String>> {
        if !self.has_asset_override {
            return None;
        }
        self.assets
            .get(&path::join(&[dir_path, &format!("{basename}{extension}")]))
            .cloned()
    }

    fn log_warning(&self, _message: &str) -> Result<(), HostError> {
        Ok(())
    }
}

fn string_list(v: &Value) -> Vec<String> {
    v.as_array()
        .expect("array")
        .iter()
        .map(|s| s.as_str().expect("string").to_string())
        .collect()
}

fn build_files(case: &Value) -> HashMap<String, Entry> {
    let mut files = HashMap::new();
    for (k, v) in case["files"].as_object().expect("files") {
        let entry = match v {
            Value::Object(o) if o.contains_key("realPath") => {
                Entry::Link(o["realPath"].as_str().map(str::to_string))
            }
            Value::String(s) => Entry::File(s.clone()),
            other => Entry::File(other.to_string()),
        };
        files.insert(k.clone(), entry);
    }
    files
}

/// Returns the config and the custom resolver options for the case.
fn build_config(case: &Value) -> (ResolverConfig, MemoryHost) {
    let mut host = MemoryHost::new(build_files(case));
    if let Some(modules) = case["haste"]["modules"].as_object() {
        host.has_haste_override = true;
        for (k, v) in modules {
            host.haste_modules
                .insert(k.clone(), v.as_str().unwrap_or("").to_string());
        }
    }
    if let Some(assets) = case["assets"].as_object() {
        host.has_asset_override = true;
        for (k, v) in assets {
            host.assets.insert(k.clone(), string_list(v));
        }
    }

    let mut config = ResolverConfig::new();
    let none = serde_json::Map::new();
    let overrides = case["context"].as_object().unwrap_or(&none);
    for (key, v) in overrides {
        match key.as_str() {
            "assetExts" => config.asset_exts = string_list(v).into_iter().collect(),
            "disableHierarchicalLookup" => {
                config.disable_hierarchical_lookup = v.as_bool().expect("bool")
            }
            "extraNodeModules" => {
                config.extra_node_modules = v.as_object().map(|o| {
                    o.iter()
                        .map(|(k, v)| (k.clone(), v.as_str().expect("string").to_string()))
                        .collect()
                })
            }
            "mainFields" => config.main_fields = string_list(v),
            "nodeModulesPaths" => config.node_modules_paths = string_list(v),
            "preferNativePlatform" => config.prefer_native_platform = v.as_bool().expect("bool"),
            "sourceExts" => config.source_exts = string_list(v),
            "unstable_conditionNames" => config.unstable_condition_names = string_list(v),
            "unstable_conditionsByPlatform" => {
                config.unstable_conditions_by_platform = v
                    .as_object()
                    .expect("object")
                    .iter()
                    .map(|(k, v)| (k.clone(), string_list(v)))
                    .collect()
            }
            "unstable_enablePackageExports" => {
                config.unstable_enable_package_exports = v.as_bool().expect("bool")
            }
            "unstable_incrementalResolution" => {
                config.unstable_incremental_resolution = v.as_bool().expect("bool")
            }
            // Per-call request fields, read in `run_case`.
            "allowHaste" | "customResolverOptions" | "dev" | "isESMImport" => {}
            other => panic!("fixture context override not supported by the harness: {other}"),
        }
    }
    // The JS runner: `unstable_conditionNames` defaults to ['require'] when
    // package exports are enabled (it is also the default of the context).
    if overrides.get("unstable_conditionNames").is_none() && config.unstable_enable_package_exports
    {
        config.unstable_condition_names = vec!["require".to_string()];
    }
    (config, host)
}

fn run_case(case: &Value) -> Result<Resolution, String> {
    let (config, host) = build_config(case);
    let overrides = &case["context"];
    let options = overrides
        .get("customResolverOptions")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    let request = ResolveRequest {
        origin_module_path: case["origin"].as_str().expect("origin"),
        allow_haste: overrides["allowHaste"].as_bool().unwrap_or(true),
        is_esm_import: overrides["isESMImport"].as_bool().unwrap_or(false),
        dev: overrides["dev"].as_bool().unwrap_or(true),
        custom_resolver_options: &options,
    };
    resolve(
        &config,
        &host,
        &request,
        case["request"].as_str().expect("request"),
        case["platform"].as_str(),
    )
    .map_err(|e| e.class_name().to_string())
}

#[test]
fn resolver_fixtures() {
    let fixture_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../port/blackbox/fixtures/resolver.json"
    );
    let text = std::fs::read_to_string(fixture_path).expect("fixtures file");
    let data: Value = serde_json::from_str(&text).expect("fixtures are JSON");
    let cases = data["cases"].as_array().expect("cases");
    assert!(!cases.is_empty());

    let mut failures = Vec::new();
    for case in cases {
        let name = case["name"].as_str().expect("name");
        let actual = run_case(case);
        let ok = if let Some(expected_error) = case["expectError"].as_str() {
            actual == Err(expected_error.to_string())
        } else {
            let expect = &case["expect"];
            let expected = match expect["type"].as_str().expect("expect.type") {
                "sourceFile" => Resolution::SourceFile {
                    file_path: expect["filePath"].as_str().expect("filePath").to_string(),
                },
                "assetFiles" => Resolution::AssetFiles {
                    file_paths: string_list(&expect["filePaths"]),
                },
                "empty" => Resolution::Empty,
                other => panic!("unknown resolution type {other}"),
            };
            actual == Ok(expected)
        };
        if !ok {
            failures.push(format!("{name}: got {actual:?}"));
        }
    }
    println!(
        "{}/{} fixture cases pass",
        cases.len() - failures.len(),
        cases.len()
    );
    assert!(
        failures.is_empty(),
        "failing cases:\n{}",
        failures.join("\n")
    );
}
