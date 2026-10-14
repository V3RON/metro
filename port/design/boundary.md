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
   - The store is borrowed immutably for the call. A change applied re-entrantly from a callback fails loudly instead of aliasing.
7. **Result.**
   - A string means a source file, an array means asset files, and null means the empty module.
   - Otherwise `{error, ...fields}` comes back, and rust.js constructs the same JS error class from those fields. Messages are computed by the same constructors, and `instanceof` keeps working.
   - A callback that threw comes back as the exception object itself, rethrown unchanged.

## Config handles

- **When a handle is created.** `new ResolverConfig({...})` is created once per configuration. rust.js caches handles in a WeakMap keyed by the context's `sourceExts` array, and reuses one only when every other configuration value is the same object or primitive.
- **What a call carries.** The handle plus scalars and three strings: origin, specifier and platform.

## The file store

`metro-rs/crates/file-store` is a port of metro-file-map's `TreeFS` (lookup, exists, hierarchicalLookup, including `'..'` ancestors and symlinks) and of Metro's `PackageCache`, including its invalidation rules.

- **Built:** when `DependencyGraph` is ready, in one crossing (RustFileStore.js). It sends all normal paths joined with `\0`, a kind per path, and the symlink targets as the file map stores them.
- **Kept current:** from every change batch in `DependencyGraph._onHasteChange`. That is the same place the JS package and resolution caches are invalidated, and the store invalidates its package cache there with the same rules.
- **package.json:** read and parsed in Rust, with JS object key order (array-index keys first) and duplicate keys handled like `JSON.parse`. When Rust cannot be sure it matches `JSON.parse(readFileSync(p, 'utf8'))`, it defers to Metro's JS callback for that request, so JS errors and messages are kept. This covers unreadable files, invalid UTF-8, a BOM, lone surrogates and other parse errors.
- **METRO_RUST_VERIFY=resolver:** every native answer is compared with the JS callback's answer, including package.json key order. The resolve throws on the first difference. The gate turns this on in its Rust-mode Jest run.

## Where phase 2 departs from the design

- **FileId and the JS id→string array (rule 4).** The store uses `u32` node ids internally and never reuses them. Nothing crosses as an id yet: the only paths that cross are a resolve's inputs and its result, at the public API edge (rule 5). The JS id array starts when the graph moves to Rust (phase 4) and ids have a consumer.
- **Symlink changes.** File map change events do not carry symlink targets. A change batch that adds or modifies a symlink therefore rebuilds the store from the JS file map, as at startup. Symlink changes are rare. Phase 3 removes this, because the file map itself moves to Rust.
- **Haste lookups and `resolveAsset`** are still JS callbacks. The design allows this for phase 2. Metro creates its Haste callbacks per call, so they cannot be tagged.
- **`unstable_logWarning`** is called through the trampoline at the point JS calls it, so ordering is unchanged. If the warning callback itself throws, the exception is ignored, where JS would propagate it.
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
