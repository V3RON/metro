# JS/Rust boundary design for the Metro port

Status: **approved by Szymon on 2026-10-14**, including the three decisions at the end. Binding for phases 2–5 through RULES.md section 7.
Starts from the feasibility thread's boundary findings (phase 1 evidence and laptop measurements). Covers phases 2 to 5. The phase 2 note in the repo (`port/design/boundary.md` on `port/phase-2`) should follow this one, or say where it differs and why.

## The short version

Measured on the bench app: a Rust resolver that asks JavaScript for every file check is slower than today's JavaScript resolver, so it cannot reach phase 2's 1.5× target. The fix is to keep the data next to the code that reads it. JavaScript calls into Rust once per unit of work, and Rust answers file questions from its own copy of the file map. User hooks stay where they are, called from JavaScript.

The ten rules:

1. **JavaScript calls Rust; Rust almost never calls JavaScript.** User hooks (`resolveRequest`, `schemeResolvers`, `customSerializer`, `getTransformOptions`, custom cache managers) are dispatched by the JavaScript shell before or after it calls Rust, never from inside a Rust loop.
2. **One crossing per unit of work, never per lookup.** That means one call per `resolve()` in phases 2 and 3, one per module's dependency list from phase 4, and one per watcher change batch.
3. **Rust holds the file system state.** It is a read replica of the JavaScript file map in phase 2, and it becomes the file map itself in phase 3. There is one store, used by the resolver, the Haste map, the package cache and later the graph.
4. **Paths are interned once.** A `FileId` (u32) indexes Rust's table of normalised, root-relative paths. Ids are never reused within a process, so a stale id is harmless. JavaScript keeps an append-only `id → string` array, extended in batches, so turning an id into a string in JavaScript is an array read, not a call.
5. **Strings cross only at public API edges.** Inside Rust, and between Rust components, everything is ids.
6. **Configuration crosses once.** `sourceExts`, `mainFields`, conditions, `assetExts`, `nodeModulesPaths`, `extraNodeModules` and the like are registered once and get a handle. The JavaScript side caches handles by object identity in a `WeakMap`. A per-call crossing carries the handle and a few scalars.
7. **Native or JavaScript is decided per callback, by function identity.** Metro's own callbacks (the ones `createModuleResolver` builds) are tagged with a symbol pointing at the Rust store, and Rust serves those natively. If a caller replaces one (for example a `resolveRequest` that passes `{...context, fileSystemLookup: myLookup}` to the default resolver), only that callback goes through JavaScript. It stays correct, but it is slower for that project.
8. **Rust is re-entrant around JavaScript calls.** Rust never holds a lock or a mutable borrow while it calls JavaScript, because a JavaScript callback may call back into Rust (for example `context.resolveRequest`). Calls that return values are synchronous and on the main thread. `ThreadsafeFunction` is only for notifications such as watcher batches.
9. **The public JavaScript objects stay real JavaScript objects.** `Graph` and its `dependencies` map, change events and resolution results are what Expo, React Native CLI and custom serializers read. Rust updates them from batched deltas; it doesn't replace them with proxies.
10. **Transform outputs stay in the JavaScript heap until phase 4 decides otherwise.** They are produced by Babel in JavaScript workers and read by JavaScript serializers until phase 5. Where they live once serializers move to Rust is the phase 4 open question below.

## What was measured

Run in the 4-core cloud container on 2026-10-14 (Node 22, napi-rs 2.16, release build with LTO). Absolute numbers will differ on the M3 laptop; the ratios are what matter. The probe crate and the counting script are in the project's shared folder, under `metro-rs-plan/boundary/`.

**Crossing costs (napi-rs 2, per operation):**

| Operation | Cost |
|---|---:|
| JavaScript calls an empty Rust function | 13 ns |
| JavaScript calls Rust with 3 path strings, gets 1 string back (shape of a `resolve` call) | 305 ns |
| The same file lookup done in pure JavaScript (`Map.has` and a result object) | 292 ns |
| Rust calls a JavaScript lookup callback, reads `{exists, type, realPath}` back | 2,062 ns |
| Rust calls a JavaScript callback that returns a number | 1,095 ns |
| Rust reads the 28 properties of a resolution context object | 8,685 ns |
| Loading 24,000 paths into a Rust hash map as one `\0`-joined string | 9–14 ms once |

Calling Rust from JavaScript is cheap. Calling JavaScript from Rust, or reading JavaScript objects from Rust, is several times dearer than doing the same work in JavaScript.

**What the resolver does on the bench app (warm build, iOS, 2,741 modules):**

- 7,445 calls reach `metro-resolver`'s `resolve()` after `DependencyGraph`'s own cache (6,300 relative, 1,145 bare specifiers).
- Those calls make 171,000 context callbacks, about 23 per resolution:
  - `fileSystemLookup`: 86,706
  - `getPackageForModule`: 83,922 (the `browser` field redirect check on every candidate)
  - `resolveHastePackage`: 1,145; `resolveHasteModule`: 243
  - `getPackage` and `doesFileExist`: 66 each; `resolveAsset`: 5
- `resolve()` took 415 ms in total, 276 ms of it inside the callbacks. The counting wrapper inflates both numbers somewhat, but not the counts.

**What that means for phase 2:**

- With callbacks into JavaScript, each of the 171,000 callbacks costs about 2 µs of crossing on top of the JavaScript work it still does. That's roughly 340 ms of overhead to save at most the 140 ms of resolver logic that moves to Rust. Expect around 0.8×, not 1.5×.
- With a Rust replica, each `resolve()` is one crossing of about 0.3–0.5 µs plus about 23 native lookups. The 1.5× target leaves plenty of room, so straightforward code is enough.
- **The laptop confirms this** (M3 Pro, Node 24, measured by the phase 2 session):
  - A JavaScript resolve takes about 11 µs, so 1.5× needs about 7 µs.
  - Per-lookup callbacks would add about 15.8 µs per resolve before any resolver logic runs.
  - Converting a `package.json` object through JSON costs 31.7 µs per call, against 777 ns when it is kept as a handle.
  - Only 42 distinct `package.json` objects are involved, which is why Rust parses and caches them itself.

## Phase by phase

### Phase 2: resolver in Rust, file map still in JavaScript

**Call flow of the switched `resolve(context, specifier, platform)`:**

1. In JavaScript, exactly as today: if `context.resolveRequest` is a user function, call it with a context whose `resolveRequest` is the switched `resolve`. The same applies to `schemeResolvers`. User code is never called from Rust.
2. In JavaScript: look up the config handle for this context's configuration objects (a `WeakMap` hit), and collect which callbacks are Metro's tagged ones and which were replaced.
3. One call to Rust: `resolve(configHandle, storeHandle, originPath, specifier, platform, flags, replacedCallbacks)`. `flags` is a bitmask for `isESMImport`, `dev`, `allowHaste`, `preferNativePlatform` and similar booleans. `replacedCallbacks` is usually empty.
4. Rust runs the resolution algorithm. It answers tagged callbacks natively and calls any replaced ones synchronously.
5. Rust returns a compact result: the resolution type and path(s), or an error code with its payload (candidates, directory paths). JavaScript builds the same error classes (`FailedToResolvePathError` and the others), so `instanceof` checks and messages are unchanged.

**The Rust file-store replica (crate `metro-rs-file-store`):**

- Built in bulk from the JavaScript file map when `DependencyGraph` is ready. Paths are `\0`-joined, types and symlink targets go in a parallel buffer, and it's one crossing.
- Kept current by forwarding each change batch from `DependencyGraph._onHasteChange`. That's the place that already invalidates the package cache and the resolution cache, so the replica updates at the same moment.
- Answers `lookup` (including symlink real paths), `exists` and `hierarchicalLookup` (for the closest `package.json`).
- Reads and parses `package.json` files itself, caches them by `FileId`, and drops them on change. If a file needs `JSON.parse`'s exact semantics, Rust defers that file to JavaScript's `getPackage`. The known case is lone surrogate escapes, which `serde_json` rejects.
- Haste lookups (about 1,400 per build) cross into JavaScript in phase 2, at a cost of about 3 ms. They become native in phase 3.
- A debug switch (`METRO_RUST_VERIFY=resolver`) runs both the native and the JavaScript lookup and throws on any difference. The gate's Rust-mode Jest run should turn it on, so replica drift fails tests instead of producing a subtly different bundle.

**What phase 2 changes outside metro-resolver:** `createModuleResolver` tags its callbacks and passes the store handle, and `DependencyGraph` builds and updates the replica. That is glue the switch needs, so it stays within RULES.md's "leave alone what the port does not need".

**Benchmark honesty:** `micro:resolver` should include building the replica inside the timed region on the Rust side, because the JavaScript side gets its file map for free.

### Phase 3: file map in Rust

- The replica becomes the file map. The crawler (Node crawler and Watchman) and the watcher write into the Rust store directly. The bulk load and the change forwarding from phase 2 go away; nothing else in the resolver changes. This is the payoff of building the store in phase 2.
- The Haste map and the package cache move into the store, so the resolver makes no JavaScript calls on Metro's default setup.
- `metro-file-map`'s public `FileSystem` API (`lookup`, `exists`, `matchFiles`, `hierarchicalLookup`, `getAllFiles`, `getSha1` and the rest) becomes a thin wrapper that makes one call per method. Its remaining JavaScript callers are few and not hot once the resolver is native.
- Change events: Rust batches watcher events and delivers each batch with one `ThreadsafeFunction` call. JavaScript builds the same public `change` event object as today.
- File map cache: with the default `DiskCacheManager`, Rust writes and reads its own format and no JavaScript snapshot is built. A custom `unstable_fileMapCacheManagerFactory` gets the JavaScript snapshot materialised on demand, which is the slow but correct path.
- Built-in file map plugins (Haste, dependency extraction, mocks) run in Rust. A custom plugin falls back to the JavaScript file map.

### Phase 4: incremental graph

- Graph topology (modules as `FileId`s, edges, inverse dependencies, cycle collection) and `DependencyGraph`'s resolution cache move to Rust.
- One crossing per transformed module: JavaScript hands Rust the module's dependency list and gets back resolved `FileId`s or errors. This replaces about 10 `resolve()` calls per module.
- The public `Graph` stays a JavaScript object (rule 9). After each traversal, Rust returns one delta (added, modified and deleted ids, plus changed edges), and JavaScript updates `graph.dependencies` and the `Module` objects. The cost is per changed module, not per lookup. A lazy `Map` proxy was considered and rejected: serializers, Expo and React Native CLI read the map directly, and its iteration order has to match today's byte for byte.
- **Open question, decided with numbers at the start of phase 4:** whether transform outputs (code and maps) are stored as Rust-owned buffers when they come out of the cache or a worker, with a lazy JavaScript getter. That's needed if phase 5's Rust serializers are to avoid copying every module's code across the boundary on every bundle request.

### Phase 5: output and server

- Rust serializers read topology and outputs natively. When a `customSerializer` or `experimentalSerializerHook` is set, the JavaScript `Graph` from phase 4 is already there, so those hooks keep working unchanged.
- The kept-in-JS list below gets re-measured. Once the serializers are in Rust, most of the reasons for keeping these in JavaScript disappear.

## Kept in JavaScript for performance, to revisit in phase 5

| Item | Why it stayed in JavaScript | Phase 1 evidence | Revisit when |
|---|---|---|---|
| `BundleBuilder` | Slower through napi: called from JavaScript serializers with many small strings | 57–70 ms in Rust vs 50–57 ms in JavaScript | Serializers move to Rust (phase 5) |
| metro-core `canonicalize` | A `JSON.stringify` replacer V8 calls once per value; one napi call is already 54 ns, and a small options object takes 620 ns in JavaScript | per-value cost | Resolver options are registered as handles (rule 6), which may remove the call |
| JavaScript `ob1` package | One-line arithmetic; the Rust crate is used inside source-map only | about 50 ns per call | Only if a JavaScript caller remains hot |
| Source map `sources`, `sourcesContent`, `x_*` assembly | Key-order parity, and lone surrogates can't cross napi faithfully | correctness, not speed | Serializers move to Rust |
| Haste lookups during phase 2 | About 1,400 calls per build, about 3 ms | this note | Phase 3 (native) |

## How other Rust-core bundlers do it

Primary sources only; the full report with links is in [js-rust-boundary-prior-art.md](js-rust-boundary-prior-art.md).

- **Everyone keeps files, paths and the resolver in Rust and calls JavaScript as rarely as possible.**
  - Rspack's resolver runs fully in Rust. It dropped webpack's JavaScript `resolve.plugins`, and Rspack's docs warn that a JavaScript file system "will slow down Rspack a lot".
  - Rolldown and Vite 8 moved Vite's own resolve plugin to Rust.
  - Turbopack runs JavaScript tools (loaders, Babel, PostCSS) in a separate pool of Node processes.
- **JavaScript hooks are the measured bottleneck.** Rolldown published numbers for 10,000 modules:
  - With no plugins the build took 746 ms. With 10 JavaScript `transform` hooks it took 2.79 s, about 20 µs per hook call including the source string.
  - With hook filters checked in Rust, the 10-plugin build took 747 ms. Rspack also skips hooks with no taps and caches the tap list of hooks that run once per module.
- **Rspack and Rolldown call hooks through `ThreadsafeFunction`, asynchronously.** They need that because their Rust core runs on many threads. Metro's `resolve()` is synchronous and runs on the main thread, so rule 1 avoids the problem instead: hooks are called from JavaScript, before Rust is entered. No Rust thread ever waits for JavaScript.
- **Off-the-shelf resolvers don't fit as they are.** oxc_resolver (28× faster than enhanced-resolve) has no per-path invalidation. It only has a global `clear_cache()`, and that is unsafe while resolutions are running. It also implements webpack semantics, not Metro's: platform extensions, Haste, the `browser` field redirect on every candidate. Parcel's Rust resolver returns invalidation sets with each result, but it is built for a fresh cache per build.
  - So phase 2 ports `metro-resolver`'s own algorithm onto the Metro file store, as planned.
  - oxc_resolver's interned `CachedPath` design (hash, parent pointer and lazily filled `package.json` slot per path) is a good model for the store.
- **No one publishes per-call napi costs for Rust calling JavaScript.** The probe in this folder is the only measurement we have (about 2 µs per synchronous callback). Node-API's JavaScript-to-native cost of 7–15 ns, measured by the Bun team, matches our 13 ns.
- **Serialisation costs more than the call itself.** Oxc and Marvin Hagemeister both report this: on 610,000 AST nodes, a flat buffer with lazy objects took 0.62 s against 2.91 s for `serde_json` plus `JSON.parse`. That supports ids and flat buffers (rules 4 and 5) over rich objects.
- **Filters are worth considering for user resolvers.** Expo's and React Native CLI's `resolveRequest` run for every resolution. A declarative filter (for example a specifier regex that says when the hook applies) would let Rust skip JavaScript for most requests. That's a new config option, so it's not in scope. It's listed as a phase 5 idea, not a decision.

## Decisions (approved)

1. **Adopt the ten rules as binding for phases 2–5** (RULES.md section 7). Approved.
2. **Phase 2 builds the Rust file-store replica and the native `package.json` reader**, which were phase 3 work. Without them, phase 2 can't reach 1.5×, and phase 3 shrinks by the same amount. Approved.
3. **Projects that replace context callbacks resolve at JavaScript speed or a little below.** They stay correct, but each replaced callback costs about 2 µs per crossing. Approved.
