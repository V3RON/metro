# Handoff: Metro → Rust port, phase 4 in progress

Written 2026-10-14 at about 20:40 UTC, when Szymon stopped work for the day. Read [RULES.md](../RULES.md) first; it is binding. Then read this file, then [port/design/phase-4.md](design/phase-4.md).

## 1. Where things stand

- **Phases 0–3** are merged into `main` and approved by Szymon (`port/approvals/phase-0.md` … `phase-3.md`).
- **Phase 4, "Incremental graph", is current** (`node port/lib/phase.js current` prints 4).
  - Branch: `port/phase-4` in `~/Projects/metro-rs` on Szymon's Mac. Head: `85eeb28e7`.
  - Everything, including the step 3 WIP and this handoff, is pushed to `origin/port/phase-4`.
- **Scope**, widened by Szymon (see §2):
  - components `delta-core`, `incremental`, `transform-cache` and `worker-farm`;
  - target warm-build 1.6×; edit and cold-filemap are informational.

Phase 4 commits, oldest first:

| Commit | What it does |
|---|---|
| `2ed9199ac` | Design note [port/design/phase-4.md](design/phase-4.md): plan, step order, rule 10 plan, HMR check, verify mode |
| `5f0c2c640` | **worker-farm, step 1, done.** The Transformer creates its jest-worker farm on the first cache miss when `worker-farm` is on. It lives in `Transformer.js`, not `WorkerFarm.js`, because `WorkerFarm-test` expects eager construction. |
| `4505aff4c` | **transform-cache, step 2, done.** metro-cache `FileStore.get` reads each entry with one native read on a worker thread (`metro-rs/crates/binding/src/transform_cache.rs`). The errors are Node-shaped, and verify compares the read with `fs.promises.readFile`. |
| `1dd4a08a9` | HMR session differential, [port/diff/hmr-session.js](diff/hmr-session.js): 7 scripted edits, JS against Rust, 40 messages identical |
| `9bf90b8ec` | `gate.sh` runs the HMR differential in the tests gate from phase 4 |
| `0bf502879` | FileStore and AutoCleanFileStore memfs tests are js-only for transform-cache; their cases run on the real disk in [port/blackbox/__tests__/file-store-test.js](blackbox/__tests__/file-store-test.js) |
| `24dd55c3d` | Black-box tests for resolving a module's dependencies, [port/blackbox/__tests__/resolution-per-module-test.js](blackbox/__tests__/resolution-per-module-test.js), 12 tests. They pass on JS. |
| `85eeb28e7` | **Step 3, batch resolution: WIP, gates not run.** Details below. |

**Steps 3 and 4 are not done.**

### How far step 3 got (`85eeb28e7`)

Implemented:

- **Rust cache.** `metro-rs/crates/resolver/src/resolution_cache.rs` holds the resolution cache. It uses DependencyGraph's compound key, and the `assumeFlatNodeModules` origin-key rule. It has unit tests.
- **Binding** (`metro-rs/crates/binding/src/resolver.rs`):
  - the `ResolutionCache` napi class;
  - `resolverResolveBatch`. It resolves a module's dependencies in order and stops at the first one it can't answer natively: a scheme resolver, an error, or an `empty` or `assetFiles` result. Warnings are buffered per dependency and only emitted once that dependency is answered.
- **JS:**
  - `rust.js`: `getBatchSupport` (fully native contexts only) and `resolveBatchWithRust`.
  - `DependencyGraph.js`:
    - the native cache replaces `_resolutionCache` when `incremental` is on and the resolver and file map are native;
    - it is cleared in `_onHasteChange`;
    - `prepareBatchResolution` and `supportsBatchResolution`.
  - `transformHelpers.getResolveDependencyFn` tags the resolve function with `BATCH_RESOLVE` (`DeltaBundler/batchResolve.js`).
  - `buildSubgraph.resolveDependencies` batches, then resolves the stopped dependency per dependency as today, then re-batches from the next one.
- **Verify** (`METRO_RUST_VERIFY=incremental`): every natively answered dependency is re-resolved through the JS path with a shadow cache.

Checked:

- cargo fmt, clippy and test;
- build:fast, flow, eslint;
- the new black-box test, 12/12 in JS, and in Rust mode with verify;
- a throwaway spy, not committed, confirmed the batch path is really taken.

Not yet checked:

- full Rust-mode Jest with verify (packages/metro, metro-resolver, port/blackbox, js-only list applied);
- `port/diff/differential.js --phase 4` and `port/diff/hmr-session.js --phase 4`;
- `./gate.sh --only compile --phase 4`;
- release-build perf.

One JS-only failure in the step's run, `port/blackbox/__tests__/file-map-test.js`, is probably the known watcher timeout. Re-run it alone.

Not yet written down: the step 3 design choices (a separate `ResolutionCache` class; verify re-resolves with the Rust resolver, so it checks cache and batch semantics, not the resolver). Add them to [port/design/phase-4.md](design/phase-4.md).

Known limit: an invalid `require.context` regexp that throws in the middle of a module happens after the batch has already resolved the later dependencies. That gives extra cache entries and warnings that today's order wouldn't. It is an extreme edge case; mention it in the PR.

## 2. Decisions Szymon made (binding)

| Date | Decision | Recorded in |
|---|---|---|
| 2026-10-14 | Phase 4 scope widened: `transform-cache` and `worker-farm`; target warm-build 1.6× | `port/phases.json` on main, `07a185410` |
| 2026-10-14 | Edit target moved from phase 4 to phase 5 | same commit, `07a185410` |
| 2026-10-14 | The perf gate's memory check uses RSS after a forced full GC, not peak RSS | main `4d4092dc0` |
| 2026-10-14 | The cold-filemap 2× target moved from phase 3 to phase 5; it is informational in phases 3 and 4 | main `98d462544`; [port/design/phase-3.md](design/phase-3.md) |
| 2026-10-14 | Phase 3 accepted with warm-build at 1.19× against 1.3× | [port/approvals/phase-3.md](approvals/phase-3.md) (main `20dd1b3e2`); report [port/reports/phase-3.md](reports/phase-3.md) |
| 2026-10-14 | FileStore-test.js and AutoCleanFileStore-test.js are js-only for transform-cache, with real-disk copies of every case | `port/js-only-tests.json`, `0bf502879` |
| 2026-10-14 | Rule 10 for phase 4: option A. Transform outputs stay JS objects and V8 parses the JSON (it parses all 36 MB in 70 ms). Rust-owned buffers are deferred to phase 5's serializers. | [port/design/phase-4.md](design/phase-4.md) |
| 2026-10-14 | Phase reports run in the cloud container, not on the Mac | phase 2 and phase 3 reports |
| standing | Approval files are added only on Szymon's own words (RULES.md §2) | RULES.md |

## 3. Next steps, in order

1. **Pull `port/phase-4`** and confirm the head is this handoff commit or later.
2. **Finish step 3, batch resolution.**
   - It removes about 27k JS `resolveDependency` calls per bench-app build.
   - A native resolve costs about 32 µs per call (cloud), which is why the cache moves too.
   - Run the unchecked items above and fix what fails. Then record the design choices and measure warm-build with and without `incremental`.
3. **Step 4, graph topology in Rust** ([port/design/phase-4.md](design/phase-4.md) §4).
   - Modules become FileIds, plus edges, inverse dependencies and cycle collection.
   - Rust returns one delta per traversal. JS updates the real `graph.dependencies` and `Module` objects in exactly today's insertion order (rule 9).
   - This starts the JS id→path array (rule 4).
   - The exit criterion needs the graph, incremental-rebuild and HMR suites on both sides, so step 4 is required even though warm-build already looks met.
4. **Verify mode for `delta-core`:** a shadow JS graph per traversal, comparing order, edges, inverse deps and deltas.
5. **Full gates on the Mac:** `./gate.sh --only compile --phase 4`, then `--only tests`.
6. **Push.** The coordinator session runs `./gate.sh --report` in the cloud.
7. **PR "Phase 4: Incremental graph"** with the report. Name Szymon's decisions: widened scope, js-only FileStore tests, rule 10 = A. Name every new crate (none so far in phase 4).
8. **Stop and wait for approval.** Never touch `port/approvals/`.

## 4. Measurements so far

**Phase 4 profile.** The cloud only has the full file at `/mnt/project-files/metro-rs-plan/phase-4-profile.md`. Warm-build after phase 3 was JS 2877 ms against Rust 2189 ms (medians of 5):

| Area | Time |
|---|---:|
| Transform-cache reads (FileStore read, JSON.parse, SHA-1, key) | 632 ms |
| Worker farm (3 forks with all hits; `server.end()` waited about 410 ms) | about 480 ms |
| Resolution: native 239 ms (7,445 calls, p50 23 µs, p99 113 µs) plus JS glue about 120 ms (26,885 calls) | 359 ms |
| Serialisation (phase 5) | 226 ms |
| Graph bookkeeping | 82 ms |
| File map | 47 ms |

**Cloud worker-farm check** (`5f0c2c640`, 3 interleaved rounds, after a cold-disk round 1):

| | Warm-build |
|---|---:|
| JS | about 3.0 s |
| Phase 3 components alone | about 1.9–2.1 s |
| Phase 3 + worker-farm | about 1.54 s, i.e. **about 1.95× over JS** |

After-GC RSS was 337–342 MB with worker-farm, against 360–378 MB for JS.

**Mac only (not valid for decisions)** — transform-cache on top of worker-farm, dev build, warm-build:

| | Round 1 | Round 2 | Round 3 |
|---|---:|---:|---:|
| without transform-cache | 1924 ms | 1845 ms | 2354 ms |
| with transform-cache | 1640 ms | 1754 ms | 1522 ms |

transform-cache has no cloud numbers yet.

## 5. Machines and how to work

- **This Mac** (`~/Projects/metro-rs`) is for implementation and the compile and tests gates. Run `source "$HOME/.cargo/env"` before cargo.
- **The cloud container** (driven by the coordinator session) is for `./gate.sh --report` and perf decisions.
- **Mac file-open caveat.** File opens from non-Apple binaries cost about 100–150 µs each and are serialised across threads and processes, most likely an endpoint-security scanner. IO-heavy Mac timings are meaningless. See [port/design/phase-3.md](design/phase-3.md), "Potential improvements".
- **Cloud noise** is about ±10%. Paired warm-build ratios have ranged 1.27–1.61× on the same code, so use interleaved runs and medians.
- **Implementation:** Sonnet sub-agents with explicit specs, then review their work yourself.

## 6. Gotchas met

- **Commit hashes:** quote them only after reading them from git. Several wrong hashes were posted today.
- **glibc:** on Linux, native memory needs trimming after the first build (`992e869`, fixed malloc thresholds plus `malloc_trim`), or after-GC RSS looks inflated.
- **napi3, not napi4.** napi4 makes napi-rs create a module-lifetime "CustomGC" async resource, which breaks `server-torn-down-test`. Async napi tasks are followed by one timer tick (`settled()` in metro-file-map) for the same reason.
- **Running as root** (the cloud) makes chmod-based unreadable-directory tests readable. The file-map oracle tests account for it.
- **The HMR runner** collapses duplicate watcher cycles (repeated error and trailing empty updates), because identical JS-only runs vary there. It prints how many it collapsed.
- **No report on failure:** `gate.sh` writes no `port/reports/phase-<n>.md` when a gate fails. On a failed run the numbers are only in `port/.gate/perf/perf.md`.
- **Watcher tests time out on this loaded Mac:** the metro-file-map watcher integration tests and the black-box watch mode. Re-run alone and compare with the baseline worktree in `port/.cache/baseline-*`. Never skip a test.

## 7. Open questions for Szymon

- **Multi-process hashing for corporate Macs.** Would a process pool, or a signed binary, avoid the per-open cost? Rust threads and Rust processes were equally slow; only Apple-signed tools scaled.
- **bincode 1.x is unmaintained** (RustSec). It is only used for our own file-map cache file. Should we move to bincode 2 or postcard?
- **Expo fallback risk.** A custom cache manager, custom file-map plugins or a replaced `resolve` function silently fall back to JS. Expo setups may hit these and get no speed-up.
- **Trying the WIP on a real app.** Szymon asked whether the port can be used in an existing project. It is possible with package-manager overrides plus a linked `metro-rs`, but it is untested. A real-app trial is still open.
