//! An approximation of `String.prototype.localeCompare` (ICU root collation,
//! which is what Node uses) for the ids and platforms that `compareStrings`
//! sorts. Exact for ASCII; other characters sort after letters by code point.

use std::cmp::Ordering;

/// ICU root order of the ASCII punctuation and symbols, before digits.
const PUNCTUATION: &str = " _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";

fn keys(s: &str) -> (Vec<u32>, Vec<u8>) {
    let mut primary = Vec::new();
    let mut tertiary = Vec::new();
    for c in s.chars() {
        let (p, t) = if let Some(i) = PUNCTUATION.find(c) {
            (1 + i as u32, 0)
        } else if c.is_ascii_digit() {
            (100 + (c as u32 - '0' as u32), 0)
        } else if c.is_ascii_alphabetic() {
            (
                200 + (c.to_ascii_lowercase() as u32 - 'a' as u32),
                c.is_ascii_uppercase() as u8,
            )
        } else {
            (1000 + c as u32, 0)
        };
        primary.push(p);
        tertiary.push(t);
    }
    (primary, tertiary)
}

pub fn locale_compare(a: &str, b: &str) -> Ordering {
    let (pa, ta) = keys(a);
    let (pb, tb) = keys(b);
    pa.cmp(&pb).then_with(|| ta.cmp(&tb))
}

/// `compareStrings` from `lib/sorting.js`.
pub fn compare_strings(a: Option<&str>, b: Option<&str>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => locale_compare(a, b),
    }
}

/// Default `Array.prototype.sort()` order: by UTF-16 code units.
pub fn compare_utf16(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}
