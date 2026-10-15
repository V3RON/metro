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

import RustFileStore from '../DependencyGraph/RustFileStore';
import H from 'metro-file-map/private/constants';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

// What the Rust replica is loaded with, for file maps whose symlink targets
// are not all read yet. Metro's own TreeFS reads every link while it builds,
// so this state only comes from other file maps (Expo's reads links lazily,
// on first traversal). Observed through the one crossing to the native store.

const SYMLINK = H.SYMLINK;
const KIND_FILE = 0;
const KIND_SYMLINK = 1;

function metadata(symlink: 0 | 1 | string): Array<unknown> {
  const m: Array<unknown> = [0, 0, 0, null, 0, null];
  m[SYMLINK] = symlink;
  return m;
}

describe('RustFileStore.rebuild', () => {
  let rootDir: string;
  let rebuilds: Array<{
    paths: Array<string>,
    kinds: Array<number>,
    targets: Array<string>,
  }>;

  class FakeNativeFileStore {
    rebuild(paths: string, kinds: Uint8Array, symlinkTargets: string) {
      const split = (s: string): Array<string> =>
        s === '' ? [] : s.split('\0');
      rebuilds.push({
        paths: split(paths),
        kinds: [...kinds],
        targets: split(symlinkTargets),
      });
    }
    invalidatePackages() {}
    applyChanges() {}
  }
  const binding = {FileStore: FakeNativeFileStore};

  beforeEach(() => {
    rootDir = fs.realpathSync(
      fs.mkdtempSync(path.join(os.tmpdir(), 'metro-rust-file-store-')),
    );
    rebuilds = [];
  });
  afterEach(() => {
    fs.rmSync(rootDir, {recursive: true, force: true});
  });

  // A file system with the given entries, whose lookup() reads a link the way
  // Expo's TreeFS does when `readsOnLookup` is true.
  function fileSystem(
    entries: {[string]: Array<unknown>},
    readsOnLookup: boolean,
  ) {
    const lookups: Array<string> = [];
    return {
      lookups,
      metadataIterator: () =>
        Object.entries(entries).map(([canonicalPath, m]) => ({
          canonicalPath,
          metadata: m,
        })),
      lookup(absolutePath: string) {
        lookups.push(absolutePath);
        const canonicalPath = path.relative(rootDir, absolutePath);
        const m = entries[canonicalPath];
        if (readsOnLookup && m != null && m[SYMLINK] === 1) {
          m[SYMLINK] = path.posix.normalize(
            path.posix.join(
              path.posix.dirname(canonicalPath),
              fs.readlinkSync(absolutePath),
            ),
          );
        }
        return {exists: false};
      },
    };
  }

  function linkWorkspace() {
    fs.mkdirSync(path.join(rootDir, 'modules/handler'), {recursive: true});
    fs.mkdirSync(path.join(rootDir, 'node_modules/@scope'), {recursive: true});
    fs.symlinkSync(
      '../../modules/handler',
      path.join(rootDir, 'node_modules/@scope/handler'),
    );
  }

  test('a link the file map has not read is read through lookup()', () => {
    linkWorkspace();
    const fsys = fileSystem(
      {
        'modules/handler/index.js': metadata(0),
        'node_modules/@scope/handler': metadata(1),
        'node_modules/read': metadata('modules/handler'),
      },
      true,
    );
    // $FlowFixMe[incompatible-type] a fake file system
    const store = new RustFileStore(binding, rootDir, fsys);
    expect(store.native).toBeInstanceOf(FakeNativeFileStore);
    expect(fsys.lookups).toEqual([
      path.join(rootDir, 'node_modules/@scope/handler'),
    ]);
    expect(rebuilds).toEqual([
      {
        paths: [
          'modules/handler/index.js',
          'node_modules/read',
          'node_modules/@scope/handler',
        ],
        kinds: [KIND_FILE, KIND_SYMLINK, KIND_SYMLINK],
        targets: ['modules/handler', 'modules/handler'],
      },
    ]);
  });

  test('a link that lookup() does not read is read from disk, resolved like the file map does', () => {
    linkWorkspace();
    const fsys = fileSystem(
      {'node_modules/@scope/handler': metadata(1)},
      false,
    );
    // $FlowFixMe[incompatible-type] a fake file system
    const store = new RustFileStore(binding, rootDir, fsys);
    expect(store.native).toBeInstanceOf(FakeNativeFileStore);
    expect(rebuilds[0]).toEqual({
      paths: ['node_modules/@scope/handler'],
      kinds: [KIND_SYMLINK],
      targets: ['modules/handler'],
    });
  });

  test('a link that is gone is left out', () => {
    const fsys = fileSystem(
      {'a.js': metadata(0), 'gone-link': metadata(1)},
      false,
    );
    // $FlowFixMe[incompatible-type] a fake file system
    const store = new RustFileStore(binding, rootDir, fsys);
    expect(store.native).toBeInstanceOf(FakeNativeFileStore);
    expect(rebuilds[0]).toEqual({
      paths: ['a.js'],
      kinds: [KIND_FILE],
      targets: [],
    });
  });
});
