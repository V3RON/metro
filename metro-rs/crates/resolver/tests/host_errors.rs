//! A host callback that fails: `resolve` returns the host's error unchanged.
//! Rust-only: there is no fixture equivalent for a throwing JS callback.

use metro_rs_resolver::{
    resolve, HostError, Lookup, PackageForModule, PackageJson, ResolveError, ResolveRequest,
    ResolverConfig, ResolverHost,
};

struct FailingHost;

fn boom() -> HostError {
    HostError {
        name: "Error".to_string(),
        message: "lookup exploded".to_string(),
    }
}

impl ResolverHost for FailingHost {
    fn file_system_lookup(&self, _path: &str) -> Result<Lookup, HostError> {
        Err(boom())
    }
    fn does_file_exist(&self, _path: &str) -> Result<bool, HostError> {
        Ok(false)
    }
    fn get_package(&self, _p: &str) -> Result<Option<PackageJson>, HostError> {
        Ok(None)
    }
    fn get_package_for_module(&self, _p: &str) -> Result<Option<PackageForModule>, HostError> {
        Ok(None)
    }
    fn resolve_haste_module(&self, _n: &str) -> Result<Option<String>, HostError> {
        Ok(None)
    }
    fn resolve_haste_package(&self, _n: &str) -> Result<Option<String>, HostError> {
        Ok(None)
    }
    fn resolve_asset(&self, _d: &str, _n: &str, _e: &str) -> Option<Vec<String>> {
        None
    }
    fn log_warning(&self, _m: &str) {}
}

#[test]
fn host_error_propagates_out_of_resolve() {
    let config = ResolverConfig::new();
    let options = serde_json::json!({});
    let request = ResolveRequest {
        origin_module_path: "/root/main.js",
        allow_haste: false,
        is_esm_import: false,
        dev: true,
        custom_resolver_options: &options,
    };
    for specifier in ["./a", "pkg"] {
        let err = resolve(&config, &FailingHost, &request, specifier, None).unwrap_err();
        assert_eq!(err, ResolveError::Host(boom()), "{specifier}");
        assert_eq!(err.class_name(), "Error");
        assert_eq!(err.message(), "lookup exploded");
    }
}
