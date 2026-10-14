# Phase 2 boundary: the resolver and the file store

This note applies the JS/Rust boundary design (port/design/js-rust-boundary.md, which arrives with the RULES.md section 7 PR) to phase 2. It describes what was built and lists each place where phase 2 departs from that design, with the reason. The measurements behind it are in port/design/phase-2-notes.md.

## Call flow of `resolve(context, specifier, platform)`

The switch sits inside metro-resolver's public `resolve()` (packages/metro-resolver/src/resolve.js). Every caller and test therefore goes through it, and `context.resolveRequest` stays the same function object in both modes.

1. **Custom resolver (JS).** As before, a user `resolveRequest` is called with a context whose `resolveRequest` is the public `resolve`.
2. **Choosing Rust.** `getResolverBinding` (packages/metro-resolver/src/rust.js) decides whether to use Rust.
   - **METRO_RUST** decides when it is set. The parsed value is cached per distinct value of the variable.
   - **Otherwise,** Rust is used when the context's `fileSystemLookup` carries a file store. Metro only creates a store when `unstable_rust.resolver` is on.
3. **Scheme resolvers (JS).** These are dispatched in rust.js with the same branch order and error wrapping as resolve.js. The specifier's `URL.canParse` result crosses as a flag, so the JS answer is used and Rust needs no URL parser.
4. **Callbacks (JS).** Metro's own `fileSystemLookup`, `doesFileExist`, `getPackage` and `getPackageForModule` carry `Symbol.for('metro.resolver.fileStore')`, which points at the store (createModuleResolver). A callback without the tag, or one the caller replaced, is answered through JS.
5. **One Rust call.** It passes:
   - `resolverResolve(configHandle, store, originModulePath, specifier, platform, flags, trampoline)`
   - `flags` carries `allowHaste`, `isESMImport`, `URL.canParse`, one bit per natively answerable callback, and verify.
6. **Rust (metro-rs/crates/binding/src/resolver.rs, metro-rs/crates/resolver).**
   - The algorithm runs against a `ResolverHost`.
   - Tagged callbacks are answered from the store.
   - Everything else is called synchronously through the single `trampoline(index, a, b, c)`, which reads the callback from the context of the resolve in progress.
   - The store is borrowed only for the duration of each native query, never while JS runs. A callback may therefore change the file map, and the next native query sees the change, as a JS lookup would (rule 8).
   - Where the JS file map would throw an invariant error, the store returns an error instead of answering. That query then goes to the JS callback, which throws the real JS error.
   - `extraNodeModules` is read per package name on the JS object, through the trampoline, so a Proxy or getters behave as in JS. That is about one call per bare specifier that misses node_modules.
7. **Result.**
   - A string means a source file, an array means asset files, and null means the empty module.
   - Otherwise `{error, ...fields}` comes back, and rust.js constructs the same JS error class from those fields. Messages are computed by the same constructors, and `instanceof` keeps working.
   - A callback that threw comes back as the exception object itself, rethrown unchanged.

## Config handles

- **When a handle is created.** `new ResolverConfig({...})` is created once per configuration. rust.js matches configurations by content on every resolve: the arrays, the `assetExts` Set and `unstable_conditionsByPlatform`, element by element. This is cheap, since they are a few short lists.
- **Why by content.** An in-place change such as `sourceExts.push(...)` gets a new handle. An equal configuration in new objects reuses the old one, together with its "exports" cache. Metro builds a new `assetExts` Set after every change batch.
- **How many are kept.** At most 8 handles, most recently used first.
- **What a call carries.** The handle plus scalars and three strings: origin, specifier and platform.

## The file store

`metro-rs/crates/file-store` is a port of metro-file-map's `TreeFS` (lookup, exists, hierarchicalLookup, including `'..'` ancestors and symlinks) and of Metro's `PackageCache`, including its invalidation rules.

- **Built:** when `DependencyGraph` is ready, in one crossing (RustFileStore.js). It sends all normal paths joined with `\0`, a kind per path, and the symlink targets as the file map stores them.
- **Kept current:** from every change batch in `DependencyGraph._onHasteChange`. That is the same place the JS package and resolution caches are invalidated, and the store invalidates its package cache there with the same rules.
- **package.json:** read and parsed in Rust, with JS object key order (array-index keys first) and duplicate keys handled like `JSON.parse`. When Rust cannot be sure it matches `JSON.parse(readFileSync(p, 'utf8'))`, it defers to Metro's JS callback for that request, so JS errors and messages are kept. This covers unreadable files, invalid UTF-8, a BOM, lone surrogates and other parse errors.
- **Package cache across rebuilds.** A rebuild of the tree, at startup or after a change batch with a symlink, keeps the package.json cache, as a reload of the JS file map keeps the JS `PackageCache`. Changed files are invalidated exactly as `_onHasteChange` invalidates the JS cache.
- **Interned directories.** For a path inside the project root, `lookup` and `exists` resolve the directory once, through a table from directory path to `FileId`, and then look the basename up in it. These lookups are answered from that table instead of walking every segment from the root. Symlinks at the leaf, paths above the root and paths with empty, `.` or `..` segments take the full walk. Every change to the tree clears the table.
- **Switch decisions.** The METRO_RUST and METRO_RUST_VERIFY decisions are cached per value of the variable. The cache is written only after parsing and loading the binding succeed, so an invalid value or a missing binding throws on every resolve rather than falling back to JS.
- **METRO_RUST_VERIFY=resolver:** every native answer is compared with the JS callback's answer, including package.json key order. The resolve throws on the first difference. The gate turns this on in its Rust-mode Jest run. That run therefore checks native answers against JS, but no Jest run covers native-only answers without verify. The differential bundles and the perf runs use native answers without verify.

## Resolver-side caches

- **"exports":** the normalised and condition-reduced map is cached per package.json value, `isESMImport` and platform, in the config handle. The JS caches only the normalised map, per object, in a WeakMap. Each entry holds its package.json, so a replaced package.json gets a new entry and an address is never reused. Only successful reductions are cached, so errors and messages are unchanged.
- **`path.normalize`:** returns an already normal path as it is. A unit test checks this against the full algorithm.

## micro:resolver and cache state (Szymon's decision, 2026-10-14)

The benchmark times a rebuild of the Rust replica's tree, then 5 passes of `resolve()` over the 7,445 recorded resolutions. Both sides start with the package.json cache warm from the recording build.

An earlier version of the benchmark also emptied the Rust package cache inside the timed region. It therefore measured a cold Rust cache against a warm JS one. Szymon chose equal cache states. Per-pass times on this machine (ms):

| | Replica | Pass 1 | Passes 2–5 | Total |
|---|---:|---:|---:|---:|
| JS baseline | none | 82–88 | 76–86 | 395–421 |
| Rust, package cache kept (now) | 12 | 46–52 | 46–49 | 245–263 |
| Rust, package cache emptied (before) | 11 | 80 | about 48 | about 305 |

A pass with a cold Rust package cache costs about 32 ms more than a warm pass. That is roughly 28,000 `hierarchicalLookup`s plus reading and parsing every package.json. A real cold start pays for this on both sides.

## Where phase 2 departs from the design

- **FileId and the JS id→string array (rule 4).** The store uses `u32` node ids internally and never reuses them, and it interns directory paths to ids for lookups. Nothing crosses as an id yet: the only paths that cross are a resolve's inputs and its result, at the public API edge (rule 5). The resolver algorithm itself still works on path strings. Threading ids through it would be a rewrite, and the profile did not need it. The JS id array starts when the graph moves to Rust (phase 4) and ids have a consumer.
- **Symlink changes.** File map change events do not carry symlink targets. A change batch that adds or modifies a symlink therefore rebuilds the store from the JS file map, as at startup. Symlink changes are rare. Phase 3 removes this, because the file map itself moves to Rust.
- **Haste lookups and `resolveAsset`** are still JS callbacks. The design allows this for phase 2. Metro creates its Haste callbacks per call, so they cannot be tagged.
- **`unstable_logWarning`** is called through the trampoline at the point JS calls it, so ordering is unchanged, and an exception from it propagates as in JS.
- **Config from `unstable_rust`.** metro-resolver's `resolve()` has no Metro config. With METRO_RUST unset, it uses Rust when Metro tagged the callbacks, which Metro does only when `unstable_rust.resolver` is on.

## Kept in JS for performance, to revisit in phase 5

From phase 1:

- `BundleBuilder`: slower through napi.
- metro-core `canonicalize`: a per-value JSON replacer.
- The JS `ob1` package.
- source-map `sources`, `sourcesContent` and `x_*` assembly.

Added in phase 2:

- **Replaced callbacks.** Callbacks a project replaces, and `resolveRequest` and `schemeResolvers` themselves, run in JS. That is by design: such projects resolve at about JS speed.
- **Error classes.** These stay JS classes, built from Rust's fields on the error path only.
