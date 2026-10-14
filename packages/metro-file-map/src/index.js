/**
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 *
 * @flow strict-local
 * @format
 * @oncall react_native
 */

import type {
  BuildParameters,
  BuildResult,
  CacheData,
  CacheManager,
  CacheManagerFactory,
  CacheManagerFactoryOptions,
  CacheManagerWriteOptions,
  CanonicalPath,
  ChangedFileMetadata,
  ChangeEvent,
  ChangeEventClock,
  ChangeEventMetadata,
  Console,
  Crawler,
  CrawlerFactory,
  CrawlerOptions,
  CrawlResult,
  FileData,
  FileMapPlugin,
  FileMapPluginInitOptions,
  FileMapPluginWorker,
  FileMapRoot,
  FileMetadata,
  FileSystem,
  HasteMapData,
  HasteMapItem,
  HType,
  InputFileMapPlugin,
  MutableFileSystem,
  Path,
  PerfLogger,
  PerfLoggerFactory,
  ProcessFileFunction,
  ReadonlyFileSystemChanges,
  V8Serializable,
  WatcherBackendChangeEvent,
  WatchmanClockSpec,
  WatchmanClocks,
} from './flow-types';

import {DiskCacheManager} from './cache/DiskCacheManager';
import {NoopCacheManager} from './cache/NoopCacheManager';
import H from './constants';
import checkWatchmanCapabilities from './lib/checkWatchmanCapabilities';
import {FileProcessor} from './lib/FileProcessor';
import {FileSystemChangeAggregator} from './lib/FileSystemChangeAggregator';
import normalizePathSeparatorsToPosix from './lib/normalizePathSeparatorsToPosix';
import normalizePathSeparatorsToSystem from './lib/normalizePathSeparatorsToSystem';
import {RootPathUtils} from './lib/RootPathUtils';
import RustFileSystem, {toCompactFiles} from './lib/RustFileSystem';
import {RustVerifier, isRustVerifyEnabled} from './lib/RustVerifier';
import TreeFS from './lib/TreeFS';
import DependencyPlugin from './plugins/DependencyPlugin';
import HastePlugin from './plugins/HastePlugin';
import {Watcher} from './Watcher';
import debugModule from 'debug';
import invariant from 'invariant';
import EventEmitter from 'node:events';
import {promises as fsPromises} from 'node:fs';
import * as path from 'node:path';
import {performance} from 'node:perf_hooks';
import {isRustEnabled, loadRustBinding} from 'metro-rust-switch';
import nullthrows from 'nullthrows';

const debug = debugModule('Metro:FileMap');

export type {
  BuildParameters,
  BuildResult,
  CacheData,
  ChangeEventMetadata,
  FileData,
  FileMap,
  FileSystem,
  HasteMapData,
  HasteMapItem,
  InputFileMapPlugin,
};

export type InputOptions = Readonly<{
  computeSha1?: ?boolean,
  enableSymlinks?: ?boolean,
  extensions: ReadonlyArray<string>,
  ignorePattern?: ?RegExp,
  plugins?: ReadonlyArray<InputFileMapPlugin>,
  retainAllFiles: boolean,
  rootDir: string,
  roots: ReadonlyArray<string>,

  cacheManagerFactory?: ?CacheManagerFactory,
  console?: Console,
  /**
   * Replaces the built-in Watchman/node crawlers. Watch mode, if enabled, still
   * uses the built-in watcher backends.
   */
  crawlerFactory?: ?CrawlerFactory,
  healthCheck: HealthCheckOptions,
  maxFilesPerWorker?: ?number,
  maxWorkers: number,
  perfLoggerFactory?: ?PerfLoggerFactory,
  resetCache?: ?boolean,
  useWatchman?: ?boolean,
  watch?: ?boolean,
  watchmanDeferStates?: ReadonlyArray<string>,
  /**
   * `unstable_rust` of Metro's config: `{'file-map': true}` runs this file map
   * on the Rust core when it can (see `getRustBinding`). METRO_RUST decides
   * when it is set.
   */
  unstable_rust?: ?Readonly<{[componentId: string]: boolean}>,
}>;

type HealthCheckOptions = Readonly<{
  enabled: boolean,
  interval: number,
  timeout: number,
  filePrefix: string,
}>;

type InternalOptions = Readonly<{
  ...BuildParameters,
  healthCheck: HealthCheckOptions,
  perfLoggerFactory: ?PerfLoggerFactory,
  resetCache: ?boolean,
  useWatchman: boolean,
  watch: boolean,
  watchmanDeferStates: ReadonlyArray<string>,
}>;

type IndexedPlugin = Readonly<{
  // $FlowFixMe[unclear-type] Plugin types cannot be known statically
  plugin: FileMapPlugin<any, any>,
  dataIdx: ?number,
}>;
type InternalEnqueuedEvent = Readonly<
  | {
      clock: ?ChangeEventClock,
      relativeFilePath: string,
      // With the Rust core, only the mtime of this is read (to drop
      // duplicates): the file's metadata is held by the native file map,
      // under `ticket`.
      metadata: FileMetadata,
      type: 'touch',
      ticket?: number,
      // METRO_RUST_VERIFY: the metadata the JS implementation made.
      verifyMetadata?: FileMetadata,
    }
  | {
      clock: ?ChangeEventClock,
      relativeFilePath: string,
      type: 'delete',
    },
>;

export {DiskCacheManager} from './cache/DiskCacheManager';
export {NoopCacheManager} from './cache/NoopCacheManager';
export {default as DependencyPlugin} from './plugins/DependencyPlugin';
export {default as FileDataPlugin} from './plugins/FileDataPlugin';
export type {FileDataPluginOptions} from './plugins/FileDataPlugin';
export type {DependencyPluginOptions} from './plugins/DependencyPlugin';
export {DuplicateHasteCandidatesError} from './plugins/haste/DuplicateHasteCandidatesError';
export {HasteConflictsError} from './plugins/haste/HasteConflictsError';
export {default as HastePlugin} from './plugins/HastePlugin';

export type {HasteMap} from './flow-types';
export type {HealthCheckResult} from './Watcher';
export type {
  CacheManager,
  CacheManagerFactory,
  CacheManagerFactoryOptions,
  CacheManagerWriteOptions,
  ChangeEvent,
  Crawler,
  CrawlerFactory,
  CrawlerFactoryOptions,
  CrawlerOptions,
  CrawlResult,
  DependencyExtractor,
  FileMapRoot,
  WatcherStatus,
} from './flow-types';

// The Rust core keeps its own cache file next to DiskCacheManager's, so that a
// run with Rust off never reads it and the other way round. The version is the
// native format's (FORMAT_VERSION in metro-rs/crates/file-map/src/cache.rs).
const RUST_CACHE_SUFFIX = '.rs-v1';

// This should be bumped whenever a code change to `metro-file-map` itself
// would cause a change to the cache data structure and/or content (for a given
// filesystem state and build parameters).
const CACHE_BREAKER = '13';

const CHANGE_INTERVAL = 30;

const NODE_MODULES = path.sep + 'node_modules' + path.sep;
const VCS_DIRECTORIES = /[/\\]\.(git|hg)[/\\]/.source;
const WATCHMAN_REQUIRED_CAPABILITIES = [
  'field-content.sha1hex',
  'relative_root',
  'suffix-set',
  'wildmatch',
];

// The binding's `NativeFileMap` (metro-rs/crates/binding/src/file_map.rs).
// $FlowFixMe[unclear-type]
type NativeFileMapApi = any;

// The plugins' and the cache manager's methods for the Rust core are named with
// a leading underscore, which makes them private to Flow, and keeps them out
// of the public types.
// $FlowFixMe[unclear-type]
type Internal = any;

type RustBinding = {
  NativeFileMap: Class<NativeFileMapApi>,
  fileMapSupportsRegexp: (source: string, flags: string) => boolean,
  ...
};

type RustChangeList = {
  paths: Array<string>,
  symlinks: Uint8Array,
  mtimes: Float64Array,
  data: Array<?string>,
};
type RustChangeView = {
  addedDirectories: Array<string>,
  removedDirectories: Array<string>,
  added: RustChangeList,
  modified: RustChangeList,
  removed: RustChangeList,
};

/** What the native file map keeps in its cache besides the files. */
type RustCache = {
  clocks: Map<string, WatchmanClockSpec>,
  pluginStates: Map<string, void | V8Serializable>,
};

/**
 * What FileMap holds when it runs on the Rust core.
 */
type RustMode = Readonly<{
  native: NativeFileMapApi,
  fileSystem: RustFileSystem,
  // The bound HastePlugin, which the native file map keeps up to date itself.
  hastePlugin: ?HastePlugin,
  // The other plugins (DependencyPlugin), which FileMap updates from the
  // native change views. `position` is where their data is in a view.
  jsPlugins: ReadonlyArray<
    Readonly<{plugin: InputFileMapPlugin, dataIdx: number, position: number}>,
  >,
  verifier: ?RustVerifier,
}>;

/**
 * The Rust binding if this FileMap should run on the Rust core, else null. The
 * Rust core replaces the whole data path of FileMap: crawl, `TreeFS`, Haste and
 * the cache file. So it is used only if all of these hold, and otherwise
 * FileMap is the JS implementation, unchanged:
 *
 * - the switch is on (METRO_RUST, or `unstable_rust` in the options);
 * - there is no `crawlerFactory`;
 * - the cache manager is exactly a `DiskCacheManager` or a `NoopCacheManager`
 *   (the Rust core writes its own cache file through the former; any other
 *   cache manager expects a JS snapshot of the data);
 * - every plugin is Metro's own `HastePlugin` or `DependencyPlugin` (the Rust
 *   core models their worker filters and keeps Haste natively), at most one of
 *   each, with a Haste `rootDir` that is the file map's;
 * - the ignore pattern compiles in Rust (not with the `g` or `y` flag, whose
 *   `test` is stateful).
 */
function getRustBinding(
  options: InputOptions,
  ignorePattern: RegExp,
  plugins: ReadonlyArray<InputFileMapPlugin>,
  cacheManager: CacheManager,
): ?RustBinding {
  if (!isRustEnabled('file-map', options.unstable_rust)) {
    return null;
  }
  if (options.crawlerFactory != null) {
    return null;
  }
  if (!(
    (cacheManager instanceof DiskCacheManager &&
      cacheManager.constructor === DiskCacheManager) ||
    (cacheManager instanceof NoopCacheManager &&
      cacheManager.constructor === NoopCacheManager)
  )) {
    return null;
  }
  let hasteCount = 0;
  let dependencyCount = 0;
  for (const plugin of plugins) {
    if (plugin instanceof HastePlugin && plugin.constructor === HastePlugin) {
      hasteCount++;
      if ((plugin as Internal)._getRustOptions().rootDir !== options.rootDir) {
        return null;
      }
    } else if (
      plugin instanceof DependencyPlugin &&
      plugin.constructor === DependencyPlugin
    ) {
      dependencyCount++;
    } else {
      return null;
    }
  }
  if (hasteCount > 1 || dependencyCount > 1) {
    return null;
  }
  // Loading the binding throws if it is missing: never a silent fallback.
  // $FlowFixMe[incompatible-type] The binding exports more than version().
  const binding: RustBinding = loadRustBinding();
  return binding.fileMapSupportsRegexp(
    ignorePattern.source,
    ignorePattern.flags,
  )
    ? binding
    : null;
}

/**
 * FileMap includes a JavaScript implementation of Facebook's haste module system.
 *
 * This implementation is inspired by https://github.com/facebook/node-haste
 * and was built with for high-performance in large code repositories with
 * hundreds of thousands of files. This implementation is scalable and provides
 * predictable performance.
 *
 * Because the file map creation and synchronization is critical to startup
 * performance and most tasks are blocked by I/O this class makes heavy use of
 * synchronous operations. It uses worker processes for parallelizing file
 * access and metadata extraction.
 *
 * The data structures created by `metro-file-map` can be used directly from the
 * cache without further processing. The metadata objects in the `files` and
 * `map` objects contain cross-references: a metadata object from one can look
 * up the corresponding metadata object in the other map. Note that in most
 * projects, the number of files will be greater than the number of haste
 * modules one module can refer to many files based on platform extensions.
 *
 * type CacheData = {
 *   clocks: WatchmanClocks,
 *   files: {[filepath: string]: FileMetadata},
 *   map: {[id: string]: HasteMapItem},
 *   mocks: {[id: string]: string},
 * }
 *
 * // Watchman clocks are used for query synchronization and file system deltas.
 * type WatchmanClocks = {[filepath: string]: string};
 *
 * type FileMetadata = {
 *   id: ?string, // used to look up module metadata objects in `map`.
 *   mtime: number, // check for outdated files.
 *   size: number, // size of the file in bytes.
 *   visited: boolean, // whether the file has been parsed or not.
 *   dependencies: Array<string>, // all relative dependencies of this file.
 *   sha1: ?string, // SHA-1 of the file, if requested via options.
 *   symlink: ?(1 | 0 | string), // Truthy if symlink, string is the target,
 *                               // lexically resolved to a normal POSIX path
 * };
 *
 * // Modules can be targeted to a specific platform based on the file name.
 * // Example: platform.ios.js and Platform.android.js will both map to the same
 * // `Platform` module. The platform should be specified during resolution.
 * type HasteMapItem = {[platform: string]: ModuleMetadata};
 *
 * //
 * type ModuleMetadata = {
 *   path: string, // the path to look up the file object in `files`.
 *   type: string, // the module type (either `package` or `module`).
 * };
 *
 * Note that the data structures described above are conceptual only. The actual
 * implementation uses arrays and constant keys for metadata storage. Instead of
 * `{id: 'flatMap', mtime: 3421, size: 42, visited: true, dependencies: []}` the real
 * representation is similar to `['flatMap', 3421, 42, 1, []]` to save storage space
 * and reduce parse and write time of a big JSON blob.
 *
 * The FileMap is created as follows:
 *  1. read data from the cache or create an empty structure.
 *
 *  2. crawl the file system.
 *     * empty cache: crawl the entire file system.
 *     * cache available:
 *       * if watchman is available: get file system delta changes.
 *       * if watchman is unavailable: crawl the entire file system.
 *     * build metadata objects for every file. This builds the `files` part of
 *       the `FileMap`.
 *
 *  3. visit and extract metadata from changed files, including sha1,
 *     depedendencies, and any plugins.
 *     * this is done in parallel over worker processes to improve performance.
 *     * the worst case is to visit all files.
 *     * the best case is no file system access and retrieving all data from
 *       the cache.
 *     * the average case is a small number of changed files.
 *
 *  4. serialize the new `FileMap` in a cache file.
 *
 */
export default class FileMap extends EventEmitter {
  #buildPromise: ?Promise<BuildResult>;
  readonly #cacheManager: CacheManager;
  #canUseWatchmanPromise: Promise<boolean>;
  #changeID: number;
  #changeInterval: ?ReturnType<typeof setInterval>;
  readonly #console: Console;
  readonly #crawlerAbortController: AbortController;
  readonly #fileProcessor: FileProcessor;
  #healthCheckInterval: ?ReturnType<typeof setInterval>;
  readonly #options: InternalOptions;
  #roots: ReadonlyArray<FileMapRoot>;
  readonly #pathUtils: RootPathUtils;
  readonly #crawler: ?Crawler;
  readonly #plugins: ReadonlyArray<IndexedPlugin>;
  readonly #rust: ?RustMode;
  readonly #startupPerfLogger: ?PerfLogger;
  #watcher: ?Watcher;

  static create(options: InputOptions): FileMap {
    return new FileMap(options);
  }

  constructor(options: InputOptions) {
    super();

    if (options.perfLoggerFactory) {
      this.#startupPerfLogger =
        options.perfLoggerFactory?.('START_UP').subSpan('fileMap') ?? null;
      this.#startupPerfLogger?.point('constructor_start');
    }

    // Add VCS_DIRECTORIES to provided ignorePattern
    let ignorePattern;
    if (options.ignorePattern) {
      const inputIgnorePattern = options.ignorePattern;
      if (inputIgnorePattern instanceof RegExp) {
        ignorePattern = new RegExp(
          inputIgnorePattern.source.concat('|' + VCS_DIRECTORIES),
          inputIgnorePattern.flags,
        );
      } else {
        throw new Error(
          'metro-file-map: the `ignorePattern` option must be a RegExp',
        );
      }
    } else {
      ignorePattern = new RegExp(VCS_DIRECTORIES);
    }

    this.#console = options.console || global.console;

    let dataSlot: number = H.PLUGINDATA;

    const indexedPlugins: Array<IndexedPlugin> = [];
    const pluginWorkers: Array<FileMapPluginWorker> = [];
    const pluginDataIndices = new Map<string, number>();
    const plugins = options.plugins ?? [];
    for (const plugin of plugins) {
      const maybeWorker = plugin.getWorker();
      const dataIdx = maybeWorker != null ? dataSlot++ : null;
      indexedPlugins.push({plugin, dataIdx});
      if (dataIdx != null) {
        // Crawlers address plugin data by name, so names must be unique among
        // plugins holding a slot - otherwise a crawler would silently write to
        // the shadowed plugin's slot.
        invariant(
          !pluginDataIndices.has(plugin.name),
          'metro-file-map: Duplicate plugin name: %s',
          plugin.name,
        );
        pluginDataIndices.set(plugin.name, dataIdx);
      }
      if (maybeWorker != null) {
        pluginWorkers.push(maybeWorker);
      }
    }
    this.#plugins = indexedPlugins;

    const buildParameters: BuildParameters = {
      cacheBreaker: CACHE_BREAKER,
      computeSha1: options.computeSha1 || false,
      enableSymlinks: options.enableSymlinks || false,
      extensions: options.extensions,
      ignorePattern,
      plugins,
      retainAllFiles: options.retainAllFiles,
      rootDir: options.rootDir,
      roots: Array.from(new Set(options.roots)),
    };

    this.#options = {
      ...buildParameters,
      healthCheck: options.healthCheck,
      perfLoggerFactory: options.perfLoggerFactory,
      resetCache: options.resetCache,
      useWatchman: options.useWatchman == null ? true : options.useWatchman,
      watch: !!options.watch,
      watchmanDeferStates: options.watchmanDeferStates ?? [],
    };

    this.#crawler = options.crawlerFactory
      ? options.crawlerFactory.call(null, {buildParameters, pluginDataIndices})
      : null;

    const cacheFactoryOptions: CacheManagerFactoryOptions = {
      buildParameters,
    };
    this.#cacheManager = options.cacheManagerFactory
      ? options.cacheManagerFactory.call(null, cacheFactoryOptions)
      : new DiskCacheManager(cacheFactoryOptions, {});

    this.#fileProcessor = new FileProcessor({
      maxFilesPerWorker: options.maxFilesPerWorker,
      maxWorkers: options.maxWorkers,
      perfLogger: this.#startupPerfLogger,
      pluginWorkers,
      rootDir: options.rootDir,
    });

    this.#rust = this.#createRustMode(
      options,
      ignorePattern,
      indexedPlugins,
      plugins,
    );

    this.#buildPromise = null;
    this.#pathUtils = new RootPathUtils(options.rootDir);
    this.#roots = this.#options.roots.map(absolutePath => ({
      absolutePath,
      rootRelativePath: this.#pathUtils.absoluteToNormal(absolutePath),
      dynamic: false,
    }));
    this.#startupPerfLogger?.point('constructor_end');
    this.#crawlerAbortController = new AbortController();
    this.#changeID = 0;
  }

  /**
   * Sets up the Rust core, if `getRustBinding` allows it: the native file
   * map, a `RustFileSystem` over it, and the HastePlugin bound to it.
   */
  #createRustMode(
    options: InputOptions,
    ignorePattern: RegExp,
    indexedPlugins: ReadonlyArray<IndexedPlugin>,
    plugins: ReadonlyArray<InputFileMapPlugin>,
  ): ?RustMode {
    const binding = getRustBinding(
      options,
      ignorePattern,
      plugins,
      this.#cacheManager,
    );
    if (binding == null) {
      return null;
    }
    const verify = isRustVerifyEnabled();
    const pluginFilters = [];
    let haste = null;
    let hastePlugin: ?HastePlugin = null;
    const jsPlugins: Array<{
      plugin: InputFileMapPlugin,
      dataIdx: number,
      position: number,
    }> = [];
    for (const {plugin, dataIdx} of indexedPlugins) {
      if (dataIdx == null) {
        continue;
      }
      if (plugin instanceof HastePlugin) {
        const hasteOptions = (plugin as Internal)._getRustOptions();
        pluginFilters.push({
          lazy: false,
          nodeModules: false,
          other: hasteOptions.hasteImplModulePath != null,
          packageJson: hasteOptions.enableHastePackages,
          excludedExtensions: [],
        });
        haste = {
          slot: dataIdx - H.PLUGINDATA,
          enableHastePackages: hasteOptions.enableHastePackages,
          platforms: [...hasteOptions.platforms],
          failValidationOnConflicts: hasteOptions.failValidationOnConflicts,
        };
        hastePlugin = plugin;
      } else if (plugin instanceof DependencyPlugin) {
        const filter = (plugin as Internal)._getRustFilter();
        pluginFilters.push({
          lazy: false,
          nodeModules: false,
          other: filter.other,
          packageJson: undefined,
          excludedExtensions: [...filter.excludedExtensions],
        });
        jsPlugins.push({plugin, dataIdx, position: jsPlugins.length});
      }
    }
    const native = new binding.NativeFileMap({
      rootDir: options.rootDir,
      extensions: [...options.extensions],
      computeSha1: options.computeSha1 || false,
      includeSymlinks: options.enableSymlinks || false,
      retainAllFiles: options.retainAllFiles,
      ignoreSource: ignorePattern.source,
      ignoreFlags: ignorePattern.flags,
      healthCheckFilePrefix: options.healthCheck.filePrefix,
      plugins: pluginFilters,
      haste: haste ?? undefined,
      keepCrawlInput: verify,
    });
    const verifier = verify
      ? new RustVerifier({
          applyFileDelta: (fileSystem, filePlugins, delta) =>
            this.#applyFileDelta(fileSystem, filePlugins, delta),
          console: this.#console,
          fileProcessor: this.#fileProcessor,
          hastePlugin,
          native,
          plugins: indexedPlugins,
          rootDir: options.rootDir,
        })
      : null;
    (hastePlugin as Internal)?._bindToNative(
      native,
      (warnings: ReadonlyArray<string>) =>
        verifier?.recordNativeWarnings(warnings),
    );
    const fileSystem = new RustFileSystem(native, {
      onMetadata: () => {
        // Inform caches that there is new data to save.
        this.emit('metadata');
      },
      pluginCount: pluginFilters.length,
      rootDir: options.rootDir,
      verifier,
    });
    return {native, fileSystem, hastePlugin, jsPlugins, verifier};
  }

  build(): Promise<BuildResult> {
    this.#startupPerfLogger?.point('build_start');
    if (!this.#buildPromise) {
      this.#buildPromise = (async () => {
        const rust = this.#rust;
        if (rust != null) {
          return this.#buildRust(rust);
        }
        let initialData: ?CacheData;
        if (this.#options.resetCache !== true) {
          initialData = await this.read();
        }
        if (!initialData) {
          debug('Not using a cache');
        } else {
          debug('Cache loaded (%d clock(s))', initialData.clocks.size);
        }

        const rootDir = this.#options.rootDir;
        this.#startupPerfLogger?.point('constructFileSystem_start');
        const processFile: ProcessFileFunction = (
          normalPath,
          metadata,
          opts,
        ) => {
          const result = this.#fileProcessor.processRegularFile(
            normalPath,
            metadata,
            {
              computeSha1: opts.computeSha1,
              maybeReturnContent: true,
            },
          );
          debug('Lazily processed file: %s', normalPath);
          // Emit an event to inform caches that there is new data to save.
          this.emit('metadata');
          return result?.content;
        };
        const fileSystem =
          initialData != null
            ? TreeFS.fromDeserializedSnapshot({
                // Typed `mixed` because we've read this from an external
                // source. It'd be too expensive to validate at runtime, so
                // trust our cache manager that this is correct.
                // $FlowFixMe[incompatible-type]
                fileSystemData: initialData.fileSystemData,
                processFile,
                rootDir,
              })
            : new TreeFS({processFile, rootDir});
        this.#startupPerfLogger?.point('constructFileSystem_end');

        const plugins = this.#plugins;

        // Initialize plugins from cached file system and plugin state while
        // crawling to build a diff of current state vs cached. `fileSystem`
        // is not mutated during either operation.
        const [fileDelta] = await Promise.all([
          this.#buildFileDelta({
            clocks: initialData?.clocks ?? new Map(),
            fileSystem,
          }),
          Promise.all(
            plugins.map(({plugin, dataIdx}) =>
              plugin.initialize({
                files: {
                  lookup: mixedPath => {
                    const result = fileSystem.lookup(mixedPath);
                    if (!result.exists) {
                      return {exists: false};
                    }
                    if (result.type === 'd') {
                      return {exists: true, type: 'd'};
                    }
                    return {
                      exists: true,
                      type: 'f',
                      pluginData:
                        dataIdx != null ? result.metadata[dataIdx] : null,
                    };
                  },
                  fileIterator: opts =>
                    mapIterable(
                      fileSystem.metadataIterator(opts),
                      ({baseName, canonicalPath, metadata}) => ({
                        baseName,
                        canonicalPath,
                        pluginData: dataIdx != null ? metadata[dataIdx] : null,
                      }),
                    ),
                },
                pluginState: initialData?.plugins.get(plugin.name),
                processFile: mixedPath => {
                  invariant(
                    dataIdx != null,
                    'metro-file-map: Plugin "%s" has no worker to process files with',
                    plugin.name,
                  );
                  const result = fileSystem.lookup(mixedPath);
                  if (!result.exists || result.type !== 'f') {
                    throw new Error(
                      `metro-file-map: Cannot process ${mixedPath}, which is not a regular file`,
                    );
                  }
                  const pluginData = this.#fileProcessor.processFileForPlugin(
                    result.realPath,
                    result.metadata,
                    dataIdx - H.PLUGINDATA,
                  );
                  debug(
                    'Lazily processed file for %s: %s',
                    plugin.name,
                    mixedPath,
                  );
                  // Inform caches that there is new data to save.
                  this.emit('metadata');
                  return pluginData;
                },
              }),
            ),
          ),
        ]);

        // Update `fileSystem` and plugins based on the file delta.
        const actualChanges = await this.#applyFileDelta(
          fileSystem,
          plugins,
          fileDelta,
        );

        const changeSize = actualChanges.getSize();

        // Validate plugins before persisting them.
        plugins.forEach(({plugin}) => plugin.assertValid());

        const watchmanClocks = new Map(fileDelta.clocks ?? []);
        await this.#takeSnapshotAndPersist(
          fileSystem,
          watchmanClocks,
          plugins,
          changeSize > 0,
        );
        debug('Finished mapping files (%d changes).', changeSize);

        await this.#watch(fileSystem, watchmanClocks, plugins);
        return {fileSystem};
      })();
    }
    return this.#buildPromise.then(result => {
      this.#startupPerfLogger?.point('build_end');
      return result;
    });
  }

  /**
   * The directories whose files this file map holds. The same array is
   * returned until the roots change.
   */
  getRoots(): ReadonlyArray<FileMapRoot> {
    return this.#roots;
  }

  /**
   * 1. read data from the cache or create an empty structure.
   */
  async read(): Promise<?CacheData> {
    let data: ?CacheData;
    this.#startupPerfLogger?.point('read_start');
    try {
      data = await this.#cacheManager.read();
    } catch (e) {
      this.#console.warn(
        'Error while reading cache, falling back to a full crawl:\n',
        e,
      );
      this.#startupPerfLogger?.annotate({
        string: {cacheReadError: e.toString()},
      });
    }
    this.#startupPerfLogger?.point('read_end');
    return data;
  }

  /**
   * 2. crawl the file system.
   */
  async #buildFileDelta(
    previousState: CrawlerOptions['previousState'],
  ): Promise<CrawlResult> {
    this.#startupPerfLogger?.point('buildFileDelta_start');
    const {watcher} = await this.#createWatcher(previousState);
    const result = await watcher.crawl();
    this.#startupPerfLogger?.point('buildFileDelta_end');
    return result;
  }

  async #createWatcher(
    previousState: CrawlerOptions['previousState'],
  ): Promise<{watcher: Watcher, useWatchman: boolean}> {
    const {
      computeSha1,
      enableSymlinks,
      extensions,
      ignorePattern,
      retainAllFiles,
      roots,
      rootDir,
      watch,
      watchmanDeferStates,
    } = this.#options;

    const useWatchman =
      this.#crawler == null && (await this.#shouldUseWatchman());
    this.#watcher = new Watcher({
      abortSignal: this.#crawlerAbortController.signal,
      computeSha1,
      console: this.#console,
      crawl: this.#crawler,
      enableSymlinks,
      extensions,
      healthCheckFilePrefix: this.#options.healthCheck.filePrefix,
      // TODO: Refactor out the two different ignore strategies here.
      ignoreForCrawl: filePath => {
        const ignoreMatched = ignorePattern.test(filePath);
        return (
          ignoreMatched || (!retainAllFiles && filePath.includes(NODE_MODULES))
        );
      },
      ignorePatternForWatch: ignorePattern,
      perfLogger: this.#startupPerfLogger,
      previousState,
      rootDir,
      roots,
      // A supplied crawler makes the Watchman capability probe - which spawns
      // watchman - pointless.
      useWatchman,
      watch,
      watchmanDeferStates,
    });
    const watcher = this.#watcher;

    watcher.on('status', status => this.emit('status', status));

    return {watcher, useWatchman};
  }

  #maybeReadLink(normalPath: Path, fileMetadata: FileMetadata): ?Promise<void> {
    // If we only need to read a link, it's more efficient to do it in-band
    // (with async file IO) than to have the overhead of worker IO.
    if (fileMetadata[H.SYMLINK] === 1) {
      return fsPromises
        .readlink(this.#pathUtils.normalToAbsolute(normalPath))
        .then(symlinkTarget => {
          fileMetadata[H.VISITED] = 1;
          fileMetadata[H.SYMLINK] = normalizePathSeparatorsToPosix(
            this.#pathUtils.resolveSymlinkToNormal(normalPath, symlinkTarget),
          );
        });
    }
    return null;
  }

  /** What `onChange` does for a touched file, in JS. */
  async #processTouchJs(
    relativeFilePath: string,
    metadata: ChangeEventMetadata,
    fileMetadata: FileMetadata,
  ): Promise<void> {
    if (metadata.type === 'l') {
      await this.#maybeReadLink(relativeFilePath, fileMetadata);
    } else {
      await this.#fileProcessor.processRegularFile(
        relativeFilePath,
        fileMetadata,
        {
          computeSha1: this.#options.computeSha1,
          maybeReturnContent: false,
        },
      );
    }
  }

  async #applyFileDelta(
    fileSystem: MutableFileSystem,
    plugins: ReadonlyArray<IndexedPlugin>,
    delta: Readonly<{
      changedFiles: FileData,
      removedFiles: ReadonlySet<CanonicalPath>,
      clocks?: WatchmanClocks,
    }>,
  ): Promise<FileSystemChangeAggregator> {
    this.#startupPerfLogger?.point('applyFileDelta_start');
    const {changedFiles, removedFiles} = delta;
    this.#startupPerfLogger?.point('applyFileDelta_preprocess_start');
    // Remove files first so that we don't mistake moved modules
    // modules as duplicates.
    this.#startupPerfLogger?.point('applyFileDelta_remove_start');
    const changeAggregator = new FileSystemChangeAggregator();
    for (const relativeFilePath of removedFiles) {
      fileSystem.remove(relativeFilePath, changeAggregator);
    }
    this.#startupPerfLogger?.point('applyFileDelta_remove_end');

    const readLinkPromises: Array<Promise<void>> = [];
    const readLinkErrors: Array<{
      normalFilePath: string,
      error: Error & {code?: string},
    }> = [];
    const filesToProcess: Array<[string, FileMetadata]> = [];

    for (const [normalFilePath, fileData] of changedFiles) {
      // A crawler may preserve the H.VISITED flag to indicate that the file
      // contents are unchanged and it doesn't need visiting again.
      if (fileData[H.VISITED] === 1) {
        continue;
      }

      if (fileData[H.SYMLINK] === 0) {
        filesToProcess.push([normalFilePath, fileData]);
      } else {
        const maybeReadLink = this.#maybeReadLink(normalFilePath, fileData);
        if (maybeReadLink) {
          readLinkPromises.push(
            maybeReadLink.catch(error => {
              readLinkErrors.push({normalFilePath, error});
            }),
          );
        }
      }
    }
    this.#startupPerfLogger?.point('applyFileDelta_preprocess_end');

    debug(
      'Found %d added/modified files and %d symlinks.',
      filesToProcess.length,
      readLinkPromises.length,
    );

    this.#startupPerfLogger?.point('applyFileDelta_process_start');
    const [batchResult] = await Promise.all([
      this.#fileProcessor.processBatch(filesToProcess, {
        computeSha1: this.#options.computeSha1,
        maybeReturnContent: false,
      }),
      Promise.all(readLinkPromises),
    ]);
    this.#startupPerfLogger?.point('applyFileDelta_process_end');

    // It's possible that a file could be deleted between being seen by the
    // crawler and our attempt to process it. For our purposes, this is
    // equivalent to the file being deleted before the crawl, being absent
    // from `changedFiles`, and (if we loaded from cache, and the file
    // existed previously) possibly being reported in `removedFiles`.
    //
    // Treat the file accordingly - don't add it to `FileSystem`, and remove
    // it if it already exists. We're not emitting events at this point in
    // startup, so there's nothing more to do.
    this.#startupPerfLogger?.point('applyFileDelta_missing_start');
    for (const {normalFilePath, error} of batchResult.errors.concat(
      readLinkErrors,
    )) {
      /* $FlowFixMe[incompatible-type] Error exposed after improved typing of
       * Array.{includes,indexOf,lastIndexOf} */
      if (['ENOENT', 'EACCESS'].includes(error.code)) {
        delta.changedFiles.delete(normalFilePath);
        fileSystem.remove(normalFilePath, changeAggregator);
      } else {
        // Anything else is fatal.
        throw error;
      }
    }

    this.#startupPerfLogger?.point('applyFileDelta_missing_end');

    this.#startupPerfLogger?.point('applyFileDelta_add_start');
    fileSystem.bulkAddOrModify(changedFiles, changeAggregator);
    this.#startupPerfLogger?.point('applyFileDelta_add_end');

    this.#startupPerfLogger?.point('applyFileDelta_updatePlugins_start');
    plugins.forEach(({plugin, dataIdx}) => {
      plugin.onChanged(
        changeAggregator.getMappedView(
          dataIdx != null ? metadata => metadata[dataIdx] : () => null,
        ),
      );
    });
    this.#startupPerfLogger?.point('applyFileDelta_updatePlugins_end');
    this.#startupPerfLogger?.point('applyFileDelta_end');

    return changeAggregator;
  }

  /**
   * 4. Serialize a snapshot of our raw data via the configured cache manager
   */
  async #takeSnapshotAndPersist(
    fileSystem: FileSystem,
    clocks: WatchmanClocks,
    plugins: ReadonlyArray<IndexedPlugin>,
    changedSinceCacheRead: boolean,
  ) {
    this.#startupPerfLogger?.point('persist_start');
    await this.#cacheManager.write(
      () => ({
        clocks: new Map(clocks),
        fileSystemData: fileSystem.getSerializableSnapshot(),
        plugins: new Map(
          plugins.map(({plugin}) => [
            plugin.name,
            plugin.getSerializableSnapshot(),
          ]),
        ),
      }),
      this.#getCacheWriteOptions(changedSinceCacheRead),
    );
    this.#startupPerfLogger?.point('persist_end');
  }

  #getCacheWriteOptions(
    changedSinceCacheRead: boolean,
  ): CacheManagerWriteOptions {
    return {
      changedSinceCacheRead,
      eventSource: {
        onChange: cb => {
          // Inform the cache about changes to internal state, including:
          //  - File system changes
          this.on('change', cb);
          //  - Changes to stored metadata, e.g. on lazy processing.
          this.on('metadata', cb);
          return () => {
            this.removeListener('change', cb);
            this.removeListener('metadata', cb);
          };
        },
      },
      onWriteError: error => {
        this.#console.warn('[metro-file-map] Cache write error\n:', error);
      },
    };
  }

  // ---------------------------------------------------------------------
  // The Rust core. `build()`, `#watch()` and `end()` keep their structure; the
  // data work (the node crawl, hashing, the file system, Haste and the cache
  // file) is the native file map's, in one call per unit of work. What stays
  // here: the Watcher and its Watchman crawl, plugin workers (user code),
  // event queueing, perf logger points and events. See
  // port/design/phase-3.md.
  // ---------------------------------------------------------------------

  async #buildRust(rust: RustMode): Promise<BuildResult> {
    const {fileSystem, verifier} = rust;
    let cache: ?RustCache = null;
    if (this.#options.resetCache !== true) {
      cache = await this.#readRust(rust);
    }
    if (!cache) {
      debug('Not using a cache');
    } else {
      debug('Cache loaded (%d clock(s))', cache.clocks.size);
    }

    // The native file map was made empty, or loaded from the cache above.
    this.#startupPerfLogger?.point('constructFileSystem_start');
    this.#startupPerfLogger?.point('constructFileSystem_end');
    await verifier?.start();

    const plugins = this.#plugins;

    // Initialize plugins from the cached file system and plugin state while
    // crawling to build a diff of current state vs cached. The native file map
    // is not mutated during either operation.
    const [fileDelta] = await Promise.all([
      this.#buildFileDeltaRust(rust, cache?.clocks ?? new Map()),
      Promise.all(
        plugins.map(({plugin, dataIdx}) =>
          plugin.initialize(
            this.#getRustInitOptions(
              rust,
              plugin,
              dataIdx,
              cache?.pluginStates.get(plugin.name),
            ),
          ),
        ),
      ),
    ]);
    verifier?.checkInitialized();

    // Update the native file map and plugins based on the file delta.
    const changeSize = await this.#applyFileDeltaRust(rust);

    // Validate plugins before persisting them.
    plugins.forEach(({plugin}) => plugin.assertValid());

    const watchmanClocks = new Map(fileDelta.clocks ?? []);
    await this.#takeSnapshotAndPersistRust(
      rust,
      watchmanClocks,
      plugins,
      changeSize > 0,
    );
    debug('Finished mapping files (%d changes).', changeSize);

    await this.#watch(null, watchmanClocks, plugins);
    return {fileSystem};
  }

  /**
   * 1. read data from the cache: the native file map loads its own file,
   * which is the DiskCacheManager's path plus a suffix. A NoopCacheManager
   * has no data.
   */
  async #readRust(rust: RustMode): Promise<?RustCache> {
    const cacheManager = this.#cacheManager;
    this.#startupPerfLogger?.point('read_start');
    let cache = null;
    if (cacheManager instanceof DiskCacheManager) {
      try {
        const extras = rust.native.readCache(
          cacheManager.getCacheFilePath() + RUST_CACHE_SUFFIX,
        );
        if (extras != null) {
          cache = {
            clocks: new Map<string, WatchmanClockSpec>(
              extras.clockPaths.map((clockPath: string, i: number) => [
                clockPath,
                JSON.parse(extras.clockSpecs[i]),
              ]),
            ),
            pluginStates: new Map<string, void | V8Serializable>(
              extras.pluginNames.map((name: string, i: number) => [
                name,
                extras.pluginStates[i] == null
                  ? undefined
                  : JSON.parse(extras.pluginStates[i]),
              ]),
            ),
          };
        }
      } catch (e) {
        this.#console.warn(
          'Error while reading cache, falling back to a full crawl:\n',
          e,
        );
        this.#startupPerfLogger?.annotate({
          string: {cacheReadError: e.toString()},
        });
      }
    }
    rust.fileSystem.invalidate();
    this.#startupPerfLogger?.point('read_end');
    return cache;
  }

  /**
   * 2. crawl the file system. The node crawl is native; the Watchman crawl
   * is the JS one, and its result is handed to the native file map.
   */
  async #buildFileDeltaRust(
    rust: RustMode,
    clocks: Map<string, WatchmanClockSpec>,
  ): Promise<{clocks?: WatchmanClocks}> {
    this.#startupPerfLogger?.point('buildFileDelta_start');
    const {watcher, useWatchman} = await this.#createWatcher({
      clocks,
      fileSystem: rust.fileSystem,
    });
    let resultClocks;
    if (useWatchman) {
      const result = await watcher.crawl();
      this.#keepDeltaRust(rust, result, null);
      resultClocks = result.clocks;
    } else {
      this.#startupPerfLogger?.point('crawl_start');
      this.#crawlNodeRust(rust, this.#options.roots, null);
      this.#startupPerfLogger?.point('crawl_end');
    }
    this.#startupPerfLogger?.point('buildFileDelta_end');
    return {clocks: resultClocks};
  }

  /**
   * `nodeCrawl`: the native file map keeps the difference for
   * `#applyFileDeltaRust`. Returns how many files changed and were removed.
   */
  #crawlNodeRust(
    rust: RustMode,
    roots: ReadonlyArray<string>,
    subpath: ?string,
  ): {changed: number, removed: number} {
    const signal = this.#crawlerAbortController.signal;
    signal.throwIfAborted();
    debug('Crawling roots: %s with %s crawler.', roots, 'node');
    this.#startupPerfLogger?.point('nodeCrawl_start');
    const summary = rust.native.nodeCrawl(roots, subpath ?? null);
    for (const warning of summary.warnings) {
      this.#console.warn(warning);
    }
    signal.throwIfAborted();
    this.#startupPerfLogger?.point('nodeCrawl_end');
    debug(
      'Crawler "%s" returned %d added/modified, %d removed.',
      'node',
      summary.changed,
      summary.removed,
    );
    rust.verifier?.crawled(subpath ?? null);
    return summary;
  }

  /**
   * Hands the delta of a JS crawler to the native file map, unless it is the
   * native file map's own `getDifference` result, which it holds already.
   */
  #keepDeltaRust(rust: RustMode, delta: CrawlResult, subpath: ?string): void {
    if (!rust.fileSystem.isLastDifference(delta.changedFiles)) {
      rust.native.setDifference(
        toCompactFiles(delta.changedFiles, 0),
        Array.from(delta.removedFiles),
      );
    }
    rust.verifier?.crawledByJs(delta, subpath ?? null);
  }

  /**
   * 3. visit and extract metadata from changed files: the native file map
   * removes, hashes and reads links, JS runs the plugin workers (which are
   * user code) on the files that need them, and the native file map adds the
   * files and updates Haste. Returns the number of changes.
   */
  async #applyFileDeltaRust(rust: RustMode): Promise<number> {
    const perf = this.#startupPerfLogger;
    perf?.point('applyFileDelta_start');
    perf?.point('applyFileDelta_preprocess_start');
    const jobs = rust.native.beginDelta();
    perf?.point('applyFileDelta_preprocess_end');

    debug('Found %d files for plugin workers.', jobs.paths.length);

    perf?.point('applyFileDelta_process_start');
    const results = await this.#runPluginJobs(jobs);
    perf?.point('applyFileDelta_process_end');

    perf?.point('applyFileDelta_add_start');
    const applied = rust.native.finishDelta(results);
    rust.fileSystem.invalidate();
    perf?.point('applyFileDelta_add_end');

    perf?.point('applyFileDelta_updatePlugins_start');
    (rust.hastePlugin as Internal)?._warn(applied.warnings);
    if (rust.jsPlugins.length > 0) {
      this.#updateJsPlugins(rust, rust.native.changeView());
    }
    perf?.point('applyFileDelta_updatePlugins_end');
    perf?.point('applyFileDelta_end');

    await rust.verifier?.applied(applied.size, () =>
      toPublicChanges(rust.native.changeView()),
    );
    return applied.size;
  }

  async #runPluginJobs(jobs: {
    paths: Array<string>,
    plugins: Array<Array<number>>,
  }): Promise<
    Array<{
      path: string,
      data?: Array<string>,
      errorCode?: ?string,
      errorMessage?: ?string,
    }>,
  > {
    if (jobs.paths.length === 0) {
      return [];
    }
    const replies = await this.#fileProcessor.processPluginJobs(
      jobs.paths.map((normalPath, i) => ({
        normalPath,
        pluginsToRun: jobs.plugins[i],
      })),
    );
    return replies.map((reply, i) =>
      reply.error != null
        ? {
            path: jobs.paths[i],
            errorCode: reply.error.code,
            errorMessage: reply.error.message,
          }
        : {path: jobs.paths[i], data: jsonOfPluginData(reply.pluginData)},
    );
  }

  /** Calls `onChanged` of the plugins that FileMap itself keeps up to date. */
  #updateJsPlugins(rust: RustMode, view: RustChangeView): void {
    for (const {plugin, position} of rust.jsPlugins) {
      plugin.onChanged(
        toPluginChanges(view, position, rust.jsPlugins.length) as Internal,
      );
    }
  }

  /**
   * What `plugin.initialize` gets with the Rust core. Plugin data is read from
   * the native file map, by the plugin's slot.
   */
  #getRustInitOptions(
    rust: RustMode,
    plugin: InputFileMapPlugin,
    dataIdx: ?number,
    pluginState: void | V8Serializable,
  ): FileMapPluginInitOptions<void | V8Serializable, void | V8Serializable> {
    const {native} = rust;
    const slot = dataIdx != null ? dataIdx - H.PLUGINDATA : null;
    return {
      files: {
        lookup: mixedPath => {
          const result = native.pluginLookup(mixedPath, slot ?? 0);
          if (result.kind === 0) {
            return {exists: false};
          }
          if (result.kind === 1) {
            return {exists: true, type: 'd'};
          }
          return {
            exists: true,
            type: 'f',
            pluginData:
              slot == null
                ? null
                : result.data == null
                  ? undefined
                  : JSON.parse(result.data),
          };
        },
        fileIterator: opts => {
          const files = native.pluginFiles(
            slot ?? 0,
            opts.includeSymlinks,
            opts.includeNodeModules,
          );
          return (function* iterate() {
            for (let i = 0; i < files.canonicalPaths.length; i++) {
              const json = files.data[i];
              yield {
                baseName: files.baseNames[i],
                canonicalPath: files.canonicalPaths[i],
                pluginData:
                  slot == null
                    ? null
                    : json == null
                      ? undefined
                      : JSON.parse(json),
              };
            }
          })();
        },
      },
      pluginState,
      processFile: mixedPath => {
        // Only a lazy plugin asks for this, and the Rust core does not run
        // those (see getRustBinding).
        throw new Error(
          `metro-file-map: Plugin "${plugin.name}" cannot process ${mixedPath}: the Rust file map does not run plugin workers on demand`,
        );
      },
    };
  }

  /**
   * 4. persist: the native file map writes its own cache file. The
   * DiskCacheManager keeps its write semantics (the debounced auto-save, the
   * write queue, `onWriteError`) through `_writeWith`.
   */
  async #takeSnapshotAndPersistRust(
    rust: RustMode,
    clocks: WatchmanClocks,
    plugins: ReadonlyArray<IndexedPlugin>,
    changedSinceCacheRead: boolean,
  ) {
    this.#startupPerfLogger?.point('persist_start');
    const cacheManager = this.#cacheManager;
    if (cacheManager instanceof DiskCacheManager) {
      const cacheFile = cacheManager.getCacheFilePath() + RUST_CACHE_SUFFIX;
      await (cacheManager as Internal)._writeWith(
        () => ({
          clockPaths: Array.from(clocks.keys()),
          clockSpecs: Array.from(clocks.values(), spec => JSON.stringify(spec)),
          pluginNames: plugins.map(({plugin}) => plugin.name),
          pluginStates: plugins.map(({plugin}) => {
            const state = plugin.getSerializableSnapshot();
            return state === undefined ? null : JSON.stringify(state);
          }),
        }),
        (extras: unknown) => rust.native.writeCache(cacheFile, extras),
        this.#getCacheWriteOptions(changedSinceCacheRead),
      );
    }
    this.#startupPerfLogger?.point('persist_end');
  }

  /**
   * `onChange` of watch mode for a touched file, with the Rust core: the
   * native file map reads the link or hashes the file and keeps the metadata
   * under a ticket, JS runs the plugin workers.
   */
  async #processTouchRust(
    rust: RustMode,
    relativeFilePath: string,
    metadata: ChangeEventMetadata,
  ): Promise<number> {
    const {native} = rust;
    const {ticket, plugins} = native.processTouch(
      relativeFilePath,
      metadata.modifiedTime,
      metadata.size,
      metadata.type === 'l',
    );
    if (plugins.length > 0) {
      try {
        const [reply] = await this.#fileProcessor.processPluginJobs([
          {normalPath: relativeFilePath, pluginsToRun: plugins},
        ]);
        if (reply.error != null) {
          throw reply.error;
        }
        native.setTouchPluginData(ticket, jsonOfPluginData(reply.pluginData));
      } catch (e) {
        native.discardTouch(ticket);
        throw e;
      }
    }
    return ticket;
  }

  /**
   * Watch mode
   */
  async #watch(
    fileSystem: ?MutableFileSystem,
    clocks: WatchmanClocks,
    plugins: ReadonlyArray<IndexedPlugin>,
  ): Promise<void> {
    this.#startupPerfLogger?.point('watch_start');
    if (!this.#options.watch) {
      this.#startupPerfLogger?.point('watch_end');
      return;
    }

    const rust = this.#rust;

    const hasWatchedExtension = (filePath: string) =>
      this.#options.extensions.some(ext => filePath.endsWith(ext));

    let nextEmit: ?{
      events: Array<InternalEnqueuedEvent>,
      firstEventTimestamp: number,
      firstEnqueuedTimestamp: number,
    } = null;

    const emitChange = () => {
      if (nextEmit == null) {
        // Nothing to emit
        return;
      }
      const {events, firstEventTimestamp, firstEnqueuedTimestamp} = nextEmit;

      let changeSize;
      let changesWithMetadata;
      if (rust != null) {
        // The native file map applies the events in order, and returns the
        // net change.
        const kinds = new Uint8Array(events.length);
        const tickets = new Uint32Array(events.length);
        events.forEach((event, i) => {
          if (event.type === 'touch') {
            kinds[i] = 1;
            tickets[i] = event.ticket ?? 0;
          }
        });
        const applied = rust.native.applyEvents(
          kinds,
          events.map(event => event.relativeFilePath),
          tickets,
        );
        rust.fileSystem.invalidate();
        for (const {clock} of events) {
          this.#updateClock(clocks, clock);
        }
        changeSize = applied.size;
        if (changeSize === 0) {
          // We had events, but they've exactly cancelled each other out, reset
          // so that timers are correct for the next change.
          rust.verifier?.events(events, 0, null);
          nextEmit = null;
          return;
        }
        (rust.hastePlugin as Internal)?._warn(applied.warnings);
        this.#updateJsPlugins(rust, applied.view);
        changesWithMetadata = toPublicChanges(applied.view);
        rust.verifier?.events(events, changeSize, changesWithMetadata);
      } else {
        const changeAggregator = new FileSystemChangeAggregator();

        // Process a sequence of events. Note that preserving ordering is
        // important here - a file may be both removed and added in the same
        // batch.
        // `changeAggregator` flattens this over time into the net change from
        // this sequence.
        for (const event of events) {
          const {relativeFilePath, clock} = event;
          if (event.type === 'delete') {
            nullthrows(fileSystem).remove(relativeFilePath, changeAggregator);
          } else {
            nullthrows(fileSystem).addOrModify(
              relativeFilePath,
              event.metadata,
              changeAggregator,
            );
          }
          this.#updateClock(clocks, clock);
        }

        changeSize = changeAggregator.getSize();

        if (changeSize === 0) {
          // We had events, but they've exactly cancelled each other out, reset
          // so that timers are correct for the next change.
          nextEmit = null;
          return;
        }

        const _netChange = changeAggregator.getView();
        this.#plugins.forEach(({plugin, dataIdx}) => {
          plugin.onChanged(
            changeAggregator.getMappedView(
              dataIdx != null ? metadata => metadata[dataIdx] : () => null,
            ),
          );
        });

        const toPublicMetadata = (
          metadata: Readonly<FileMetadata>,
        ): ChangedFileMetadata => ({
          isSymlink: metadata[H.SYMLINK] !== 0,
          modifiedTime: metadata[H.MTIME] ?? null,
        });

        changesWithMetadata = changeAggregator.getMappedView(toPublicMetadata);
      }

      const hmrPerfLogger = this.#options.perfLoggerFactory?.('HMR', {
        key: this.#getNextChangeID(),
      });
      if (hmrPerfLogger != null) {
        hmrPerfLogger.start({timestamp: firstEventTimestamp});
        hmrPerfLogger.point('waitingForChangeInterval_start', {
          timestamp: firstEnqueuedTimestamp,
        });
        hmrPerfLogger.point('waitingForChangeInterval_end');
        hmrPerfLogger.annotate({int: {changeSize}});
        hmrPerfLogger.point('fileChange_start');
      }
      const changeEvent: ChangeEvent = {
        changes: changesWithMetadata,
        logger: hmrPerfLogger,
        rootDir: this.#options.rootDir,
      };
      this.emit('change', changeEvent);
      nextEmit = null;
    };

    let changeQueue: Promise<null | void> = Promise.resolve();

    const onChange = (change: WatcherBackendChangeEvent) => {
      // Recrawl events bypass normal filtering - they trigger a full subdirectory scan
      if (
        change.event !== 'recrawl' &&
        change.metadata &&
        // Ignore all directory events
        (change.metadata.type === 'd' ||
          // Ignore regular files with unwatched extensions
          (change.metadata.type === 'f' &&
            !hasWatchedExtension(change.relativePath)) ||
          // Don't emit events relating to symlinks if enableSymlinks: false
          (!this.#options.enableSymlinks && change.metadata?.type === 'l'))
      ) {
        return;
      }

      const absoluteFilePath = path.join(
        change.root,
        normalizePathSeparatorsToSystem(change.relativePath),
      );

      // Ignore files (including symlinks) whose path matches ignorePattern
      // (we don't ignore node_modules in watch mode)
      if (this.#options.ignorePattern.test(absoluteFilePath)) {
        return;
      }

      const relativeFilePath =
        this.#pathUtils.absoluteToNormal(absoluteFilePath);

      const onChangeStartTime = performance.timeOrigin + performance.now();

      const enqueueEvent = (event: InternalEnqueuedEvent) => {
        nextEmit ??= {
          events: [],
          firstEnqueuedTimestamp: performance.timeOrigin + performance.now(),
          firstEventTimestamp: onChangeStartTime,
        };
        nextEmit.events.push(event);
      };

      changeQueue = changeQueue
        .then(async () => {
          // If we get duplicate events for the same file, ignore them.
          if (
            nextEmit != null &&
            nextEmit.events.find(
              event =>
                event.type === change.event &&
                event.relativeFilePath === relativeFilePath &&
                ((!event.metadata && !change.metadata) ||
                  (event.metadata &&
                    change.metadata &&
                    event.metadata[H.MTIME] != null &&
                    change.metadata.modifiedTime != null &&
                    event.metadata[H.MTIME] === change.metadata.modifiedTime)),
            )
          ) {
            return null;
          }

          // If the file was added or modified,
          // parse it and update the file map.
          if (change.event === 'touch') {
            invariant(
              change.metadata.size != null,
              'since the file exists or changed, it should have known size',
            );
            const fileMetadata: FileMetadata = [
              change.metadata.modifiedTime,
              change.metadata.size,
              0,
              null,
              change.metadata.type === 'l' ? 1 : 0,
            ];

            try {
              let ticket;
              let verifyMetadata;
              if (rust != null) {
                ticket = await this.#processTouchRust(
                  rust,
                  relativeFilePath,
                  change.metadata,
                );
                if (rust.verifier != null) {
                  // The JS implementation makes its own metadata.
                  verifyMetadata = [...fileMetadata] as FileMetadata;
                  await this.#processTouchJs(
                    relativeFilePath,
                    change.metadata,
                    verifyMetadata,
                  );
                }
              } else {
                await this.#processTouchJs(
                  relativeFilePath,
                  change.metadata,
                  fileMetadata,
                );
              }
              enqueueEvent({
                clock: change.clock,
                relativeFilePath,
                metadata: fileMetadata,
                type: change.event,
                ticket,
                verifyMetadata,
              });
            } catch (e) {
              if (!['ENOENT', 'EACCESS'].includes(e.code)) {
                throw e;
              }
              // Swallow ENOENT/ACCESS errors silently. Safe because either:
              // - We never knew about the file, so neither did any consumers.
              // Or,
              // - The watcher will soon (or has already) report a "delete"
              //   event for it, and we'll clean up in the usual way at that
              //   point.
            }
          } else if (change.event === 'delete') {
            enqueueEvent({
              clock: change.clock,
              relativeFilePath,
              type: 'delete',
            });
          } else if (change.event === 'recrawl') {
            // Recrawl event: flush pending changes and re-crawl the directory
            emitChange();

            // The relativePath is relative to the watcher root (change.root),
            // but we need a path relative to rootDir for the recrawl.
            const absoluteDirPath = path.join(
              change.root,
              normalizePathSeparatorsToSystem(change.relativePath),
            );
            const subpath = this.#pathUtils.absoluteToNormal(absoluteDirPath);

            if (rust != null) {
              // Crawl the specific subdirectory, in Rust
              const summary = this.#crawlNodeRust(
                rust,
                [path.join(this.#options.rootDir, subpath)],
                subpath,
              );

              // Skip if no changes
              if (summary.changed === 0 && summary.removed === 0) {
                return null;
              }

              // Reuse the same batch processing logic as build()
              const size = await this.#applyFileDeltaRust(rust);

              // Update clock if provided
              this.#updateClock(clocks, change.clock);

              // Skip emit if no changes after processing
              if (size === 0) {
                return null;
              }

              // Emit changes directly
              const changeEvent: ChangeEvent = {
                changes: toPublicChanges(rust.native.changeView()),
                logger: null,
                rootDir: this.#options.rootDir,
              };
              this.emit('change', changeEvent);
              return null;
            }

            // Crawl the specific subdirectory
            const watcher = this.#watcher;
            invariant(watcher != null, 'Watcher must be initialized');
            const crawlResult = await watcher.recrawl(
              subpath,
              nullthrows(fileSystem),
            );

            // Skip if no changes
            if (
              crawlResult.changedFiles.size === 0 &&
              crawlResult.removedFiles.size === 0
            ) {
              return null;
            }

            // Reuse the same batch processing logic as build()
            const recrawlChangeAggregator = await this.#applyFileDelta(
              nullthrows(fileSystem),
              this.#plugins,
              crawlResult,
            );

            // Update clock if provided
            this.#updateClock(clocks, change.clock);

            // Skip emit if no changes after processing
            if (recrawlChangeAggregator.getSize() === 0) {
              return null;
            }

            // Emit changes directly
            const toPublicMetadata = (
              metadata: Readonly<FileMetadata>,
            ): ChangedFileMetadata => ({
              isSymlink: metadata[H.SYMLINK] !== 0,
              modifiedTime: metadata[H.MTIME] ?? null,
            });

            const changesWithMetadata =
              recrawlChangeAggregator.getMappedView(toPublicMetadata);

            const changeEvent: ChangeEvent = {
              changes: changesWithMetadata,
              logger: null,
              rootDir: this.#options.rootDir,
            };
            this.emit('change', changeEvent);
          } else {
            throw new Error(
              `metro-file-map: Unrecognized event type from watcher: ${change.event}`,
            );
          }
          return null;
        })
        .catch((error: Error) => {
          this.#console.error(
            `metro-file-map: watch error:\n  ${error.stack}\n`,
          );
        });
    };

    this.#changeInterval = setInterval(emitChange, CHANGE_INTERVAL);

    invariant(
      this.#watcher != null,
      'Expected #watcher to have been initialised by build()',
    );
    await this.#watcher.watch(onChange);

    if (this.#options.healthCheck.enabled) {
      const performHealthCheck = () => {
        if (!this.#watcher) {
          return;
        }
        // $FlowFixMe[unused-promise]
        this.#watcher
          .checkHealth(this.#options.healthCheck.timeout)
          .then(result => {
            this.emit('healthCheck', result);
          });
      };
      performHealthCheck();
      this.#healthCheckInterval = setInterval(
        performHealthCheck,
        this.#options.healthCheck.interval,
      );
    }
    this.#startupPerfLogger?.point('watch_end');
  }

  async end(): Promise<void> {
    if (this.#changeInterval) {
      clearInterval(this.#changeInterval);
    }
    if (this.#healthCheckInterval) {
      clearInterval(this.#healthCheckInterval);
    }

    this.#crawlerAbortController.abort();

    await Promise.all([
      this.#fileProcessor.end(),
      this.#watcher?.close(),
      this.#cacheManager.end(),
    ]);
  }

  async #shouldUseWatchman(): Promise<boolean> {
    if (!this.#options.useWatchman) {
      return false;
    }
    if (!this.#canUseWatchmanPromise) {
      this.#canUseWatchmanPromise = checkWatchmanCapabilities(
        WATCHMAN_REQUIRED_CAPABILITIES,
      )
        .then(({version}) => {
          this.#startupPerfLogger?.annotate({
            string: {
              watchmanVersion: version,
            },
          });
          return true;
        })
        .catch(e => {
          // TODO: Advise people to either install Watchman or set
          // `useWatchman: false` here?
          this.#startupPerfLogger?.annotate({
            string: {
              watchmanFailedCapabilityCheck: e?.message ?? '[missing]',
            },
          });
          return false;
        });
    }
    return this.#canUseWatchmanPromise;
  }

  #getNextChangeID(): number {
    if (this.#changeID >= Number.MAX_SAFE_INTEGER) {
      this.#changeID = 0;
    }
    return ++this.#changeID;
  }

  #updateClock(clocks: WatchmanClocks, newClock?: ?ChangeEventClock): void {
    if (newClock == null) {
      return;
    }
    const [absoluteWatchRoot, clockSpec] = newClock;
    const relativeFsRoot = this.#pathUtils.absoluteToNormal(absoluteWatchRoot);
    clocks.set(normalizePathSeparatorsToPosix(relativeFsRoot), clockSpec);
  }

  static H: HType = H;
}

/** What the plugin workers returned, as the JSON text the native core stores. */
function jsonOfPluginData(
  pluginData: ReadonlyArray<V8Serializable>,
): Array<string> {
  // `undefined` is reserved to mean that no worker has run.
  return pluginData.map(value => JSON.stringify(value ?? null));
}

function parsePluginData(json: ?string): unknown {
  return json == null ? undefined : JSON.parse(json);
}

/**
 * The change view of a plugin that has a data slot, for `plugin.onChanged`.
 * `position` is its place among the `count` slots the native view carries.
 */
function toPluginChanges(
  view: RustChangeView,
  position: number,
  count: number,
): ReadonlyFileSystemChanges<unknown> {
  const entries = (list: RustChangeList) =>
    list.paths.map((canonicalPath, i) => [
      canonicalPath,
      parsePluginData(list.data[i * count + position]),
    ]);
  return {
    addedDirectories: new Set(view.addedDirectories),
    removedDirectories: new Set(view.removedDirectories),
    addedFiles: entries(view.added),
    modifiedFiles: entries(view.modified),
    removedFiles: entries(view.removed),
  };
}

/** The public form of a native change view (`ChangeEvent['changes']`). */
function toPublicChanges(
  view: RustChangeView,
): ReadonlyFileSystemChanges<ChangedFileMetadata> {
  const entries = (list: RustChangeList) =>
    list.paths.map((canonicalPath, i) => [
      canonicalPath,
      {
        isSymlink: list.symlinks[i] === 1,
        modifiedTime: Number.isNaN(list.mtimes[i]) ? null : list.mtimes[i],
      },
    ]);
  return {
    addedDirectories: new Set(view.addedDirectories),
    removedDirectories: new Set(view.removedDirectories),
    addedFiles: entries(view.added),
    modifiedFiles: entries(view.modified),
    removedFiles: entries(view.removed),
  };
}

// TODO: Replace with it.map() from Node 22+
const mapIterable: <T, S>(Iterable<T>, (T) => S) => IteratorObject<S> = (
  it,
  fn,
) =>
  (function* mapped() {
    for (const item of it) {
      yield fn(item);
    }
  })();
