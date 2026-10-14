//! Ports of `haste/DuplicateHasteCandidatesError.js` and
//! `haste/HasteConflictsError.js`.

use crate::collate::compare_utf16;
use crate::haste_map::{ConflictType, HasteConflict, ModuleType, GENERIC_PLATFORM};
use metro_rs_resolver::path;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateHasteCandidatesError {
    pub message: String,
    pub haste_name: String,
    pub platform: Option<String>,
    pub supports_native_platform: bool,
    /// Absolute path -> type, in `Map` insertion order.
    pub duplicates_set: Vec<(String, ModuleType)>,
}

impl DuplicateHasteCandidatesError {
    pub fn new(
        name: &str,
        platform: &str,
        supports_native_platform: bool,
        duplicates_set: Vec<(String, ModuleType)>,
    ) -> Self {
        let platform_message = get_platform_message(platform);
        let mut lines: Vec<String> = duplicates_set
            .iter()
            .map(|(dup_file_path, dup_file_type)| {
                format!(
                    "  * `{dup_file_path}` ({})\n",
                    get_type_message(*dup_file_type)
                )
            })
            .collect();
        lines.sort_by(|a, b| compare_utf16(a, b));
        let message = format!(
            "The name `{name}` was looked up in the Haste module map. It \
             cannot be resolved, because there exists several different \
             files, or packages, that provide a module for \
             that particular name and platform. {platform_message} You must \
             delete or exclude files until there remains only one of these:\n\n{}",
            lines.concat()
        );
        Self {
            message,
            haste_name: name.to_string(),
            platform: Some(platform.to_string()),
            supports_native_platform,
            duplicates_set,
        }
    }
}

fn get_platform_message(platform: &str) -> String {
    if platform == GENERIC_PLATFORM {
        return "The platform is generic (no extension).".to_string();
    }
    format!("The platform extension is `{platform}`.")
}

fn get_type_message(ty: ModuleType) -> &'static str {
    match ty {
        ModuleType::Module => "module",
        ModuleType::Package => "package",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HasteConflictsError {
    pub message: String,
    conflicts: Vec<HasteConflict>,
}

impl HasteConflictsError {
    pub fn new(conflicts: Vec<HasteConflict>) -> Self {
        Self {
            message: format!(
                "Found {} Haste conflict(s). Haste module IDs must be globally unique in the codebase.",
                conflicts.len()
            ),
            conflicts,
        }
    }

    pub fn conflicts(&self) -> &[HasteConflict] {
        &self.conflicts
    }

    pub fn get_detailed_message(&self, paths_relative_to_root: Option<&str>) -> String {
        let mut messages: Vec<String> = Vec::new();
        let conflicts = &self.conflicts;
        if conflicts
            .iter()
            .any(|c| c.r#type == ConflictType::Duplicate)
        {
            messages.push(
                "Advice: Resolve conflicts of type \"duplicate\" by renaming one or both of the conflicting modules, or by excluding conflicting paths from Haste.".to_string(),
            );
        }
        if conflicts
            .iter()
            .any(|c| c.r#type == ConflictType::Shadowing)
        {
            messages.push(
                "Advice: Resolve conflicts of type \"shadowing\" by moving the modules to the same folder, or by excluding conflicting paths from Haste.".to_string(),
            );
        }
        for (index, conflict) in conflicts.iter().enumerate() {
            let item_header = format!("{}. ", index + 1);
            let indent = " ".repeat(item_header.len() + 2);
            let platform = conflict
                .platform
                .as_ref()
                .map_or(String::new(), |p| format!(".{p}"));
            messages.push(format!(
                "\n{item_header}{}{platform} ({})",
                conflict.id,
                conflict.r#type.as_str()
            ));
            for module_path in &conflict.absolute_paths {
                messages.push(match paths_relative_to_root {
                    Some(root) => format!("{indent}{}", path::relative(root, module_path)),
                    None => format!("{indent}{module_path}"),
                });
            }
        }
        messages.join("\n")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HasteError {
    DuplicateHasteCandidates(DuplicateHasteCandidatesError),
    HasteConflicts(HasteConflictsError),
}

impl fmt::Display for HasteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HasteError::DuplicateHasteCandidates(e) => f.write_str(&e.message),
            HasteError::HasteConflicts(e) => f.write_str(&e.message),
        }
    }
}

impl std::error::Error for HasteError {}
