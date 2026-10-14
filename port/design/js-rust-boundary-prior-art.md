# Prior art: how Rust-core bundlers handle the JS boundary

Research report, 2026-10-14. These are notes, with a source link for every claim. The conclusions drawn from them are in [js-rust-boundary.md](js-rust-boundary.md#how-other-rust-core-bundlers-do-it).

## JS hooks called from Rust
- **Rspack:** JS callbacks are queued and run one by one on the JS thread, which creates a "serial execution point". Taps are `ThreadsafeFunction`s called asynchronously. A hook with no taps is skipped entirely, and for per-module hooks (`before_resolve`, `build_module`) the tap list is cached. https://rspack.rs/api/javascript-api/architecture, `crates/rspack_binding_api/src/plugins/interceptor.rs`
- **Rolldown:** hook filters are checked in Rust. Mean build times on 10,000 modules with transform-only plugins:

  | Plugins | No filter | Filtered |
  |---|---:|---:|
  | 0 | 745.6 ms | 739.1 ms |
  | 1 | 862.6 ms | 760.6 ms |
  | 2 | 1.106 s | 731.2 ms |
  | 5 | 1.848 s | 741.5 ms |
  | 10 | 2.792 s | 747.3 ms |

  That is about 20 µs per JS hook call. This is derived from the table, not published. https://rolldown.rs/in-depth/why-plugin-hook-filter, https://rolldown.rs/plugins/hook-filters
- **Vite on Rolldown:** Vite's internal alias and resolve plugins are now in Rust (`experimental.enableNativePlugin`), and a `withFilter` wrapper adds filters to existing plugins. https://v7.vite.dev/guide/rolldown
- **Turbopack:** Node tools run in a child-process pool over sockets, with worker threads as an opt-in. On Node ≥24.13.1, a worker that exits while a native addon holds a live Node-API thread-safe function can abort the process. https://nextjs.org/docs/app/api-reference/config/next-config-js/turbopackPluginRuntimeStrategy
- **SWC:** rejected JS plugins because of transfer cost and the single JS thread, and uses Wasm plugins instead. https://github.com/swc-project/swc/blob/main/docs/adr/00001-support-transformation-plugin-native.md
- **Biome:** plugins are written in GritQL, so no JS runs. https://biomejs.dev/linter/plugins
- **napi-rs:** a plain `Function` is for synchronous calls within the same call, and `ThreadsafeFunction` has "more overhead". Blocking mode must never be used on the JS thread. Re-entrancy is undocumented; a Rust thread that blocks on JS while JS is blocked in Rust deadlocks. https://napi.rs/blog/function-and-callbacks, https://napi.rs/docs/more/async-concurrency.md

## File ownership and caching
- **Turbopack:** files are read through value cells. The watcher marks the readers of a changed file dirty and the change propagates up the graph. The graph has been persisted to disk since Next.js 16.1. https://nextjs.org/blog/turbopack-incremental-computation
- **Rspack:** the file system lives in Rust. A JS `inputFileSystem` is opt-in, and the docs warn it "will slow down Rspack a lot". A native watcher is available. https://www.rspack.rs/config/experiments, https://rspack.rs/blog/announcing-1-4
- **oxc_resolver 11.24:** has a `FileSystem` trait. Its cache interns paths as `CachedPath` entries, each with a hash, a weak parent pointer, type bits and lazily filled slots for the canonical path, `package.json` and tsconfig, stored in a `DashMap`. It has no per-path invalidation, only `clear_cache()`, which is unsafe while resolutions are running.
- **Parcel resolver (Rust, 2.9):** returns `Invalidations` with each result, including files whose creation would change the answer, and recommends a fresh cache per build. https://docs.rs/parcel-resolver, https://parceljs.org/blog/v2-9-0/

## Resolver
- **oxc_resolver:** a port of enhanced-resolve that claims 28× its speed. It has no JS resolver plugins. https://github.com/oxc-project/oxc-resolver
- **Rspack 0.4:** made oxc_resolver its default after it passed all of enhanced-resolve's tests, and dropped `resolve.plugins`. https://rspack.rs/blog/announcing-0-4, https://rspack.rs/guide/migration/webpack

## Published boundary costs
- JS calling native through Node-API: 7–15 ns per call. https://bun.com/blog/compile-and-run-c-in-js
- Rust calling JS through `ThreadsafeFunction` or `napi_call_function`: no published microbenchmark. Our probe measured about 2 µs for a synchronous call; see design.md.
- Serialisation dominates. On 610,000 AST nodes, `serde_json` plus `JSON.parse` took 2.91 s, against 0.62 s for a flat buffer with lazy objects. https://marvinh.dev/blog/speeding-up-javascript-ecosystem-part-11/. Oxc names serialisation as the biggest barrier, with GC second. https://oxc.rs/blog/2025-10-09-oxlint-js-plugins
