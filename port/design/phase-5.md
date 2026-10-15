# Phase 5: output and server

**Status: draft.** The profile split comes from the Mac and gets replaced by cloud numbers before implementation. The step list is for Szymon's review.

## Exception: started before phase 4 was approved

On 2026-10-15 at 07:34 UTC Szymon chose "Code now" on the card "Start phase 5 code before you approve phase 4?". RULES.md §2 says not to start the next phase before approval, so this is a one-time exception:

- **Branch:** `port/phase-5` was first stacked on `port/phase-4` before that was approved. Only the design note and black-box tests in JS were written in that window; no Rust code.
- **Closed:** Szymon approved phase 4 at 08:01 UTC on 2026-10-15 (`port/approvals/phase-4.md`, `02dc592`). Its PR, V3RON/metro#8, is merged, and this branch was rebased onto `main` at `02dc592`. From here on phase 5 is the current phase, and `./gate.sh` applies normally.
- **`port/approvals/`** is not touched.

## Scope and targets (`port/phases.json`; the yardstick is unchanged)

- **Components:** `serializers`, `output`, `assets`, `hmr`, `server`, `lib`.
- **Exit:**
  - the server endpoint, CLI and output suites pass on both sides;
  - output is byte-identical on the differential apps;
  - the JS fallback still works.
- **Targets** against pure JS, all enforced:

| Scenario | Target |
|---|---:|
| warm-build | 2.5× |
| edit | 1.5× |
| cold-filemap | 2.0× |

## Where we start from

From the phase 4 report (`port/reports/phase-4.md`, `./gate.sh --report` at `569d83d`, cloud runner, medians of 5 interleaved runs):

| Scenario | Pure JS | Phase 4 | Speed-up | Noise | Phase 5 target | Phase 5 ms | Still to remove |
|---|---:|---:|---:|---:|---:|---:|---:|
| warm-build | 3504 ms | 1805 ms | 1.94× | ±16.2% | 2.5× | 1402 ms | about 400 ms |
| edit | 305 ms | 293 ms | 1.04× | ±5.5% | 1.5× | 203 ms | about 90 ms |
| cold-filemap | 3504 ms | 1929 ms | 1.82× | ±5.4% | 2.0× | 1752 ms | about 180 ms |

The "Phase 5 ms" column is the time each target allows, assuming the JS baseline stays where it is.

- **warm-build:** about 400 ms has to go, out of 1805.
  - The phase 4 cloud profile put serialisation at about 226 ms, before the source reads for `sourcesContent` (`module.getSource()`, a sync `readFileSync` per module).
  - Serialisation plus those reads is the largest single piece phase 5 owns. Moving all of it off the JS thread may still fall short of 400 ms on its own.
  - The rest has to come from what the server does around a build:
    - the `getSortedModules` sort;
    - building `bundleOptions`;
    - module ids (`createModuleId` per module, for every serialisation);
    - GC pressure from about 30 MB of transform output being copied into strings (rule 10).
  - **Reachable but not certain.** The cloud profile decides.
- **edit:** 90 ms has to go out of 293 ms. Phase 4 owned about 2 ms of an edit, so the rest is:
  - file-change notification and the delta calculation, which wait on the watcher;
  - `hmrJSBundle`: `wrapModule`, the inverse dependencies, `addParamsToDefineCall`;
  - JSON and WebSocket sends.

  How much of the 293 ms is waiting on the watcher (a debounce, fixed time) decides whether 1.5× is reachable at all. **Unknown until the cloud edit profile.**
- **cold-filemap:** 180 ms has to go. It shares the build and serialisation path with warm-build, so it follows warm-build's gains. Since phase 4 it is 124 ms slower than warm-build, which is the cold crawl and hashing. **Likely if warm-build gets there.**

## Mac CPU profile at the phase 4 head (Mac only, inflated by load and profiling)

Inclusive CPU time, warm `runBuild`, with all phase 4 components on. The total is 6.2 s of CPU in a 4.3 s wall-clock run, with the Mac under load.

| Area | CPU time |
|---|---:|
| Serializers (`DeltaBundler/Serializers/*`) | 1194 ms |
| `module.getSource()` source reads for source maps (part of the serializers row) | 624 ms |
| metro-source-map (part of the serializers row) | 345 ms |
| Graph traversal (Graph, buildSubgraph, rustGraph) | 1206 ms, mostly awaiting transforms |
| Transformer | 1471 ms (async, overlaps) |
| Resolver glue | 426 ms |
| File map | 151 ms |
| Module loading (Babel register, the bench harness, same on both sides) | 2135 ms |
| idle | 640 ms |
| GC | 215 ms |

What this says:

- **The serializer path is the largest piece phase 5 owns.** The synchronous source reads for `sourcesContent` alone are about half of it. On the Mac they are inflated by the file-open cost, but they cost real time on Linux too.
- The absolute values aren't valid for decisions. The cloud profile decides.

## Planned steps (in order, each measured in the cloud before the next)

1. **Source-map sources off the JS thread** (`serializers`).
   - Today `getSourceMapInfo` calls `module.getSource()`, a `readFileSync` per module, on every source-map request.
   - Instead, Rust reads all the sources of a bundle in one parallel batch, from the file map's store, and hands them to the source-map encoder (`metro-source-map` is in Rust since phase 1).
   - The JS shape of `getSource()` stays for anyone else who calls it.
2. **The bundle body in Rust** (`serializers`, `lib`).
   - `baseJSBundle` and `processModules` / `wrapModule` (`helpers/js.js`) build the code by concatenating every module's wrapped code.
   - This is where rule 10 option B pays off. The transform outputs are kept as Rust-owned buffers when they come out of the cache or a worker: the cloud measured about 30 MB and 20 ms to copy them. The serializer then concatenates in Rust and returns one string or Buffer.
   - Phase 4 kept option A, so this step revisits rule 10, with measurements.
   - `customSerializer` and serializer options that Rust can't model stay JS (rule 1).
3. **Source maps for the whole bundle** (`serializers`). `sourceMapString` / `sourceMapObject` build from the same Rust-held per-module maps, and only the final map string crosses.
4. **HMR deltas** (`hmr`). `hmrJSBundle` (added, modified and deleted modules wrapped for HMR) builds in Rust from the phase 4 graph delta, and `HmrServer` sends it. This is the main lever for edit 1.5×.
5. **Server bundle path** (`server`).
   - URL parsing, options, the user hooks (`rewriteRequestUrl`, `enhanceMiddleware`, `customSerializer`) and caching headers stay JS.
   - The response body comes from steps 2 and 3.
   - The revision cache and delta calculation reuse phase 4's graph.
6. **Output files** (`output`). `shared/output/bundle.js` writes the bundle, the map and the asset files. Rust writes them off-thread from the buffers in steps 2 and 3. The RAM-bundle and unbundle formats stay JS unless the profile says otherwise.
7. **Assets** (`assets`): image size reading and hashing in `Assets.js`, natively, if the profile shows them. The bench app has few assets.
8. **The "kept in JS for performance" list** (RULES.md §7), to re-measure:
   - BundleBuilder (from phase 1);
   - metro-core `canonicalize`;
   - the JS `ob1` package;
   - the source-map `sources` and `sourcesContent` assembly (covered by steps 1 and 3).

## Tests first

**Existing coverage:**

- `server-endpoints-test.js`, `cli-and-build-output-test.js`, `hmr-protocol-test.js`, `incremental-rebuild-test.js`;
- the bundle differential and the HMR differential (`port/diff/`).

**Added for phase 5, in JS, before any Rust.** Every case passes with `METRO_RUST=off` and with the phase 4 components in Rust.

- **`serializer-output-test.js`, 21 tests:**
  - every `serializer.*` hook (`getPolyfills`, `polyfillModuleNames`, `getModulesRunBeforeMainModule`, `getRunModuleStatement`, `processModuleFilter`, `createModuleIdFactory`, `customSerializer` returning a string or `{code, map}`, `isThirdPartyModule` → `x_google_ignoreList`, `experimentalSerializerHook`);
  - runBuild source maps: absolute sources, `sourcesContent` exactly as on disk (including non-ASCII text and CRLF), external and inline maps;
  - server URL options: `sourcePaths=url-server`, `lazy`, `shallow`, `modulesOnly` with `runModule=false`;
  - `bundleOut` and `sourceMapOut`.
- **`hmr-update-payload-test.js`, 2 tests:** the exact define call of an HMR update, including the inverse-dependency chain up to the entry point, and the per-module source and map URLs (path encoding, the JSC-safe form, `excludeSource` dropped, the modules-only and shallow options added).
- **`output-save-test.js`, 5 tests:** `metro/private/shared/output/bundle`'s `save`, the module React Native's CLI writes bundles with: `sourcemapSourcesRoot`, `bundleEncoding`, its log lines, a missing directory.

**Quirk pinned as it is:** `excludeSource=true` does not apply to an inline source map (`inlineSourceMap=true`). The bundle serializer builds that map and ignores the option, so only the `.map` endpoint honours it.

**Still to add:** the inline map and `excludeSource` cases in `port/diff/differential.js`, alongside the Rust work for steps 1 to 3.

## Verify mode

`METRO_RUST_VERIFY` for each phase 5 component runs the JS serializer and server path alongside and compares the produced strings and buffers byte for byte, before anything is sent or written.

## Rules in play

- **Rule 1:** user hooks (`customSerializer`, `getRunModuleStatement`, `getPolyfills` and the like) are called from JS.
- **Rule 9:** the public `Graph`, delta objects and results stay JS objects.
- **Rule 10:** transform outputs move to Rust-owned buffers in step 2, with a measurement, as the boundary note planned for phase 5.
- **Byte-identical output** is the bar (RULES.md §4), so no minifier change happens here.
