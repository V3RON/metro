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

## Cloud profile at the phase 4 head

Run by the coordinator on the cloud runner at `34fd43f`, with the phase 4 components on. The project's copy is `metro-rs-plan/phase-5-profile.md`. It replaces the Mac profile this note had before, which file opens distorted.

### warm-build: JS 3274 ms, Rust 1681 ms (1.95×); 2.5× needs 1310 ms

Serialisation takes about 361 ms in Rust, against 303 ms in JS. Rust is slower here mostly because of the source reads.

| Inside serialisation | ms |
|---|---:|
| `getSource()` reads for `sourcesContent` (2,739 `readFileSync`) | 66 |
| building the map | 80 |
| `JSON.stringify` of the 25.6 MB index map, the biggest single piece | 166 |
| `baseJSBundle` + `bundleToString` | 44 |

Other costs:

- **Handing strings back to JS** would cost about 152 ms, if Rust builds them: `Buffer.toString` takes 59 ms for the bundle and 93 ms for the map.
- **Transform-cache reads** take 451 ms in Rust, mostly `JSON.parse` of the cache entries. This is the largest bucket left, and it isn't serialisation.

What this means for the target:

- Even if every serialisation bucket went to zero, warm-build reaches only **2.44×**. With the string hand-back it is about **2.1×**.
- **2.5× depends on removing the transform-cache `JSON.parse`.** That needs transform outputs owned by Rust (rule 10 option B).

### edit: JS 298 ms, Rust 298 ms (1.0×); 1.5× needs 199 ms

About 148 ms of an edit is phase 5's. The bench's edit scenario fetches the whole bundle again after the change, so this time is the server's response path.

| Phase 5's part of an edit | ms |
|---|---:|
| `res.end` encoding the 16 MB bundle string to UTF-8 | 70 |
| two `Buffer.byteLength` calls on that string (a rope) | 48 |
| `baseJSBundle` | 27 |
| `hmrJSBundle` | 0.05 |

Outside phase 5 are the Babel transform of the edited file (71 ms) and the bench client decoding the response (71 ms).

- **Sending a Buffer instead of a string** brings `res.end` to 0.7 ms.
- **If Rust assembles the bundle and the server sends a Buffer,** an edit takes about 163 ms, which is **1.83×**.
- **If a JS string is created anywhere on that path,** edit stays at about 1.06×.
- `hmrJSBundle` is negligible, so the HMR step moves to the end.

### Noise and gaps

- None of the 50 edits hit the late-watcher case.
- warm-build pairs ranged from 1.79× to 2.07×.
- Edit ratios per run ranged from 0.81× to 1.07×.
- cold-filemap wasn't measured. It shares the build path, so it follows steps 1 to 3.

## Planned steps (revised after the cloud profile; each measured in the cloud before the next)

1. **Bundle and map responses as Buffers built in Rust** (`server`, `serializers`, `lib`).
   - The server's `.bundle` and `.map` responses come from one Rust call per request, which assembles the bytes:
     - the prelude and polyfills;
     - each module's wrapped code (`wrapModule`: the `__d(...)` call with its id, dependency ids and verbose name);
     - the run-module statements;
     - the trailing comments.
   - The server sends them with `res.end(buffer)` and sets the length from the buffer. No JS string is made on that path.
   - User hooks are called in JS before the Rust call and their results passed in (rule 1). That covers `getRunModuleStatement`, `processModuleFilter`, `createModuleId`, `getModulesRunBeforeMainModule` and `isThirdPartyModule`.
   - With `customSerializer` set, the JS path runs as it does today.
   - `runBuild`, `Server.build` and every public API keep returning strings. Only the HTTP path uses Buffers.
   - The cloud profile's target for this step: edit about 1.8×.
2. **The source map built and serialised in Rust** (`serializers`).
   - The index map's JSON is written by Rust straight into a buffer, which removes the 166 ms `JSON.stringify`.
   - Rust reads `sourcesContent` in one parallel batch through the file store, which removes the 2,739 sync reads.
   - The same code serves `.map` responses (as a Buffer), inline maps and `runBuild` (as a string, since that is the public API).
   - `excludeSource`, `sourcePaths` and the ignore list behave as the black-box suite pins them, including the inline-map quirk.
3. **Transform outputs owned by Rust, rule 10 option B** (`serializers`, with `transform-cache` from phase 4).
   - Rust reads and parses transform-cache entries and keeps the code and maps, which removes the 451 ms of `JSON.parse` in JS.
   - JS gets what the graph needs: dependencies and output type. Steps 1 and 2 then read the code and maps from Rust without copying.
   - **Needs Szymon's decision before it starts: this conflicts with boundary rule 9.** `module.output[].data.code` and `.map` are part of the public `Graph` objects, which custom serializers, `processModuleFilter`, `hmrJSBundle` and `getSource` read. Keeping them as real JS values defeats option B. Options:
     - **(a)** lazy getters on `data` that fetch from Rust on first access. That is no proxy, but the objects change shape.
     - **(b)** materialise `output` only when a JS consumer is present: a `customSerializer`, an HMR client, or `buildGraph`.
     - **(c)** keep JS strings and accept about 2.1× on warm-build.

   The step starts with a measurement and a short proposal to Szymon.
4. **Output files** (`output`). `runBuild` writes `bundleOut` and `sourceMapOut` from the step 1 and 2 buffers when the default `output.save` is used, and calls a custom `output` in JS as it does today.
5. **HMR, assets and the server's remaining paths** (`hmr`, `assets`, `server`), only where a measurement shows a cost. `hmrJSBundle` is 0.05 ms, so it stays JS unless something changes.
6. **Re-measure what was kept in JS for speed** (RULES.md §7): BundleBuilder, metro-core `canonicalize`, the JS `ob1`.

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
