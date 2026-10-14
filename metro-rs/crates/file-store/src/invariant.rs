use std::fmt;

/// A JS `invariant(...)` that the JS would have thrown. The message is the JS
/// message. Callers that can ask the JS implementation should do so, so that
/// the real JS error is raised.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invariant(pub String);

impl fmt::Display for Invariant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Invariant {}
