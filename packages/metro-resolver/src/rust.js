/**
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 *
 * @flow
 * @format
 * @oncall react_native
 */

/**
 * The Rust implementation of `resolve()`, used when the `resolver` component
 * is switched on (port/design/boundary.md).
 *
 * User hooks (`resolveRequest`, `schemeResolvers`) are dispatched here, in JS,
 * exactly as resolve.js does. Then one call crosses into Rust per resolve. It
 * carries a config handle (created once per configuration, cached by object
 * identity), the file store behind Metro's own callbacks (if they are tagged
 * with FILE_STORE), the request strings and a bitmask. Rust answers tagged
 * callbacks natively and calls every other callback back through
 * `callbackTrampoline`, synchronously.
 */

import type {
  FileAndDirCandidates,
  FileCandidates,
  Resolution,
  ResolutionContext,
} from './types';

import FailedToResolveNameError from './errors/FailedToResolveNameError';
import FailedToResolvePathError from './errors/FailedToResolvePathError';
import FailedToResolveUnsupportedError from './errors/FailedToResolveUnsupportedError';
import InvalidPackageConfigurationError from './errors/InvalidPackageConfigurationError';
import InvalidPackageError from './errors/InvalidPackageError';
import PackageImportNotResolvedError from './errors/PackageImportNotResolvedError';
import PackagePathNotExportedError from './errors/PackagePathNotExportedError';
import {getRustBinding, isRustEnabled, parseMetroRust} from 'metro-rust-switch';
import path from 'node:path';

/**
 * Metro tags the `fileSystemLookup`, `doesFileExist`, `getPackage` and
 * `getPackageForModule` callbacks it creates with this symbol. The value is
 * the file store that answers the same questions natively: a Rust replica of
 * its file map, or the Rust file map itself (its `RustFileSystem`). A callback
 * without the tag, or replaced by the caller, is called through JS.
 *
 * With the Rust file map, `resolveHasteModule` and `resolveHastePackage` carry
 * the tag too, and `HASTE_PLATFORM` names the platform they are bound to.
 * They are answered natively only for a resolve of that platform.
 */
export const FILE_STORE: symbol = Symbol.for('metro.resolver.fileStore');
export const HASTE_PLATFORM: symbol = Symbol.for(
  'metro.resolver.hastePlatform',
);

export type NativeFileStore = {...};
export interface FileStoreHolder {
  readonly native: NativeFileStore;
}

type RustResolverConfig = {...};

type RustResolution =
  | string
  | ReadonlyArray<string>
  | null
  | {readonly error: string, readonly [string]: unknown};

type ResolverBinding = {
  ResolverConfig: Class<RustResolverConfig> & {
    new(options: {...}): RustResolverConfig,
  },
  resolverResolve(
    config: RustResolverConfig,
    store: ?NativeFileStore,
    originModulePath: string,
    specifier: string,
    platform: string | null,
    flags: number,
    callback: (index: number, a: unknown, b: unknown, c: unknown) => unknown,
  ): RustResolution,
  ...
};

// Bits of `flags`. Keep in sync with metro-rs/crates/binding/src/resolver.rs.
const ALLOW_HASTE = 1;
const IS_ESM_IMPORT = 2;
const CAN_PARSE_URL = 4;
const NATIVE_FILE_SYSTEM_LOOKUP = 8;
const NATIVE_DOES_FILE_EXIST = 16;
const NATIVE_GET_PACKAGE = 32;
const NATIVE_GET_PACKAGE_FOR_MODULE = 64;
const VERIFY = 128;
const EXTRA_NODE_MODULES = 256;
const NATIVE_HASTE = 512;

// Indices passed to `callbackTrampoline`. Keep in sync with resolver.rs.
const CB_FILE_SYSTEM_LOOKUP = 0;
const CB_DOES_FILE_EXIST = 1;
const CB_GET_PACKAGE = 2;
const CB_GET_PACKAGE_FOR_MODULE = 3;
const CB_RESOLVE_HASTE_MODULE = 4;
const CB_RESOLVE_HASTE_PACKAGE = 5;
const CB_RESOLVE_ASSET = 6;
const CB_LOG_WARNING = 7;
const CB_EXTRA_NODE_MODULE = 8;

// Returned to Rust instead of a value when a callback throws. Rust then
// returns {error: 'Host'} and the exception is rethrown unchanged.
const THREW: symbol = Symbol('metro-resolver callback threw');

let lastMetroRust: ?string;
let lastEnvDecides = false;
let lastEnvBinding: ?ResolverBinding = null;

/**
 * The Rust binding if `resolve()` should run in Rust for this context,
 * otherwise null. METRO_RUST decides when set. Otherwise the resolver runs in
 * Rust when Metro tagged the context's callbacks with a file store, which it
 * only creates when `unstable_rust.resolver` is on in its config.
 */
export function getResolverBinding(
  context: ResolutionContext,
): ?ResolverBinding {
  const metroRust = process.env.METRO_RUST;
  if (metroRust !== lastMetroRust) {
    // Either call may throw (an invalid METRO_RUST, a missing binding). The
    // cache is only updated once both succeed, so the next resolve throws
    // again instead of silently running JS.
    const envDecides = parseMetroRust(metroRust) != null;
    // $FlowFixMe[incompatible-type] The binding exports more than version().
    const envBinding: ?ResolverBinding = getRustBinding('resolver');
    lastEnvDecides = envDecides;
    lastEnvBinding = envBinding;
    lastMetroRust = metroRust;
  }
  if (lastEnvDecides) {
    return lastEnvBinding;
  }
  // $FlowFixMe[invalid-computed-prop]
  if (context.fileSystemLookup?.[FILE_STORE] != null) {
    // $FlowFixMe[incompatible-type]
    return getRustBinding('resolver', {resolver: true});
  }
  return null;
}

let lastMetroRustVerify: ?string;
let lastVerify = false;

/** Whether lookups are checked against the JS callbacks (METRO_RUST_VERIFY). */
function isVerifyEnabled(): boolean {
  const verify = process.env.METRO_RUST_VERIFY;
  if (verify !== lastMetroRustVerify) {
    // Cached only after isRustEnabled succeeds (it throws on an invalid list).
    lastVerify =
      verify != null &&
      verify !== '' &&
      isRustEnabled('resolver', null, {METRO_RUST: verify});
    lastMetroRustVerify = verify;
  }
  return lastVerify;
}

// The context of the innermost Rust resolve in progress, read by the
// trampoline. A callback may itself call resolve(), so this is saved and
// restored around every call.
let currentContext: ?ResolutionContext = null;
let thrownError: unknown = null;
let hasThrown = false;

const packageJsonStrings: WeakMap<interface {}, string> = new WeakMap();

function packageJsonToString(packageJson: unknown): ?string {
  if (packageJson == null || typeof packageJson !== 'object') {
    return null;
  }
  let json = packageJsonStrings.get(packageJson);
  if (json == null) {
    json = JSON.stringify(packageJson);
    packageJsonStrings.set(packageJson, json);
  }
  return json;
}

/**
 * Called from Rust for every callback it does not answer natively. Package
 * JSON objects cross as JSON strings (cached per object).
 */
function callbackTrampoline(
  index: number,
  a: unknown,
  b: unknown,
  c: unknown,
): unknown {
  const context = currentContext;
  if (context == null) {
    throw new Error('metro-resolver: Rust callback outside of a resolve');
  }
  try {
    switch (index) {
      case CB_FILE_SYSTEM_LOOKUP:
        // $FlowFixMe[incompatible-type]
        return context.fileSystemLookup(a);
      case CB_DOES_FILE_EXIST:
        // $FlowFixMe[incompatible-type]
        return context.doesFileExist(a) === true;
      case CB_GET_PACKAGE:
        // $FlowFixMe[incompatible-type]
        return packageJsonToString(context.getPackage(a));
      case CB_GET_PACKAGE_FOR_MODULE: {
        // $FlowFixMe[incompatible-type]
        const pkg = context.getPackageForModule(a);
        if (pkg == null) {
          return null;
        }
        return [
          pkg.rootPath,
          pkg.packageRelativePath,
          packageJsonToString(pkg.packageJson) ?? '{}',
        ];
      }
      case CB_RESOLVE_HASTE_MODULE:
        // $FlowFixMe[incompatible-type]
        return context.resolveHasteModule(a) ?? null;
      case CB_RESOLVE_HASTE_PACKAGE:
        // $FlowFixMe[incompatible-type]
        return context.resolveHastePackage(a) ?? null;
      case CB_RESOLVE_ASSET:
        try {
          // $FlowFixMe[incompatible-type]
          return context.resolveAsset(a, b, c) ?? null;
        } catch {
          // resolveAsset.js ignores exceptions from this callback.
          return null;
        }
      case CB_LOG_WARNING:
        // $FlowFixMe[incompatible-type]
        context.unstable_logWarning(a);
        return null;
      case CB_EXTRA_NODE_MODULE: {
        // `extraNodeModules[packageName]`, read on the object itself (it may
        // be a Proxy or have getters). resolve.js follows truthy values and
        // joins them as paths, so a truthy non-string throws here as it
        // would there.
        const packageName: string = a as any;
        const target = context.extraNodeModules?.[packageName];
        if (!target) {
          return null;
        }
        // $FlowFixMe[incompatible-type]
        return typeof target === 'string' ? target : path.join(target, '.');
      }
      default:
        throw new Error(`metro-resolver: unknown Rust callback ${index}`);
    }
  } catch (error) {
    thrownError = error;
    hasThrown = true;
    return THREW;
  }
}

// The configuration values a handle is built from, as plain data, compared
// by content on every resolve (cheap: a few short arrays). Matching by
// content keeps one handle for equal configurations, also when Metro builds
// a new assetExts Set after every change batch, and notices in-place changes
// such as sourceExts.push(...).
type ConfigSnapshot = {
  assetExts: Array<string>,
  disableHierarchicalLookup: boolean,
  mainFields: Array<string>,
  nodeModulesPaths: Array<string>,
  preferNativePlatform: boolean,
  sourceExts: Array<string>,
  unstable_conditionNames: Array<string>,
  unstable_conditionsByPlatform: Array<[string, Array<string>]>,
  unstable_enablePackageExports: boolean,
  unstable_incrementalResolution: boolean,
};

// The objects a snapshot was last matched against. When the context passes
// the same objects with the same sizes, the entry matches without comparing
// elements (the common case: Metro passes the same objects every time).
type ConfigSources = {
  assetExts: ReadonlySet<string>,
  mainFields: ReadonlyArray<string>,
  nodeModulesPaths: ReadonlyArray<string>,
  sourceExts: ReadonlyArray<string>,
  unstable_conditionNames: ReadonlyArray<string>,
  unstable_conditionsByPlatform: ResolutionContext['unstable_conditionsByPlatform'],
};

type ConfigEntry = {
  snapshot: ConfigSnapshot,
  sources: ConfigSources,
  handle: RustResolverConfig,
};

// Most recently used first. A handle holds the "exports" cache, so reusing it
// matters; the list is bounded so configurations that are never seen again
// are dropped.
const configHandles: Array<ConfigEntry> = [];
const MAX_CONFIG_HANDLES = 8;

function sameStrings(
  snapshot: ReadonlyArray<string>,
  values: Iterable<string>,
  size: number,
): boolean {
  if (snapshot.length !== size) {
    return false;
  }
  let i = 0;
  for (const value of values) {
    if (snapshot[i++] !== value) {
      return false;
    }
  }
  return true;
}

function matches(
  snapshot: ConfigSnapshot,
  context: ResolutionContext,
): boolean {
  if (
    snapshot.disableHierarchicalLookup !==
      (context.disableHierarchicalLookup === true) ||
    snapshot.preferNativePlatform !== (context.preferNativePlatform === true) ||
    snapshot.unstable_enablePackageExports !==
      (context.unstable_enablePackageExports === true) ||
    snapshot.unstable_incrementalResolution !==
      (context.unstable_incrementalResolution === true) ||
    !sameStrings(
      snapshot.sourceExts,
      context.sourceExts,
      context.sourceExts.length,
    ) ||
    !sameStrings(
      snapshot.mainFields,
      context.mainFields,
      context.mainFields.length,
    ) ||
    !sameStrings(
      snapshot.nodeModulesPaths,
      context.nodeModulesPaths,
      context.nodeModulesPaths.length,
    ) ||
    !sameStrings(
      snapshot.unstable_conditionNames,
      context.unstable_conditionNames,
      context.unstable_conditionNames.length,
    ) ||
    !sameStrings(snapshot.assetExts, context.assetExts, context.assetExts.size)
  ) {
    return false;
  }
  const byPlatform = context.unstable_conditionsByPlatform ?? {};
  const platforms = Object.keys(byPlatform);
  if (platforms.length !== snapshot.unstable_conditionsByPlatform.length) {
    return false;
  }
  for (let i = 0; i < platforms.length; i++) {
    const [platform, names] = snapshot.unstable_conditionsByPlatform[i];
    const current = byPlatform[platforms[i]];
    if (
      platforms[i] !== platform ||
      current == null ||
      !sameStrings(names, current, current.length)
    ) {
      return false;
    }
  }
  return true;
}

function snapshotOf(context: ResolutionContext): ConfigSnapshot {
  return {
    assetExts: [...context.assetExts],
    disableHierarchicalLookup: context.disableHierarchicalLookup === true,
    mainFields: [...context.mainFields],
    nodeModulesPaths: [...context.nodeModulesPaths],
    preferNativePlatform: context.preferNativePlatform === true,
    sourceExts: [...context.sourceExts],
    unstable_conditionNames: [...context.unstable_conditionNames],
    unstable_conditionsByPlatform: Object.entries(
      context.unstable_conditionsByPlatform ?? {},
    ).map(([platform, names]) => [
      platform,
      // $FlowFixMe[incompatible-type]
      [...names],
    ]),
    unstable_enablePackageExports:
      context.unstable_enablePackageExports === true,
    unstable_incrementalResolution:
      context.unstable_incrementalResolution === true,
  };
}

function sourcesOf(context: ResolutionContext): ConfigSources {
  return {
    assetExts: context.assetExts,
    mainFields: context.mainFields,
    nodeModulesPaths: context.nodeModulesPaths,
    sourceExts: context.sourceExts,
    unstable_conditionNames: context.unstable_conditionNames,
    unstable_conditionsByPlatform: context.unstable_conditionsByPlatform,
  };
}

/**
 * The cheap check: the same objects as last time, with the sizes of the
 * snapshot, and the same flags. A push or add changes a size; replacing an
 * element in place without changing the size is not noticed (see
 * port/design/boundary.md).
 */
function sameSources(entry: ConfigEntry, context: ResolutionContext): boolean {
  const {sources, snapshot} = entry;
  return (
    sources.sourceExts === context.sourceExts &&
    sources.assetExts === context.assetExts &&
    sources.mainFields === context.mainFields &&
    sources.nodeModulesPaths === context.nodeModulesPaths &&
    sources.unstable_conditionNames === context.unstable_conditionNames &&
    sources.unstable_conditionsByPlatform ===
      context.unstable_conditionsByPlatform &&
    snapshot.sourceExts.length === context.sourceExts.length &&
    snapshot.assetExts.length === context.assetExts.size &&
    snapshot.mainFields.length === context.mainFields.length &&
    snapshot.nodeModulesPaths.length === context.nodeModulesPaths.length &&
    snapshot.unstable_conditionNames.length ===
      context.unstable_conditionNames.length &&
    snapshot.disableHierarchicalLookup ===
      (context.disableHierarchicalLookup === true) &&
    snapshot.preferNativePlatform === (context.preferNativePlatform === true) &&
    snapshot.unstable_enablePackageExports ===
      (context.unstable_enablePackageExports === true) &&
    snapshot.unstable_incrementalResolution ===
      (context.unstable_incrementalResolution === true)
  );
}

function getConfigHandle(
  binding: ResolverBinding,
  context: ResolutionContext,
): RustResolverConfig {
  const first = configHandles[0];
  if (first != null && sameSources(first, context)) {
    return first.handle;
  }
  for (let i = 0; i < configHandles.length; i++) {
    const entry = configHandles[i];
    if (matches(entry.snapshot, context)) {
      entry.sources = sourcesOf(context);
      if (i !== 0) {
        configHandles.splice(i, 1);
        configHandles.unshift(entry);
      }
      return entry.handle;
    }
  }
  const snapshot = snapshotOf(context);
  const handle = new binding.ResolverConfig({
    assetExts: snapshot.assetExts,
    disableHierarchicalLookup: snapshot.disableHierarchicalLookup,
    // Asked per package name through the trampoline (EXTRA_NODE_MODULES), so
    // a Proxy or getters work as in JS.
    extraNodeModules: undefined,
    mainFields: snapshot.mainFields,
    nodeModulesPaths: snapshot.nodeModulesPaths,
    preferNativePlatform: snapshot.preferNativePlatform,
    sourceExts: snapshot.sourceExts,
    unstableConditionNames: snapshot.unstable_conditionNames,
    unstableConditionsByPlatform: Object.fromEntries(
      snapshot.unstable_conditionsByPlatform,
    ),
    unstableEnablePackageExports: snapshot.unstable_enablePackageExports,
    unstableIncrementalResolution: snapshot.unstable_incrementalResolution,
  });
  configHandles.unshift({snapshot, sources: sourcesOf(context), handle});
  if (configHandles.length > MAX_CONFIG_HANDLES) {
    configHandles.pop();
  }
  return handle;
}

function nativeStoreOf(callback: unknown): ?NativeFileStore {
  // $FlowFixMe[incompatible-use]
  const holder: ?FileStoreHolder = callback?.[FILE_STORE];
  return holder?.native;
}

/**
 * Whether a Haste callback is Metro's own, bound to this store and to the
 * platform of this resolve.
 */
function isNativeHaste(
  callback: unknown,
  store: NativeFileStore,
  platform: ?string,
): boolean {
  return (
    nativeStoreOf(callback) === store &&
    // $FlowFixMe[incompatible-use]
    callback?.[HASTE_PLATFORM] === (platform ?? null)
  );
}

/**
 * Resolve in Rust. `resolve` is the public (switched) `resolve`, which custom
 * and scheme resolvers receive as `context.resolveRequest`.
 */
export function resolveWithRust(
  binding: ResolverBinding,
  context: ResolutionContext,
  specifier: string,
  platform: string | null,
  resolve: (
    context: ResolutionContext,
    specifier: string,
    platform: string | null,
  ) => Resolution,
): Resolution {
  let flags = 0;
  if (context.allowHaste) {
    flags |= ALLOW_HASTE;
  }
  if (context.isESMImport === true) {
    flags |= IS_ESM_IMPORT;
  }
  if (context.extraNodeModules) {
    flags |= EXTRA_NODE_MODULES;
  }

  // The same branch order as resolve.js: only a specifier that is not
  // relative, absolute or a subpath import is a candidate for a scheme.
  if (
    !isRelativeImport(specifier) &&
    !path.isAbsolute(specifier) &&
    !specifier.startsWith('#') &&
    specifier.indexOf(':') > 0 &&
    URL.canParse(specifier)
  ) {
    const scheme = specifier.slice(0, specifier.indexOf(':')).toLowerCase();
    const schemeResolvers = context.schemeResolvers;
    if (schemeResolvers != null && Object.hasOwn(schemeResolvers, scheme)) {
      try {
        return schemeResolvers[scheme](
          Object.freeze({...context, resolveRequest: resolve}),
          specifier,
          platform,
        );
      } catch (thrown: unknown) {
        // See resolve.js: resolution errors pass through, anything else is
        // an unsupported specifier.
        if (isResolutionError(thrown)) {
          throw thrown;
        }
        throw new FailedToResolveUnsupportedError(
          thrown instanceof Error ? thrown.message : String(thrown),
          {cause: thrown},
        );
      }
    }
    flags |= CAN_PARSE_URL;
  }

  // Metro's own callbacks are answered natively when they all belong to the
  // same store; replaced callbacks are called through JS.
  const store = nativeStoreOf(context.fileSystemLookup);
  if (store != null) {
    flags |= NATIVE_FILE_SYSTEM_LOOKUP;
    if (nativeStoreOf(context.doesFileExist) === store) {
      flags |= NATIVE_DOES_FILE_EXIST;
    }
    if (nativeStoreOf(context.getPackage) === store) {
      flags |= NATIVE_GET_PACKAGE;
    }
    if (nativeStoreOf(context.getPackageForModule) === store) {
      flags |= NATIVE_GET_PACKAGE_FOR_MODULE;
    }
    if (
      isNativeHaste(context.resolveHasteModule, store, platform) &&
      isNativeHaste(context.resolveHastePackage, store, platform)
    ) {
      flags |= NATIVE_HASTE;
    }
  }
  if (isVerifyEnabled()) {
    flags |= VERIFY;
  }

  const config = getConfigHandle(binding, context);
  const previousContext = currentContext;
  currentContext = context;
  hasThrown = false;
  let result;
  try {
    result = binding.resolverResolve(
      config,
      store,
      context.originModulePath,
      specifier,
      platform,
      flags,
      callbackTrampoline,
    );
  } finally {
    currentContext = previousContext;
  }

  if (typeof result === 'string') {
    return {type: 'sourceFile', filePath: result};
  }
  if (result == null) {
    return {type: 'empty'};
  }
  if (Array.isArray(result)) {
    return {type: 'assetFiles', filePaths: result};
  }
  // $FlowFixMe[incompatible-type]
  const error: {readonly error: string, readonly [string]: unknown} = result;
  throw toJsError(error);
}

function toJsError(error: {
  readonly error: string,
  readonly [string]: any,
}): unknown {
  switch (error.error) {
    case 'Host': {
      const thrown = thrownError;
      thrownError = null;
      if (!hasThrown) {
        return new Error('metro-resolver: Rust reported a callback error');
      }
      hasThrown = false;
      return thrown;
    }
    case 'Verify':
      return new Error(error.message);
    case 'FailedToResolveNameError':
      return new FailedToResolveNameError(error.dirPaths, error.extraPaths);
    case 'FailedToResolvePathError':
      return new FailedToResolvePathError(
        toFileAndDirCandidates(error.candidates),
      );
    case 'FailedToResolveUnsupportedError':
      return new FailedToResolveUnsupportedError(error.message);
    case 'InvalidPackageError':
      return new InvalidPackageError({
        fileCandidates: toFileCandidates(error.fileCandidates),
        indexCandidates: toFileCandidates(error.indexCandidates),
        mainModulePath: error.mainModulePath,
        packageJsonPath: error.packageJsonPath,
      });
    case 'InvalidPackageConfigurationError':
      return new InvalidPackageConfigurationError({
        reason: error.reason,
        packagePath: error.packagePath,
      });
    case 'PackagePathNotExportedError':
      return new PackagePathNotExportedError(error.message);
    case 'PackageImportNotResolvedError':
      return new PackageImportNotResolvedError({
        importSpecifier: error.importSpecifier,
        reason: error.reason,
      });
    case 'MissingFileInHastePackageError':
      return new MissingFileInHastePackageError({
        candidates: toFileAndDirCandidates(error.candidates),
        moduleName: error.moduleName,
        packageName: error.packageName,
        pathInModule: error.pathInModule,
      });
    default:
      return new Error(`metro-resolver: unknown Rust error ${error.error}`);
  }
}

function toFileCandidates(candidates: any): FileCandidates {
  return candidates.type === 'asset'
    ? {type: 'asset', name: candidates.name}
    : {
        type: 'sourceFile',
        filePathPrefix: candidates.filePathPrefix,
        candidateExts: candidates.candidateExts,
      };
}

function toFileAndDirCandidates(candidates: any): FileAndDirCandidates {
  return {
    file: candidates.file != null ? toFileCandidates(candidates.file) : null,
    dir: candidates.dir != null ? toFileCandidates(candidates.dir) : null,
  };
}

function isRelativeImport(filePath: string) {
  return /^[.][.]?(?:[/]|$)/.test(filePath);
}

/** Same list as resolve.js `isResolutionError`. */
function isResolutionError(error: unknown): boolean {
  return (
    error instanceof FailedToResolveNameError ||
    error instanceof FailedToResolvePathError ||
    error instanceof FailedToResolveUnsupportedError ||
    error instanceof InvalidPackageConfigurationError ||
    error instanceof InvalidPackageError ||
    error instanceof PackageImportNotResolvedError ||
    error instanceof PackagePathNotExportedError
  );
}

// Set by resolve.js, which defines the class (it is not part of the public
// error exports).
let MissingFileInHastePackageError: any;
export function setMissingFileInHastePackageError(errorClass: any): void {
  MissingFileInHastePackageError = errorClass;
}
