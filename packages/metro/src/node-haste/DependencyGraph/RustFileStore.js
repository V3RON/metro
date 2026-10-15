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

import type {ChangeEvent, FileSystem} from 'metro-file-map';

import H from 'metro-file-map/private/constants';
import normalizePathSeparatorsToPosix from 'metro-file-map/private/lib/normalizePathSeparatorsToPosix';
import {RootPathUtils} from 'metro-file-map/private/lib/RootPathUtils';
import fs from 'node:fs';
import path from 'node:path';

/**
 * A Rust read replica of the file map (a `FileStore` from metro-rs), used by
 * the Rust resolver to answer Metro's own `fileSystemLookup`, `doesFileExist`,
 * `getPackage` and `getPackageForModule` callbacks natively. It also holds
 * the package.json cache those callbacks use. See port/design/boundary.md.
 *
 * Built in one crossing from the JS file map, and kept current from each of
 * its change batches.
 */

type NativeFileStore = {
  rebuild(paths: string, kinds: Uint8Array, symlinkTargets: string): void,
  invalidatePackages(absolutePaths: Array<string>): void,
  applyChanges(
    removedFiles: Array<string>,
    removedDirectories: Array<string>,
    addedDirectories: Array<string>,
    changedFiles: Array<string>,
    changedKinds: Uint8Array,
    changedSymlinkTargets: Array<string>,
  ): void,
  ...
};

type Binding = {
  FileStore: Class<NativeFileStore> & {new(rootDir: string): NativeFileStore},
  ...
};

// Entry kinds. Keep in sync with metro-rs/crates/binding/src/resolver.rs.
const KIND_FILE = 0;
const KIND_SYMLINK = 1;

type TreeFSEntries = {
  metadataIterator(opts: {
    includeSymlinks: boolean,
    includeNodeModules: boolean,
  }): Iterable<{canonicalPath: string, metadata: ReadonlyArray<unknown>}>,
};

export default class RustFileStore {
  readonly native: NativeFileStore;
  #fileSystem: FileSystem;
  #pathUtils: RootPathUtils;

  constructor(binding: Binding, rootDir: string, fileSystem: FileSystem) {
    this.native = new binding.FileStore(rootDir);
    this.#fileSystem = fileSystem;
    this.#pathUtils = new RootPathUtils(rootDir);
    this.rebuild();
  }

  /**
   * Loads every file and symlink of the JS file map, in one crossing: the
   * normal paths joined with \0, a kind per path, and the symlink targets
   * (normal POSIX paths, as the file map stores them) joined with \0.
   * The store's package.json cache is kept, like the JS PackageCache.
   */
  rebuild(): void {
    // Metro's TreeFS has this, and so do file maps built on it (Expo's).
    const treeFS: TreeFSEntries = this.#fileSystem as any;
    const paths: Array<string> = [];
    const kinds: Array<number> = [];
    const symlinkTargets: Array<string> = [];
    // Symlinks whose target the file map has not read yet (H.SYMLINK === 1).
    // Metro's own file map reads every link while it builds, but other file
    // maps (Expo's) read them lazily, on first traversal. They are read after
    // the iteration, which reading could disturb.
    const unread: Array<{
      canonicalPath: string,
      metadata: ReadonlyArray<unknown>,
    }> = [];
    for (const {canonicalPath, metadata} of treeFS.metadataIterator({
      includeNodeModules: true,
      includeSymlinks: true,
    })) {
      const symlink = metadata[H.SYMLINK];
      if (symlink === 0) {
        paths.push(canonicalPath);
        kinds.push(KIND_FILE);
      } else if (typeof symlink === 'string') {
        paths.push(canonicalPath);
        kinds.push(KIND_SYMLINK);
        symlinkTargets.push(symlink);
      } else {
        unread.push({canonicalPath, metadata});
      }
    }
    for (const {canonicalPath, metadata} of unread) {
      const target = this.#readSymlink(canonicalPath, metadata);
      // A link that is gone is left out, as a lookup would not find it.
      if (target != null) {
        paths.push(canonicalPath);
        kinds.push(KIND_SYMLINK);
        symlinkTargets.push(target);
      }
    }
    this.native.rebuild(
      paths.join('\0'),
      Uint8Array.from(kinds),
      symlinkTargets.join('\0'),
    );
  }

  /**
   * The target of a symlink the file map has not read yet, as a normal POSIX
   * path like the ones the file map stores, or null when the link is gone.
   */
  #readSymlink(
    canonicalPath: string,
    metadata: ReadonlyArray<unknown>,
  ): ?string {
    const absolutePath = this.#pathUtils.normalToAbsolute(canonicalPath);
    try {
      this.#fileSystem.lookup(absolutePath);
    } catch {
      // A file map that cannot look it up still has the fallback below.
    }
    const stored = metadata[H.SYMLINK];
    if (typeof stored === 'string') {
      return stored;
    }
    let readlinkResult;
    try {
      readlinkResult = fs.readlinkSync(absolutePath);
    } catch {
      return null;
    }
    return normalizePathSeparatorsToPosix(
      this.#pathUtils.resolveSymlinkToNormal(canonicalPath, readlinkResult),
    );
  }

  /**
   * Applies a change batch of the file map, which has already been applied to
   * the JS file map. Change events do not carry symlink targets, so a batch
   * that adds or modifies a symlink reloads everything instead.
   */
  applyChanges({changes, rootDir}: ChangeEvent): void {
    const changedFiles: Array<string> = [];
    const changedKinds: Array<number> = [];
    for (const [canonicalPath, metadata] of [
      ...changes.addedFiles,
      ...changes.modifiedFiles,
    ]) {
      if (metadata.isSymlink) {
        this.rebuild();
        // The rebuild keeps the package.json cache, so invalidate as
        // DependencyGraph._onHasteChange does for the JS one.
        this.native.invalidatePackages(
          [
            ...changes.addedFiles,
            ...changes.modifiedFiles,
            ...changes.removedFiles,
          ].map(([changedPath]) => path.join(rootDir, changedPath)),
        );
        return;
      }
      changedFiles.push(canonicalPath);
      changedKinds.push(KIND_FILE);
    }
    this.native.applyChanges(
      Array.from(changes.removedFiles, ([canonicalPath]) => canonicalPath),
      Array.from(changes.removedDirectories),
      Array.from(changes.addedDirectories),
      changedFiles,
      Uint8Array.from(changedKinds),
      [],
    );
  }
}
