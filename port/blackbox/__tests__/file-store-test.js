/**
 * Black-box tests of metro-cache's FileStore and AutoCleanFileStore on a real
 * temporary directory, through the package's public exports only.
 *
 * Every case of the package's own FileStore-test.js and
 * AutoCleanFileStore-test.js, which replace node:fs with memfs, so they only
 * test the JavaScript implementation (Szymon's decision, 2026-10-14: those two
 * files are JS-only for the transform-cache component, and their cases run
 * here against the real disk in both modes).
 */
'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {PACKAGES} = require('../helpers/project');

const {AutoCleanFileStore, FileStore} = require(path.join(PACKAGES, 'metro-cache'));

const KEY = Buffer.from([0xfa, 0xce, 0xb0, 0x0c]);

// The one file under `root`, found by walking the directory (the entry's path
// is FileStore's own business).
function onlyFile(root) {
  const files = fs
    .readdirSync(root, {recursive: true, withFileTypes: true})
    .filter(dirent => dirent.isFile())
    .map(dirent => path.join(dirent.parentPath, dirent.name));
  expect(files).toHaveLength(1);
  return files[0];
}

describe('FileStore on a real directory', () => {
  let root;
  beforeEach(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'metro-bb-filestore-'));
  });
  afterEach(() => {
    fs.rmSync(root, {recursive: true, force: true});
  });

  test('sets and writes into the cache', async () => {
    const fileStore = new FileStore({root});
    await fileStore.set(KEY, {foo: 42});
    expect(await fileStore.get(KEY)).toEqual({foo: 42});
  });

  test('returns null when reading a non-existing file', async () => {
    const fileStore = new FileStore({root});
    expect(await fileStore.get(KEY)).toEqual(null);
  });

  test('returns null when reading an empty file', async () => {
    const fileStore = new FileStore({root});
    await fileStore.set(KEY, {foo: 42});
    fs.writeFileSync(onlyFile(root), '');
    expect(await fileStore.get(KEY)).toEqual(null);
  });

  test('writes into cache if folder is missing', async () => {
    const fileStore = new FileStore({root});
    const data = Buffer.from([0xca, 0xc4, 0xe5]);
    fs.rmSync(root, {recursive: true, force: true});
    await fileStore.set(KEY, data);
    expect(await fileStore.get(KEY)).toEqual(data);
  });

  test('reads and writes binary data', async () => {
    const fileStore = new FileStore({root});
    const data = Buffer.from([0xca, 0xc4, 0xe5]);
    await fileStore.set(KEY, data);
    expect(await fileStore.get(KEY)).toEqual(data);
  });
});

describe('AutoCleanFileStore on a real directory', () => {
  let root;
  beforeEach(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'metro-bb-autoclean-'));
  });
  afterEach(() => {
    jest.useRealTimers();
    fs.rmSync(root, {recursive: true, force: true});
  });

  test('sets and writes into the cache, then cleans up stale entries', async () => {
    // Fake timers also fake Date.now, which the cleanup compares with the
    // file's real mtime. The margins (an entry written at T0 plus a few real
    // ms, an interval of 1 s, a threshold of 1.5 s) keep that comparison
    // unambiguous.
    jest.useFakeTimers();
    const fileStore = new AutoCleanFileStore({
      root,
      intervalMs: 1000,
      cleanupThresholdMs: 1500,
    });

    await fileStore.set(KEY, {foo: 42});
    expect(await fileStore.get(KEY)).toEqual({foo: 42});

    // Before the first cleanup, the entry is cached.
    jest.advanceTimersByTime(600);
    expect(await fileStore.get(KEY)).toEqual({foo: 42});

    // The first cleanup (at 1 s) finds the entry fresh enough.
    jest.advanceTimersByTime(500);
    expect(await fileStore.get(KEY)).toEqual({foo: 42});

    // The second cleanup (at 2 s) finds it older than the threshold.
    jest.advanceTimersByTime(1000);
    expect(await fileStore.get(KEY)).toEqual(null);
  });

  test('returns null when reading a non-existing file', async () => {
    jest.useFakeTimers();
    const fileStore = new AutoCleanFileStore({root});
    expect(await fileStore.get(KEY)).toEqual(null);
  });
});
