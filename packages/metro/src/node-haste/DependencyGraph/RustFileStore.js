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
  FileStore: Class<NativeFileStore> & {new (rootDir: string): NativeFileStore},
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

  constructor(binding: Binding, rootDir: string, fileSystem: FileSystem) {
    this.native = new binding.FileStore(rootDir);
    this.#fileSystem = fileSystem;
    this.rebuild();
  }

  /**
   * Loads every file and symlink of the JS file map, in one crossing: the
   * normal paths joined with \0, a kind per path, and the symlink targets
   * (normal POSIX paths, as the file map stores them) joined with \0.
   */
  rebuild(): void {
    // TreeFS, the only FileSystem implementation, has this.
    const treeFS: TreeFSEntries = this.#fileSystem as any;
    const paths: Array<string> = [];
    const kinds: Array<number> = [];
    const symlinkTargets: Array<string> = [];
    for (const {canonicalPath, metadata} of treeFS.metadataIterator({
      includeNodeModules: true,
      includeSymlinks: true,
    })) {
      paths.push(canonicalPath);
      const symlink = metadata[H.SYMLINK];
      if (typeof symlink === 'string') {
        kinds.push(KIND_SYMLINK);
        symlinkTargets.push(symlink);
      } else {
        kinds.push(KIND_FILE);
      }
    }
    this.native.rebuild(
      paths.join('\0'),
      Uint8Array.from(kinds),
      symlinkTargets.join('\0'),
    );
  }

  /**
   * Applies a change batch of the file map, which has already been applied to
   * the JS file map. Change events do not carry symlink targets, so a batch
   * that adds or modifies a symlink reloads everything instead.
   */
  applyChanges({changes}: ChangeEvent): void {
    const changedFiles: Array<string> = [];
    const changedKinds: Array<number> = [];
    for (const [canonicalPath, metadata] of [
      ...changes.addedFiles,
      ...changes.modifiedFiles,
    ]) {
      if (metadata.isSymlink) {
        this.rebuild();
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

