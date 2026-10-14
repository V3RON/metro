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
  CacheData,
  FileData,
  FileMetadata,
  FileStats,
  FileSystem,
  LookupResult,
  Observations,
  Path,
} from '../flow-types';

import TreeFS from './TreeFS';
import invariant from 'invariant';
import * as path from 'node:path';

/**
 * The `FileSystem` of a FileMap whose data lives in Rust (`NativeFileMap` in
 * metro-rs). Each method is one call into the binding, and returns what the
 * `TreeFS` method returns. See port/design/phase-3.md.
 *
 * `observations` are not supported by the native file map. A call that passes
 * them is answered by a `TreeFS` rebuilt from a snapshot of the native one
 * (rebuilt again after the file map changes). Metro passes none.
 */

type CompactFiles = {
  paths: Array<string>,
  mtimes: Float64Array,
  sizes: Float64Array,
  visited: Uint8Array,
  hashes: Array<?string>,
  kinds: Uint8Array,
  targets: Array<string>,
  pluginData: Array<?string>,
};

export type NativeFileMap = {
  getDifference(
    files: CompactFiles,
    subpath: ?string,
  ): {changed: CompactFiles, removed: Array<string>},
  exists(mixedPath: string): boolean,
  getAllFiles(): Array<string>,
  getSha1(mixedPath: string): ?string,
  getSize(mixedPath: string): ?number,
  getOrComputeSha1(mixedPath: string): Promise<?{
    sha1?: string,
    content?: Buffer,
    id?: number,
    version?: number,
    errorCode?: string,
    errorMessage?: string,
  }>,
  finishSha1(id: number, version: number, sha1: string): boolean,
  hierarchicalLookup(
    startPath: string,
    subpath: string,
    breakOnSegment: ?string,
    subpathIsDir: boolean,
  ): ?{absolutePath: string, containerRelativePath: string},
  linkStats(mixedPath: string): ?{
    isSymlink: boolean,
    modifiedTime?: ?number,
    size: number,
  },
  lookup(mixedPath: string): {kind: number, realPath: string},
  matchFiles(args: {
    filterSource?: string,
    filterFlags?: string,
    filterCompareAbsolute: boolean,
    filterComparePosix: boolean,
    follow: boolean,
    recursive: boolean,
    rootDir?: string,
  }): ?Array<string>,
  metadataTuple(mixedPath: string): ?string,
  getSerializableSnapshot(): string,
  ...
};

/**
 * The checks of METRO_RUST_VERIFY=file-map, applied to each call.
 */
export interface FileSystemVerifier {
  check(
    method: string,
    args: ReadonlyArray<unknown>,
    result: unknown,
    error: unknown,
  ): void;
  checkAsync(
    method: string,
    args: ReadonlyArray<unknown>,
    result: unknown,
    error: unknown,
  ): Promise<void>;
}

type RustFileSystemOptions = Readonly<{
  /** The number of plugins that have a worker (data slots per file). */
  pluginCount: number,
  rootDir: string,
  /** Inform caches that there is new data to save. */
  onMetadata: () => void,
  verifier?: ?FileSystemVerifier,
}>;

export default class RustFileSystem implements FileSystem {
  /** The binding's object; what the Rust resolver reads the store from. */
  readonly native: NativeFileMap;
  readonly #options: RustFileSystemOptions;
  #fallback: ?TreeFS = null;
  #generation: number = 0;
  #fallbackGeneration: number = -1;
  // The `changedFiles` that `getDifference` returned last, so that FileMap can
  // tell that a crawler passed on the native file map's own difference.
  #lastDifference: ?FileData = null;

  constructor(native: NativeFileMap, options: RustFileSystemOptions) {
    this.native = native;
    this.#options = options;
  }

  /** The native file map changed: drop what was derived from it. */
  invalidate(): void {
    this.#generation++;
  }

  /**
   * Whether `changedFiles` is the one `getDifference` returned last, which
   * the native file map still holds as its pending difference.
   */
  isLastDifference(changedFiles: FileData): boolean {
    return this.#lastDifference === changedFiles;
  }

  #run<T>(method: string, args: ReadonlyArray<unknown>, call: () => T): T {
    const verifier = this.#options.verifier;
    if (verifier == null) {
      return call();
    }
    let result: T;
    try {
      result = call();
    } catch (error) {
      verifier.check(method, args, undefined, error);
      throw error;
    }
    verifier.check(method, args, result, undefined);
    return result;
  }

  exists(file: Path): boolean {
    return this.#run('exists', [file], () => this.native.exists(file));
  }

  getAllFiles(): Array<Path> {
    return this.#run('getAllFiles', [], () => this.native.getAllFiles());
  }

  getDifference(
    files: FileData,
    options?: Readonly<{subpath?: string}>,
  ): {
    changedFiles: FileData,
    removedFiles: Set<string>,
  } {
    const {pluginCount} = this.#options;
    const subpath = options?.subpath;
    return this.#run('getDifference', [files, options], () => {
      const result = this.native.getDifference(
        toCompactFiles(files, pluginCount),
        subpath ?? null,
      );
      const changedFiles = fromCompactFiles(result.changed, pluginCount);
      this.#lastDifference = changedFiles;
      return {changedFiles, removedFiles: new Set(result.removed)};
    });
  }

  getSerializableSnapshot(): CacheData['fileSystemData'] {
    return this.#run('getSerializableSnapshot', [], () =>
      buildTree(JSON.parse(this.native.getSerializableSnapshot())),
    );
  }

  getSize(file: Path): ?number {
    return this.#run('getSize', [file], () => this.native.getSize(file));
  }

  getSha1(file: Path): ?string {
    return this.#run('getSha1', [file], () => this.native.getSha1(file));
  }

  async getOrComputeSha1(
    file: Path,
  ): Promise<?{sha1: string, content?: Buffer}> {
    const verifier = this.#options.verifier;
    let result: ?{sha1: string, content?: Buffer};
    try {
      result = await this.#getOrComputeSha1(file);
    } catch (error) {
      await verifier?.checkAsync('getOrComputeSha1', [file], undefined, error);
      throw error;
    }
    await verifier?.checkAsync('getOrComputeSha1', [file], result, undefined);
    return result;
  }

  async #getOrComputeSha1(
    file: Path,
  ): Promise<?{sha1: string, content?: Buffer}> {
    const result = await this.native.getOrComputeSha1(file);
    if (result == null) {
      return null;
    }
    if (result.errorMessage != null) {
      const error = new Error(result.errorMessage);
      if (result.errorCode != null) {
        // $FlowFixMe[prop-missing] Node's errors have a code.
        error.code = result.errorCode;
      }
      throw error;
    }
    const {id, version, sha1} = result;
    invariant(sha1 != null, 'the native file map returned no SHA-1');
    if (id != null && version != null) {
      // Hashed on a worker thread; store it unless the file changed meanwhile.
      this.native.finishSha1(id, version, sha1);
      // Inform caches that there is new data to save.
      this.#options.onMetadata();
      return {content: result.content, sha1};
    }
    return {sha1};
  }

  hierarchicalLookup(
    mixedStartPath: string,
    subpath: string,
    opts: {
      breakOnSegment?: ?string,
      subpathType: 'f' | 'd',
    },
    observations?: ?Observations,
  ): ?{
    absolutePath: string,
    containerRelativePath: string,
  } {
    if (observations != null) {
      return this.#fallbackTree().hierarchicalLookup(
        mixedStartPath,
        subpath,
        opts,
        observations,
      );
    }
    return this.#run(
      'hierarchicalLookup',
      [mixedStartPath, subpath, opts],
      () => {
        const result = this.native.hierarchicalLookup(
          mixedStartPath,
          subpath,
          opts.breakOnSegment ?? null,
          opts.subpathType === 'd',
        );
        return result == null
          ? null
          : {
              absolutePath: result.absolutePath,
              containerRelativePath: result.containerRelativePath,
            };
      },
    );
  }

  linkStats(file: Path): ?FileStats {
    return this.#run('linkStats', [file], () => {
      const result = this.native.linkStats(file);
      return result == null
        ? null
        : {
            fileType: result.isSymlink ? 'l' : 'f',
            modifiedTime: result.modifiedTime ?? null,
            size: result.size,
          };
    });
  }

  lookup(mixedPath: Path, observations?: ?Observations): LookupResult {
    if (observations != null) {
      return this.#fallbackTree().lookup(mixedPath, observations);
    }
    return this.#run('lookup', [mixedPath], () => {
      const {kind, realPath} = this.native.lookup(mixedPath);
      if (kind === 0) {
        return {exists: false};
      }
      if (kind === 1) {
        return {exists: true, realPath, type: 'd'};
      }
      const native = this.native;
      let metadata: ?FileMetadata = null;
      const result: {
        exists: true,
        realPath: string,
        type: 'f',
        metadata: FileMetadata,
        // $FlowFixMe[incompatible-type] `metadata` is defined just below.
      } = {exists: true, realPath, type: 'f'};
      // The tuple is made when someone asks for it, and is a copy: the
      // metadata lives in Rust. Nothing in Metro reads it.
      Object.defineProperty(result, 'metadata', {
        configurable: true,
        enumerable: true,
        get(): FileMetadata {
          if (metadata == null) {
            const json = native.metadataTuple(realPath);
            metadata = JSON.parse(json ?? 'null') as FileMetadata;
          }
          return metadata;
        },
      });
      return result;
    });
  }

  matchFiles(opts: {
    filter?: RegExp | null,
    filterCompareAbsolute?: boolean,
    filterComparePosix?: boolean,
    follow?: boolean,
    recursive?: boolean,
    rootDir?: Path | null,
  }): Iterable<Path> {
    return this.#run('matchFiles', [opts], () => {
      const {
        filter = null,
        filterCompareAbsolute = false,
        filterComparePosix = false,
        follow = false,
        recursive = true,
        rootDir = null,
      } = opts;
      const args = {
        filterCompareAbsolute,
        filterComparePosix,
        follow,
        recursive,
        rootDir: rootDir ?? undefined,
      };
      const matches = this.native.matchFiles({
        ...args,
        filterSource: filter?.source,
        filterFlags: filter?.flags,
      });
      if (matches != null || filter == null) {
        return matches ?? [];
      }
      // The Rust engine does not run this RegExp (it has the g or y flag, so
      // `test` is stateful, or it uses syntax the engine rejects): filter here.
      const candidates =
        this.native.matchFiles({
          ...args,
          filterSource: undefined,
          filterFlags: undefined,
        }) ?? [];
      let contextRoot = this.#options.rootDir;
      if (rootDir != null) {
        const root = this.lookup(rootDir);
        if (!root.exists) {
          return [];
        }
        contextRoot = root.realPath;
      }
      return candidates.filter(candidate =>
        filter.test(
          filterCompareAbsolute
            ? candidate
            : './' + path.relative(contextRoot, candidate),
        ),
      );
    });
  }

  /**
   * A `TreeFS` with the current state, for the calls that pass observations.
   */
  #fallbackTree(): TreeFS {
    if (
      this.#fallback == null ||
      this.#fallbackGeneration !== this.#generation
    ) {
      this.#fallback = TreeFS.fromDeserializedSnapshot({
        fileSystemData: buildTree(
          JSON.parse(this.native.getSerializableSnapshot()),
        ),
        processFile: () => {
          throw new Error(
            'metro-file-map: this TreeFS is a snapshot of the Rust file map and cannot process files',
          );
        },
        rootDir: this.#options.rootDir,
      });
      this.#fallbackGeneration = this.#generation;
    }
    return this.#fallback;
  }
}

// `{e: [[name, node], ...]}` is a directory, an array is a file's metadata.
type SnapshotNode = {e: Array<[string, SnapshotNode | FileMetadata]>};
type DirectoryNode = Map<string, DirectoryNode | FileMetadata>;

export function buildTree(node: SnapshotNode): DirectoryNode {
  const directory: DirectoryNode = new Map();
  for (const [name, child] of node.e) {
    directory.set(name, Array.isArray(child) ? child : buildTree(child));
  }
  return directory;
}

/** `FileData` as the binding's parallel arrays. */
export function toCompactFiles(
  files: FileData,
  pluginCount: number,
): CompactFiles {
  const n = files.size;
  const paths = new Array<string>(n);
  const mtimes = new Float64Array(n);
  const sizes = new Float64Array(n);
  const visited = new Uint8Array(n);
  const hashes = new Array<?string>(n);
  const kinds = new Uint8Array(n);
  const targets: Array<string> = [];
  const pluginData: Array<?string> = [];
  let i = 0;
  for (const [filePath, metadata] of files) {
    paths[i] = filePath;
    mtimes[i] = metadata[0] ?? NaN;
    sizes[i] = metadata[1];
    visited[i] = metadata[2];
    hashes[i] = metadata[3];
    const symlink = metadata[4];
    if (typeof symlink === 'string') {
      kinds[i] = 2;
      targets.push(symlink);
    } else {
      kinds[i] = symlink;
    }
    for (let slot = 0; slot < pluginCount; slot++) {
      const value: unknown = metadata[5 + slot];
      pluginData.push(value === undefined ? null : JSON.stringify(value));
    }
    i++;
  }
  return {paths, mtimes, sizes, visited, hashes, kinds, targets, pluginData};
}

/**
 * The inverse of `toCompactFiles`. A plugin slot that is `undefined` ("no
 * worker has run") comes back as `undefined`, and trailing ones are left off.
 */
export function fromCompactFiles(
  compact: CompactFiles,
  pluginCount: number,
): FileData {
  const files: FileData = new Map();
  let target = 0;
  for (let i = 0; i < compact.paths.length; i++) {
    const kind = compact.kinds[i];
    const tuple: Array<unknown> = [
      Number.isNaN(compact.mtimes[i]) ? null : compact.mtimes[i],
      compact.sizes[i],
      compact.visited[i],
      compact.hashes[i] ?? null,
      kind === 2 ? compact.targets[target++] : kind,
    ];
    for (let slot = 0; slot < pluginCount; slot++) {
      const json = compact.pluginData[i * pluginCount + slot];
      tuple.push(json == null ? undefined : JSON.parse(json));
    }
    while (tuple.length > 5 && tuple[tuple.length - 1] === undefined) {
      tuple.pop();
    }
    // $FlowFixMe[invalid-tuple-arity] A tuple of the right shape.
    files.set(compact.paths[i], tuple);
  }
  return files;
}
