//! Port of `plugins/HastePlugin.js` (the maps and everything that reads or
//! writes them) and `plugins/haste/computeConflicts.js`.
//!
//! Paths in the maps are canonical paths: normal (root-relative) paths, as
//! `RootPathUtils` defines them. Paths that leave the Haste API are absolute.

use crate::collate::{compare_strings, compare_utf16};
use crate::errors::{DuplicateHasteCandidatesError, HasteConflictsError, HasteError};
use crate::platform::get_platform_extension;
use metro_rs_file_store::RootPathUtils;
use metro_rs_resolver::path;
use std::collections::{HashMap, HashSet};

/// `H.GENERIC_PLATFORM`
pub const GENERIC_PLATFORM: &str = "g";
/// `H.NATIVE_PLATFORM`
pub const NATIVE_PLATFORM: &str = "native";

/// `H.MODULE` / `H.PACKAGE`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleType {
    Module = 0,
    Package = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictType {
    Duplicate,
    Shadowing,
}

impl ConflictType {
    pub fn as_str(self) -> &'static str {
        match self {
            ConflictType::Duplicate => "duplicate",
            ConflictType::Shadowing => "shadowing",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HasteConflict {
    pub absolute_paths: Vec<String>,
    pub id: String,
    pub platform: Option<String>,
    pub r#type: ConflictType,
}

pub struct HasteOptions {
    pub enable_haste_packages: bool,
    pub platforms: Vec<String>,
    pub root_dir: String,
    pub fail_validation_on_conflicts: bool,
}

/// The parts of `ReadonlyFileSystemChanges<?string>` that the plugin reads:
/// `(canonicalPath, hasteName)` pairs. `onChanged` itself only reads
/// `removedFiles` and `addedFiles`; `modified_files` is used to keep the
/// path -> name table behind `get_module_name_by_path` current, because the JS
/// reads the file system's live plugin data there.
#[derive(Debug, Default, Clone)]
pub struct HasteDelta {
    pub added_files: Vec<(String, Option<String>)>,
    pub modified_files: Vec<(String, Option<String>)>,
    pub removed_files: Vec<(String, Option<String>)>,
}

/// `HasteMapItemMetadata`: `[path, type]`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Module {
    path: String,
    ty: ModuleType,
}

/// An insertion-ordered `Map<String, V>` (or null-prototype object).
#[derive(Debug, Clone)]
struct Ordered<V>(Vec<(String, V)>);

impl<V> Default for Ordered<V> {
    fn default() -> Self {
        Ordered(Vec::new())
    }
}

impl<V> Ordered<V> {
    fn get(&self, key: &str) -> Option<&V> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        self.0.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    /// Overwriting keeps the key's position, like `Map.prototype.set`.
    fn set(&mut self, key: &str, value: V) {
        match self.get_mut(key) {
            Some(slot) => *slot = value,
            None => self.0.push((key.to_string(), value)),
        }
    }
    fn delete(&mut self, key: &str) {
        self.0.retain(|(k, _)| k != key);
    }
    fn len(&self) -> usize {
        self.0.len()
    }
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// `HasteMapItem`: platform -> module.
type Item = Ordered<Module>;
/// `DuplicatesSet`: normal path -> type.
type DuplicatesSet = Ordered<ModuleType>;
/// `DuplicatesIndex` value: platform -> duplicates.
type DuplicatesByPlatform = Ordered<DuplicatesSet>;

pub struct HasteMap {
    duplicates: HashMap<String, DuplicatesByPlatform>,
    enable_haste_packages: bool,
    fail_validation_on_conflicts: bool,
    map: HashMap<String, Item>,
    /// normal path -> Haste name, for `get_module_name_by_path`.
    names: Option<HashMap<String, String>>,
    path_utils: RootPathUtils,
    platforms: HashSet<String>,
    root_dir: String,
    /// What the JS logs through `console.warn`, oldest first.
    warnings: Vec<String>,
}

impl HasteMap {
    pub fn new(opts: HasteOptions) -> Self {
        Self {
            duplicates: HashMap::new(),
            enable_haste_packages: opts.enable_haste_packages,
            fail_validation_on_conflicts: opts.fail_validation_on_conflicts,
            map: HashMap::new(),
            names: None,
            path_utils: RootPathUtils::new(&opts.root_dir),
            platforms: opts.platforms.into_iter().collect(),
            root_dir: opts.root_dir,
            warnings: Vec::new(),
        }
    }

    /// The `console.warn` messages emitted so far, drained.
    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }

    pub fn root_dir(&self) -> &str {
        &self.root_dir
    }

    /// `initialize`. `files` are the `(canonicalPath, hasteName)` pairs of the
    /// file map's non-symlink, non-node_modules files, in iteration order.
    pub fn initialize<'a>(&mut self, files: impl IntoIterator<Item = (&'a str, Option<&'a str>)>) {
        let mut names = self.names.take().unwrap_or_default();
        for (canonical_path, haste_id) in files {
            let Some(haste_id) = haste_id else {
                continue;
            };
            names.insert(canonical_path.to_string(), haste_id.to_string());
            let ty = self.module_type_of(canonical_path);
            self.set_module(
                haste_id,
                Module {
                    path: canonical_path.to_string(),
                    ty,
                },
            );
        }
        self.names = Some(names);
    }

    /// `this.#enableHastePackages && baseName === 'package.json' ? PACKAGE : MODULE`
    fn module_type_of(&self, canonical_path: &str) -> ModuleType {
        let base_name = canonical_path.rsplit('/').next().unwrap_or("");
        if self.enable_haste_packages && base_name == "package.json" {
            ModuleType::Package
        } else {
            ModuleType::Module
        }
    }

    fn platform_of<'a>(&self, file: &'a str) -> &'a str {
        match get_platform_extension(file, &self.platforms) {
            Some(p) if !p.is_empty() => p,
            _ => GENERIC_PLATFORM,
        }
    }

    pub fn get_module(
        &self,
        name: &str,
        platform: Option<&str>,
        supports_native_platform: bool,
        module_type: Option<ModuleType>,
    ) -> Result<Option<String>, HasteError> {
        let module = self.get_module_metadata(name, platform, supports_native_platform)?;
        if let Some(module) = module {
            if module.ty == module_type.unwrap_or(ModuleType::Module) {
                let module_path = &module.path;
                // `modulePath && normalToAbsolute(modulePath)`
                return Ok(Some(if module_path.is_empty() {
                    String::new()
                } else {
                    self.path_utils.normal_to_absolute(module_path)
                }));
            }
        }
        Ok(None)
    }

    /// Fails with an error if `initialize` has not run, like the JS throws.
    pub fn get_module_name_by_path(&self, mixed_path: &str) -> Option<String> {
        let names = self
            .names
            .as_ref()
            .expect("HastePlugin has not been initialized before getModuleNameByPath");
        let normal = if path::is_absolute(mixed_path) {
            self.path_utils.absolute_to_normal(mixed_path)
        } else {
            mixed_path.into()
        };
        names.get(normal.as_ref()).cloned()
    }

    pub fn get_package(
        &self,
        name: &str,
        platform: Option<&str>,
        _supports_native_platform: bool,
    ) -> Result<Option<String>, HasteError> {
        self.get_module(name, platform, false, Some(ModuleType::Package))
    }

    /// When looking up a module's data, we walk through each eligible platform for
    /// the query. For each platform, we want to check if there are known
    /// duplicates for that name+platform pair. The duplication logic normally
    /// removes elements from the `map` object, but we want to check upfront to be
    /// extra sure. If metadata exists both in the `duplicates` object and the
    /// `map`, this would be a bug.
    fn get_module_metadata(
        &self,
        name: &str,
        platform: Option<&str>,
        supports_native_platform: bool,
    ) -> Result<Option<&Module>, HasteError> {
        let map = self.map.get(name);
        let dup_map = self.duplicates.get(name);
        let in_map = |p: &str| map.and_then(|m| m.get(p));
        let dups = |p: &str| dup_map.and_then(|d| d.get(p));
        if let Some(platform) = platform {
            self.assert_no_duplicates(name, platform, supports_native_platform, dups(platform))?;
            if let Some(m) = in_map(platform) {
                return Ok(Some(m));
            }
        }
        if supports_native_platform {
            self.assert_no_duplicates(
                name,
                NATIVE_PLATFORM,
                supports_native_platform,
                dups(NATIVE_PLATFORM),
            )?;
            if let Some(m) = in_map(NATIVE_PLATFORM) {
                return Ok(Some(m));
            }
        }
        self.assert_no_duplicates(
            name,
            GENERIC_PLATFORM,
            supports_native_platform,
            dups(GENERIC_PLATFORM),
        )?;
        if let Some(m) = in_map(GENERIC_PLATFORM) {
            return Ok(Some(m));
        }
        Ok(None)
    }

    fn assert_no_duplicates(
        &self,
        name: &str,
        platform: &str,
        supports_native_platform: bool,
        relative_path_set: Option<&DuplicatesSet>,
    ) -> Result<(), HasteError> {
        let Some(relative_path_set) = relative_path_set else {
            return Ok(());
        };
        let mut duplicates = DuplicatesSet::default();
        for (relative_path, ty) in &relative_path_set.0 {
            let duplicate_path = self.path_utils.normal_to_absolute(relative_path);
            duplicates.set(&duplicate_path, *ty);
        }
        Err(HasteError::DuplicateHasteCandidates(
            DuplicateHasteCandidatesError::new(
                name,
                platform,
                supports_native_platform,
                duplicates.0,
            ),
        ))
    }

    pub fn on_changed(&mut self, delta: &HasteDelta) {
        // Process removals first so that moves aren't treated as duplicates.
        for (canonical_path, maybe_haste_id) in &delta.removed_files {
            self.on_removed_file(canonical_path, maybe_haste_id.as_deref());
        }
        for (canonical_path, maybe_haste_id) in &delta.added_files {
            self.on_new_file(canonical_path, maybe_haste_id.as_deref());
        }
        // The file system's plugin data, as `getModuleNameByPath` reads it.
        if let Some(names) = self.names.as_mut() {
            for (canonical_path, _) in &delta.removed_files {
                names.remove(canonical_path);
            }
            for (canonical_path, id) in delta.added_files.iter().chain(&delta.modified_files) {
                match id {
                    Some(id) => names.insert(canonical_path.clone(), id.clone()),
                    None => names.remove(canonical_path),
                };
            }
        }
    }

    fn on_new_file(&mut self, canonical_path: &str, id: Option<&str>) {
        let Some(id) = id else {
            // Not a Haste module or package
            return;
        };

        let module = Module {
            path: canonical_path.to_string(),
            ty: self.module_type_of(canonical_path),
        };

        self.set_module(id, module);
    }

    fn set_module(&mut self, id: &str, module: Module) {
        let has_item = self.map.contains_key(id);
        if !has_item {
            self.map.insert(id.to_string(), Item::default());
        }
        let platform = self.platform_of_owned(&module.path);

        let existing_module = self.map[id].get(&platform).cloned();

        if let Some(existing_module) = existing_module.filter(|e| e.path != module.path) {
            self.warnings.push(
                [
                    format!("metro-file-map: Haste module naming collision: {id}"),
                    "  The following files share their name; please adjust your hasteImpl:"
                        .to_string(),
                    format!("    * <rootDir>/{}", existing_module.path),
                    format!("    * <rootDir>/{}", module.path),
                    String::new(),
                ]
                .join("\n"),
            );

            // We do NOT want consumers to use a module that is ambiguous.
            let hastemap_item = self.map.get_mut(id).unwrap();
            hastemap_item.delete(&platform);

            if hastemap_item.is_empty() {
                self.map.remove(id);
            }

            let dups_by_platform = self.duplicates.entry(id.to_string()).or_default();

            let mut dups = DuplicatesSet::default();
            dups.set(&module.path, module.ty);
            dups.set(&existing_module.path, existing_module.ty);
            dups_by_platform.set(&platform, dups);

            return;
        }

        if let Some(dups_by_platform) = self.duplicates.get_mut(id) {
            if let Some(dups) = dups_by_platform.get_mut(&platform) {
                dups.set(&module.path, module.ty);
            }
            return;
        }

        self.map.get_mut(id).unwrap().set(&platform, module);
    }

    fn platform_of_owned(&self, file: &str) -> String {
        self.platform_of(file).to_string()
    }

    fn on_removed_file(&mut self, canonical_path: &str, module_name: Option<&str>) {
        let Some(module_name) = module_name else {
            // Not a Haste module or package
            return;
        };

        let platform = self.platform_of_owned(canonical_path);

        if let Some(hastemap_item) = self.map.get_mut(module_name) {
            hastemap_item.delete(&platform);
            if hastemap_item.is_empty() {
                self.map.remove(module_name);
            }
        }

        self.recover_duplicates(module_name, canonical_path);
    }

    pub fn assert_valid(&self) -> Result<(), HasteError> {
        if !self.fail_validation_on_conflicts {
            return Ok(());
        }
        let conflicts = self.compute_conflicts();
        if !conflicts.is_empty() {
            return Err(HasteError::HasteConflicts(HasteConflictsError::new(
                conflicts,
            )));
        }
        Ok(())
    }

    /// This function should be called when the file under `filePath` is removed
    /// or changed. When that happens, we want to figure out if that file was
    /// part of a group of files that had the same ID. If it was, we want to
    /// remove it from the group. Furthermore, if there is only one file
    /// remaining in the group, then we want to restore that single file as the
    /// correct resolution for its ID, and cleanup the duplicates index.
    fn recover_duplicates(&mut self, module_name: &str, relative_file_path: &str) {
        let platform = self.platform_of_owned(relative_file_path);
        let Some(dups_by_platform) = self.duplicates.get_mut(module_name) else {
            return;
        };
        let Some(dups) = dups_by_platform.get_mut(&platform) else {
            return;
        };

        dups.delete(relative_file_path);

        if dups.len() != 1 {
            return;
        }

        let (unique_path, unique_ty) = dups.0[0].clone();
        let unique_module = Module {
            path: unique_path,
            ty: unique_ty,
        };

        dups_by_platform.delete(&platform);
        if dups_by_platform.is_empty() {
            self.duplicates.remove(module_name);
        }

        let dedup_map = self.map.entry(module_name.to_string()).or_default();
        dedup_map.set(&platform, unique_module);
    }

    pub fn compute_conflicts(&self) -> Vec<HasteConflict> {
        let mut conflicts: Vec<HasteConflict> = Vec::new();

        // Add literal duplicates tracked in the #duplicates map
        for (id, dups_by_platform) in &self.duplicates {
            for (platform, conflicting_modules) in &dups_by_platform.0 {
                let mut absolute_paths: Vec<String> = conflicting_modules
                    .0
                    .iter()
                    .map(|(module_path, _)| self.path_utils.normal_to_absolute(module_path))
                    .collect();
                // Sort for ease of testing
                absolute_paths.sort_by(|a, b| compare_utf16(a, b));
                conflicts.push(HasteConflict {
                    absolute_paths,
                    id: id.clone(),
                    platform: (platform != GENERIC_PLATFORM).then(|| platform.clone()),
                    r#type: ConflictType::Duplicate,
                });
            }
        }

        // Add cases of "shadowing at a distance": a module with a platform suffix and
        // a module with a lower priority platform suffix (or no suffix), in different
        // directories.
        for (id, data) in &self.map {
            let mut conflict_paths: Vec<&str> = Vec::new();
            let mut base_paths: Vec<&str> = Vec::new();
            for base_platform in [NATIVE_PLATFORM, GENERIC_PLATFORM] {
                let Some(base) = data.get(base_platform) else {
                    continue;
                };
                let base_path = base.path.as_str();
                base_paths.push(base_path);
                let base_path_dir = path::dirname(base_path);
                // Find all platforms that can shadow basePlatform
                // Given that X.(specific platform).js > x.native.js > X.js
                // and basePlatform is either 'native' or generic (no platform).
                for (platform, module) in &data.0 {
                    if platform == base_platform || platform == GENERIC_PLATFORM
                    /* lowest priority */
                    {
                        continue;
                    }
                    let platform_path = module.path.as_str();
                    if path::dirname(platform_path) != base_path_dir
                        && !conflict_paths.contains(&platform_path)
                    {
                        conflict_paths.push(platform_path);
                    }
                }
            }
            if !conflict_paths.is_empty() {
                // `new Set([...conflictPaths, ...basePaths])`
                let mut all: Vec<&str> = conflict_paths;
                for p in base_paths {
                    if !all.contains(&p) {
                        all.push(p);
                    }
                }
                let mut absolute_paths: Vec<String> = all
                    .into_iter()
                    .map(|module_path| self.path_utils.normal_to_absolute(module_path))
                    .collect();
                // Sort for ease of testing
                absolute_paths.sort_by(|a, b| compare_utf16(a, b));
                conflicts.push(HasteConflict {
                    absolute_paths,
                    id: id.clone(),
                    platform: None,
                    r#type: ConflictType::Shadowing,
                });
            }
        }

        // Sort for ease of testing
        conflicts.sort_by(|a, b| {
            compare_strings(Some(a.r#type.as_str()), Some(b.r#type.as_str()))
                .then_with(|| compare_strings(Some(&a.id), Some(&b.id)))
                .then_with(|| compare_strings(a.platform.as_deref(), b.platform.as_deref()))
        });

        conflicts
    }
}
