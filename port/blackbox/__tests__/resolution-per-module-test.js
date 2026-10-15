/**
 * Black-box: how dependencies of one module resolve, observed through
 * Metro.runBuild and the reporter.
 *
 * Pins what batch resolution per module (phase 4) must keep: the outcome of
 * each kind of resolution (source file, asset, empty module), the order of
 * resolver warnings, optional dependencies, the first error of a module,
 * require.context next to ordinary dependencies, a custom resolveRequest, and
 * that edits invalidate earlier resolutions (a failure followed by a fix).
 */
'use strict';

const path = require('node:path');
const {createProject, startServer, fetchFresh, execBundle, waitFor, Metro} = require('../helpers/project');
const png = require('../helpers/png');

jest.useRealTimers();
jest.setTimeout(90000);

describe('resolution of the dependencies of a module', () => {
  let project;
  let events;

  afterEach(() => {
    project?.cleanup();
    project = null;
  });

  async function configFor(overrides = {}) {
    events = [];
    return project.loadConfig({
      reporter: {update: event => events.push(event)},
      ...overrides,
    });
  }

  async function build(overrides = {}, entry = 'index.js', platform = 'ios') {
    const config = await configFor(overrides);
    return Metro.runBuild(config, {
      entry,
      dev: true,
      minify: false,
      platform,
      sourceMap: false,
    });
  }

  const warnings = () =>
    events.filter(e => e.type === 'resolver_warning').map(e => e.message);

  test('source files, platform files, directories and packages resolve to the same bundle', async () => {
    project = createProject({
      'index.js':
        "global.result = [require('./a'), require('./b'), require('./dir'), require('pkg'), require('./a')];",
      'a.js': "module.exports = 'a';",
      'b.js': "module.exports = 'b-default';",
      'b.ios.js': "module.exports = 'b-ios';",
      'dir/index.js': "module.exports = 'dir';",
      'node_modules/pkg/package.json': JSON.stringify({name: 'pkg', main: 'lib/main.js'}),
      'node_modules/pkg/lib/main.js': "module.exports = 'pkg';",
    });
    const ios = await build();
    expect(execBundle(ios.code).result).toEqual(['a', 'b-ios', 'dir', 'pkg', 'a']);
    const android = await build({}, 'index.js', 'android');
    expect(execBundle(android.code).result).toEqual(['a', 'b-default', 'dir', 'pkg', 'a']);
  });

  test('an unavailable optional dependency is skipped, the others still resolve', async () => {
    project = createProject({
      'index.js': [
        'let missing = null;',
        "try { missing = require('./missing'); } catch (e) { missing = 'caught'; }",
        "global.result = [missing, require('./a')];",
      ].join('\n'),
      'a.js': "module.exports = 'a';",
    });
    const {code} = await build();
    expect(execBundle(code, {}).result).toEqual(['caught', 'a']);
  });

  test('the first unresolvable dependency of a module is the error, and the ones before it are not reported', async () => {
    project = createProject({
      'index.js':
        "require('./a'); require('./nope-one'); require('./nope-two'); require('./a');",
      'a.js': "module.exports = 'a';",
    });
    let error;
    try {
      await build();
    } catch (e) {
      error = e;
    }
    expect(error).toBeDefined();
    expect(error.constructor.name).toBe('UnableToResolveError');
    expect(error.message).toContain('./nope-one');
    expect(error.message.split('\n')[0]).not.toContain('./nope-two');
    expect(error.originModulePath).toBe(path.join(project.root, 'index.js'));
    expect(error.targetModuleName).toBe('./nope-one');
  });

  test('assets resolve to the lowest-sorted file, and the module records both scales', async () => {
    project = createProject({
      'AssetRegistry.js': 'module.exports = {registerAsset: asset => asset};',
      'index.js': "global.result = [require('./img.png'), require('./a')];",
      'a.js': "module.exports = 'a';",
    });
    require('node:fs').writeFileSync(path.join(project.root, 'img.png'), png(1, 1));
    require('node:fs').writeFileSync(path.join(project.root, 'img@2x.png'), png(2, 2));
    const {code} = await build({
      transformer: {assetRegistryPath: path.join(project.root, 'AssetRegistry.js')},
    });
    const result = execBundle(code).result;
    expect(result[0]).toMatchObject({__packager_asset: true, scales: [1, 2]});
    expect(result[1]).toBe('a');
  });

  test('a file that a package maps to false resolves to the empty module', async () => {
    project = createProject({
      'index.js': "global.result = [require('./a'), require('./skipped'), require('./b')];",
      'a.js': "module.exports = 'a';",
      'b.js': "module.exports = 'b';",
      'skipped.js': "module.exports = 'must not be bundled';",
      'package.json': JSON.stringify({name: 'app', browser: {'./skipped.js': false}}),
    });
    const {code} = await build();
    expect(code).not.toContain('must not be bundled');
    expect(execBundle(code).result).toEqual(['a', {}, 'b']);
  });

  test('warnings of a module come out once each, in the order of its dependencies', async () => {
    project = createProject({
      'index.js': [
        "require('pkg/lib/one');",
        "require('./a');",
        "require('pkg/lib/two');",
        "global.result = [require('pkg/lib/one'), require('pkg/lib/two')];",
      ].join('\n'),
      'a.js': "module.exports = 'a';",
      'node_modules/pkg/package.json': JSON.stringify({
        name: 'pkg',
        exports: {'.': './index.js'},
      }),
      'node_modules/pkg/index.js': 'module.exports = 0;',
      'node_modules/pkg/lib/one.js': "module.exports = 'one';",
      'node_modules/pkg/lib/two.js': "module.exports = 'two';",
    });
    const {code} = await build({resolver: {unstable_enablePackageExports: true}});
    expect(execBundle(code).result).toEqual(['one', 'two']);
    const messages = warnings();
    // Two distinct targets: one warning each, one run each (the second use of
    // a target is a cache hit and warns no more).
    expect(messages).toHaveLength(2);
    expect(messages[0]).toContain('./lib/one');
    expect(messages[1]).toContain('./lib/two');
  });

  test('a warning from a dependency that then fails is reported once', async () => {
    project = createProject({
      'index.js': "require('pkg/lib/one'); require('./nope');",
      'node_modules/pkg/package.json': JSON.stringify({
        name: 'pkg',
        exports: {'.': './index.js'},
      }),
      'node_modules/pkg/index.js': 'module.exports = 0;',
      'node_modules/pkg/lib/one.js': "module.exports = 'one';",
    });
    await expect(
      build({resolver: {unstable_enablePackageExports: true}}),
    ).rejects.toThrow('./nope');
    expect(warnings()).toHaveLength(1);
  });

  test('require.context next to ordinary dependencies', async () => {
    project = createProject({
      'index.js': [
        "const ctx = require.context('./ctx', false, /\\.js$/);",
        "global.result = [require('./a'), ctx.keys().sort(), require('./b')];",
      ].join('\n'),
      'a.js': "module.exports = 'a';",
      'b.js': "module.exports = 'b';",
      'ctx/x.js': "module.exports = 'x';",
      'ctx/y.js': "module.exports = 'y';",
    });
    const {code} = await build({transformer: {unstable_allowRequireContext: true}});
    expect(execBundle(code).result).toEqual(['a', ['./x.js', './y.js'], 'b']);
  });

  test('a custom resolveRequest sees every dependency in order and its answer is used', async () => {
    project = createProject({
      'index.js': "global.result = [require('./a'), require('virtual-b'), require('./c')];",
      'a.js': "module.exports = 'a';",
      'b-real.js': "module.exports = 'b';",
      'c.js': "module.exports = 'c';",
    });
    const seen = [];
    const {code} = await build({
      resolver: {
        resolveRequest: (context, moduleName, platform) => {
          seen.push(moduleName);
          if (moduleName === 'virtual-b') {
            return {type: 'sourceFile', filePath: path.join(project.root, 'b-real.js')};
          }
          return context.resolveRequest(context, moduleName, platform);
        },
      },
    });
    expect(execBundle(code).result).toEqual(['a', 'b', 'c']);
    expect(seen.filter(n => ['./a', 'virtual-b', './c'].includes(n))).toEqual([
      './a',
      'virtual-b',
      './c',
    ]);
  });

  test('a scheme resolver answers its own scheme and the rest resolve normally', async () => {
    project = createProject({
      'index.js': "global.result = [require('./a'), require('thing:one'), require('./c')];",
      'a.js': "module.exports = 'a';",
      'one.js': "module.exports = 'one';",
      'c.js': "module.exports = 'c';",
    });
    const {code} = await build({
      resolver: {
        schemeResolvers: {
          thing: () => ({type: 'sourceFile', filePath: path.join(project.root, 'one.js')}),
        },
      },
    });
    expect(execBundle(code).result).toEqual(['a', 'one', 'c']);
  });

  test('a file added after a failed resolution is found on the next build (the cache is cleared)', async () => {
    project = createProject({
      'index.js': "global.result = require('./late');",
    });
    const config = await configFor();
    const server = await startServer(config);
    try {
      const url = '/index.bundle?platform=ios&dev=true&minify=false';
      const failed = await fetchFresh(server.url(url));
      expect(failed.status).toBe(500);
      await failed.text();
      project.write('late.js', "module.exports = 'late';");
      await waitFor(async () => {
        const res = await fetchFresh(server.url(url));
        const body = await res.text();
        return res.status === 200 && execBundle(body).result === 'late';
      }, {label: 'the added file resolves'});
      // Remove it again: the earlier success must not be remembered.
      project.remove('late.js');
      await waitFor(async () => (await fetchFresh(server.url(url))).status === 500, {
        label: 'the removed file no longer resolves',
      });
    } finally {
      await server.close();
    }
  });

  test('one dependency name from two folders resolves per folder', async () => {
    project = createProject({
      'index.js': "global.result = [require('./one/m'), require('./two/m')];",
      'one/m.js': "module.exports = require('./shared');",
      'one/shared.js': "module.exports = 'one';",
      'two/m.js': "module.exports = require('./shared');",
      'two/shared.js': "module.exports = 'two';",
    });
    const {code} = await build();
    expect(execBundle(code).result).toEqual(['one', 'two']);
  });
});
