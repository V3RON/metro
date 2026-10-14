/**
 * Language-neutral resolver conformance runner (JS side).
 *
 * Reads ../fixtures/resolver.json and runs every case through
 * metro-resolver's public `resolve()`. A Rust harness reads the same file.
 * UPDATE_FIXTURES=1 records the current JS result into each case, which is how
 * the remaining JS-only resolver tests can be converted in bulk.
 */
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const {PACKAGES} = require('../helpers/project');

const Resolver = require(path.join(PACKAGES, 'metro-resolver'));
const {createResolutionContext} = require(path.join(PACKAGES, 'metro-resolver/src/__tests__/utils'));

const FIXTURES = path.join(__dirname, '..', 'fixtures', 'resolver.json');
const data = JSON.parse(fs.readFileSync(FIXTURES, 'utf8'));
const UPDATE = process.env.UPDATE_FIXTURES === '1';
// Fixtures use POSIX paths; on Windows prefix a drive letter.
const p = posix => (path.sep === '/' ? posix : 'C:' + posix.split('/').join('\\'));
const unp = native => (path.sep === '/' ? native : native.replace(/^C:/, '').split('\\').join('/'));

function buildContext(c) {
  const files = {};
  for (const [k, v] of Object.entries(c.files)) {
    if (v != null && typeof v === 'object' && 'realPath' in v) {
      files[p(k)] = {realPath: p(v.realPath)};
    } else {
      files[p(k)] = typeof v === 'string' ? v : JSON.stringify(v);
    }
  }
  const overrides = {...(c.context ?? {})};
  if (overrides.nodeModulesPaths) overrides.nodeModulesPaths = overrides.nodeModulesPaths.map(p);
  if (overrides.extraNodeModules) {
    overrides.extraNodeModules = Object.fromEntries(Object.entries(overrides.extraNodeModules).map(([k, v]) => [k, p(v)]));
  }
  if (overrides.unstable_conditionNames == null && overrides.unstable_enablePackageExports) {
    overrides.unstable_conditionNames = ['require'];
  }
  const ctx = {
    ...createResolutionContext(files),
    ...overrides,
    originModulePath: p(c.origin),
  };
  if (c.haste?.modules) {
    ctx.resolveHasteModule = name => (c.haste.modules[name] ? p(c.haste.modules[name]) : null);
  }
  if (c.assets) {
    ctx.resolveAsset = (dirPath, basename, extension) =>
      c.assets[unp(path.join(dirPath, basename + extension))]?.map(p) ?? null;
  }
  return ctx;
}

function run(c) {
  try {
    const r = Resolver.resolve(buildContext(c), c.request, c.platform);
    const out = {...r};
    if (out.filePath) out.filePath = unp(out.filePath);
    if (out.filePaths) out.filePaths = out.filePaths.map(unp);
    return {expect: out};
  } catch (e) {
    return {expectError: e.constructor.name};
  }
}

afterAll(() => {
  if (UPDATE) {
    for (const c of data.cases) {
      delete c.expect;
      delete c.expectError;
      Object.assign(c, run(c));
    }
    fs.writeFileSync(FIXTURES, JSON.stringify(data, null, 2) + '\n');
  }
});

describe('resolver conformance fixtures', () => {
  test.each(data.cases.map(c => [c.name, c]))('%s', (_name, c) => {
    const actual = run(c);
    if (UPDATE) return;
    if (c.expectError) {
      expect(actual).toEqual({expectError: c.expectError});
    } else {
      expect(actual).toEqual({expect: c.expect});
    }
  });
});
