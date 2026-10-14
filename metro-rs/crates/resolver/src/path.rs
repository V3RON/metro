//! The subset of Node's `node:path` that metro-resolver uses, with **POSIX**
//! semantics, ported function by function from Node's `lib/path.js`.
//!
//! TODO(windows): Windows separators and drive roots are out of scope for now.
//! `SEP` is `/` and everything here treats `\` as an ordinary character. Only
//! `win32_is_absolute` knows about Windows, because `resolve.js` calls
//! `path.win32.isAbsolute` on purpose, for package.json values.

pub const SEP: &str = "/";

const SLASH: u8 = b'/';
const DOT: u8 = b'.';

fn to_string(bytes: &[u8]) -> String {
    // Every slice boundary is at an ASCII byte ('/' or '.'), so this is valid.
    String::from_utf8_lossy(bytes).into_owned()
}

/// `path.posix.isAbsolute`.
pub fn is_absolute(p: &str) -> bool {
    p.as_bytes().first() == Some(&SLASH)
}

/// `path.win32.isAbsolute`.
pub fn win32_is_absolute(p: &str) -> bool {
    let b = p.as_bytes();
    let is_sep = |c: u8| c == b'/' || c == b'\\';
    if b.is_empty() {
        return false;
    }
    is_sep(b[0]) || (b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && is_sep(b[2]))
}

/// Resolves . and .. segments of a path with directory names.
fn normalize_string(p: &[u8], allow_above_root: bool) -> Vec<u8> {
    let mut res: Vec<u8> = Vec::new();
    let mut last_segment_length: isize = 0;
    let mut last_slash: isize = -1;
    let mut dots: isize = 0;
    let len = p.len() as isize;
    let mut i: isize = 0;
    while i <= len {
        let code = if i < len { p[i as usize] } else { SLASH };
        if code == SLASH {
            if last_slash == i - 1 || dots == 1 {
                // NOOP
            } else if dots == 2 {
                if res.len() < 2
                    || last_segment_length != 2
                    || res[res.len() - 1] != DOT
                    || res[res.len() - 2] != DOT
                {
                    if res.len() > 2 {
                        match res.iter().rposition(|&c| c == SLASH) {
                            None => {
                                res.clear();
                                last_segment_length = 0;
                            }
                            Some(idx) => {
                                res.truncate(idx);
                                last_segment_length = match res.iter().rposition(|&c| c == SLASH) {
                                    Some(j) => res.len() as isize - 1 - j as isize,
                                    None => res.len() as isize,
                                };
                            }
                        }
                        last_slash = i;
                        dots = 0;
                        i += 1;
                        continue;
                    } else if !res.is_empty() {
                        res.clear();
                        last_segment_length = 0;
                        last_slash = i;
                        dots = 0;
                        i += 1;
                        continue;
                    }
                }
                if allow_above_root {
                    if res.is_empty() {
                        res.extend_from_slice(b"..");
                    } else {
                        res.extend_from_slice(b"/..");
                    }
                    last_segment_length = 2;
                }
            } else {
                let seg = &p[(last_slash + 1) as usize..i as usize];
                if !res.is_empty() {
                    res.push(SLASH);
                }
                res.extend_from_slice(seg);
                last_segment_length = i - last_slash - 1;
            }
            last_slash = i;
            dots = 0;
        } else if code == DOT && dots != -1 {
            dots += 1;
        } else {
            dots = -1;
        }
        i += 1;
    }
    res
}

/// `path.normalize`.
pub fn normalize(p: &str) -> String {
    if p.is_empty() {
        return ".".to_string();
    }
    if is_normalized(p) {
        return p.to_string();
    }
    let b = p.as_bytes();
    let absolute = b[0] == SLASH;
    let trailing_separator = b[b.len() - 1] == SLASH;
    let mut out = normalize_string(b, !absolute);
    if out.is_empty() {
        if absolute {
            return "/".to_string();
        }
        return if trailing_separator { "./" } else { "." }.to_string();
    }
    if trailing_separator {
        out.push(SLASH);
    }
    if absolute {
        format!("/{}", to_string(&out))
    } else {
        to_string(&out)
    }
}

/// Whether `normalize(p)` would return `p` unchanged: no empty, `.` or `..`
/// segments, other than the empty segments of a leading `/` (absolute) and
/// of one trailing `/` (kept by normalize). Most paths the resolver builds are
/// already normal, and this avoids rebuilding them.
fn is_normalized(p: &str) -> bool {
    if p.is_empty() {
        return false;
    }
    let inner = p.strip_prefix('/').unwrap_or(p);
    let inner = inner.strip_suffix('/').unwrap_or(inner);
    if inner.is_empty() {
        // "/" is normal; "" and "//" are not (handled above or slow path).
        return p == "/";
    }
    inner
        .split('/')
        .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

/// `path.join`.
pub fn join(args: &[&str]) -> String {
    let mut joined: Option<String> = None;
    for arg in args {
        if !arg.is_empty() {
            match &mut joined {
                None => joined = Some((*arg).to_string()),
                Some(j) => {
                    j.push('/');
                    j.push_str(arg);
                }
            }
        }
    }
    match joined {
        None => ".".to_string(),
        Some(j) if is_normalized(&j) => j,
        Some(j) => normalize(&j),
    }
}

/// `path.resolve`. Relative inputs resolve against the process working
/// directory, like Node.
pub fn resolve(args: &[&str]) -> String {
    let mut resolved_path = String::new();
    let mut resolved_absolute = false;
    let mut i = args.len() as isize - 1;
    while i >= -1 && !resolved_absolute {
        let cwd;
        let p: &str = if i >= 0 {
            args[i as usize]
        } else {
            cwd = std::env::current_dir()
                .ok()
                .and_then(|d| d.to_str().map(str::to_string))
                .unwrap_or_else(|| "/".to_string());
            &cwd
        };
        i -= 1;
        if p.is_empty() {
            continue;
        }
        resolved_path = format!("{p}/{resolved_path}");
        resolved_absolute = p.as_bytes()[0] == SLASH;
    }
    let normalized = to_string(&normalize_string(
        resolved_path.as_bytes(),
        !resolved_absolute,
    ));
    if resolved_absolute {
        return format!("/{normalized}");
    }
    if normalized.is_empty() {
        ".".to_string()
    } else {
        normalized
    }
}

/// `path.dirname`.
pub fn dirname(p: &str) -> String {
    let b = p.as_bytes();
    if b.is_empty() {
        return ".".to_string();
    }
    let has_root = b[0] == SLASH;
    let mut end: isize = -1;
    let mut matched_slash = true;
    let mut i = b.len() as isize - 1;
    while i >= 1 {
        if b[i as usize] == SLASH {
            if !matched_slash {
                end = i;
                break;
            }
        } else {
            matched_slash = false;
        }
        i -= 1;
    }
    if end == -1 {
        return if has_root { "/" } else { "." }.to_string();
    }
    if has_root && end == 1 {
        return "//".to_string();
    }
    p[..end as usize].to_string()
}

/// `path.basename(p)` (without the suffix argument).
pub fn basename(p: &str) -> String {
    let b = p.as_bytes();
    let mut start = 0usize;
    let mut end: isize = -1;
    let mut matched_slash = true;
    let mut i = b.len() as isize - 1;
    while i >= 0 {
        if b[i as usize] == SLASH {
            if !matched_slash {
                start = i as usize + 1;
                break;
            }
        } else if end == -1 {
            matched_slash = false;
            end = i + 1;
        }
        i -= 1;
    }
    if end == -1 {
        return String::new();
    }
    p[start..end as usize].to_string()
}

/// `path.basename(p, suffix)`.
pub fn basename_with_suffix(p: &str, suffix: &str) -> String {
    if !suffix.is_empty() && suffix.len() <= p.len() {
        if suffix == p {
            return String::new();
        }
        let base = basename(p);
        // Node does not strip the suffix when it is the entire base name.
        if base != suffix && base.ends_with(suffix) {
            return base[..base.len() - suffix.len()].to_string();
        }
        return base;
    }
    basename(p)
}

/// The shared scan of `extname` and `parse`: returns
/// `(start_dot, start_part, end, pre_dot_state)`.
fn scan_ext(b: &[u8], lowest: usize) -> (isize, usize, isize, i32) {
    let mut start_dot: isize = -1;
    let mut start_part = 0usize;
    let mut end: isize = -1;
    let mut matched_slash = true;
    let mut pre_dot_state = 0;
    let mut i = b.len() as isize - 1;
    while i >= lowest as isize {
        let code = b[i as usize];
        if code == SLASH {
            if !matched_slash {
                start_part = i as usize + 1;
                break;
            }
            i -= 1;
            continue;
        }
        if end == -1 {
            matched_slash = false;
            end = i + 1;
        }
        if code == DOT {
            if start_dot == -1 {
                start_dot = i;
            } else if pre_dot_state != 1 {
                pre_dot_state = 1;
            }
        } else if start_dot != -1 {
            pre_dot_state = -1;
        }
        i -= 1;
    }
    (start_dot, start_part, end, pre_dot_state)
}

/// `path.extname`.
pub fn extname(p: &str) -> String {
    let (start_dot, start_part, end, pre_dot_state) = scan_ext(p.as_bytes(), 0);
    if start_dot == -1
        || end == -1
        || pre_dot_state == 0
        || (pre_dot_state == 1 && start_dot == end - 1 && start_dot == start_part as isize + 1)
    {
        return String::new();
    }
    p[start_dot as usize..end as usize].to_string()
}

/// The result of `path.parse`.
#[derive(Debug, PartialEq, Eq, Default)]
pub struct ParsedPath {
    pub root: String,
    pub dir: String,
    pub base: String,
    pub ext: String,
    pub name: String,
}

/// `path.parse`.
pub fn parse(p: &str) -> ParsedPath {
    let mut ret = ParsedPath::default();
    let b = p.as_bytes();
    if b.is_empty() {
        return ret;
    }
    let absolute = b[0] == SLASH;
    let start = if absolute {
        ret.root = "/".to_string();
        1
    } else {
        0
    };
    let (start_dot, start_part, end, pre_dot_state) = scan_ext(b, start);
    if end != -1 {
        let start = if start_part == 0 && absolute {
            1
        } else {
            start_part
        };
        if start_dot == -1
            || pre_dot_state == 0
            || (pre_dot_state == 1 && start_dot == end - 1 && start_dot == start_part as isize + 1)
        {
            ret.base = p[start..end as usize].to_string();
            ret.name = ret.base.clone();
        } else {
            ret.name = p[start..start_dot as usize].to_string();
            ret.base = p[start..end as usize].to_string();
            ret.ext = p[start_dot as usize..end as usize].to_string();
        }
    }
    if start_part > 0 {
        ret.dir = p[..start_part - 1].to_string();
    } else if absolute {
        ret.dir = "/".to_string();
    }
    ret
}

/// `path.relative`.
pub fn relative(from: &str, to: &str) -> String {
    if from == to {
        return String::new();
    }
    let from = resolve(&[from]);
    let to = resolve(&[to]);
    if from == to {
        return String::new();
    }
    let f = from.as_bytes();
    let t = to.as_bytes();
    let from_start = 1usize;
    let from_end = f.len();
    let from_len = from_end - from_start;
    let to_start = 1usize;
    let to_len = t.len() - to_start;
    let length = from_len.min(to_len);
    let mut last_common_sep: isize = -1;
    let mut i = 0usize;
    while i < length {
        let from_code = f[from_start + i];
        if from_code != t[to_start + i] {
            break;
        } else if from_code == SLASH {
            last_common_sep = i as isize;
        }
        i += 1;
    }
    if i == length {
        if to_len > length {
            if t[to_start + i] == SLASH {
                return to[to_start + i + 1..].to_string();
            }
            if i == 0 {
                return to[to_start + i..].to_string();
            }
        } else if from_len > length {
            if f[from_start + i] == SLASH {
                last_common_sep = i as isize;
            } else if i == 0 {
                last_common_sep = 0;
            }
        }
    }
    let mut out = String::new();
    let mut i = from_start as isize + last_common_sep + 1;
    while i <= from_end as isize {
        if i == from_end as isize || f[i as usize] == SLASH {
            out.push_str(if out.is_empty() { ".." } else { "/.." });
        }
        i += 1;
    }
    format!(
        "{}{}",
        out,
        &to[(to_start as isize + last_common_sep) as usize..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected values were produced with Node (`node:path`, POSIX).
    #[test]
    fn join_matches_node() {
        assert_eq!(join(&["/a", "b", "../c"]), "/a/c");
        assert_eq!(join(&["a", ".."]), ".");
        assert_eq!(join(&["", ""]), ".");
        assert_eq!(join(&[]), ".");
        assert_eq!(join(&["/a/b/", "./c/"]), "/a/b/c/");
        assert_eq!(join(&["../x", "../.."]), "../..");
        assert_eq!(join(&["/..", "a"]), "/a");
        assert_eq!(join(&["/pkg", ".."]), "/");
        assert_eq!(join(&["/root", "./a//b/../c.js"]), "/root/a/c.js");
    }

    #[test]
    fn normalize_matches_node() {
        assert_eq!(normalize(""), ".");
        assert_eq!(normalize("./"), "./");
        assert_eq!(normalize("a/./b/.."), "a");
        assert_eq!(normalize("//a"), "/a");
        assert_eq!(normalize("../../a"), "../../a");
        assert_eq!(normalize("/a/b/../../.."), "/");
        assert_eq!(normalize("a/b/../../../c"), "../c");
    }

    #[test]
    fn dirname_basename_extname_match_node() {
        assert_eq!(dirname("/a/b/c"), "/a/b");
        assert_eq!(dirname("/a"), "/");
        assert_eq!(dirname("/"), "/");
        assert_eq!(dirname("a"), ".");
        assert_eq!(dirname("a/b/"), "a");
        assert_eq!(dirname("//a"), "//");
        assert_eq!(basename("/a/b.js"), "b.js");
        assert_eq!(basename("/a/b/"), "b");
        assert_eq!(basename("/"), "");
        assert_eq!(basename_with_suffix("/a/b.js", ".js"), "b");
        assert_eq!(basename_with_suffix("/a/.js", ".js"), ".js");
        assert_eq!(extname("a/b.tar.gz"), ".gz");
        assert_eq!(extname(".bashrc"), "");
        assert_eq!(extname("a."), ".");
        assert_eq!(extname("a/b"), "");
        assert_eq!(extname("..."), ".");
        assert_eq!(extname("a/.b/c"), "");
    }

    #[test]
    fn resolve_and_relative_match_node() {
        assert_eq!(resolve(&["/a/b", "../c"]), "/a/c");
        // The is_normalized fast path agrees with the full algorithm.
        for p in [
            "/",
            "/a",
            "/a/",
            "a",
            "a/",
            "a/b/c",
            "/a/b.js",
            "//",
            "/a//b",
            "a/./b",
            "./a",
            "a/..",
            "../a",
            "/..",
            ".",
            "./",
            "a/b/",
            "/a/.b/..c/d.",
        ] {
            let b = p.as_bytes();
            let absolute = b[0] == SLASH;
            let trailing = b[b.len() - 1] == SLASH;
            let mut out = normalize_string(b, !absolute);
            let slow = if out.is_empty() {
                (if absolute {
                    "/"
                } else if trailing {
                    "./"
                } else {
                    "."
                })
                .to_string()
            } else {
                if trailing {
                    out.push(SLASH);
                }
                if absolute {
                    format!("/{}", to_string(&out))
                } else {
                    to_string(&out)
                }
            };
            assert_eq!(normalize(p), slow, "{p}");
        }
        assert_eq!(resolve(&["/a", "/b", "c/"]), "/b/c");
        assert_eq!(resolve(&["/"]), "/");
        assert_eq!(relative("/a/b/c", "/a/d"), "../../d");
        assert_eq!(relative("/a/b", "/a/b/c/d"), "c/d");
        assert_eq!(relative("/a/b", "/a/b"), "");
        assert_eq!(relative("/a", "/"), "..");
        assert_eq!(relative("/", "/a"), "a");
        assert_eq!(relative("/a/bc", "/a/b"), "../b");
    }

    #[test]
    fn parse_matches_node() {
        let p = parse("/home/user/dir/file.txt");
        assert_eq!(
            (
                p.root.as_str(),
                p.dir.as_str(),
                p.base.as_str(),
                p.ext.as_str(),
                p.name.as_str()
            ),
            ("/", "/home/user/dir", "file.txt", ".txt", "file")
        );
        let p = parse("/a");
        assert_eq!(
            (p.root.as_str(), p.dir.as_str(), p.base.as_str()),
            ("/", "/", "a")
        );
        let p = parse("a/b/");
        assert_eq!((p.dir.as_str(), p.base.as_str()), ("a", "b"));
        let p = parse("/");
        assert_eq!(
            (p.root.as_str(), p.dir.as_str(), p.base.as_str()),
            ("/", "/", "")
        );
    }

    #[test]
    fn is_absolute_checks() {
        assert!(is_absolute("/a"));
        assert!(!is_absolute("a"));
        assert!(!is_absolute("C:\\a"));
        assert!(win32_is_absolute("C:\\a"));
        assert!(win32_is_absolute("\\a"));
        assert!(win32_is_absolute("c:/a"));
        assert!(!win32_is_absolute("C:a"));
        assert!(!win32_is_absolute("a"));
    }
}
