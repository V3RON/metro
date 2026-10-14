//! A JS `RegExp` for `RegExp.prototype.test`, on `regress` (an ECMAScript
//! regex engine).
//!
//! Flags: `i`, `m`, `s`, `u`, `v` and `d` (which does not affect `test`) are
//! supported. **`g` and `y` are unsupported**: with them `test` is stateful
//! (`lastIndex`), and a file map calling it for thousands of paths on several
//! threads cannot reproduce that. [`JsRegExp::new`] returns
//! [`RegExpError::Unsupported`] and the caller falls back to the JS
//! implementation.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegExpError {
    /// A valid JS RegExp this engine does not reproduce (flags `g` or `y`).
    Unsupported(String),
    /// The pattern or the flags are not valid ECMAScript.
    Invalid(String),
}

impl fmt::Display for RegExpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RegExpError::Unsupported(m) | RegExpError::Invalid(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for RegExpError {}

#[derive(Debug, Clone)]
pub struct JsRegExp {
    regex: regress::Regex,
    source: String,
    flags: String,
    /// The `u` or `v` flag.
    unicode: bool,
}

impl JsRegExp {
    /// `new RegExp(source, flags)`.
    pub fn new(source: &str, flags: &str) -> Result<Self, RegExpError> {
        let mut seen = String::new();
        for c in flags.chars() {
            if !"dgimsuvy".contains(c) || seen.contains(c) {
                return Err(RegExpError::Invalid(format!(
                    "Invalid flags supplied to RegExp constructor '{flags}'"
                )));
            }
            seen.push(c);
        }
        if seen.contains('g') || seen.contains('y') {
            return Err(RegExpError::Unsupported(format!(
                "RegExp flags '{flags}': `test` is stateful with the g and y flags"
            )));
        }
        let regex = regress::Regex::with_flags(source, flags)
            .map_err(|e| RegExpError::Invalid(format!("Invalid regular expression: {e}")))?;
        Ok(Self {
            regex,
            source: source.to_string(),
            flags: flags.to_string(),
            unicode: seen.contains('u') || seen.contains('v'),
        })
    }

    /// `regexp.test(text)` for a RegExp without `g` or `y`.
    pub fn test(&self, text: &str) -> bool {
        if text.is_ascii() {
            return self.regex.find(text).is_some();
        }
        // JS strings are UTF-16: without the `u` and `v` flags an astral
        // character is two code units (`/^.$/.test('\u{1F600}')` is false).
        let units: Vec<u16> = text.encode_utf16().collect();
        if self.unicode {
            self.regex.find_from_utf16(&units, 0).next().is_some()
        } else {
            self.regex.find_from_ucs2(&units, 0).next().is_some()
        }
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn flags(&self) -> &str {
        &self.flags
    }
}
