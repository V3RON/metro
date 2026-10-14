//! napi-rs binding. Each ported component adds its exports here in later phases.

use napi_derive::napi;

/// Version of the Rust core.
#[napi]
pub fn version() -> String {
    metro_rs_core::version().to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_matches_core() {
        assert_eq!(super::version(), metro_rs_core::version());
    }
}
