//! `String.prototype.slice` / `indexOf` semantics in UTF-16 code units.
//!
//! Only used where the JS slices a string at an index that is not guaranteed to
//! sit on a segment boundary (see `hierarchical_lookup`). For ASCII strings
//! (practically all paths) this is plain byte arithmetic.

fn clamp_index(i: isize, len: usize) -> usize {
    if i < 0 {
        (len as isize + i).max(0) as usize
    } else {
        (i as usize).min(len)
    }
}

/// `s.slice(start, end)`.
pub fn slice(s: &str, start: isize, end: Option<isize>) -> String {
    if s.is_ascii() {
        let len = s.len();
        let a = clamp_index(start, len);
        let b = end.map_or(len, |e| clamp_index(e, len));
        return if a < b {
            s[a..b].to_string()
        } else {
            String::new()
        };
    }
    let u: Vec<u16> = s.encode_utf16().collect();
    let a = clamp_index(start, u.len());
    let b = end.map_or(u.len(), |e| clamp_index(e, u.len()));
    if a < b {
        String::from_utf16_lossy(&u[a..b])
    } else {
        String::new()
    }
}

/// `s.indexOf(needle, from)` for an ASCII needle; -1 when absent.
pub fn index_of(s: &str, needle: u8, from: isize) -> isize {
    if s.is_ascii() {
        let from = from.max(0) as usize;
        if from >= s.len() {
            return -1;
        }
        return s.as_bytes()[from..]
            .iter()
            .position(|&c| c == needle)
            .map_or(-1, |i| (i + from) as isize);
    }
    let from = from.max(0) as usize;
    s.encode_utf16()
        .enumerate()
        .skip(from)
        .find(|&(_, c)| c == needle as u16)
        .map_or(-1, |(i, _)| i as isize)
}

/// Length in UTF-16 code units.
pub fn utf16_len(s: &str) -> usize {
    if s.is_ascii() {
        s.len()
    } else {
        s.encode_utf16().count()
    }
}
