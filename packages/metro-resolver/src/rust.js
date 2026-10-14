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
 * the file store (a Rust replica of its file map) that answers the same
 * questions natively. A callback without the tag, or replaced by the caller,
 * is called through JS.
 */
export const FILE_STORE: symbol = Symbol.for('metro.resolver.fileStore');

export type NativeFileStore = {...};
export type FileStoreHolder = {readonly native: NativeFileStore, ...};

type RustResolverConfig = {...};

type RustResolution =
  | string
  | ReadonlyArray<string>
  | null
  | {readonly error: string, readonly [string]: unknown};

type ResolverBinding = {
  ResolverConfig: Class<RustResolverConfig> & {
    new (options: {...}): RustResolverConfig,
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

// Indices passed to `callbackTrampoline`. Keep in sync with resolver.rs.
const CB_FILE_SYSTEM_LOOKUP = 0;
const CB_DOES_FILE_EXIST = 1;
const CB_GET_PACKAGE = 2;
const CB_GET_PACKAGE_FOR_MODULE = 3;
const CB_RESOLVE_HASTE_MODULE = 4;
const CB_RESOLVE_HASTE_PACKAGE = 5;
const CB_RESOLVE_ASSET = 6;
const CB_LOG_WARNING = 7;

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
    lastMetroRust = metroRust;
    lastEnvDecides = parseMetroRust(metroRust) != null;
    // $FlowFixMe[incompatible-type] The binding exports more than version().
    lastEnvBinding = getRustBinding('resolver');
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

/** Whether lookups are checked against the JS callbacks (METRO_RUST_VERIFY). */
function isVerifyEnabled(): boolean {
  const verify = process.env.METRO_RUST_VERIFY;
  return (
    verify != null &&
    verify !== '' &&
    isRustEnabled('resolver', null, {METRO_RUST: verify})
  );
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
      default:
        throw new Error(`metro-resolver: unknown Rust callback ${index}`);
    }
  } catch (error) {
    thrownError = error;
    hasThrown = true;
    return THREW;
  }
}

type ConfigEntry = {
  assetExts: ReadonlySet<string>,
  disableHierarchicalLookup: boolean,
  extraNodeModules: ResolutionContext['extraNodeModules'],
  mainFields: ReadonlyArray<string>,
  nodeModulesPaths: ReadonlyArray<string>,
  preferNativePlatform: boolean,
  sourceExts: ReadonlyArray<string>,
  unstable_conditionNames: ReadonlyArray<string>,
  unstable_conditionsByPlatform: ResolutionContext['unstable_conditionsByPlatform'],
  unstable_enablePackageExports: boolean,
  unstable_incrementalResolution: boolean,
  handle: RustResolverConfig,
};

// Config handles, keyed by the identity of the context's `sourceExts` array
// and matched on the identity of every other configuration value. Metro
// passes the same objects for every resolution.
const configHandles: WeakMap<
  ReadonlyArray<string>,
  Array<ConfigEntry>,
> = new WeakMap();

function getConfigHandle(
  binding: ResolverBinding,
  context: ResolutionContext,
): RustResolverConfig {
  let entries = configHandles.get(context.sourceExts);
  if (entries == null) {
    entries = [];
    configHandles.set(context.sourceExts, entries);
  }
  for (const entry of entries) {
    if (
      entry.assetExts === context.assetExts &&
      entry.disableHierarchicalLookup === context.disableHierarchicalLookup &&
      entry.extraNodeModules === context.extraNodeModules &&
      entry.mainFields === context.mainFields &&
      entry.nodeModulesPaths === context.nodeModulesPaths &&
      entry.preferNativePlatform === context.preferNativePlatform &&
      entry.unstable_conditionNames === context.unstable_conditionNames &&
      entry.unstable_conditionsByPlatform ===
        context.unstable_conditionsByPlatform &&
      entry.unstable_enablePackageExports ===
        context.unstable_enablePackageExports &&
      entry.unstable_incrementalResolution ===
        context.unstable_incrementalResolution
    ) {
      return entry.handle;
    }
  }
  const extraNodeModules: {[string]: string} = {};
  if (context.extraNodeModules != null) {
    for (const [name, target] of Object.entries(context.extraNodeModules)) {
      // resolve.js only follows truthy entries, joined as paths.
      if (typeof target === 'string' && target !== '') {
        extraNodeModules[name] = target;
      }
    }
  }
  const handle = new binding.ResolverConfig({
    assetExts: [...context.assetExts],
    disableHierarchicalLookup: context.disableHierarchicalLookup === true,
    extraNodeModules:
      context.extraNodeModules != null ? extraNodeModules : undefined,
    mainFields: [...context.mainFields],
    nodeModulesPaths: [...context.nodeModulesPaths],
    preferNativePlatform: context.preferNativePlatform === true,
    sourceExts: [...context.sourceExts],
    unstableConditionNames: [...context.unstable_conditionNames],
    unstableConditionsByPlatform: Object.fromEntries(
      Object.entries(context.unstable_conditionsByPlatform ?? {}).map(
        ([platform, names]) => [
          platform,
          // $FlowFixMe[incompatible-type]
          [...names],
        ],
      ),
    ),
    unstableEnablePackageExports: context.unstable_enablePackageExports === true,
    unstableIncrementalResolution:
      context.unstable_incrementalResolution === true,
  });
  entries.push({
    assetExts: context.assetExts,
    disableHierarchicalLookup: context.disableHierarchicalLookup,
    extraNodeModules: context.extraNodeModules,
    mainFields: context.mainFields,
    nodeModulesPaths: context.nodeModulesPaths,
    preferNativePlatform: context.preferNativePlatform,
    sourceExts: context.sourceExts,
    unstable_conditionNames: context.unstable_conditionNames,
    unstable_conditionsByPlatform: context.unstable_conditionsByPlatform,
    unstable_enablePackageExports: context.unstable_enablePackageExports,
    unstable_incrementalResolution: context.unstable_incrementalResolution,
    handle,
  });
  return handle;
}

function nativeStoreOf(callback: unknown): ?NativeFileStore {
  // $FlowFixMe[incompatible-use]
  const holder: ?FileStoreHolder = callback?.[FILE_STORE];
  return holder?.native;
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

function toJsError(error: {readonly error: string, readonly [string]: any}): unknown {
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
