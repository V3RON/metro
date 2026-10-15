# Phase 4: the incremental graph, transform-cache reads and lazy workers

This note is written before any code. It applies RULES.md §7 and the "Phase 4" section of [js-rust-boundary.md](js-rust-boundary.md). Szymon widened phase 4's scope on 2026-10-14 ("Widen scope"). On main (07a1854) the components are `delta-core`, `incremental`, `transform-cache` and `worker-farm`, with a warm-build target of 1.6×. edit and cold-filemap are informational. The places where this note departs from the boundary note are listed at the end.

## Where the time goes

Cloud profile of warm-build after phase 3 (medians of 5): pure JS 2877 ms, Rust 2189 ms.

| Area | Time | Phase 4 |
|---|---:|---|
| Transform-cache reads: FileStore `readFile`, `JSON.parse`, SHA-1, cache key | 632 ms | in scope (widened) |
| Worker farm: 3 workers forked although every transform is a cache hit; `server.end()` waits about 410 ms for them | about 480 ms | in scope (widened): start lazily |
| Resolution (see below) | 359 ms | in scope: batch per module |
| Serialisation | 226 ms | phase 5 |
| Graph bookkeeping | 82 ms | in scope: topology |
| File map | 47 ms | done (phase 3) |

Resolution splits into:

- **Native `resolverResolve`:** 239 ms over 7,445 calls (p50 23 µs, p99 113 µs).
- **JS glue:** about 120 ms over 26,885 `resolveDependency` calls, of which 19,440 are hits in the JS resolution cache.

The graph work alone has a ceiling of about 1.64×. With the transform-cache reads and lazy workers, about 2× is possible. The target is 1.6×. edit is informational, because phase 4's components own about 2 ms of a 270 ms edit.

The native resolver costs about 32 µs per call on the cloud machine. That is what a miss in the batched resolution of step 3 costs, and why the JS resolution cache moves into Rust with it.

## Plan, in order

Each step is measured in the cloud before the next one starts.

### 1. Lazy transform workers

- **Today:** `WorkerFarm` forks its workers in the constructor, and `Transformer.end()` waits for `kill()`.
- **Change:** create the farm on the first `transform()` call. A build whose transforms are all cache hits then never forks. `kill()` with no farm resolves at once.
- **Language:** this is JS (the `WorkerFarm` and `Transformer` shell). It needs no Rust.
- **Behaviour:** unchanged. The first real transform pays the fork cost it pays today at startup.
- **Switch:** the `worker-farm` component. With it off (`METRO_RUST=off`), the farm starts in the constructor exactly as upstream.
- **Tests:** tests that count forks or rely on eager startup run unchanged in JS mode. In Rust mode they show whether anyone depends on eager startup.

### 2. Transform-cache reads in Rust

- **The JS path:** for each module, `Transformer.transformFile` computes a partial key (`stableHash` of options and path), gets the file's SHA-1 (native since phase 3), and calls `cache.get(fullKey)`. With metro-cache's `FileStore` that means `fs.promises.readFile` (four libuv round trips) and then `JSON.parse`.
- **Switch:** the `transform-cache` component.
- **Change:** with the default `FileStore` (exactly that class, otherwise JS as today), the read and parse move to one napi `AsyncTask` per key. It does a single read on a worker thread and returns either the raw bytes for JS to `JSON.parse`, or the `\0`-prefixed buffer as is. Reads for many keys of one traversal can be batched into one task.
- **Rule 10, decided with numbers in this step:**
  - **Option A:** return the JSON text and let V8 parse it, as today. Transform outputs stay in the JS heap.
  - **Option B:** keep the parsed output in Rust and give JS lazy getters.

  The cloud profile measured about 30 MB of code and map strings per build, and about 20 ms to copy them into Rust. Rust-owned buffers are therefore affordable, and they are what phase 5's serializers need to avoid copying every module's code on every bundle request. I measure A and B on the bench app (read plus parse, then a serializer pass) and pick B unless it is slower for phase 4's own warm-build. Either way, the public `TransformResult` objects stay JS objects with the same shape (rule 9).
- **Cache keys** (`stableHash`, `getTransformCacheKey`) stay in JS unless the profile shows them as significant. They are user-config dependent (`getTransformOptions`, `customTransformOptions`).
- **`Cache.get`'s logging and backfill** (the quirk where the backfill relies on `Buffer` identity) stay in JS around the native read.

### 3. Batch resolution per module

- **Today:** `buildSubgraph.resolveDependencies` calls `resolve(parentPath, dep)` once per dependency.
  - Metro's resolve function is `DependencyGraph.resolveDependency`. It builds a compound key with `JSON.stringify(resolverOptions, canonicalize)`, walks four nested JS `Map`s, and on a miss calls `ModuleResolver`, which builds a context and calls `resolve()`.
- **Change, when Metro's own resolve function is in use (tagged, rule 7):**
  - `buildSubgraph` hands Rust the module's whole dependency list in one call: origin, names, `isESMImport` flags, platform, and the resolver-options key.
  - Rust keeps `DependencyGraph`'s resolution cache, keyed on the same compound key, and resolves the misses natively.
  - It returns the resolved paths, or per-dependency errors. JS turns those into the same error objects (`UnableToResolveError` and the like, built where `ModuleResolution` builds them today).
- **What stays in JS:**
  - a custom `resolveRequest`, scheme resolvers and replaced callbacks still go through the per-dependency JS path for that module;
  - the resolution cache is cleared on file changes, as `_onHasteChange` does today.

### Step 3 as built (batch resolution)

- **A separate `ResolutionCache` napi class**, not an extension of the resolver binding's file-system handle. Its key is DependencyGraph's compound key (origin, resolver options, platform, name, `isESMImport`) with the `assumeFlatNodeModules` origin-key rule. `DependencyGraph` swaps it in for `_resolutionCache` only when `incremental` is on and the resolver and file map are native. `_onHasteChange` clears it, as it clears the JS cache.
- **`resolverResolveBatch`** resolves one module's dependencies in order and stops at the first one it cannot answer natively: a scheme resolver, an error, or an `empty` or `assetFiles` result. JS resolves that one per dependency, as today (so errors and user hooks run in JS), then re-batches from the next dependency. Warnings are buffered per dependency and emitted only once that dependency is answered.
- **Tagging (rule 7).** `transformHelpers.getResolveDependencyFn` tags Metro's own resolve function with `BATCH_RESOLVE`. A replaced resolve function has no tag and takes the per-dependency path.
- **Verify** (`METRO_RUST_VERIFY=incremental`) re-resolves each natively answered dependency through the JS path with a shadow cache. The JS path in verify mode still uses the Rust resolver (`resolver` is on), so verify checks the cache and batch semantics, not the resolver itself. The resolver is checked by its own verify mode and the differential.
- **Known limit.** An invalid `require.context` regexp that throws in the middle of a module's dependency list throws after the batch has already resolved the later dependencies. That leaves extra cache entries and warnings that today's order would not produce. The bundle result and the error are the same. It is an extreme edge case; named in the PR.

### 4. Graph topology in Rust

- **Moves to Rust:** modules as `FileId`s, forward edges with their keys, inverse dependencies, the entry points, cycle collection (`Graph.#collectCycles`), and the traversal and deletion order of `traverseDependencies`.
- **One delta per traversal back to JS:** the added, modified and deleted module paths, and the changed edges. JS updates the real `graph.dependencies` `Map` and the `Module` objects in exactly today's insertion order (rule 9). Serializers, Expo and React Native CLI read that map directly.
- **Transform results stay JS objects** (rule 10, per step 2's decision). Rust holds only topology.
- **The JS `id → string` array starts here** (rule 4). Rust returns module ids in deltas, and JS keeps an append-only path array extended in batches.

## HMR differential check

The exit criterion asks for identical HMR deltas across a scripted edit session.

- **First, in JS,** `port/diff/` gains an HMR session runner. It starts a dev server on the bench app, opens the `/hot` WebSocket, applies a fixed script of edits (modify, add a file, add and remove an import, delete a file, rename), and records every HMR message.
- **It runs with `METRO_RUST=off` and with the phase's components on.** It compares the messages byte for byte, with ids and timings normalised exactly as JS emits them.
- **When it's added:** before any Rust code, and checked against JS against JS.

## Verify mode

`METRO_RUST_VERIFY` gains `incremental` and `delta-core`:

- **Batch resolution:** also runs the JS `resolveDependency` per dependency and compares.
- **Each graph traversal:** the Rust delta is applied to a shadow JS `Graph` built by the JS implementation from the same inputs. The two are compared: `dependencies` order, every module's dependencies and inverse dependencies, entry points, and the deltas.
- **Cache reads:** the native read is compared with the JS `FileStore.get`.
- **The gate** sets it in its Rust-mode Jest run, as for phases 2 and 3.

## Departures from js-rust-boundary.md

- **Wider scope than the boundary note's phase 4:** transform-cache reads and lazy workers are included. That is Szymon's decision.
- **The cache key stays in JS.** Only the read moves.
- **Rule 10 is decided inside step 2** with measurements, not at the very start of the phase. Steps 1 and 2's read path don't depend on it.

## Risks

- **The ~2× warm-build target** depends on the cache read win. If V8's `JSON.parse` is the bulk of the 632 ms, option A gains little, and option B, which needs phase 5 serializers to use it, becomes the only lever. I report with numbers before choosing.
- **Lazy workers** change when forks happen. A user who relies on the startup cost being paid early (warm-up) sees it on the first edit instead. This is behaviour-neutral for output, but it is noted in the PR.
- **Mac timings** stay unreliable for IO (phase 3's notes). All perf decisions use the cloud.
