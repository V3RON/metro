//! Port of `metro-file-map/src/lib/RootPathUtils.js`.
//!
//! TODO(windows): POSIX separators only. `path.sep` is `/`, `IS_WIN32` is
//! false, and `\` is an ordinary character.
//!
//! Index arithmetic is on bytes. Every index the JS computes sits on a segment
//! boundary (it advances by whole matched segments plus a separator), so byte
//! and UTF-16 positions select the same text.
//!
//! This module provides path utility functions - similar to `node:path` -
//! optimised for Metro's use case (many paths, few roots) under assumptions
//! typically safe to make within Metro - namely:
//!
//!  - All input path separators must be system-native.
//!  - Double/redundant separators like '/foo//bar' are not supported.
//!  - All characters except separators are assumed to be valid in path segments.
//!
//!  - A "well-formed" path is any path following the rules above.
//!  - A "normal" path is a root-relative well-formed path with no redundant
//!    indirections. Normal paths have no leading './`, and the normal path of
//!    the root is the empty string.
//!
//! Trailing path separators are preserved, except for fs roots in
//! `normal_to_absolute` (fs roots always have a trailing separator), and the
//! project root in `absolute_to_normal` and `relative_to_normal` (the project
//! root is always the empty string, and is always a directory, so a trailing
//! separator is redundant).

use metro_rs_resolver::path;

const SEP: u8 = b'/';
const UP_FRAGMENT_SEP: &str = "../";
const UP_FRAGMENT_SEP_LENGTH: usize = 3;
const CURRENT_FRAGMENT: &[u8] = b"./";
/// `ROOT_BASE_IDX` (`IS_WIN32 ? 0 : 1`).
const ROOT_BASE_IDX: usize = 1;

pub struct JoinResult {
    pub normal_path: String,
    pub collapsed_segments: usize,
}

pub struct RootPathUtils {
    root_dir: String,
    root_dirnames: Vec<String>,
    root_parts: Vec<String>,
    root_depth: usize,
}

/// `s.slice(pos)` for a byte position on a boundary (empty when past the end).
fn slice_from(s: &str, pos: usize) -> &str {
    s.get(pos..).unwrap_or("")
}

/// `s.startsWith(search, pos)`: `pos` is clamped to the string length.
fn starts_with_at(s: &[u8], search: &[u8], pos: usize) -> bool {
    s[pos.min(s.len())..].starts_with(search)
}

/// `s.indexOf(needle, pos)`: -1 (here `None`) when `pos` is past the end.
fn index_of(s: &[u8], needle: &[u8], pos: usize) -> Option<usize> {
    if pos > s.len() {
        return None;
    }
    s[pos..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i + pos)
}

impl RootPathUtils {
    pub fn new(root_dir: &str) -> Self {
        let mut root_dirnames = Vec::new();
        let mut previous: Option<String> = None;
        let mut next = root_dir.to_string();
        while previous.as_deref() != Some(next.as_str()) {
            root_dirnames.push(next.clone());
            let dir = path::dirname(&next);
            previous = Some(std::mem::replace(&mut next, dir));
        }
        let mut root_parts: Vec<String> = root_dir.split('/').map(str::to_string).collect();
        let root_depth = root_dirnames.len() - 1;
        // If rootDir is a filesystem root (C:\ or /), it will end in a separator and
        // give a spurious empty entry at the end of rootParts.
        if root_depth == 0 {
            root_parts.pop();
        }
        Self {
            root_dir: root_dir.to_string(),
            root_dirnames,
            root_parts,
            root_depth,
        }
    }

    pub fn root_dir(&self) -> &str {
        &self.root_dir
    }

    /// `getBasenameOfNthAncestor`. JS yields `undefined` out of range; here "".
    pub fn get_basename_of_nth_ancestor(&self, n: usize) -> &str {
        (self.root_parts.len())
            .checked_sub(1 + n)
            .and_then(|i| self.root_parts.get(i))
            .map_or("", String::as_str)
    }

    pub fn get_parts(&self) -> &[String] {
        &self.root_parts
    }

    /// `absolutePath` may be any well-formed absolute path.
    pub fn absolute_to_normal(&self, absolute_path: &str) -> String {
        let ab = absolute_path.as_bytes();
        let mut end_of_matching_prefix = 0usize;
        let mut last_matching_part_idx = 0usize;

        while let Some(next_part) = self.root_parts.get(last_matching_part_idx) {
            let next_length = next_part.len();
            // Check that absolutePath is equal to nextPart + '/' or ends with
            // nextPart, starting from endOfMatchingPrefix.
            if starts_with_at(ab, next_part.as_bytes(), end_of_matching_prefix)
                && (ab.len() == end_of_matching_prefix + next_length
                    || ab.get(end_of_matching_prefix + next_length) == Some(&SEP))
            {
                // Move our matching pointer forward and load the next part.
                end_of_matching_prefix += next_length + 1;
                last_matching_part_idx += 1;
            } else {
                break;
            }
        }

        // If our root is /project/root and we're given /project/bar/foo.js, we
        // have matched up to '/project', and will need to return a path
        // beginning '../' (one prepended indirection, to go up from 'root').
        //
        // If we're given /project/../project2/otherroot, we have one level of
        // indirection up to prepend in the same way as above. There's another
        // explicit indirection already present in the input - we'll account for
        // that in try_collapse_indirections_in_suffix.
        let up_indirections_to_prepend = self.root_parts.len() - last_matching_part_idx;

        match self.try_collapse_indirections_in_suffix(
            absolute_path,
            end_of_matching_prefix,
            up_indirections_to_prepend,
        ) {
            Some((collapsed, _)) => collapsed,
            None => self.slow_absolute_to_normal(absolute_path),
        }
    }

    fn slow_absolute_to_normal(&self, absolute_path: &str) -> String {
        let ends_with_sep = absolute_path.ends_with('/');
        let result = path::relative(&self.root_dir, absolute_path);
        if ends_with_sep && !result.ends_with('/') {
            result + "/"
        } else {
            result
        }
    }

    /// `normal_path` is assumed to be normal (root-relative, no redundant
    /// indirection), per the definition above.
    pub fn normal_to_absolute(&self, normal_path: &str) -> String {
        let mut left: &str = &self.root_dir;
        let mut i = 0usize;
        let mut pos = 0usize;
        while normal_path
            .get(pos..)
            .is_some_and(|s| s.starts_with(UP_FRAGMENT_SEP))
            || (normal_path.ends_with("..") && normal_path.len() == 2 + pos)
        {
            if i != self.root_depth {
                i += 1;
            }
            left = &self.root_dirnames[i];
            pos += UP_FRAGMENT_SEP_LENGTH;
        }
        let right = if pos == 0 {
            normal_path
        } else {
            slice_from(normal_path, pos)
        };
        if right.is_empty() {
            return left.to_string();
        }
        // left may already end in a path separator only if it is a filesystem root,
        // '/' or 'X:\'.
        if i == self.root_depth {
            return format!("{left}{right}");
        }
        format!("{left}/{right}")
    }

    pub fn relative_to_normal(&self, relative_path: &str) -> String {
        match self.try_collapse_indirections_in_suffix(relative_path, 0, 0) {
            Some((collapsed, _)) => collapsed,
            None => path::relative(
                &self.root_dir,
                &path::join(&[&self.root_dir, relative_path]),
            ),
        }
    }

    pub fn resolve_symlink_to_normal(
        &self,
        symlink_normal_path: &str,
        readlink_result: &str,
    ) -> String {
        // Lexically resolves the target against the symlink's directory. This is
        // string manipulation only: symlinks within the target are not followed,
        // and the target need not exist, so the result is not a real path.
        //
        // readlink returns whatever the link was created with, which need not be
        // well-formed (e.g. '..', 'a/./b', 'a//b'), so resolve with node:path.
        let normal = self.absolute_to_normal(&path::resolve(&[
            &self.root_dir,
            symlink_normal_path,
            "..",
            readlink_result,
        ]));
        // Normalization keeps a trailing separator when the result is the root or
        // an ancestor of it (e.g. a link to '/'), and readlink itself may return
        // one. A stored symlink target never has one.
        match normal.strip_suffix('/') {
            Some(s) => s.to_string(),
            None => normal,
        }
    }

    /// Takes a normal and relative path, and joins them efficiently into a normal
    /// path, including collapsing trailing '..' in the first part with leading
    /// project root segments in the relative part.
    ///
    /// Panics where the JS `invariant` throws ('Failed to collapse'), which
    /// needs a non-normal `relative_path` (a `./` after its first segment).
    pub fn join_normal_to_relative(&self, normal_path: &str, relative_path: &str) -> JoinResult {
        if normal_path.is_empty() {
            return JoinResult {
                collapsed_segments: 0,
                normal_path: relative_path.to_string(),
            };
        }
        if relative_path.is_empty() {
            return JoinResult {
                collapsed_segments: 0,
                normal_path: normal_path.to_string(),
            };
        }
        let mut raw_path = String::with_capacity(normal_path.len() + 1 + relative_path.len());
        raw_path.push_str(normal_path);
        if !normal_path.ends_with('/') {
            raw_path.push('/');
        }
        raw_path.push_str(relative_path);
        if normal_path == ".." || normal_path.ends_with("/..") {
            let (collapsed_path, collapsed_segments) = self
                .try_collapse_indirections_in_suffix(&raw_path, 0, 0)
                .expect("Failed to collapse");
            return JoinResult {
                collapsed_segments,
                normal_path: collapsed_path,
            };
        }
        JoinResult {
            collapsed_segments: 0,
            normal_path: raw_path,
        }
    }

    /// Internal: Tries to collapse sequences like `../root/foo` for root
    /// `/project/root` down to the normal 'foo'.
    ///
    /// `full_path` is a string ending with the relative path to process,
    /// `start_of_relative_part` the index of the start of the part to process,
    /// and `implicit_up_indirections` 0=root-relative, 1=dirname(root)-relative...
    fn try_collapse_indirections_in_suffix(
        &self,
        full_path: &str,
        start_of_relative_part: usize,
        implicit_up_indirections: usize,
    ) -> Option<(String, usize)> {
        let fp = full_path.as_bytes();
        let mut total_up_indirections = implicit_up_indirections;
        let mut collapsed_segments = 0usize;
        let mut pos = start_of_relative_part;
        // Allow any sequence of indirection fragments at the start of the
        // unmatched suffix e.g /project/[../../foo], but bail out to Node's
        // path.relative if we find a possible indirection after any later segment,
        // or on any "./" that isn't a "../".
        loop {
            let Some(next_indirection) = index_of(fp, CURRENT_FRAGMENT, pos) else {
                // If we have any indirections, they may "collapse" if a subsequent
                // segment re-enters a directory we had previously exited, e.g:
                // /project/root/../root/foo should collapse to /project/root/foo' and
                // return foo, not ../root/foo.
                //
                // We match each segment following redirections, in turn, against the
                // part of the root path they may collapse into, and break on the first
                // mismatch.
                while total_up_indirections > 0 {
                    let Some(segment_to_maybe_collapse) = self
                        .root_parts
                        .len()
                        .checked_sub(total_up_indirections)
                        .and_then(|i| self.root_parts.get(i))
                    else {
                        break;
                    };
                    let seg = segment_to_maybe_collapse.as_bytes();
                    if starts_with_at(fp, seg, pos)
                        // The following character should be either a separator or end of
                        // string
                        && (fp.len() == seg.len() + pos || fp.get(seg.len() + pos) == Some(&SEP))
                    {
                        pos += seg.len() + 1;
                        collapsed_segments += 1;
                        total_up_indirections -= 1;
                    } else {
                        break;
                    }
                }
                // After collapsing we may have no more segments remaining (following
                // '..' indirections). Ensure that we don't drop or add a trailing
                // separator in this case by taking .slice(pos-1). In any other case,
                // we know that fullPath[pos] is a separator.
                if pos >= fp.len() {
                    let collapsed_path = if total_up_indirections > 0 {
                        format!(
                            "{}..{}",
                            UP_FRAGMENT_SEP.repeat(total_up_indirections - 1),
                            slice_from(full_path, pos.saturating_sub(1))
                        )
                    } else {
                        String::new()
                    };
                    return Some((collapsed_path, collapsed_segments));
                }
                let right = if pos > 0 {
                    slice_from(full_path, pos)
                } else {
                    full_path
                };
                if right == ".." && total_up_indirections >= self.root_parts.len() - 1 {
                    // If we have no right side (or an indirection that would take us
                    // below the root), just ensure we don't include a trailing separator.
                    let mut s = UP_FRAGMENT_SEP.repeat(total_up_indirections);
                    s.pop();
                    return Some((s, collapsed_segments));
                }
                // Optimisation for the common case, saves a concatenation.
                if total_up_indirections == 0 {
                    return Some((right.to_string(), collapsed_segments));
                }
                return Some((
                    format!("{}{}", UP_FRAGMENT_SEP.repeat(total_up_indirections), right),
                    collapsed_segments,
                ));
            };

            // Cap the number of indirections at the total number of root parts.
            // File systems treat '..' at the root as '.'. For Windows, cross-device
            // paths need to survive this.
            if total_up_indirections < self.root_parts.len() - ROOT_BASE_IDX {
                total_up_indirections += 1;
            }

            if next_indirection != pos + 1 // Fallback when ./ later in the path, or leading
                || fp.get(pos) != Some(&b'.')
            // and for anything other than a leading ../
            {
                return None;
            }
            pos += UP_FRAGMENT_SEP_LENGTH;
        }
    }
}
