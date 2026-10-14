//! `lib/FileSystemChangeAggregator.js`: flattens a sequence of tree changes
//! into the net change of a batch.

use crate::js_map::{JsMap, JsSet};
use crate::metadata::FileMetadata;

#[derive(Default)]
pub struct FileSystemChangeAggregator {
    // Mutually exclusive with removedDirectories
    added_directories: JsSet,
    // Mutually exclusive with addedDirectories
    removed_directories: JsSet,

    // Mutually exclusive with modified and removed files
    added_files: JsMap<FileMetadata>,
    // Mutually exclusive with added and removed files
    modified_files: JsMap<FileMetadata>,
    // Mutually exclusive with added and modified files
    removed_files: JsMap<FileMetadata>,

    // Removed files must be paired with the file's metadata the last time it was
    // observable by consumers - ie, immediately *before* this batch. To report
    // this accurately with minimal overhead, we'll note the current metadata of
    // a file the first time it is modified or removed within a batch. If it is
    // re-added, modified and removed again, we still have the initial metadata.
    // This is particularly important if, say, a regular file is replaced by a
    // symlink, or vice-versa.
    initial_metadata: JsMap<FileMetadata>,
}

/// `ReadonlyFileSystemChanges<FileMetadata>`: `getView()`, in the order the JS
/// `Set`s and `Map`s iterate.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Changes {
    pub added_directories: Vec<String>,
    pub removed_directories: Vec<String>,
    pub added_files: Vec<(String, FileMetadata)>,
    pub modified_files: Vec<(String, FileMetadata)>,
    pub removed_files: Vec<(String, FileMetadata)>,
}

impl Changes {
    /// `getSize()`.
    pub fn size(&self) -> usize {
        self.added_directories.len()
            + self.removed_directories.len()
            + self.added_files.len()
            + self.modified_files.len()
            + self.removed_files.len()
    }
}

impl FileSystemChangeAggregator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn directory_added(&mut self, canonical_path: &str) {
        // Only add to newDirectories if this directory wasn't previously removed
        // (i.e., it's truly new). If it was removed and re-added, the net effect
        // is no directory change.
        if !self.removed_directories.delete(canonical_path) {
            self.added_directories.add(canonical_path);
        }
    }

    pub fn directory_removed(&mut self, canonical_path: &str) {
        if !self.added_directories.delete(canonical_path) {
            self.removed_directories.add(canonical_path);
        }
    }

    pub fn file_added(&mut self, canonical_path: &str, data: FileMetadata) {
        if self.removed_files.remove(canonical_path).is_some() {
            // File was removed then re-added in the same batch - treat as modification
            self.modified_files.insert(canonical_path.to_string(), data);
        } else {
            // New file
            self.added_files.insert(canonical_path.to_string(), data);
        }
    }

    pub fn file_modified(
        &mut self,
        canonical_path: &str,
        old_data: FileMetadata,
        new_data: FileMetadata,
    ) {
        if self.added_files.contains_key(canonical_path) {
            // File did not exist before this batch. Further modification only
            // updates metadata
            self.added_files
                .insert(canonical_path.to_string(), new_data);
        } else {
            if !self.initial_metadata.contains_key(canonical_path) {
                self.initial_metadata
                    .insert(canonical_path.to_string(), old_data);
            }
            self.modified_files
                .insert(canonical_path.to_string(), new_data);
        }
    }

    pub fn file_removed(&mut self, canonical_path: &str, data: FileMetadata) {
        // Check if this file was added in the same batch
        if self.added_files.remove(canonical_path).is_none() {
            let initial_data = match self.initial_metadata.get(canonical_path) {
                Some(d) => d.clone(),
                None => {
                    self.initial_metadata
                        .insert(canonical_path.to_string(), data.clone());
                    data
                }
            };

            // File was not added in this batch, so add to removed with last metadata
            self.modified_files.remove(canonical_path);
            self.removed_files
                .insert(canonical_path.to_string(), initial_data);
        }
        // else: File was added then removed in the same batch - no net change
    }

    pub fn get_size(&self) -> usize {
        self.added_directories.len()
            + self.removed_directories.len()
            + self.added_files.len()
            + self.modified_files.len()
            + self.removed_files.len()
    }

    /// `getView()`, consuming the aggregator.
    pub fn into_changes(self) -> Changes {
        Changes {
            added_directories: self.added_directories.into_vec(),
            removed_directories: self.removed_directories.into_vec(),
            added_files: self.added_files.into_vec(),
            modified_files: self.modified_files.into_vec(),
            removed_files: self.removed_files.into_vec(),
        }
    }

    /// `getView()` without consuming.
    pub fn changes(&self) -> Changes {
        Changes {
            added_directories: self.added_directories.iter().map(String::from).collect(),
            removed_directories: self.removed_directories.iter().map(String::from).collect(),
            added_files: clone_entries(&self.added_files),
            modified_files: clone_entries(&self.modified_files),
            removed_files: clone_entries(&self.removed_files),
        }
    }
}

fn clone_entries(map: &JsMap<FileMetadata>) -> Vec<(String, FileMetadata)> {
    map.iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}
