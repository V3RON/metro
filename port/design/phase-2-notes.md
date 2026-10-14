# Phase 2 notes: resolver measurements

Input for the boundary research. These are not the boundary design. That design is decided separately and will replace the open questions at the end of this note.

Machine for every number here: Apple M3 Pro (12 cores), 36 GB, macOS 26.6.1, Node v24.14.0. This is not the machine of the phase 1 report (Linux, Xeon), so compare ratios, not absolute times.

## micro:resolver

`port/bench/micro/resolver.js` runs `resolve()` from metro-resolver through its public API on the bench app's real resolutions.

- **Setup (untimed).** One iOS dev build with `loadConfig` and `runBuild`. A recording `resolveRequest` keeps the context Metro passes for every resolution it actually asks for. Those are cache misses only, so 7,445 resolutions.
- **What the recorded contexts contain.** They keep Metro's own file-map, package-cache and Haste callbacks.
- **Timed.** Five passes over all 7,445 resolutions, after one warm-up pass.

| Checkout | Runs (ms, 5 passes) | Per resolve |
|---|---|---|
| Baseline `c3d0455a6941`, pure JS | 413, 395, 427, 424 | about 11 µs |
| This branch, `METRO_RUST=off` | 423, 430, 391 | about 11 µs |

The 1.5× target therefore means about 270 ms, or about 7 µs per resolve.

## What a resolve does

Measured with `port/design/phase-2-measure/profile.js` (raw output in `profile.json`). Specifiers: 6,299 relative, 1,145 bare, 1 absolute. Package exports are enabled, and `mainFields` is `react-native, browser, main`.

| Callback | Calls per pass | Per resolve | Distinct args | JS cost per call (Metro's own impl) |
|---|---:|---:|---:|---:|
| fileSystemLookup | 86,706 | 11.65 | 28,332 | 395 ns |
| getPackageForModule | 83,922 | 11.27 | 27,902 | 57 ns (PackageCache hit) |
| resolveHastePackage | 1,145 | 0.15 | 37 | 44 ns |
| resolveHasteModule | 243 | 0.03 | 27 | 42 ns |
| doesFileExist / getPackage | 66 / 66 | 0.01 | 15 | 358 / 27 ns |
| resolveAsset | 5 | 0 | 5 | 3.8 µs |

- **Callbacks per resolve.** About 23.
- **Time spent inside the callbacks.** About 39% of a JS resolve (39 ms of 101 ms per pass, in the instrumented run). The remaining 61% is the resolver's own logic: string and path work, exports matching, and allocating candidates.
- **Package.json objects.** Only 42 distinct objects back all 83,922 `getPackageForModule` results. The median is 1 KB and the largest is 139 KB.

## What crossing napi costs

Measured with `port/design/phase-2-measure/napi.js` and the throwaway `napi-cost` crate (napi 2, release and LTO, raw output in `napi.json`).

- **How the overhead was isolated.** The JS callbacks used here are memoised `Map` lookups, so the numbers are almost entirely boundary cost.
- **How each figure was taken.** Each figure is the best of 5 runs of 200,000 calls cycling through the recorded arguments.

| Crossing | ns per call |
|---|---:|
| Empty Rust→JS call | 30 |
| fileSystemLookup, Rust→JS (string in, read `exists`/`type`/`realPath`) | 400 (JS→JS: 34) |
| getPackageForModule, Rust→JS, read `rootPath` and `packageRelativePath`, keep packageJson as a handle | 777 (JS→JS: 31) |
| The same, plus converting packageJson (JSON.stringify, then serde_json) | 31,703 |
| resolveHastePackage, Rust→JS (string or null) | 119 (JS→JS: 26) |
| JS→Rust: read the resolve context (6 flags, 4 string arrays, 8 function checks) | 3,059 |
| JS→Rust: one string argument, no work | 78 |

The research thread measured in the cloud: about 2.0 µs per synchronous Rust→JS callback against 0.3 µs in JS, and 8.7 µs to read the 28-key context. The ratios agree with these numbers; the absolute times are 3–5× higher on that machine.

## What this means for phase 2

- **If every callback crosses napi, the target is out of reach.**
  - Boundary overhead alone would be about 11.65 × 0.37 + 11.27 × 0.75 + 3.1 ≈ 15.8 µs per resolve. That is on top of the JS callbacks' own cost of about 5.3 µs.
  - A whole JS resolve takes 11 µs, and the target is about 7 µs.
  - A Rust resolver built this way would be at least 1.9× slower than JS, even if its own logic cost nothing: (15.8 + 5.3) / 11 µs.
- **Converting package.json on every call is not viable,** at 32 µs per call. Package.json values have to live in Rust (parsed once per file, invalidated on change), or be cached by object identity.
- **The context must not be re-read per call.** Configuration has to be registered once. A call should carry only what changes per request: origin, specifier, platform, `isESMImport`, `allowHaste`, `dev` and `customResolverOptions`.
- **Even without boundary crossings, 1.5× needs native lookups.** A JS resolve is about 5.3 µs of Metro's callbacks (mostly `fileSystemLookup`, at 395 ns each in the JS file map) and about 5.7 µs of resolver logic. To reach about 7 µs, the Rust logic and its lookups together must fit in that budget. Native hash lookups in a Rust-side copy of the file map and package cache (tens of ns each) leave room for it. Any path through JS does not.

## How the Rust crate is shaped (pure, not wired)

`metro-rs/crates/resolver` ports the algorithm with every external lookup behind a `ResolverHost` trait. `cargo test` runs `port/blackbox/fixtures/resolver.json` through an in-memory host.

The context is split in two:

- **`ResolverConfig`.** Long-lived and built once. It holds the extensions, main fields, conditions, node_modules paths, flags and the host.
- **The per-call request.** Origin, specifier, platform, `isESMImport`, `allowHaste`, `dev` and `customResolverOptions`.

Package.json values are returned as shared pointers, so a fixture host and a future native file-map replica can both implement the trait without converting anything per call.

## Open questions for the boundary decision

- Where `fileSystemLookup`, `getPackageForModule` and Haste lookups are answered from in phase 2, while the file map is still JS. Phase 3 has to be able to remove that crossing.
- How a user `resolveRequest`, a scheme resolver or an overridden callback is detected and called. Detection could be by identity against Metro's own callbacks, or by explicit registration.
- How the config handle is invalidated: when the file map changes, and when `_createModuleResolver` creates a new resolver.

## Kept in JS for performance, to revisit in phase 5

Seeded from the phase 1 report:

- `BundleBuilder`: 57–70 ms through napi against 50–57 ms in JS for `measureString`.
- metro-core `canonicalize`: a per-value `JSON.stringify` replacer. An empty napi call already costs more than the JS work.
- The JS `ob1` package: one-line arithmetic. Only the Rust crate is used, inside source-map.
- The source-map `sources`, `sourcesContent` and `x_*` assembly.

Phase 2 candidates, pending the boundary decision:

- **User callbacks:** `resolveRequest`, `schemeResolvers` and any overridden context callback stay JS functions called from JS.
- **Error objects:** the `metro-resolver` error classes stay JS classes, because callers use `instanceof` on them.
