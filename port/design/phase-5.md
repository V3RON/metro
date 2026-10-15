# Phase 5: output and server

**Status: draft.** The profile split comes from the Mac and gets replaced by cloud numbers before implementation. The step list is for Szymon's review.

## Exception: started before phase 4 was approved

On 2026-10-15 at 07:34 UTC Szymon chose "Code now" on the card "Start phase 5 code before you approve phase 4?". RULES.md §2 says not to start the next phase before approval, so this is a one-time exception:

- **Branch:** `port/phase-5` is branched from `port/phase-4` at `569d83db4`, which is not yet approved.
- **Rebase:** if phase 4's review changes anything, this branch is rebased onto the result.
- **Gate:** the gate refuses `--phase 5` until phase 4 is approved, and nobody works around that. Until then we run targeted Jest and cargo tests and the differentials directly.
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

Cloud numbers measured so far (medians):

| Scenario | Pure JS | Rust | Speed-up | Measured at |
|---|---:|---:|---:|---|
| warm-build | about 3290 ms | about 1515 ms | about 2.17× | step 3; the step 4 cloud number is in phase 4's report |
| edit | 253 ms | 236 ms | 1.07× | phase 3 report |
| cold-filemap | 2795 ms | 2213 ms | 1.26× | phase 3 report; phase 4 should have lowered it |

Distance to each target:

- **warm-build 2.5×** needs about 1316 ms, so about **200 ms more** has to go.
  - The phase 4 cloud profile put serialisation at 226 ms, and the source re-reads for source maps (`module.getSource()`, a sync `readFileSync` per module) show up separately.
  - That puts the target within reach if serialisation and the source reads move off the JS thread. It is not certain; to be confirmed with the cloud profile at the phase 4 head.
- **edit 1.5×** needs about 170 ms against JS's 253 ms. Phase 4 owned about 2 ms of an edit, so nearly all of an edit is phase 5's:
  - the delta calculation;
  - `hmrJSBundle` serialisation;
  - the WebSocket send;
  - the server's bundle-request path.

  This needs the cloud edit profile before anyone can say whether 1.5× is reachable.
- **cold-filemap 2×** needs about 1400 ms in JS terms. That is warm-build plus the cold crawl plus lazily hashing the graph's 2,741 files. If warm-build reaches about 1.3 s, the cold extras (about 0.2–0.4 s) make 2× borderline. **Risk flagged early.**

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

- **Existing coverage:**
  - `port/blackbox/__tests__/server-endpoints-test.js`;
  - `port/blackbox/__tests__/cli-and-build-output-test.js`;
  - `port/blackbox/__tests__/hmr-protocol-test.js`;
  - the bundle and HMR differentials.
- **Gaps to fill in JS before any Rust:**
  - source-map requests with and without `excludeSource`;
  - `inlineSourceMap`;
  - `sourceUrl` and `sourcePaths` options;
  - lazy and async bundles (`baseJSBundleWithLazyModuleDefinition`);
  - asset requests;
  - `customSerializer` falling back to JS;
  - `writeBundle` output files.
- **The differential:** `port/diff/differential.js` gains cases for an inline source map and an `excludeSource` map.

## Verify mode

`METRO_RUST_VERIFY` for each phase 5 component runs the JS serializer and server path alongside and compares the produced strings and buffers byte for byte, before anything is sent or written.

## Rules in play

- **Rule 1:** user hooks (`customSerializer`, `getRunModuleStatement`, `getPolyfills` and the like) are called from JS.
- **Rule 9:** the public `Graph`, delta objects and results stay JS objects.
- **Rule 10:** transform outputs move to Rust-owned buffers in step 2, with a measurement, as the boundary note planned for phase 5.
- **Byte-identical output** is the bar (RULES.md §4), so no minifier change happens here.
