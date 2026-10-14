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
  ChangedFileMetadata,
  Console,
  CrawlResult,
  FileData,
  FileMetadata,
  MutableFileSystem,
  ReadonlyFileSystemChanges,
} from '../flow-types';
import type {FileSystemVerifier} from './RustFileSystem';

import H from '../constants';
import HastePlugin from '../plugins/HastePlugin';
import {FileProcessor} from './FileProcessor';
import {FileSystemChangeAggregator} from './FileSystemChangeAggregator';
import {buildTree, fromCompactFiles} from './RustFileSystem';
import TreeFS from './TreeFS';
import {isRustEnabled} from 'metro-rust-switch';

/**
 * METRO_RUST_VERIFY=file-map: runs the JS implementation next to the Rust
 * core and compares them. The JS side has its own `TreeFS` and its own
 * `HastePlugin` (made with the options of the real one), fed with the same
 * crawl inputs and the same watcher events. After `build()` and after every
 * change batch it compares the snapshots, the change event and the Haste
 * results, and every `RustFileSystem` call is also answered by the JS
 * `TreeFS` and compared. The first difference throws an Error with the diff.
 */

let lastVerify: ?string;
let lastVerifyEnabled = false;

/** Whether METRO_RUST_VERIFY lists the `file-map` component. */
export function isRustVerifyEnabled(): boolean {
  const verify = process.env.METRO_RUST_VERIFY;
  if (verify !== lastVerify) {
    // Cached only after isRustEnabled succeeds (it throws on an invalid list).
    lastVerifyEnabled =
      verify != null &&
      verify !== '' &&
      isRustEnabled('file-map', null, {METRO_RUST: verify});
    lastVerify = verify;
  }
  return lastVerifyEnabled;
}

// This is debugging code that pokes at internals (the native file map's
// methods, the plugins' underscore-prefixed ones) and compares arbitrary
// values: it is not typed.
type Loose = any;

type IndexedPlugin = Readonly<{
  plugin: Loose,
  dataIdx: ?number,
}>;

type Delta = {
  changedFiles: FileData,
  removedFiles: Set<string>,
};

type PublicChanges = ReadonlyFileSystemChanges<ChangedFileMetadata>;

type VerifierEvent = Readonly<{
  relativeFilePath: string,
  type: 'delete' | 'touch',
  verifyMetadata?: FileMetadata,
  ...
}>;

type Options = Readonly<{
  applyFileDelta: (
    fileSystem: MutableFileSystem,
    plugins: ReadonlyArray<IndexedPlugin>,
    delta: Delta,
  ) => Promise<FileSystemChangeAggregator>,
  console: Console,
  fileProcessor: FileProcessor,
  hastePlugin: ?Loose,
  native: Loose,
  plugins: ReadonlyArray<IndexedPlugin>,
  rootDir: string,
}>;

const MISSING_HASTE_NAME = 'metro-rust-verify-missing-haste-name';

export class RustVerifier implements FileSystemVerifier {
  readonly #options: Options;
  #tree: ?TreeFS = null;
  #jsHaste: ?HastePlugin = null;
  #jsPlugins: ReadonlyArray<IndexedPlugin> = [];
  #hasteSlot: ?number = null;
  #delta: ?Delta = null;
  #jsWarnings: Array<string> = [];
  #nativeWarnings: Array<string> = [];

  constructor(options: Options) {
    this.#options = options;
  }

  recordNativeWarnings(warnings: ReadonlyArray<string>): void {
    this.#nativeWarnings.push(...warnings);
  }

  // The native file map has loaded its cache (or is empty): the JS side starts
  // from the same data, with its Haste map initialised from it.
  async start(): Promise<void> {
    const {native, hastePlugin, plugins, rootDir} = this.#options;
    const fileProcessor = this.#options.fileProcessor;
    const tree = TreeFS.fromDeserializedSnapshot({
      fileSystemData: buildTree(JSON.parse(native.getSerializableSnapshot())),
      processFile: (normalPath, metadata, opts) =>
        fileProcessor.processRegularFile(normalPath, metadata, {
          computeSha1: opts.computeSha1,
          maybeReturnContent: true,
        })?.content,
      rootDir,
    });
    this.#tree = tree;
    const jsPlugins = [];
    for (const indexed of plugins) {
      if (indexed.plugin === hastePlugin && hastePlugin != null) {
        const options = hastePlugin._getRustOptions();
        const jsHaste = new HastePlugin({
          // $FlowFixMe[incompatible-type] Only `warn` is used.
          console: {
            warn: (message: string) => {
              this.#jsWarnings.push(message);
            },
          },
          enableHastePackages: options.enableHastePackages,
          failValidationOnConflicts: options.failValidationOnConflicts,
          hasteImplModulePath: options.hasteImplModulePath,
          platforms: options.platforms,
          rootDir: options.rootDir,
        });
        this.#jsHaste = jsHaste;
        this.#hasteSlot = indexed.dataIdx;
        jsPlugins.push({plugin: jsHaste, dataIdx: indexed.dataIdx});
        await jsHaste.initialize({
          files: {
            lookup: mixedPath => {
              const result = tree.lookup(mixedPath);
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
                  indexed.dataIdx != null
                    ? (result.metadata as Loose)[indexed.dataIdx]
                    : null,
              };
            },
            fileIterator: opts =>
              (function* iterate() {
                for (const {
                  baseName,
                  canonicalPath,
                  metadata,
                } of tree.metadataIterator(opts)) {
                  yield {
                    baseName,
                    canonicalPath,
                    pluginData:
                      indexed.dataIdx != null
                        ? (metadata as Loose)[indexed.dataIdx]
                        : null,
                  };
                }
              })(),
          },
          pluginState: null,
          processFile: () => {
            throw new Error('RustVerifier: processFile is not supported');
          },
        });
      } else {
        jsPlugins.push(indexed);
      }
    }
    this.#jsPlugins = jsPlugins;
  }

  // The plugins have initialised.
  checkInitialized(): void {
    this.#compareState('after initialization');
  }

  // The native file map crawled: the JS side gets the same crawl input.
  crawled(subpath: ?string): void {
    const {native} = this.#options;
    const tree = this.#getTree();
    const found = fromCompactFiles(native.crawlInput(), 0);
    const jsDifference = tree.getDifference(
      found,
      subpath != null ? {subpath} : undefined,
    );
    const pending = native.pendingDifference();
    const rustDifference = {
      changedFiles: fromCompactFiles(pending.changed, this.#pluginCount()),
      removedFiles: new Set<string>(pending.removed),
    };
    this.#compare('crawl difference', rustDifference, jsDifference);
    this.#delta = jsDifference;
  }

  // A JS crawler (Watchman) gave the native file map its delta.
  crawledByJs(delta: CrawlResult, _subpath: ?string): void {
    const changedFiles: FileData = new Map();
    for (const [filePath, metadata] of delta.changedFiles) {
      changedFiles.set(filePath, [...metadata] as FileMetadata);
    }
    this.#delta = {changedFiles, removedFiles: new Set(delta.removedFiles)};
  }

  // The native file map applied the delta: so does the JS side.
  async applied(
    rustSize: number,
    getRustChanges: () => PublicChanges,
  ): Promise<void> {
    const delta = this.#delta;
    if (delta == null) {
      throw new Error('METRO_RUST_VERIFY: a delta was applied without a crawl');
    }
    this.#delta = null;
    const aggregator = await this.#options.applyFileDelta(
      this.#getTree(),
      this.#jsPlugins,
      delta,
    );
    this.#compare('number of changes', rustSize, aggregator.getSize());
    this.#compare(
      'changes of the delta',
      normalizeChanges(getRustChanges()),
      normalizeChanges(aggregator.getMappedView(toPublicMetadata)),
    );
    this.#compareState('after applying the delta');
  }

  // The native file map applied a batch of watcher events: so does the JS side.
  events(
    events: ReadonlyArray<VerifierEvent>,
    rustSize: number,
    rustChanges: ?PublicChanges,
  ): void {
    const tree = this.#getTree();
    const aggregator = new FileSystemChangeAggregator();
    for (const event of events) {
      if (event.type === 'delete') {
        tree.remove(event.relativeFilePath, aggregator);
      } else {
        if (event.verifyMetadata == null) {
          throw new Error('METRO_RUST_VERIFY: a touch without JS metadata');
        }
        tree.addOrModify(
          event.relativeFilePath,
          event.verifyMetadata,
          aggregator,
        );
      }
    }
    this.#compare(
      'number of changes of the events',
      rustSize,
      aggregator.getSize(),
    );
    if (aggregator.getSize() > 0) {
      for (const {plugin, dataIdx} of this.#jsPlugins) {
        plugin.onChanged(
          aggregator.getMappedView(
            dataIdx != null ? metadata => metadata[dataIdx] : () => null,
          ),
        );
      }
    }
    if (rustChanges != null) {
      this.#compare(
        'change event',
        normalizeChanges(rustChanges),
        normalizeChanges(aggregator.getMappedView(toPublicMetadata)),
      );
    }
    this.#compareState('after the events');
  }

  // Every call of RustFileSystem.

  check(
    method: string,
    args: ReadonlyArray<unknown>,
    result: unknown,
    error: unknown,
  ): void {
    const tree = this.#tree;
    if (tree == null) {
      return;
    }
    let jsResult: Loose;
    let jsError;
    try {
      jsResult = (tree as Loose)[method](...args);
      if (method === 'matchFiles') {
        jsResult = [...jsResult];
      }
    } catch (e) {
      jsError = e;
    }
    this.#compareCall(
      method,
      args,
      normalizeResult(method, result),
      error,
      normalizeResult(method, jsResult),
      jsError,
    );
  }

  async checkAsync(
    method: string,
    args: ReadonlyArray<unknown>,
    result: unknown,
    error: unknown,
  ): Promise<void> {
    const tree = this.#tree;
    if (tree == null) {
      return;
    }
    let jsResult: Loose;
    let jsError;
    try {
      jsResult = await (tree as Loose)[method](...args);
    } catch (e) {
      jsError = e;
    }
    this.#compareCall(
      method,
      args,
      normalizeResult(method, result),
      error,
      normalizeResult(method, jsResult),
      jsError,
    );
  }

  #compareCall(
    method: string,
    args: ReadonlyArray<unknown>,
    rustResult: unknown,
    rustError: unknown,
    jsResult: unknown,
    jsError: unknown,
  ): void {
    const label = `${method}(${describe(args)})`;
    if (rustError != null || jsError != null) {
      this.#compare(
        `${label} (error)`,
        rustError != null ? errorMessage(rustError) : '[no error]',
        jsError != null ? errorMessage(jsError) : '[no error]',
      );
      return;
    }
    this.#compare(label, rustResult, jsResult);
  }

  // The snapshots and Haste, after a change of state.
  #compareState(when: string): void {
    const {native} = this.#options;
    const tree = this.#getTree();
    this.#compare(
      `file system snapshot ${when}`,
      normalizeTree(buildTree(JSON.parse(native.getSerializableSnapshot()))),
      normalizeTree(tree.getSerializableSnapshot()),
    );
    this.#compareHaste(when);
  }

  #compareHaste(when: string): void {
    const rustHaste = this.#options.hastePlugin;
    const jsHaste = this.#jsHaste;
    if (rustHaste == null || jsHaste == null) {
      return;
    }
    const tree = this.#getTree();
    const hasteSlot = this.#hasteSlot;

    this.#compare(
      `Haste warnings ${when}`,
      this.#nativeWarnings.splice(0),
      this.#jsWarnings.splice(0),
    );
    this.#compare(
      `Haste conflicts ${when}`,
      rustHaste.computeConflicts(),
      jsHaste.computeConflicts(),
    );
    const outcome = (plugin: HastePlugin) => {
      try {
        plugin.assertValid();
        return '[valid]';
      } catch (e) {
        return `${errorMessage(e)}\n${e.getDetailedMessage?.(this.#options.rootDir) ?? ''}`;
      }
    };
    this.#compare(
      `Haste assertValid ${when}`,
      outcome(rustHaste),
      outcome(jsHaste),
    );

    // Every name and path the files define, and one that is not defined.
    const names = new Set<string>([MISSING_HASTE_NAME]);
    const absolutePaths = [];
    for (const {canonicalPath, metadata} of tree.metadataIterator({
      includeNodeModules: true,
      includeSymlinks: false,
    })) {
      absolutePaths.push(tree.lookup(canonicalPath));
      const name = hasteSlot != null ? metadata[hasteSlot] : null;
      if (typeof name === 'string') {
        names.add(name);
      }
    }
    const platforms = [null, ...rustHaste._getRustOptions().platforms];
    const query = (plugin: HastePlugin, name: string): unknown => {
      const results = [];
      for (const platform of platforms) {
        for (const supportsNative of [true, false]) {
          results.push(
            attempt(() => plugin.getModule(name, platform, supportsNative)),
          );
        }
        results.push(
          attempt(() => plugin.getModule(name, platform, true, H.PACKAGE)),
        );
        results.push(attempt(() => plugin.getPackage(name, platform, true)));
      }
      return results;
    };
    for (const name of names) {
      this.#compare(
        `Haste lookups of ${JSON.stringify(name)} ${when}`,
        query(rustHaste, name),
        query(jsHaste, name),
      );
    }
    for (const result of absolutePaths) {
      if (result.exists && result.type === 'f') {
        this.#compare(
          `Haste name of ${result.realPath} ${when}`,
          attempt(() => rustHaste.getModuleNameByPath(result.realPath)),
          attempt(() => jsHaste.getModuleNameByPath(result.realPath)),
        );
      }
    }
  }

  #pluginCount(): number {
    return this.#options.plugins.filter(({dataIdx}) => dataIdx != null).length;
  }

  #getTree(): TreeFS {
    const tree = this.#tree;
    if (tree == null) {
      throw new Error('METRO_RUST_VERIFY: the JS file system was not started');
    }
    return tree;
  }

  #compare(what: string, rust: unknown, js: unknown): void {
    const difference = firstDifference(rust, js, '');
    if (difference != null) {
      throw new Error(
        `METRO_RUST_VERIFY=file-map: ${what} differs at ${difference.path || 'the top'}.\n` +
          `  Rust: ${describe(difference.rust)}\n` +
          `  JS:   ${describe(difference.js)}`,
      );
    }
  }
}

function toPublicMetadata(
  metadata: Readonly<FileMetadata>,
): ChangedFileMetadata {
  return {
    isSymlink: metadata[H.SYMLINK] !== 0,
    modifiedTime: metadata[H.MTIME] ?? null,
  };
}

function attempt(fn: () => unknown): unknown {
  try {
    return fn();
  } catch (e) {
    return {
      threw: errorMessage(e),
      duplicates:
        e.duplicatesSet != null
          ? Array.from(e.duplicatesSet as Map<string, number>)
          : null,
    };
  }
}

function errorMessage(error: unknown): string {
  // $FlowFixMe[incompatible-use]
  return `${error?.name ?? 'Error'}: ${error?.message ?? String(error)}`;
}

/**
 * A metadata tuple as it can be compared: a plugin slot that nobody wrote
 * (`undefined`) is `null` once serialised, and trailing `null`s are not data.
 */
function normalizeTuple(tuple: ReadonlyArray<unknown>): Array<unknown> {
  const out = [
    tuple[0] ?? null,
    tuple[1],
    tuple[2],
    tuple[3] ?? null,
    tuple[4],
  ];
  for (let i = 5; i < tuple.length; i++) {
    out.push(tuple[i] === undefined ? null : tuple[i]);
  }
  while (out.length > 5 && out[out.length - 1] === null) {
    out.pop();
  }
  return out;
}

function normalizeTree(node: Loose): unknown {
  if (node instanceof Map) {
    return new Map(
      Array.from(node, ([name, child]) => [name, normalizeTree(child)]),
    );
  }
  return normalizeTuple(node);
}

function normalizeChanges(changes: PublicChanges): unknown {
  return {
    addedDirectories: Array.from(changes.addedDirectories),
    removedDirectories: Array.from(changes.removedDirectories),
    addedFiles: Array.from(changes.addedFiles, entry => [...entry]),
    modifiedFiles: Array.from(changes.modifiedFiles, entry => [...entry]),
    removedFiles: Array.from(changes.removedFiles, entry => [...entry]),
  };
}

function normalizeResult(method: string, result: Loose): unknown {
  switch (method) {
    case 'lookup': {
      const lookup: Loose = result;
      if (lookup == null || !lookup.exists || lookup.type !== 'f') {
        return lookup == null
          ? lookup
          : {
              exists: lookup.exists,
              realPath: lookup.realPath,
              type: lookup.type,
            };
      }
      return {
        exists: true,
        metadata: normalizeTuple(lookup.metadata),
        realPath: lookup.realPath,
        type: 'f',
      };
    }
    case 'getDifference': {
      const difference: Loose = result;
      return difference == null
        ? difference
        : {
            changedFiles: new Map(
              Array.from(difference.changedFiles, ([p, tuple]) => [
                p,
                normalizeTuple(tuple),
              ]),
            ),
            removedFiles: difference.removedFiles,
          };
    }
    case 'getSerializableSnapshot':
      return normalizeTree(result);
    case 'matchFiles':
      return result == null ? result : Array.from(result);
    case 'getOrComputeSha1': {
      const sha1Result: Loose = result;
      return sha1Result == null
        ? sha1Result
        : {
            sha1: sha1Result.sha1,
            content:
              sha1Result.content != null
                ? sha1Result.content.toString('base64')
                : null,
          };
    }
    default:
      return result;
  }
}

type Difference = {path: string, rust: unknown, js: unknown};

/**
 * Deep comparison: `Map`s, `Set`s and arrays in order, objects by key.
 */
function firstDifference(rust: unknown, js: unknown, at: string): ?Difference {
  if (rust === js) {
    return null;
  }
  if (rust instanceof Map && js instanceof Map) {
    const rustEntries = Array.from(rust);
    const jsEntries = Array.from(js);
    return (
      firstDifference(
        rustEntries.map(([key]) => key),
        jsEntries.map(([key]) => key),
        `${at}/[keys]`,
      ) ?? firstChildDifference(rustEntries, jsEntries, at, true)
    );
  }
  if (rust instanceof Set && js instanceof Set) {
    return firstDifference(Array.from(rust), Array.from(js), at);
  }
  if (Array.isArray(rust) && Array.isArray(js)) {
    if (rust.length !== js.length) {
      return {path: `${at}/length`, rust: rust.length, js: js.length};
    }
    for (let i = 0; i < rust.length; i++) {
      const difference = firstDifference(rust[i], js[i], `${at}[${i}]`);
      if (difference != null) {
        return difference;
      }
    }
    return null;
  }
  if (
    rust != null &&
    js != null &&
    typeof rust === 'object' &&
    typeof js === 'object' &&
    !(rust instanceof Map) &&
    !(js instanceof Map) &&
    !Array.isArray(rust) &&
    !Array.isArray(js)
  ) {
    const rustKeys = Object.keys(rust).sort();
    const jsKeys = Object.keys(js).sort();
    const keys = firstDifference(rustKeys, jsKeys, `${at}/[properties]`);
    if (keys != null) {
      return keys;
    }
    for (const key of rustKeys) {
      const difference = firstDifference(
        // $FlowFixMe[incompatible-use]
        rust[key],
        // $FlowFixMe[incompatible-use]
        js[key],
        `${at}.${key}`,
      );
      if (difference != null) {
        return difference;
      }
    }
    return null;
  }
  if (typeof rust === 'number' && typeof js === 'number') {
    if (Number.isNaN(rust) && Number.isNaN(js)) {
      return null;
    }
  }
  return {path: at, rust, js};
}

function firstChildDifference(
  rustEntries: ReadonlyArray<[unknown, unknown]>,
  jsEntries: ReadonlyArray<[unknown, unknown]>,
  at: string,
  isMap: boolean,
): ?Difference {
  for (let i = 0; i < rustEntries.length; i++) {
    const difference = firstDifference(
      rustEntries[i][1],
      jsEntries[i][1],
      `${at}/${String(rustEntries[i][0])}`,
    );
    if (difference != null) {
      return difference;
    }
  }
  return null;
}

function describe(value: unknown): string {
  const text =
    JSON.stringify(
      value,
      (_key, item) => {
        if (item instanceof Map) {
          return {'[Map]': Array.from(item)};
        }
        if (item instanceof Set) {
          return {'[Set]': Array.from(item)};
        }
        if (item === undefined) {
          return '[undefined]';
        }
        if (typeof item === 'number' && Number.isNaN(item)) {
          return '[NaN]';
        }
        return item;
      },
      0,
    ) ?? String(value);
  return text.length > 1500 ? text.slice(0, 1500) + '...' : text;
}
