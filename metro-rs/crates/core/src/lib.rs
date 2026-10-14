//! Plain Rust core of the Metro port. Component crates are added in later phases.

/// Version of the Rust core, which is the version of the workspace.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Called once the first build of a file map has finished: from then on the
/// process returns freed memory to the operating system eagerly. Does nothing
/// on later calls, and nothing except on Linux with glibc (other allocators
/// return memory on their own).
///
/// By default glibc raises its trim and mmap thresholds (up to 64 MB and 32
/// MB) whenever the program frees a large block that was mapped separately,
/// and Node does that early (it reads and stringifies multi-megabyte
/// buffers). From then on blocks below the threshold are carved out of the
/// shared heap, and what is freed at its top stays resident until 64 MB of it
/// is free. The crawl, the cache decode and the hashing of a first build
/// allocate and free a lot of memory on several threads, and in a warm build
/// that left about 55 MB of free but resident memory in the process, long
/// after the build.
///
/// So this fixes both thresholds at 1 MB (glibc starts at 128 KB), which stops
/// the growth, maps larger blocks separately, and trims the top of the heap as
/// soon as 1 MB of it is free, and trims everything now (`malloc_trim` also covers the arenas of the
/// crawl and libuv threads). It is done after the first build, not at load
/// time, because the build itself is faster with the heap kept: returning
/// memory that the next step asks for again costs page faults.
///
/// A process that sets `MALLOC_MMAP_THRESHOLD_`, `MALLOC_TRIM_THRESHOLD_`,
/// `MALLOC_TOP_PAD_`, `MALLOC_MMAP_MAX_` or `GLIBC_TUNABLES` has chosen its own
/// allocator settings: the thresholds are left alone, and only the trim is
/// done.
pub fn release_memory_after_first_build() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(release_memory);
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn release_memory() {
    const FIXED_THRESHOLD: libc::c_int = 1024 * 1024;
    let chosen = [
        "MALLOC_MMAP_THRESHOLD_",
        "MALLOC_TRIM_THRESHOLD_",
        "MALLOC_TOP_PAD_",
        "MALLOC_MMAP_MAX_",
        "GLIBC_TUNABLES",
    ]
    .iter()
    .any(|name| std::env::var_os(name).is_some());
    // SAFETY: `mallopt` and `malloc_trim` are thread-safe and take plain
    // integers.
    unsafe {
        if !chosen {
            libc::mallopt(libc::M_MMAP_THRESHOLD, FIXED_THRESHOLD);
            libc::mallopt(libc::M_TRIM_THRESHOLD, FIXED_THRESHOLD);
        }
        libc::malloc_trim(0);
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn release_memory() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn releasing_memory_keeps_live_allocations_and_is_repeatable() {
        let data: Vec<Vec<u8>> = (0..64).map(|i| vec![i as u8; 4096]).collect();
        release_memory_after_first_build();
        release_memory_after_first_build();
        assert!(data
            .iter()
            .enumerate()
            .all(|(i, v)| v.iter().all(|b| *b == i as u8)));
    }

    #[test]
    fn version_is_semver_like() {
        let parts: Vec<&str> = version().split('.').collect();
        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|p| p.parse::<u32>().is_ok()));
    }
}
