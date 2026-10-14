//! Port of `plugins/haste/getPlatformExtension.js`.

use std::collections::HashSet;

/// `lastIndexOf('.', from)`: a negative `from` is clamped to 0.
fn last_index_of_dot(s: &str, from: isize) -> Option<usize> {
    let b = s.as_bytes();
    if b.is_empty() {
        return None;
    }
    let from = from.max(0) as usize;
    let start = from.min(b.len() - 1);
    (0..=start).rev().find(|&i| b[i] == b'.')
}

/// Extract platform extension: index.ios.js -> ios
pub fn get_platform_extension<'a>(file: &'a str, platforms: &HashSet<String>) -> Option<&'a str> {
    let last = last_index_of_dot(file, file.len() as isize)?;
    let second_to_last = last_index_of_dot(file, last as isize - 1)?;
    // `substring` swaps its arguments when the first is larger (`last == 0`).
    let (a, b) = (second_to_last + 1, last);
    let platform = if a <= b { &file[a..b] } else { &file[b..a] };
    platforms.contains(platform).then_some(platform)
}
