//! Assembles the bytes of a plain JS bundle: `bundleToString(baseJSBundle(..))`
//! of packages/metro, without ever making a JavaScript string of the whole
//! bundle.
//!
//! Module code is held here as UTF-8, keyed by a token the JavaScript side
//! chooses (one per distinct code string). A bundle is a list of entries in
//! module id order: a token plus the parameters of its `__d(...)` call (the
//! text `addParamsToDefineCall` inserts before the call's last `)`), or a
//! literal that JavaScript already wrapped.
//!
//! The JavaScript length of the result (UTF-16 code units, what
//! `bundle.length` reports) is computed along the way, so the server can log
//! it without decoding anything.

use std::collections::{HashMap, HashSet};

/// A module's code as stored.
struct StoredCode {
    bytes: Vec<u8>,
    /// Byte index of the last `)`, if there is one.
    last_paren: Option<usize>,
    /// Length in UTF-16 code units.
    utf16: u64,
}

/// One bundle entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry<'a> {
    /// The stored code of `token`; with non-empty `params`, they are inserted
    /// as `,<params>` before the last `)`.
    Token { token: u32, params: &'a [u8] },
    /// Code that is used as it is.
    Literal(&'a [u8]),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A token that was never inserted (or was evicted).
    UnknownToken(u32),
    /// Parameters for code that has no `)` to put them before. JavaScript
    /// wraps such code itself and sends it as a literal.
    NoClosingParen(u32),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::UnknownToken(t) => write!(f, "module code token {t} is not held"),
            Error::NoClosingParen(t) => {
                write!(f, "module code token {t} has no ')' to add parameters to")
            }
        }
    }
}

impl std::error::Error for Error {}

/// Length of `bytes` (valid UTF-8) in UTF-16 code units: characters from
/// U+10000 up (4 bytes in UTF-8) count 2.
pub fn utf16_len(bytes: &[u8]) -> u64 {
    // Every byte that is not a continuation byte starts a character; a
    // 4-byte lead (>= 0xF0) is a surrogate pair in UTF-16.
    let mut n: u64 = 0;
    for &b in bytes {
        if (b & 0xC0) != 0x80 {
            n += 1;
        }
        if b >= 0xF0 {
            n += 1;
        }
    }
    n
}

#[derive(Default)]
pub struct ModuleStore {
    codes: HashMap<u32, StoredCode>,
}

impl ModuleStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores `code` (UTF-8) under `token`, replacing what was there.
    pub fn insert(&mut self, token: u32, code: Vec<u8>) {
        let last_paren = code.iter().rposition(|&b| b == b')');
        let utf16 = utf16_len(&code);
        self.codes.insert(
            token,
            StoredCode {
                bytes: code,
                last_paren,
                utf16,
            },
        );
    }

    pub fn contains(&self, token: u32) -> bool {
        self.codes.contains_key(&token)
    }

    pub fn len(&self) -> usize {
        self.codes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.codes.is_empty()
    }

    /// Drops every token that is not in `tokens`.
    pub fn retain(&mut self, tokens: &HashSet<u32>) {
        self.codes.retain(|t, _| tokens.contains(t));
    }

    /// Drops every token that no entry refers to.
    pub fn retain_entries(&mut self, entries: &[Entry<'_>]) {
        let used: HashSet<u32> = entries
            .iter()
            .filter_map(|e| match e {
                Entry::Token { token, .. } => Some(*token),
                Entry::Literal(_) => None,
            })
            .collect();
        self.retain(&used);
    }
}

/// `bundleToString` over the given parts. Returns the bytes and their length
/// in UTF-16 code units.
pub fn assemble(
    store: &ModuleStore,
    pre: &[u8],
    entries: &[Entry<'_>],
    post: &[u8],
) -> Result<(Vec<u8>, u64), Error> {
    // First pass: validate, and compute the exact sizes.
    let mut bytes: usize = 0;
    let mut utf16: u64 = 0;
    if !pre.is_empty() {
        bytes += pre.len() + 1;
        utf16 += utf16_len(pre) + 1;
    }
    for entry in entries {
        match *entry {
            Entry::Token { token, params } => {
                let stored = store.codes.get(&token).ok_or(Error::UnknownToken(token))?;
                if stored.bytes.is_empty() {
                    // Empty code is skipped, whatever the parameters (JS
                    // would not find a ')' either; it sends a literal).
                    if !params.is_empty() {
                        return Err(Error::NoClosingParen(token));
                    }
                    continue;
                }
                let mut b = stored.bytes.len();
                let mut u = stored.utf16;
                if !params.is_empty() {
                    if stored.last_paren.is_none() {
                        return Err(Error::NoClosingParen(token));
                    }
                    b += 1 + params.len();
                    u += 1 + utf16_len(params);
                }
                bytes += b + 1;
                utf16 += u + 1;
            }
            Entry::Literal(code) => {
                if !code.is_empty() {
                    bytes += code.len() + 1;
                    utf16 += utf16_len(code) + 1;
                }
            }
        }
    }
    if !post.is_empty() {
        bytes += post.len();
        utf16 += utf16_len(post);
    } else if bytes > 0 {
        // The last "\n" is dropped.
        bytes -= 1;
        utf16 -= 1;
    }

    // Second pass: write.
    let mut out: Vec<u8> = Vec::with_capacity(bytes);
    if !pre.is_empty() {
        out.extend_from_slice(pre);
        out.push(b'\n');
    }
    for entry in entries {
        match *entry {
            Entry::Token { token, params } => {
                let stored = &store.codes[&token];
                if stored.bytes.is_empty() {
                    continue;
                }
                if params.is_empty() {
                    out.extend_from_slice(&stored.bytes);
                } else {
                    let idx = stored.last_paren.unwrap_or(0);
                    out.extend_from_slice(&stored.bytes[..idx]);
                    out.push(b',');
                    out.extend_from_slice(params);
                    out.extend_from_slice(&stored.bytes[idx..]);
                }
                out.push(b'\n');
            }
            Entry::Literal(code) => {
                if !code.is_empty() {
                    out.extend_from_slice(code);
                    out.push(b'\n');
                }
            }
        }
    }
    if !post.is_empty() {
        out.extend_from_slice(post);
    } else {
        out.pop();
    }
    debug_assert_eq!(out.len(), bytes);
    Ok((out, utf16))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(items: &[(u32, &str)]) -> ModuleStore {
        let mut s = ModuleStore::new();
        for (t, c) in items {
            s.insert(*t, c.as_bytes().to_vec());
        }
        s
    }

    fn run(s: &ModuleStore, pre: &str, entries: &[Entry], post: &str) -> (String, u64) {
        let (b, u) = assemble(s, pre.as_bytes(), entries, post.as_bytes()).unwrap();
        (String::from_utf8(b).unwrap(), u)
    }

    fn tok(token: u32, params: &str) -> Entry<'_> {
        Entry::Token {
            token,
            params: params.as_bytes(),
        }
    }

    #[test]
    fn pre_post_combinations() {
        let s = store(&[(1, "a()"), (2, "b()")]);
        let e = [tok(1, ""), tok(2, "")];
        assert_eq!(run(&s, "", &e, "").0, "a()\nb()");
        assert_eq!(run(&s, "P", &e, "").0, "P\na()\nb()");
        assert_eq!(run(&s, "", &e, "Q").0, "a()\nb()\nQ");
        assert_eq!(run(&s, "P", &e, "Q").0, "P\na()\nb()\nQ");
        assert_eq!(run(&s, "", &[], "").0, "");
        assert_eq!(run(&s, "P", &[], "").0, "P");
        assert_eq!(run(&s, "", &[], "Q").0, "Q");
    }

    #[test]
    fn empty_code_is_skipped() {
        let s = store(&[(1, ""), (2, "b()")]);
        let e = [tok(1, ""), Entry::Literal(b""), tok(2, "")];
        assert_eq!(run(&s, "", &e, "").0, "b()");
        assert_eq!(run(&s, "", &[tok(1, "")], "").0, "");
        assert_eq!(run(&s, "", &[tok(1, "")], "Q").0, "Q");
    }

    #[test]
    fn params_are_spliced_before_the_last_paren() {
        let s = store(&[(1, "__d(function(){f(1)})")]);
        let e = [tok(1, "7,null,\"x\"")];
        assert_eq!(run(&s, "", &e, "").0, "__d(function(){f(1)},7,null,\"x\")");
    }

    #[test]
    fn literals_are_used_as_they_are() {
        let s = store(&[]);
        let e = [Entry::Literal(b"no parens"), Entry::Literal(b"x")];
        assert_eq!(run(&s, "", &e, "").0, "no parens\nx");
    }

    #[test]
    fn missing_paren_and_unknown_token_are_errors() {
        let s = store(&[(1, "no paren")]);
        assert_eq!(
            assemble(&s, b"", &[tok(1, "1")], b"").unwrap_err(),
            Error::NoClosingParen(1)
        );
        assert_eq!(
            assemble(&s, b"", &[tok(9, "")], b"").unwrap_err(),
            Error::UnknownToken(9)
        );
        // Without parameters the code is used as it is.
        assert_eq!(run(&s, "", &[tok(1, "")], "").0, "no paren");
    }

    #[test]
    fn utf16_length_counts_astral_characters_twice() {
        let code = "a\u{1F600}\u{e9}\u{4e2d})";
        let s = store(&[(1, code)]);
        let (text, u) = run(&s, "p\u{1F600}", &[tok(1, "\"\u{1F600}\"")], "q\u{10000}");
        let expected: Vec<u16> = text.encode_utf16().collect();
        assert_eq!(u as usize, expected.len());
        assert_eq!(utf16_len("\u{1F600}".as_bytes()), 2);
        assert_eq!(utf16_len("\u{7ff}\u{ffff}".as_bytes()), 2);
    }

    #[test]
    fn utf16_length_without_post() {
        let s = store(&[(1, "\u{1F600}()")]);
        let (text, u) = run(&s, "", &[tok(1, "")], "");
        assert_eq!(u as usize, text.encode_utf16().count());
    }

    #[test]
    fn retain_evicts_unreferenced_tokens() {
        let mut s = store(&[(1, "a()"), (2, "b()"), (3, "c()")]);
        let e = [tok(2, ""), Entry::Literal(b"z"), tok(3, "")];
        s.retain_entries(&e);
        assert!(!s.contains(1));
        assert!(s.contains(2) && s.contains(3));
        assert_eq!(s.len(), 2);
    }
}
