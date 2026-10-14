/**
 * Black-box: config loading from real files, and the on-disk transform cache.
 *
 * Config loading stays JavaScript in the port, but the resolved config is what
 * gets serialized into the Rust core, so its observable result is pinned here.
 * The cache tests use a real directory instead of spying on fs.
 */
'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {PACKAGES, createProject} = require('../helpers/project');

const {spawnSync} = require('node:child_process');
const {loadConfig, mergeConfig, getDefaultConfig} = require(path.join(PACKAGES, 'metro-config'));

/**
 * Loads a config in a plain Node child process. Needed for ESM configs: Jest's
 * VM sandbox cannot run dynamic import() without --experimental-vm-modules.
 */
function loadConfigInNode(configPath) {
  const script = `
    require(${JSON.stringify(path.join(PACKAGES, 'metro-babel-register'))}).unstable_registerForMetroMonorepo();
    require(${JSON.stringify(path.join(PACKAGES, 'metro-config'))})
      .loadConfig({config: ${JSON.stringify(configPath)}})
      .then(c => console.log(JSON.stringify({ok: true, port: c.server.port})))
      .catch(e => console.log(JSON.stringify({ok: false, message: String(e && e.message)})));`;
  const r = spawnSync(process.execPath, ['-e', script], {encoding: 'utf8'});
  return JSON.parse(r.stdout.trim().split('\n').pop());
}
const {Cache, FileStore} = require(path.join(PACKAGES, 'metro-cache'));

jest.useRealTimers();
jest.setTimeout(30000);

describe('loadConfig from real files', () => {
  let project;
  afterEach(() => project?.cleanup());

  test('loads an object export from an explicit path and fills in defaults', async () => {
    project = createProject({'metro.config.js': 'module.exports = {server: {port: 9999}, resolver: {sourceExts: ["js", "mine"]}};'});
    const config = await loadConfig({config: path.join(project.root, 'metro.config.js')});
    expect(config.server.port).toBe(9999);
    expect(config.resolver.sourceExts).toEqual(['js', 'mine']);
    // Untouched sections keep defaults.
    const defaults = await getDefaultConfig(project.root);
    expect(config.resolver.assetExts).toEqual(defaults.resolver.assetExts);
    expect(config.projectRoot).toBe(project.root);
  });

  test('a function export receives the default config and its return value is used', async () => {
    project = createProject({
      'metro.config.js': 'module.exports = defaults => ({resolver: {sourceExts: [...defaults.resolver.sourceExts, "extra"]}});',
    });
    const config = await loadConfig({config: path.join(project.root, 'metro.config.js')});
    expect(config.resolver.sourceExts[config.resolver.sourceExts.length - 1]).toBe('extra');
    expect(config.resolver.sourceExts).toContain('js');
  });

  test('async functions and promises are awaited', async () => {
    project = createProject({
      'a.config.js': 'module.exports = async () => ({server: {port: 1111}});',
      'b.config.js': 'module.exports = Promise.resolve({server: {port: 2222}});',
    });
    expect((await loadConfig({config: path.join(project.root, 'a.config.js')})).server.port).toBe(1111);
    expect((await loadConfig({config: path.join(project.root, 'b.config.js')})).server.port).toBe(2222);
  });

  test('ESM (.mjs) default exports and JSON configs load', async () => {
    project = createProject({
      'metro.config.mjs': 'export default {server: {port: 3333}};',
      'metro.config.json': JSON.stringify({server: {port: 4444}}),
    });
    expect(loadConfigInNode(path.join(project.root, 'metro.config.mjs'))).toEqual({ok: true, port: 3333});
    expect((await loadConfig({config: path.join(project.root, 'metro.config.json')})).server.port).toBe(4444);
  });

  test('searches from cwd upwards: metro.config.js beats a package.json "metro" key', async () => {
    project = createProject({
      'package.json': JSON.stringify({name: 'x', metro: {server: {port: 5555}}}),
      'sub/dir/file.txt': '',
    });
    const fromSub = await loadConfig({cwd: path.join(project.root, 'sub/dir')});
    expect(fromSub.server.port).toBe(5555);
    project.write('metro.config.js', 'module.exports = {server: {port: 6666}};');
    expect((await loadConfig({cwd: project.root})).server.port).toBe(6666);
  });

  test('finds .config/metro.js', async () => {
    project = createProject({'.config/metro.js': 'module.exports = {server: {port: 7777}};'});
    expect((await loadConfig({cwd: project.root})).server.port).toBe(7777);
  });

  test('command-line style arguments override the file', async () => {
    project = createProject({'metro.config.js': 'module.exports = {server: {port: 8888}, maxWorkers: 7};'});
    const config = await loadConfig({config: path.join(project.root, 'metro.config.js'), port: 1234, maxWorkers: 2, resetCache: true});
    expect(config.server.port).toBe(1234);
    expect(config.maxWorkers).toBe(2);
    expect(config.resetCache).toBe(true);
  });

  test('projectRoot is always the first watch folder', async () => {
    project = createProject({'other/x.js': '', 'metro.config.js': `module.exports = {watchFolders: [${JSON.stringify('PLACEHOLDER')}]};`});
    project.write('metro.config.js', `module.exports = {watchFolders: [${JSON.stringify(path.join(project.root, 'other'))}]};`);
    const config = await loadConfig({config: path.join(project.root, 'metro.config.js')});
    expect(config.watchFolders[0]).toBe(project.root);
    expect(config.watchFolders).toContain(path.join(project.root, 'other'));
  });

  test('a value of the wrong type is rejected', async () => {
    project = createProject({'metro.config.js': 'module.exports = {server: {port: "not-a-number"}};'});
    const warn = jest.spyOn(console, 'warn').mockImplementation(() => {});
    try {
      await expect(loadConfig({config: path.join(project.root, 'metro.config.js')})).rejects.toThrow(/port/);
    } finally {
      warn.mockRestore();
    }
  });

  test('a config file that throws is reported with its path and the original error', () => {
    project = createProject({'metro.config.js': 'throw new Error("CONFIG_EXPLODED");'});
    const r = loadConfigInNode(path.join(project.root, 'metro.config.js'));
    expect(r.ok).toBe(false);
    expect(r.message).toContain('CONFIG_EXPLODED');
  });
});

describe('mergeConfig', () => {
  test('merges sections deeply, replaces arrays, and later configs win', () => {
    const merged = mergeConfig(
      {resolver: {sourceExts: ['js'], assetExts: ['png']}, server: {port: 1}},
      {resolver: {sourceExts: ['ts']}, server: {port: 2}},
    );
    // mergeConfig always materialises resolver.schemeResolvers.
    expect(merged.resolver).toEqual({sourceExts: ['ts'], assetExts: ['png'], schemeResolvers: {}});
    expect(merged.server.port).toBe(2);
  });
});

describe('metro-cache FileStore and Cache on a real directory', () => {
  let root;
  beforeEach(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'metro-bb-store-'));
  });
  afterEach(() => fs.rmSync(root, {recursive: true, force: true}));

  const key = hex => Buffer.from(hex, 'hex');

  test('round-trips JSON values and Buffers', async () => {
    const store = new FileStore({root});
    await store.set(key('aa01'), {code: 'x', deps: [1, 2]});
    await store.set(key('aa02'), Buffer.from([0, 1, 2, 255]));
    expect(await store.get(key('aa01'))).toEqual({code: 'x', deps: [1, 2]});
    expect(await store.get(key('aa02'))).toEqual(Buffer.from([0, 1, 2, 255]));
  });

  test('a missing key reads as null', async () => {
    expect(await new FileStore({root}).get(key('bb01'))).toBeNull();
  });

  test('lays files out as <root>/<first key byte hex>/<rest of key hex>', async () => {
    await new FileStore({root}).set(key('c0ffee'), {v: 1});
    expect(fs.existsSync(path.join(root, 'c0', 'ffee'))).toBe(true);
  });

  test('a corrupt JSON entry reads as null instead of throwing', async () => {
    const store = new FileStore({root});
    await store.set(key('dd01'), {v: 1});
    fs.writeFileSync(path.join(root, 'dd', '01'), '{not json');
    expect(await store.get(key('dd01'))).toBeNull();
  });

  test('a second store instance on the same directory sees earlier writes (persistence)', async () => {
    await new FileStore({root}).set(key('ee01'), {persisted: true});
    expect(await new FileStore({root}).get(key('ee01'))).toEqual({persisted: true});
  });

  test('clear() removes all entries', async () => {
    const store = new FileStore({root});
    await store.set(key('ff01'), {v: 1});
    store.clear();
    expect(await store.get(key('ff01'))).toBeNull();
  });

  test('Cache reads through layers; set() backfills only layers before the hit when given the same key Buffer', async () => {
    const rootB = fs.mkdtempSync(path.join(os.tmpdir(), 'metro-bb-store-'));
    const rootC = fs.mkdtempSync(path.join(os.tmpdir(), 'metro-bb-store-'));
    try {
      const a = new FileStore({root});
      const b = new FileStore({root: rootB});
      const c = new FileStore({root: rootC});
      await b.set(key('1234'), {from: 'b'});
      const cache = new Cache([a, b, c]);
      const k = key('1234');
      expect(await cache.get(k)).toEqual({from: 'b'});
      await cache.set(k, {from: 'b'});
      expect(await a.get(key('1234'))).toEqual({from: 'b'});
      expect(await c.get(key('1234'))).toBeNull();
    } finally {
      fs.rmSync(rootB, {recursive: true, force: true});
      fs.rmSync(rootC, {recursive: true, force: true});
    }
  });

  test('Cache hit tracking is by Buffer identity: an equal but different Buffer writes every layer', async () => {
    const rootB = fs.mkdtempSync(path.join(os.tmpdir(), 'metro-bb-store-'));
    try {
      const a = new FileStore({root});
      const b = new FileStore({root: rootB});
      await a.set(key('5678'), {from: 'a'});
      const cache = new Cache([a, b]);
      expect(await cache.get(key('5678'))).toEqual({from: 'a'});
      await cache.set(key('5678'), {from: 'new'});
      expect(await a.get(key('5678'))).toEqual({from: 'new'});
      expect(await b.get(key('5678'))).toEqual({from: 'new'});
    } finally {
      fs.rmSync(rootB, {recursive: true, force: true});
    }
  });
});
