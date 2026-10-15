/**
 * Black-box: how the module graph reacts to edits, seen through the dev server.
 *
 * Pins the graph topology work that phase 4 moves to Rust: cycles that become
 * garbage, async imports, require.context, modules deleted and added back,
 * shared modules, entry points. Observed only through bundles over HTTP, and
 * checked against a fresh server on the same files (the set of modules in an
 * incrementally updated bundle must equal a from-scratch build).
 */
'use strict';

const {createProject, execBundle, fetchFresh, startServer, waitFor} = require('../helpers/project');

jest.useRealTimers();
jest.setTimeout(120000);

const BUNDLE = '/index.bundle?platform=ios&dev=true&minify=false';

describe('graph topology through the dev server', () => {
  let project, server;

  const get = async (p = BUNDLE) => {
    const res = await fetchFresh(server.url(p));
    return {status: res.status, body: await res.text()};
  };
  const value = async (p = BUNDLE) => {
    const {status, body} = await get(p);
    return status === 200 ? execBundle(body).result : {status};
  };
  const waitForValue = (expected, label, p) =>
    waitFor(async () => JSON.stringify(await value(p)) === JSON.stringify(expected), {label});
  // The module paths declared in a bundle, in order.
  const modules = body =>
    [...body.matchAll(/__d\(function[\s\S]*?,\s*(\d+),\s*\[[^\]]*\],\s*"([^"]+)"/g)].map(m => m[2]);

  async function freshModules(p = BUNDLE) {
    const other = await startServer(await project.loadConfig(CONFIG));
    try {
      const res = await fetchFresh(other.url(p));
      return modules(await res.text());
    } finally {
      await other.close();
    }
  }
  async function expectSameModulesAsFresh(p = BUNDLE) {
    const incremental = modules((await get(p)).body);
    expect([...incremental].sort()).toEqual([...(await freshModules(p))].sort());
  }

  const CONFIG = {transformer: {unstable_allowRequireContext: true}};

  async function start(files) {
    project = createProject(files);
    server = await startServer(await project.loadConfig(CONFIG));
  }

  afterEach(async () => {
    await server.close();
    project.cleanup();
  });

  test('a cycle that loses its only importer is dropped from the bundle', async () => {
    await start({
      'index.js': "global.result = require('./a').name;",
      'a.js': "exports.name = 'a'; require('./b');",
      'b.js': "exports.name = 'MARKER_CYCLE_B'; require('./a');",
    });
    expect(await value()).toBe('a');
    expect((await get()).body).toContain('MARKER_CYCLE_B');
    project.write('index.js', "global.result = 'alone';");
    await waitForValue('alone', 'cycle removed');
    expect((await get()).body).not.toContain('MARKER_CYCLE_B');
    await expectSameModulesAsFresh();
  });

  test('a cycle that is still reachable from elsewhere stays', async () => {
    await start({
      'index.js': "global.result = require('./a').name + require('./c').name;",
      'a.js': "exports.name = 'a'; require('./b');",
      'b.js': "exports.name = 'b'; require('./a');",
      'c.js': "exports.name = 'c'; require('./b');",
    });
    expect(await value()).toBe('ac');
    project.write('index.js', "global.result = require('./c').name;");
    await waitForValue('c', 'a removed');
    expect((await get()).body).toContain("exports.name = 'b'");
    await expectSameModulesAsFresh();
    project.write('c.js', "exports.name = 'c';");
    await waitFor(async () => !(await get()).body.includes("exports.name = 'b'"), {label: 'c cut off'});
    await expectSameModulesAsFresh();
  });

  test('async imports join the bundle and leave it with their importer', async () => {
    await start({
      'index.js': "global.result = 'x'; import('./lazy');",
      'lazy.js': "module.exports = 'MARKER_LAZY';",
    });
    expect((await get()).body).toContain('MARKER_LAZY');
    project.write('index.js', "global.result = 'y';");
    await waitForValue('y', 'async import removed');
    expect((await get()).body).not.toContain('MARKER_LAZY');
    project.write('index.js', "global.result = 'z'; import('./lazy');");
    await waitForValue('z', 'async import back');
    expect((await get()).body).toContain('MARKER_LAZY');
    await expectSameModulesAsFresh();
  });

  test('require.context follows files added to and removed from the directory', async () => {
    await start({
      'index.js': "const c = require.context('./ctx'); global.result = c.keys().sort();",
      'ctx/one.js': 'module.exports = 1;',
    });
    expect(await value()).toEqual(['./one.js']);
    project.write('ctx/two.js', 'module.exports = 2;');
    await waitForValue(['./one.js', './two.js'], 'file added to context');
    project.remove('ctx/one.js');
    await waitForValue(['./two.js'], 'file removed from context');
    await expectSameModulesAsFresh();
  });

  test('a module deleted and added back is restored with its dependencies', async () => {
    await start({
      'index.js': "global.result = require('./a').v + require('./b').v;",
      'a.js': "exports.v = require('./shared').v;",
      'b.js': "exports.v = require('./shared').v;",
      'shared.js': "exports.v = 'S';",
    });
    expect(await value()).toBe('SS');
    project.write('a.js', "exports.v = 'A';");
    await waitForValue('AS', 'a drops shared');
    project.write('b.js', "exports.v = 'B';");
    await waitForValue('AB', 'b drops shared');
    expect((await get()).body).not.toContain("exports.v = 'S'");
    project.write('a.js', "exports.v = require('./shared').v;");
    await waitForValue('SB', 'shared is back');
    await expectSameModulesAsFresh();
  });

  test('reordering and duplicating imports keeps every module once', async () => {
    await start({
      'index.js': "global.result = [require('./a').v, require('./b').v, require('./a').v];",
      'a.js': "exports.v = 'a';",
      'b.js': "exports.v = 'b';",
    });
    expect(await value()).toEqual(['a', 'b', 'a']);
    project.write('index.js', "global.result = [require('./b').v, require('./a').v];");
    await waitForValue(['b', 'a'], 'reordered');
    await expectSameModulesAsFresh();
  });

  test('two entry points share modules and update independently', async () => {
    await start({
      'index.js': "global.result = 'i' + require('./shared').v;",
      'other.js': "global.result = 'o' + require('./shared').v;",
      'shared.js': 'exports.v = 1;',
    });
    const other = '/other.bundle?platform=ios&dev=true&minify=false';
    expect(await value(other)).toBe('o1');
    project.write('shared.js', 'exports.v = 2;');
    await waitForValue('i2', 'index updated');
    await waitForValue('o2', 'other updated', other);
    project.write('other.js', "global.result = 'o';");
    await waitForValue('o', 'other drops shared', other);
    expect(await value()).toBe('i2');
    await expectSameModulesAsFresh(other);
  });

  test('a resolution error in a changed module rolls the graph back and recovers', async () => {
    await start({
      'index.js': "global.result = require('./a').v;",
      'a.js': "exports.v = 'a1'; require('./b');",
      'b.js': "exports.v = 'b';",
    });
    expect(await value()).toBe('a1');
    project.write('a.js', "exports.v = 'a2'; require('./b'); require('./missing');");
    await waitFor(async () => (await get()).status === 500, {label: 'error'});
    project.write('missing.js', 'exports.m = 1;');
    await waitForValue('a2', 'recovered');
    await expectSameModulesAsFresh();
  });
});
