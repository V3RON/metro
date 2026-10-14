# Phase 3: the file map in Rust

This note applies RULES.md §7 and the "Phase 3" section of [js-rust-boundary.md](js-rust-boundary.md) to metro-file-map, node-haste and metro-cache. It is written before any code. The places where it departs from the boundary note are listed at the end.

## Where the time goes today

Measured on the bench app (23,838 files) on the Mac, pure JS:

| | Total | Crawl | Process (SHA-1, plugins) | Other |
|---|---:|---:|---:|---:|
| `FileMap.build`, cold, `computeSha1: true` (micro:file-map) | 3.9 s | 0.21 s | 3.6 s | 0.1 s |
| `FileMap.build`, warm disk cache | 0.26 s | 0.23 s | 0 | 0.03 s |

A warm `runBuild` takes about 1.8 s. Its CPU profile:

- **Lazy SHA-1s, about 0.4 s.** These are synchronous `readFileSync` calls from `getOrComputeSha1`, one per module. Metro's default config sets `watcher.unstable_lazySha1`.
- **The re-crawl, about 0.23 s.** The warm file map still re-crawls everything to diff it against the cache.
- **Source reads for source maps, about 0.26 s,** in `Transformer.getSource`. Output is phase 5's.

What the targets therefore require:

- **micro:file-map 3×:** a Rust crawl plus SHA-1 hashing in parallel threads.
- **cold-filemap 2×:** the cold file map shrinks from seconds to a few hundred ms.
- **warm-build 1.3×:** about 0.4 s has to come out. That needs both a fast crawl and SHA-1s computed off the main thread (`getOrComputeSha1` is async, so Rust can hash on a worker thread). The file map alone gives about 0.2 s. This is the target most at risk.

## Data flow when `file-map` is in Rust

The phase 2 store (`metro-rs/crates/file-store`) becomes the file map. A new crate, `metro-rs/crates/file-map`, adds what TreeFS's `FileMetadata` holds next to each file id: mtime, size, SHA-1, visited flag, symlink target and plugin data. It also adds:

- **Crawler.** The Node crawler's walk, with its exact rules: extensions, the `ignore` function, `includeSymlinks`, roots inside and outside `rootDir`, and the `lstat` mtime and size. The walk is parallel, with one crossing for the whole crawl.
- **Diffing against the previous state.** This is `getDifference`: mtime, size and the visited flag, as TreeFS does it.
- **Hashing.** SHA-1 of the changed files, in parallel. Symlinks are read with `readlink` and resolved with `resolveSymlinkToNormal`.
- **Change batches.** Watcher events are applied with the same rules as `FileMap.#watch` (directories, symlinks, mtime-based "modified", ignored files), and the batch comes back as the public change event.
- **Haste.** The `HastePlugin` maps (modules, packages, platforms, duplicates, conflicts) are ported, and the store answers `getModule` and `getPackage`. With `hasteImplModulePath` set, the per-file `getHasteName` is user JS; see "What stays in JS".
- **Disk cache.** Rust reads and writes its own file in `DiskCacheManager`'s directory, under a cache key that includes everything `DiskCacheManager` keys on (build parameters, the plugins' cache keys and a Rust format version). This needs a new serialisation crate (see "New crates"); no JS snapshot is built.

### What JS sees

- **`FileMap`** keeps its public API and events (`change`, `status`, `metadata`, `healthCheck`).
  - In Rust mode, `build()` makes four crossings: read the cache, crawl, apply the delta, persist.
  - With Watchman, the JS Watchman crawler runs as today, and its result is applied in one crossing.
- **`fileSystem`** is a `RustFileSystem` object implementing the `FileSystem` interface. Each method makes one call: `lookup`, `exists`, `hierarchicalLookup`, `matchFiles`, `getAllFiles`, `getSha1`, `getOrComputeSha1` (async, hashed off-thread), `linkStats`, `getDifference`, `getSerializableSnapshot` and so on.
- **`HastePlugin`** is the same JS class. When the Rust file map adopts it, the plugin's query methods (`getModule`, `getPackage`, `getModuleNameByPath` and so on) call the store. A `HastePlugin` used without a Rust file map, as in its own unit tests, behaves exactly as today.
- **The watchers stay in JS:** `FallbackWatcher`, `NativeWatcher`, `WatchmanWatcher` and the health check. `FileMap.#watch` keeps its debouncing. It hands each batch of raw events to Rust in one crossing. Rust lstats, hashes, updates the store and Haste, and returns the changes, and JS emits the same `change` event object as today. Porting the watchers (`notify`, `watchman_client`) is a later decision, with numbers.
- **The resolver** (`DependencyGraph` and `createModuleResolver`) tags its callbacks with the file map's own store. The phase 2 replica, `RustFileStore` with its bulk load and change forwarding, is no longer used in this mode.
  - The package cache is already in the store.
  - Haste moves there too: `getHasteModulePath` and `getHastePackagePath` are tagged, and the resolver answers Haste natively.
  - Metro's per-call Haste closures in `ModuleResolution` are replaced by tagged functions that take the platform from the request. That is a small, behaviour-neutral change in metro.

### What stays in JS, and the fallbacks

The Rust file map is used when **all** of the following hold. Otherwise `FileMap` runs its JS implementation unchanged, so behaviour is exactly today's.

- The cache manager is the default `DiskCacheManager`, or the `NoopCacheManager`.
- There is no `crawlerFactory`.
- The plugins are Metro's own `HastePlugin`, optionally `DependencyPlugin` (see below), and nothing else.

What still runs in JS inside the Rust file map:

- **`hasteImplModulePath` and `dependencyExtractor`.** These are user JS modules that run per file in workers. Rust crawls and hashes, then hands the list of files that need plugin work to the existing JS `FileProcessor`, in one crossing. The plugin data comes back in one crossing. The Haste maps themselves stay in Rust.
- **A custom `unstable_fileMapCacheManagerFactory`, a custom plugin, or a `crawlerFactory`.** These fall back to the whole JS file map, the slow but correct path. That is simpler than materialising snapshots for custom managers. A later phase can narrow it.
- **Watchman crawling.** The `watchman` crawler stays in JS. Its `FileData` result is applied in one crossing.
- **Exact JS-only semantics.** Rust uses an ECMAScript regex engine for the ignore pattern, so the user's RegExp keeps JS semantics. That needs a new crate, `regress`. The alternative was a JS call per path, which rule 2 rules out.

## Verify mode

`METRO_RUST_VERIFY=file-map` builds the JS file map alongside the Rust one, from the same crawl inputs. It compares them after `build()` and after every change batch: every file's metadata, the Haste maps and the emitted change events. Every `FileSystem` call is also answered by both and compared. The first difference throws. The gate's Rust-mode Jest run sets it, as it does for the resolver.

## The `cache` component

metro-cache's `FileStore` stores transform results. Those stay in the JS heap until phase 4 (rule 10), and the warm-build profile shows their reads are not the cost: about 50 ms. Phase 3 leaves metro-cache in JS. Its tests run in Rust mode unchanged, since nothing in it is switched. The design note for phase 4 revisits it together with transform outputs. If Szymon prefers, the `cache` switch can stay a no-op, as `core` was in phase 1.

## Cache format compatibility

- **Separate cache file.** Rust's cache file is not the JS `DiskCacheManager` file. Switching between JS and Rust therefore costs one cold start each way, and never reads the other's file. The file name includes "rs" and the format version.
- **Auto-save.** `autoSave` in watch mode keeps the same debounce, implemented by the JS shell asking Rust to persist.

## New crates

| Crate | Why |
|---|---|
| `sha1` (RustCrypto, the same family as `md-5` from phase 1) | SHA-1 of file contents |
| `regress` | ECMAScript-compatible regular expressions for `ignorePattern` and `matchFiles` filters |
| `bincode` + `serde` | the cache file |
| `rayon` | parallel crawl and hashing |

`notify` and `watchman_client` are not added this phase.

## Off the main thread

- **What runs on worker threads.** The crawl, the hashing, readlink and the cache file I/O run as napi `AsyncTask`s. The tree stays on the main thread, and only plain data crosses to the workers.
- **Why.** An earlier version did this work synchronously, which blocked the event loop for about 100 ms during a dev server's start.
- **One extra timer tick per task.** After each task, `FileMap` waits one timer tick (`await sleep(0)` from `node:timers/promises`) before going on. That lets Node destroy the task's async resource first. Without it, `server-torn-down-test` sees the resource still alive when `onClose` runs. The cost is about 1 ms per build.
- **napi3, not napi4.** The binding is built with napi3. napi4 makes napi-rs create a module-lifetime "CustomGC" threadsafe function, which that test also caught, and nothing in the port uses threadsafe functions.

## Departures from js-rust-boundary.md

- **Watchers stay in JS** (the brief's default). They feed batches into Rust; the boundary note had Rust batching watcher events with a `ThreadsafeFunction`.
- **Watchman crawling stays in JS.**
- **A custom cache manager, custom plugin or `crawlerFactory` falls back to the whole JS file map,** rather than getting a JS snapshot materialised on demand.
- **`hasteImplModulePath` and `dependencyExtractor`** run through the JS `FileProcessor` in one batch per build or change batch, because they are user code.
- **metro-cache stays in JS** (see "The `cache` component").

## Risks

- **warm-build 1.3×** depends on off-thread `getOrComputeSha1`. If it is still short after a genuine effort, I report with numbers.
- **The watcher integration tests** flake on the Mac under load; the comparison against the baseline is described in the phase 2 notes. They run JS watchers in both modes.
