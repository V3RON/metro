//! Port of `resolveAsset.js`.

use crate::config::Ctx;
use crate::path;
use crate::Resolution;

/// Resolve a file path as an asset. Returns the set of files found after
/// expanding asset resolutions (e.g. `icon@2x.png`). Users may override this
/// behaviour via the host's `resolve_asset`.
pub fn resolve_asset(ctx: &Ctx<'_>, file_path: &str) -> Option<Resolution> {
    let dir_path = path::dirname(file_path);
    let extension = path::extname(file_path);
    let basename = path::basename_with_suffix(file_path, &extension);

    // The JS wraps this in try/catch and ignores exceptions; the host maps
    // failures to `None`.
    if !is_scaled_asset_name(&basename) {
        let assets = ctx.host().resolve_asset(&dir_path, &basename, &extension);
        if let Some(file_paths) = assets {
            return Some(Resolution::AssetFiles { file_paths });
        }
    }

    None
}

/// `/@\d+(?:\.\d+)?x$/.test(basename)`.
fn is_scaled_asset_name(basename: &str) -> bool {
    let Some(rest) = basename.strip_suffix('x') else {
        return false;
    };
    let trim_digits = |s: &str| -> (usize, usize) {
        let kept = s.trim_end_matches(|c: char| c.is_ascii_digit()).len();
        (kept, s.len() - kept)
    };
    let (kept, digits) = trim_digits(rest);
    if digits == 0 {
        return false;
    }
    let before = &rest[..kept];
    if before.ends_with('@') {
        return true;
    }
    if let Some(before_dot) = before.strip_suffix('.') {
        let (kept2, digits2) = trim_digits(before_dot);
        return digits2 > 0 && before_dot[..kept2].ends_with('@');
    }
    false
}

#[cfg(test)]
mod tests {
    use super::is_scaled_asset_name;

    #[test]
    fn scaled_names() {
        assert!(is_scaled_asset_name("icon@2x"));
        assert!(is_scaled_asset_name("icon@1.5x"));
        assert!(!is_scaled_asset_name("icon@x"));
        assert!(!is_scaled_asset_name("icon2x"));
        assert!(!is_scaled_asset_name("icon@2.x"));
        assert!(!is_scaled_asset_name("icon@2x.png"));
        assert!(!is_scaled_asset_name("icon"));
    }
}
