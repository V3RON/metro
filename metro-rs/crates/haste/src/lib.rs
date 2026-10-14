//! Port of metro-file-map's `HastePlugin` (and `haste/getPlatformExtension.js`,
//! `haste/computeConflicts.js`, `haste/DuplicateHasteCandidatesError.js`,
//! `haste/HasteConflictsError.js`). Not ported: the per-file worker (the Haste
//! name of a file is an input here), `getCacheKey` and `getWorker`.
//!
//! TODO(windows): POSIX separators only.

mod collate;
mod errors;
mod haste_map;
#[cfg(test)]
mod oracle_data;
mod platform;

pub use errors::{DuplicateHasteCandidatesError, HasteConflictsError, HasteError};
pub use haste_map::{
    ConflictType, HasteConflict, HasteDelta, HasteMap, HasteOptions, ModuleType, GENERIC_PLATFORM,
    NATIVE_PLATFORM,
};
pub use platform::get_platform_extension;

#[cfg(test)]
mod tests;
