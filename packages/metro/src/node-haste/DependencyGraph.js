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

import type {
  BundlerResolution,
  TransformResultDependency,
} from '../DeltaBundler/types';
import type {DynamicRoot} from '../lib/dynamicRoots';
import type {ResolverInputOptions} from '../shared/types';
import type {ModuleResolver} from './DependencyGraph/ModuleResolution';
import type {ConfigT} from 'metro-config';
import type {
  ChangeEvent,
  DependencyPlugin,
  FileMapRoot,
  FileSystem,
  HasteMap,
  HealthCheckResult,
  WatcherStatus,
  default as MetroFileMap,
} from 'metro-file-map';
import type {RustResolutionCache} from 'metro-resolver/private/rust';

import {registerBatchResolver} from '../DeltaBundler/batchResolve';
import {getDynamicRoots} from '../lib/dynamicRoots';
import createFileMap from './DependencyGraph/createFileMap';
import createModuleResolver from './DependencyGraph/createModuleResolver';
import RustFileStore from './DependencyGraph/RustFileStore';
import {PackageCache} from './PackageCache';
import {
  AmbiguousModuleResolutionError,
  Logger,
  PackageResolutionError,
} from 'metro-core';
import canonicalize from 'metro-core/private/canonicalize';
import {DuplicateHasteCandidatesError} from 'metro-file-map';
import RustFileSystem from 'metro-file-map/private/lib/RustFileSystem';
import {InvalidPackageError} from 'metro-resolver';
import {
  batchDependencyFlags,
  getBatchSupport,
  resolveBatchWithRust,
} from 'metro-resolver/private/rust';
import {isRustEnabled, loadRustBinding} from 'metro-rust-switch';
import EventEmitter from 'node:events';
import path from 'node:path';
import nullthrows from 'nullthrows';

const {createActionStartEntry, createActionEndEntry, log} = Logger;

const NULL_PLATFORM = Symbol();

type ResolveExtraOptions = {assumeFlatNodeModules: boolean};

/**
 * The dependencies of one module being resolved in batches, see
 * `prepareBatchResolution`.
 */
export type BatchResolution = {
  /**
   * The `filePath` of the dependencies from `start` on that could be answered
   * natively, in order. When fewer than all remaining, the dependency right
   * after them has to go through `resolveDependency`.
   */
  resolveFrom(start: number): ReadonlyArray<string>,
};

function getOrCreateMap<T>(
  map: Map<string | symbol, Map<string | symbol, T>>,
  field: string,
): Map<string | symbol, T> {
  let subMap = map.get(field);
  if (!subMap) {
    subMap = new Map();
    map.set(field, subMap);
  }
  return subMap;
}

function getOriginKey(
  originModulePath: string,
  to: string,
  extraOptions: ResolveExtraOptions,
): string {
  const isSensitiveToOriginFolder =
    // Resolution is always relative to the origin folder unless we assume a flat node_modules
    !extraOptions.assumeFlatNodeModules ||
    // Path requests are resolved relative to the origin folder
    to.includes('/') ||
    to === '.' ||
    to === '..' ||
    // Preserve standard assumptions under node_modules
    originModulePath.includes(path.sep + 'node_modules' + path.sep);
  return isSensitiveToOriginFolder ? path.dirname(originModulePath) : '';
}

export default class DependencyGraph extends EventEmitter {
  _config: ConfigT;
  _haste: MetroFileMap;
  _fileSystem: FileSystem;
  #packageCache: PackageCache;
  // The Rust replica of the file map, when the resolver runs in Rust and the
  // file map does not.
  #rustFileStore: ?RustFileStore = null;
  // The file map when it runs in Rust. With the resolver in Rust it is the
  // store itself: nothing is replicated and no change is forwarded.
  #nativeFileSystem: ?RustFileSystem = null;
  _hasteMap: HasteMap;
  #dependencyPlugin: ?DependencyPlugin;
  _moduleResolver: ModuleResolver;
  _resolutionCache: Map<
    // Custom resolver options
    string | symbol,
    Map<
      // Origin folder
      string | symbol,
      Map<
        // Dependency name
        string | symbol,
        Map<
          // Platform
          string | symbol,
          BundlerResolution,
        >,
      >,
    >,
  >;
  // With the `incremental` component and a fully native resolver, the
  // resolution cache lives here instead of in `_resolutionCache`, which then
  // only serves METRO_RUST_VERIFY (as its shadow).
  #nativeResolutionCache: ?RustResolutionCache = null;
  // The JSON key of each `resolverOptions` object, made once per object.
  #resolverOptionsKeys: WeakMap<ResolverInputOptions, string> = new WeakMap();
  #verifyBatchResolution: boolean = false;
  _initializedPromise: Promise<void>;
  #dynamicRoots: ReadonlyArray<DynamicRoot> = [];
  #dynamicRootsSource: ?ReadonlyArray<FileMapRoot> = null;

  constructor(
    config: ConfigT,
    options?: {
      readonly hasReducedPerformance?: boolean,
      readonly watch?: boolean,
    },
  ) {
    super();

    this._config = config;
    registerBatchResolver(this, {
      supports: () => this.#supportsBatchResolution(),
      prepare: (...args) => this.#prepareBatchResolution(...args),
    });

    const {hasReducedPerformance, watch} = options ?? {};
    const initializingMetroLogEntry = log(
      createActionStartEntry('Initializing Metro'),
    );

    config.reporter.update({
      type: 'dep_graph_loading',
      hasReducedPerformance: !!hasReducedPerformance,
    });
    const {fileMap, hasteMap, dependencyPlugin} = createFileMap(config, {
      throwOnModuleCollision: false,
      watch,
    });

    // We can have a lot of graphs listening to Haste for changes.
    // Bump this up to silence the max listeners EventEmitter warning.
    fileMap.setMaxListeners(1000);

    this._haste = fileMap;
    this._haste.on('status', status => this._onWatcherStatus(status));

    this._initializedPromise = fileMap.build().then(({fileSystem}) => {
      log(createActionEndEntry(initializingMetroLogEntry));
      config.reporter.update({type: 'dep_graph_loaded'});

      this._fileSystem = fileSystem;
      this._hasteMap = hasteMap;
      this.#dependencyPlugin = dependencyPlugin;

      this._haste.on('change', changeEvent => this._onHasteChange(changeEvent));
      this._haste.on('healthCheck', result =>
        this._onWatcherHealthCheck(result),
      );
      this._resolutionCache = new Map();
      this.#packageCache = new PackageCache({
        getClosestPackage: absoluteModulePath =>
          this._getClosestPackage(absoluteModulePath),
      });
      if (isRustEnabled('resolver', config.unstable_rust)) {
        if (fileSystem instanceof RustFileSystem) {
          this.#nativeFileSystem = fileSystem;
        } else {
          this.#rustFileStore = new RustFileStore(
            // The binding exports more than RustBinding's version().
            loadRustBinding() as any,
            config.projectRoot,
            fileSystem,
          );
        }
      }
      this._createModuleResolver();
    });
  }

  /**
   * Roots held by the file map in addition to `projectRoot` and
   * `watchFolders`. The same array is returned until the roots change.
   */
  getDynamicRoots(): ReadonlyArray<DynamicRoot> {
    const fileMapRoots = this._haste.getRoots();
    if (fileMapRoots !== this.#dynamicRootsSource) {
      this.#dynamicRoots = getDynamicRoots(fileMapRoots);
      this.#dynamicRootsSource = fileMapRoots;
    }
    return this.#dynamicRoots;
  }

  _onWatcherHealthCheck(result: HealthCheckResult) {
    this._config.reporter.update({type: 'watcher_health_check_result', result});
  }

  _onWatcherStatus(status: WatcherStatus) {
    this._config.reporter.update({type: 'watcher_status', status});
  }

  // Waits for the dependency graph to become ready after initialisation.
  // Don't read anything from the graph until this resolves.
  async ready(): Promise<void> {
    await this._initializedPromise;
  }

  _onHasteChange(changeEvent: ChangeEvent) {
    const {changes, rootDir} = changeEvent;
    this._resolutionCache = new Map();
    this.#nativeResolutionCache?.clear();
    // Also invalidates the store's package.json cache, as below.
    this.#rustFileStore?.applyChanges(changeEvent);
    [
      ...changes.addedFiles,
      ...changes.modifiedFiles,
      ...changes.removedFiles,
    ].forEach(([canonicalPath]) =>
      this.#packageCache.invalidate(path.join(rootDir, canonicalPath)),
    );
    this._createModuleResolver();
    this.emit('change');
  }

  _createModuleResolver() {
    const rustFileStore = this.#rustFileStore ?? this.#nativeFileSystem;
    const nativeHaste =
      this.#nativeFileSystem != null &&
      // $FlowFixMe[prop-missing] Only HastePlugin has it.
      this._hasteMap._isNative?.() === true;
    this._moduleResolver = createModuleResolver({
      config: this._config,
      fileSystem: this._fileSystem,
      hasteMap: this._hasteMap,
      packageCache: this.#packageCache,
      rustFileStore,
      nativeHaste,
    });
    if (
      this.#nativeResolutionCache == null &&
      rustFileStore != null &&
      nativeHaste &&
      // A custom resolver runs in JS for every dependency: nothing to batch.
      this._config.resolver.resolveRequest == null &&
      isRustEnabled('incremental', this._config.unstable_rust)
    ) {
      // $FlowFixMe[incompatible-use] The binding exports more than version().
      this.#nativeResolutionCache = new (
        loadRustBinding() as any
      ).ResolutionCache();
      const verify = process.env.METRO_RUST_VERIFY;
      this.#verifyBatchResolution =
        verify != null &&
        verify !== '' &&
        isRustEnabled('incremental', null, {METRO_RUST: verify});
    }
  }

  _getClosestPackage(
    absoluteModulePath: string,
  ): ?{packageJsonPath: string, packageRelativePath: string} {
    const result = this._fileSystem.hierarchicalLookup(
      absoluteModulePath,
      'package.json',
      {
        breakOnSegment: 'node_modules',
        subpathType: 'f',
      },
    );
    return result
      ? {
          packageJsonPath: result.absolutePath,
          packageRelativePath: result.containerRelativePath,
        }
      : null;
  }

  getAllFiles(): Array<string> {
    return nullthrows(this._fileSystem).getAllFiles();
  }

  /**
   * Used when watcher.unstable_lazySha1 is true
   */
  async getOrComputeSha1(
    mixedPath: string,
  ): Promise<{content?: Buffer, sha1: string}> {
    const result = await this._fileSystem.getOrComputeSha1(mixedPath);
    if (!result || !result.sha1) {
      throw new Error(`Failed to get the SHA-1 for: ${mixedPath}.
      Potential causes:
        1) The file is not watched. Ensure it is under the configured \`projectRoot\` or \`watchFolders\`.
        2) Check \`blockList\` in your metro.config.js and make sure it isn't excluding the file path.
        3) The file may have been deleted since it was resolved - try refreshing your app.
        4) Otherwise, this is a bug in Metro or the configured resolver - please report it.`);
    }
    return result;
  }

  getWatcher(): EventEmitter {
    return this._haste;
  }

  async end() {
    // A failed build is surfaced by ready(), and shouldn't prevent teardown.
    await this._initializedPromise.catch(() => {});
    await this._haste.end();
  }

  /** Given a search context, return a list of file paths matching the query. */
  matchFilesWithContext(
    from: string,
    context: Readonly<{
      /* Should search for files recursively. */
      recursive: boolean,
      /* Filter relative paths against a pattern. */
      filter: RegExp,
    }>,
  ): Iterable<string> {
    return this._fileSystem.matchFiles({
      rootDir: from,
      recursive: context.recursive,
      filter: context.filter,
      filterComparePosix: true,
      follow: true,
    });
  }

  resolveDependency(
    originModulePath: string,
    dependency: TransformResultDependency,
    platform: string | null,
    resolverOptions: ResolverInputOptions,

    // TODO: Fold assumeFlatNodeModules into resolverOptions and add to graphId
    extraOptions: {assumeFlatNodeModules: boolean} = {
      assumeFlatNodeModules: false,
    },
  ): BundlerResolution {
    const nativeCache = this.#nativeResolutionCache;
    if (nativeCache == null) {
      return this._resolveDependencyJs(
        originModulePath,
        dependency,
        platform,
        resolverOptions,
        extraOptions,
      );
    }
    const to = dependency.name;
    const optionsKey = this.#getResolverOptionsKey(resolverOptions);
    const originKey = getOriginKey(originModulePath, to, extraOptions);
    const isESMImport = dependency.data.isESMImport === true;
    const cached = nativeCache.get(
      optionsKey,
      originKey,
      to,
      isESMImport,
      platform,
    );
    if (cached != null) {
      return {type: 'sourceFile', filePath: cached};
    }
    const resolution = this.#resolveUncached(
      originModulePath,
      dependency,
      platform,
      resolverOptions,
    );
    nativeCache.set(
      optionsKey,
      originKey,
      to,
      isESMImport,
      platform,
      resolution.filePath,
    );
    return resolution;
  }

  #getResolverOptionsKey(resolverOptions: ResolverInputOptions): string {
    if (resolverOptions == null) {
      return JSON.stringify({}, canonicalize) ?? '';
    }
    let key = this.#resolverOptionsKeys.get(resolverOptions);
    if (key == null) {
      key = JSON.stringify(resolverOptions, canonicalize) ?? '';
      this.#resolverOptionsKeys.set(resolverOptions, key);
    }
    return key;
  }

  #resolveUncached(
    originModulePath: string,
    dependency: TransformResultDependency,
    platform: string | null,
    resolverOptions: ResolverInputOptions,
  ): BundlerResolution {
    try {
      return this._moduleResolver.resolveDependency(
        originModulePath,
        dependency,
        true,
        platform,
        resolverOptions,
      );
    } catch (error) {
      if (error instanceof DuplicateHasteCandidatesError) {
        throw new AmbiguousModuleResolutionError(originModulePath, error);
      }
      if (error instanceof InvalidPackageError) {
        throw new PackageResolutionError({
          packageError: error,
          originModulePath,
          targetModuleName: dependency.name,
        });
      }
      throw error;
    }
  }

  /**
   * Whether `prepareBatchResolution` can ever answer: the `incremental`
   * component is on and the resolver is fully native.
   */
  #supportsBatchResolution(): boolean {
    return this.#nativeResolutionCache != null;
  }

  /**
   * Prepares the resolution of the dependencies of one module in as few
   * native calls as possible (port/design/phase-4.md, step 3), for
   * `buildSubgraph`. `dependencies` are the ones that call `resolve`, in
   * order. Returns null when this module cannot be batched, and the caller
   * then calls `resolveDependency` for each dependency.
   *
   * What it answers is what `resolveDependency` would answer for the same
   * dependencies in the same order, from the same cache. It stops at the first
   * dependency whose resolution is not a plain source file or raises an error
   * (so that `resolveDependency` throws it, warns and records it as it does
   * today), or that a scheme resolver answers.
   */
  #prepareBatchResolution(
    originModulePath: string,
    dependencies: ReadonlyArray<TransformResultDependency>,
    platform: string | null,
    resolverOptions: ResolverInputOptions,
    extraOptions: ResolveExtraOptions = {assumeFlatNodeModules: false},
  ): ?BatchResolution {
    const cache = this.#nativeResolutionCache;
    if (cache == null || dependencies.length === 0) {
      return null;
    }
    const context = this._moduleResolver.createContext(
      originModulePath,
      dependencies[0],
      true,
      platform,
      resolverOptions,
    );
    const support = getBatchSupport(context, platform);
    if (support == null) {
      return null;
    }
    const names: Array<string> = new Array(dependencies.length);
    const depFlags = new Uint8Array(dependencies.length);
    for (let i = 0; i < dependencies.length; i++) {
      const dependency = dependencies[i];
      names[i] = dependency.name;
      depFlags[i] = batchDependencyFlags(
        context,
        dependency.name,
        dependency.data.isESMImport === true,
      );
    }
    const optionsKey = this.#getResolverOptionsKey(resolverOptions);
    const originDir = path.dirname(originModulePath);
    const underNodeModules = originModulePath.includes(
      path.sep + 'node_modules' + path.sep,
    );
    return {
      resolveFrom: (start: number) => {
        const paths = resolveBatchWithRust(
          support,
          context,
          cache,
          optionsKey,
          originDir,
          underNodeModules,
          extraOptions.assumeFlatNodeModules,
          names,
          depFlags,
          start,
          platform,
        );
        if (this.#verifyBatchResolution) {
          this.#verifyBatch(
            originModulePath,
            dependencies,
            start,
            paths,
            platform,
            resolverOptions,
            extraOptions,
          );
        }
        return paths;
      },
    };
  }

  /**
   * METRO_RUST_VERIFY=incremental: every dependency answered natively is also
   * resolved the JS way, with the JS cache as a shadow of the native one
   * (cleared at the same times), and the two must agree. Warnings of the
   * shadow resolution are dropped.
   */
  #verifyBatch(
    originModulePath: string,
    dependencies: ReadonlyArray<TransformResultDependency>,
    start: number,
    paths: ReadonlyArray<string>,
    platform: string | null,
    resolverOptions: ResolverInputOptions,
    extraOptions: ResolveExtraOptions,
  ): void {
    for (let i = 0; i < paths.length; i++) {
      const dependency = dependencies[start + i];
      let expected: string;
      try {
        expected = this._moduleResolver.withoutWarnings(
          () =>
            this._resolveDependencyJs(
              originModulePath,
              dependency,
              platform,
              resolverOptions,
              extraOptions,
            ).filePath,
        );
      } catch (error) {
        throw new Error(
          `METRO_RUST_VERIFY: batch resolution of ${JSON.stringify(dependency.name)} from ${originModulePath} (platform ${String(platform)}) answered ${paths[i]}, but resolving it in JS throws: ${error.message}`,
        );
      }
      if (expected !== paths[i]) {
        throw new Error(
          `METRO_RUST_VERIFY: batch resolution of ${JSON.stringify(dependency.name)} from ${originModulePath} (platform ${String(platform)}) differs.\n  Rust: ${paths[i]}\n  JS:   ${expected}`,
        );
      }
    }
  }

  // Today's resolution, with the JS cache (a tree of maps).
  _resolveDependencyJs(
    originModulePath: string,
    dependency: TransformResultDependency,
    platform: string | null,
    resolverOptions: ResolverInputOptions,
    extraOptions: ResolveExtraOptions,
  ): BundlerResolution {
    const to = dependency.name;

    // Compound key for the resolver cache
    const resolverOptionsKey =
      JSON.stringify(resolverOptions ?? {}, canonicalize) ?? '';
    const originKey = getOriginKey(originModulePath, to, extraOptions);
    const targetKey =
      to + (dependency.data.isESMImport === true ? '\0esm' : '\0cjs');
    const platformKey = platform ?? NULL_PLATFORM;

    // Traverse the resolver cache, which is a tree of maps
    const mapByResolverOptions = this._resolutionCache;
    const mapByOrigin = getOrCreateMap(
      mapByResolverOptions,
      resolverOptionsKey,
    );
    const mapByTarget = getOrCreateMap(mapByOrigin, originKey);
    const mapByPlatform = getOrCreateMap(mapByTarget, targetKey);
    let resolution: ?BundlerResolution = mapByPlatform.get(platformKey);

    if (!resolution) {
      resolution = this.#resolveUncached(
        originModulePath,
        dependency,
        platform,
        resolverOptions,
      );
    }

    mapByPlatform.set(platformKey, resolution);
    return resolution;
  }

  doesFileExist = (filePath: string): boolean => {
    return this._fileSystem.exists(filePath);
  };

  getHasteName(filePath: string): string {
    const hasteName = this._hasteMap.getModuleNameByPath(filePath);

    if (hasteName) {
      return hasteName;
    }

    return path.relative(this._config.projectRoot, filePath);
  }

  getDependencies(filePath: string): Array<string> {
    if (!this.#dependencyPlugin) {
      throw new Error(
        'getDependencies called but extractDependencies is false',
      );
    }
    return Array.from(
      nullthrows(this.#dependencyPlugin.getDependencies(filePath)),
    );
  }
}
