/**
 * Black-box: what the serializers put in a bundle and its source map, driven
 * through the public config hooks (`serializer.*`), runBuild's options and the
 * dev server's bundle URL options.
 *
 * Phase 5 moves the bundle body, the source map and the source reads behind
 * them to Rust. These cases pin what every one of those hooks and options does
 * to the output, so the same suite proves the Rust path honours them (or hands
 * them back to JavaScript).
 */
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const {Metro, createProject, startServer, fetchFresh, execBundle} = require('../helpers/project');

jest.useRealTimers();
jest.setTimeout(90000);

const FILES = {
  'index.js': [
    "global.order = (global.order || []).concat('index');",
    "const {a} = require('./a');",
    "const {third} = require('./node_modules/dep/third');",
    'global.result = a + third;',
  ].join('\n'),
  'a.js': ["global.order = (global.order || []).concat('a');", "exports.a = 'A';"].join('\n'),
  'early.js': "global.order = (global.order || []).concat('early');",
  'polyfill.js': "global.order = ['polyfill'];",
  'node_modules/dep/third.js': "exports.third = '3';",
  'node_modules/dep/package.json': '{"name": "dep", "version": "1.0.0"}',
};

const BUILD = {entry: 'index.js', platform: 'ios', dev: true, minify: false};

// The flat maps of a source map: itself, or each section's map of an index map.
const flatMaps = map => (map.sections ? map.sections.map(s => s.map) : [map]);

// The source map's sources and sourcesContent, as one object keyed by source.
function sourcesOf(map) {
  return Object.fromEntries(flatMaps(map).flatMap(m => m.sources.map((s, i) => [s, m.sourcesContent?.[i]])));
}

// The sources a source map lists in x_google_ignoreList.
function ignoredSources(map) {
  return flatMaps(map).flatMap(m => (m.x_google_ignoreList ?? []).map(i => m.sources[i]));
}

describe('serializer config hooks (runBuild)', () => {
  let project;
  beforeEach(() => {
    project = createProject(FILES);
  });
  afterEach(() => project.cleanup());

  const build = async (serializer, opts = {}) =>
    Metro.runBuild(await project.loadConfig({serializer}), {...BUILD, ...opts});

  test('getPolyfills: polyfills run before any module and appear in the map', async () => {
    const polyfill = path.join(project.root, 'polyfill.js');
    const {code, map} = await build({getPolyfills: () => [polyfill]}, {sourceMap: true, sourceMapUrl: 'x.map'});
    expect(execBundle(code).order).toEqual(['polyfill', 'index', 'a']);
    expect(sourcesOf(JSON.parse(map))[polyfill]).toBe(FILES['polyfill.js']);
  });

  test('polyfillModuleNames: listed files are prepended like polyfills', async () => {
    const {code} = await build({polyfillModuleNames: [path.join(project.root, 'polyfill.js')]});
    expect(execBundle(code).order).toEqual(['polyfill', 'index', 'a']);
  });

  test('getModulesRunBeforeMainModule: listed modules run before the entry point, which gets its relative path', async () => {
    const calls = [];
    const {code} = await build({
      getModulesRunBeforeMainModule: entry => {
        calls.push(entry);
        return [path.join(project.root, 'early.js')];
      },
    });
    // early.js is not in the graph, so it is only run if something requires it.
    expect(calls).toEqual(['index.js']);
    expect(execBundle(code).order).toEqual(['index', 'a']);

    project.write('index.js', "require('./early');\n" + FILES['index.js']);
    const again = await build({getModulesRunBeforeMainModule: () => [path.join(project.root, 'a.js')]});
    // a.js is in the graph: it is run by its own __r() before the entry point.
    expect(execBundle(again.code).order).toEqual(['a', 'early', 'index']);
  });

  test('getRunModuleStatement: its statement replaces __r() for the entry point', async () => {
    const {code} = await build({
      getRunModuleStatement: (id, globalPrefix) => `globalThis.ranId = ${JSON.stringify(id)}; ${globalPrefix}__r(${JSON.stringify(id)});`,
    });
    const ctx = execBundle(code);
    expect(ctx.result).toBe('A3');
    expect(code).toMatch(/globalThis\.ranId = \d+; __r\(\d+\);/);
    expect(ctx.ranId).toEqual(expect.any(Number));
  });

  test('processModuleFilter: filtered modules are left out of the code and the map', async () => {
    const {code, map} = await build(
      {processModuleFilter: module => !module.path.endsWith(`${path.sep}a.js`)},
      {sourceMap: true, sourceMapUrl: 'x.map'},
    );
    expect(code).not.toContain("exports.a = 'A'");
    expect(code).toContain("exports.third = '3'");
    const sources = Object.keys(sourcesOf(JSON.parse(map)));
    expect(sources).not.toContain(path.join(project.root, 'a.js'));
    expect(sources).toContain(path.join(project.root, 'index.js'));
  });

  test('createModuleIdFactory: the ids it returns are the ids in __d() and __r()', async () => {
    const {code} = await build({
      createModuleIdFactory: () => {
        const ids = new Map();
        return p => {
          if (!ids.has(p)) {
            ids.set(p, 1000 + ids.size * 7);
          }
          return ids.get(p);
        };
      },
    });
    expect(code).toMatch(/__r\(1000\);/);
    expect(code).toMatch(/},1000,\[1007,1014\],"index\.js"\);/);
    expect(execBundle(code).result).toBe('A3');
  });

  test('customSerializer returning a string: that string is the bundle, and the map is still Metro\'s', async () => {
    const seen = [];
    const {code, map} = await build(
      {
        customSerializer: (entryPoint, preModules, graph, options) => {
          seen.push({
            entryPoint,
            preModules: preModules.length,
            modules: [...graph.dependencies.keys()].map(p => path.relative(project.root, p)).sort(),
            runModule: options.runModule,
            dev: options.dev,
          });
          return 'global.custom = true;';
        },
      },
      {sourceMap: true, sourceMapUrl: 'x.map'},
    );
    expect(code).toBe('global.custom = true;');
    expect(seen).toEqual([
      {
        entryPoint: path.join(project.root, 'index.js'),
        preModules: expect.any(Number),
        modules: ['a.js', 'index.js', path.join('node_modules', 'dep', 'third.js')],
        runModule: true,
        dev: true,
      },
    ]);
    expect(seen[0].preModules).toBeGreaterThan(0);
    expect(Object.keys(sourcesOf(JSON.parse(map)))).toContain(path.join(project.root, 'a.js'));
  });

  test('customSerializer returning {code, map}: both are used as they are', async () => {
    const {code, map} = await build(
      {customSerializer: async () => ({code: 'global.custom = 1;', map: '{"version":3,"custom":true}'})},
      {sourceMap: true, sourceMapUrl: 'x.map'},
    );
    expect(code).toBe('global.custom = 1;');
    expect(map).toBe('{"version":3,"custom":true}');
  });

  test('isThirdPartyModule: matching sources are listed in x_google_ignoreList', async () => {
    const {map} = await build({}, {sourceMap: true, sourceMapUrl: 'x.map'});
    const ignored = ignoredSources(JSON.parse(map));
    expect(ignored).toContain(path.join(project.root, 'node_modules', 'dep', 'third.js'));
    expect(ignored).not.toContain(path.join(project.root, 'a.js'));

    const custom = (await build({isThirdPartyModule: m => m.path.endsWith(`${path.sep}a.js`)}, {sourceMap: true, sourceMapUrl: 'x.map'})).map;
    const customIgnored = ignoredSources(JSON.parse(custom));
    expect(customIgnored).toContain(path.join(project.root, 'a.js'));
    expect(customIgnored).not.toContain(path.join(project.root, 'node_modules', 'dep', 'third.js'));
  });
});

describe('source maps from runBuild', () => {
  let project, config;
  beforeEach(async () => {
    project = createProject(FILES);
    config = await project.loadConfig();
  });
  afterEach(() => project.cleanup());

  test('sources are absolute paths and sourcesContent is each file as it is on disk', async () => {
    const {map} = await Metro.runBuild(config, {...BUILD, sourceMap: true, sourceMapUrl: 'x.map'});
    const sources = sourcesOf(JSON.parse(map));
    for (const rel of ['index.js', 'a.js', 'node_modules/dep/third.js']) {
      expect(sources[path.join(project.root, rel)]).toBe(FILES[rel]);
    }
  });

  test('a file with non-ASCII text and CRLF line endings keeps both in sourcesContent', async () => {
    const text = "// zażółć gęślą jaźń 🚀\r\nexports.a = 'Ä';\r\n";
    project.write('a.js', text);
    const {map, code} = await Metro.runBuild(config, {...BUILD, sourceMap: true, sourceMapUrl: 'x.map'});
    expect(sourcesOf(JSON.parse(map))[path.join(project.root, 'a.js')]).toBe(text);
    expect(execBundle(code).result).toBe('Ä3');
  });

  test('sourceMap with a sourceMapUrl: the bundle ends with that URL and has no inline map', async () => {
    const {code} = await Metro.runBuild(config, {...BUILD, sourceMap: true, sourceMapUrl: 'https://example.test/main.map'});
    expect(code).toMatch(/\n\/\/# sourceMappingURL=https:\/\/example\.test\/main\.map$/);
    expect(code).not.toContain('base64,');
  });

  test('sourceMap without a sourceMapUrl: the map is inlined and has the same sources as the returned map', async () => {
    const {code, map} = await Metro.runBuild(config, {...BUILD, sourceMap: true});
    const m = /\/\/# sourceMappingURL=data:application\/json;charset=utf-8;base64,([A-Za-z0-9+/=]+)$/.exec(code);
    expect(m).not.toBeNull();
    const inline = JSON.parse(Buffer.from(m[1], 'base64').toString());
    expect(sourcesOf(inline)).toEqual(sourcesOf(JSON.parse(map)));
  });

  test('without sourceMap the bundle has no sourceMappingURL comment', async () => {
    const {code} = await Metro.runBuild(config, BUILD);
    expect(code).not.toContain('sourceMappingURL');
  });
});

describe('dev server bundle and map URL options', () => {
  let project, server;
  const Q = 'platform=ios&dev=true&minify=false';

  beforeAll(async () => {
    project = createProject({
      ...FILES,
      'index.js': [FILES['index.js'], "global.later = () => import('./lazy');"].join('\n'),
      'lazy.js': "exports.lazy = 'LAZY_MARKER';",
    });
    server = await startServer(await project.loadConfig());
  });
  afterAll(async () => {
    await server.close();
    project.cleanup();
  });

  const get = async p => (await fetchFresh(server.url(p))).text();

  test('sourcePaths=url-server: map sources are server URLs, not absolute paths', async () => {
    const map = JSON.parse(await get(`/index.map?${Q}&sourcePaths=url-server`));
    const sources = Object.keys(sourcesOf(map));
    expect(sources).toContain('/[metro-project]/a.js');
    expect(sources).toContain('/[metro-project]/node_modules/dep/third.js');
    expect(sources.filter(s => s.startsWith(project.root))).toEqual([]);
  });

  // A quirk pinned as it is: excludeSource only applies to the .map endpoint.
  // The inline map is built by the bundle serializer, which ignores it.
  test('excludeSource=true with inlineSourceMap=true: the inline map still has sourcesContent', async () => {
    const code = await get(`/index.bundle?${Q}&inlineSourceMap=true&excludeSource=true`);
    const m = /sourceMappingURL=data:application\/json;charset=utf-8;base64,([A-Za-z0-9+/=]+)/.exec(code);
    const inline = JSON.parse(Buffer.from(m[1], 'base64').toString());
    expect(sourcesOf(inline)[path.join(project.root, 'a.js')]).toBe(FILES['a.js']);
  });

  test('lazy=true: an import() target stays out of the bundle and its bundle path is in the dependency map', async () => {
    const eager = await get(`/index.bundle?${Q}`);
    const lazy = await get(`/index.bundle?${Q}&lazy=true`);
    expect(eager).toContain('LAZY_MARKER');
    expect(lazy).not.toContain('LAZY_MARKER');
    expect(lazy).toMatch(/"paths":\{"\d+":"\/lazy\.bundle\?/);
    // The split bundle for the lazy module is served on its own, modules only.
    const split = await get(`/lazy.bundle?${Q}&modulesOnly=true&runModule=false`);
    expect(split).toContain('LAZY_MARKER');
    expect(split).not.toContain('__BUNDLE_START_TIME__');
  });

  test('shallow=true: only the entry module is serialized', async () => {
    const code = await get(`/index.bundle?${Q}&shallow=true&modulesOnly=true&runModule=false`);
    expect(code).toContain('"index.js"');
    expect(code).not.toContain("exports.a = 'A'");
  });

  test('modulesOnly=true&runModule=false: the map covers exactly the modules in the code', async () => {
    const code = await get(`/a.bundle?${Q}&modulesOnly=true&runModule=false`);
    const map = JSON.parse(await get(`/a.map?${Q}&modulesOnly=true&runModule=false`));
    expect(code).not.toMatch(/__r\(/);
    expect(Object.keys(sourcesOf(map))).toEqual([path.join(project.root, 'a.js')]);
  });
});

describe('experimentalSerializerHook', () => {
  test('is called with the graph and the delta on every dev server bundle build', async () => {
    const project = createProject(FILES);
    const calls = [];
    const server = await startServer(
      await project.loadConfig({
        serializer: {
          experimentalSerializerHook: (graph, delta) => {
            calls.push({
              modules: graph.dependencies.size,
              added: delta.added.size,
              modified: delta.modified.size,
              deleted: delta.deleted.size,
              reset: delta.reset,
            });
          },
        },
      }),
    );
    try {
      await (await fetchFresh(server.url('/index.bundle?platform=ios&dev=true&minify=false'))).text();
      expect(calls).toEqual([{modules: 3, added: 3, modified: 0, deleted: 0, reset: true}]);
    } finally {
      await server.close();
      project.cleanup();
    }
  });
});

describe('bundle and map files written by runBuild', () => {
  test('bundleOut and sourceMapOut hold exactly the returned code and map, overwriting older files', async () => {
    const project = createProject(FILES);
    try {
      const bundleOut = project.write('dist/main.jsbundle', 'stale contents that are longer than nothing');
      const sourceMapOut = project.write('dist/main.map', 'stale');
      const result = await Metro.runBuild(await project.loadConfig(), {
        ...BUILD,
        sourceMap: true,
        sourceMapUrl: 'main.map',
        bundleOut,
        sourceMapOut,
      });
      expect(fs.readFileSync(bundleOut, 'utf8')).toBe(result.code);
      expect(fs.readFileSync(sourceMapOut, 'utf8')).toBe(result.map);
    } finally {
      project.cleanup();
    }
  });
});
