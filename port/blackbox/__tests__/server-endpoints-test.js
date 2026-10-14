/**
 * Black-box: dev server HTTP endpoints other than plain .bundle.
 *
 * Covers .map, .assets, /assets/*, /symbolicate, multipart progress responses
 * and the bundle URL options integrators rely on. Only HTTP is observed.
 */
'use strict';

const path = require('node:path');
const png = require('../helpers/png');
const {createProject, startServer, fetchFresh, execBundle} = require('../helpers/project');

jest.useRealTimers();
jest.setTimeout(90000);

const Q = 'platform=ios&dev=true&minify=false';

describe('dev server endpoints', () => {
  let project, server;

  beforeAll(async () => {
    project = createProject({
      'index.js': [
        "const img = require('./img.png');",
        "const {thrower} = require('./a');",
        'global.result = {img, dev: __DEV__, thrower};',
      ].join('\n'),
      'a.js': ['// line 1', 'function thrower() {', "  throw new Error('BOOM_MARKER');", '}', 'module.exports = {thrower};'].join('\n'),
      'AssetRegistry.js': 'module.exports = {registerAsset: asset => asset};',
    });
    project.write('img.png', png(3, 2));
    project.write('img@2x.png', png(6, 4));
    server = await startServer(
      await project.loadConfig({
        transformer: {assetRegistryPath: path.join(project.root, 'AssetRegistry.js')},
      }),
    );
  });

  afterAll(async () => {
    await server.close();
    project.cleanup();
  });

  const get = async (p, init) => {
    const res = await fetchFresh(server.url(p), init);
    return {res, status: res.status, text: await res.text()};
  };

  describe('.bundle URL options', () => {
    test('dev=true sets __DEV__ and the asset module gets its registered data', async () => {
      const {status, text} = await get(`/index.bundle?${Q}`);
      expect(status).toBe(200);
      const {result} = execBundle(text);
      expect(result.dev).toBe(true);
      expect(result.img).toEqual(
        expect.objectContaining({
          __packager_asset: true,
          name: 'img',
          type: 'png',
          width: 3,
          height: 2,
          scales: [1, 2],
          httpServerLocation: '/assets',
          hash: expect.stringMatching(/^[0-9a-f]{32}$/),
        }),
      );
    });

    test('dev=false sets __DEV__ to false', async () => {
      const {text} = await get('/index.bundle?platform=ios&dev=false&minify=false');
      expect(execBundle(text).result.dev).toBe(false);
    });

    test('minify=true produces a smaller bundle that behaves the same', async () => {
      const plain = await get('/index.bundle?platform=ios&dev=false&minify=false');
      const min = await get('/index.bundle?platform=ios&dev=false&minify=true');
      expect(min.text.length).toBeLessThan(plain.text.length);
      expect(execBundle(min.text).result.img.width).toBe(3);
    });

    test('runModule=false defines modules without running the entry point', async () => {
      const {text} = await get(`/index.bundle?${Q}&runModule=false`);
      expect(execBundle(text).result).toBeUndefined();
    });

    test('modulesOnly=true omits the module system prelude', async () => {
      const full = await get(`/index.bundle?${Q}`);
      const only = await get(`/index.bundle?${Q}&modulesOnly=true`);
      expect(full.text).toContain('__BUNDLE_START_TIME__');
      expect(only.text).not.toContain('__BUNDLE_START_TIME__');
      expect(only.text).toContain('__d(');
    });

    test('inlineSourceMap=true appends a base64 source map', async () => {
      const {text} = await get(`/index.bundle?${Q}&inlineSourceMap=true`);
      // The inline map comes before the trailing sourceURL comment.
      const m = /\/\/# sourceMappingURL=data:application\/json;charset=utf-8;base64,([A-Za-z0-9+/=]+)\n\/\/# sourceURL=/.exec(text);
      expect(m).not.toBeNull();
      expect(JSON.parse(Buffer.from(m[1], 'base64').toString()).version).toBe(3);
    });

    test('by default the bundle points at its .map URL', async () => {
      const {text} = await get(`/index.bundle?${Q}`);
      expect(text).toMatch(/\/\/# sourceMappingURL=.*\/index\.map\?/);
      // sourceURL uses the JSC-safe form: the query string follows '//&'.
      expect(text).toMatch(/\/\/# sourceURL=http:\/\/localhost:\d+\/index\.bundle\/\/&platform=ios/);
    });

    test('Accept: multipart/mixed streams JSON progress chunks before the bundle', async () => {
      // Use a URL variant not built yet so progress is reported.
      const {res, text} = await get('/index.bundle?platform=android&dev=true&minify=false', {
        headers: {Accept: 'multipart/mixed'},
      });
      expect(res.headers.get('content-type')).toMatch(/^multipart\/mixed; boundary=/);
      const boundary = /boundary="?([^";]+)"?/.exec(res.headers.get('content-type'))[1];
      const parts = text.split(`--${boundary}`).slice(1, -1);
      const progress = parts.filter(p => p.includes('application/json')).map(p => JSON.parse(p.split('\r\n\r\n')[1]));
      expect(progress.length).toBeGreaterThan(0);
      for (const p of progress) {
        expect(p).toEqual({done: expect.any(Number), total: expect.any(Number), percent: expect.any(Number)});
      }
      const last = parts[parts.length - 1];
      expect(last).toContain('__d(');
      expect(last).toMatch(/X-Metro-Files-Changed-Count: \d+/);
    });
  });

  describe('.map', () => {
    test('returns a version 3 index map whose sections list project sources with their contents', async () => {
      const {status, text} = await get(`/index.map?${Q}`);
      expect(status).toBe(200);
      const indexMap = JSON.parse(text);
      expect(indexMap.version).toBe(3);
      expect(indexMap.sections[0].map.sources).toEqual(['__prelude__']);
      const section = indexMap.sections.find(sec => sec.map.sources.some(s => /[\\/]a\.js$/.test(s)));
      expect(section).toBeDefined();
      const map = section.map;
      const idx = map.sources.findIndex(s => s.endsWith('/a.js') || s.endsWith('\\a.js'));
      expect(idx).toBeGreaterThanOrEqual(0);
      expect(map.sourcesContent[idx]).toContain('BOOM_MARKER');
      expect(typeof map.mappings).toBe('string');
    });

    test('excludeSource=true leaves sourcesContent out', async () => {
      const indexMap = JSON.parse((await get(`/index.map?${Q}&excludeSource=true`)).text);
      for (const {map} of indexMap.sections) {
        expect(map.sourcesContent == null || map.sourcesContent.every(c => c == null)).toBe(true);
      }
    });
  });

  describe('assets', () => {
    test('.assets lists the assets used by a bundle', async () => {
      const {status, text} = await get(`/index.assets?${Q}`);
      expect(status).toBe(200);
      const assets = JSON.parse(text);
      expect(assets).toHaveLength(1);
      expect(assets[0]).toEqual(
        expect.objectContaining({name: 'img', type: 'png', scales: [1, 2], width: 3, height: 2}),
      );
      expect(assets[0].files.map(f => path.basename(f))).toEqual(['img.png', 'img@2x.png']);
    });

    test('/assets/<path> serves the exact bytes of the requested scale', async () => {
      const one = await fetchFresh(server.url('/assets/img.png?platform=ios'));
      expect(one.status).toBe(200);
      expect(one.headers.get('content-type')).toBe('image/png');
      expect(Buffer.from(await one.arrayBuffer())).toEqual(png(3, 2));
      const two = await fetchFresh(server.url('/assets/img@2x.png?platform=ios'));
      expect(Buffer.from(await two.arrayBuffer())).toEqual(png(6, 4));
    });

    test('/assets supports byte ranges', async () => {
      const res = await fetchFresh(server.url('/assets/img.png?platform=ios'), {headers: {Range: 'bytes=0-7'}});
      expect(res.status).toBe(206);
      expect(res.headers.get('content-range')).toBe(`bytes 0-7/${png(3, 2).length}`);
      expect(Buffer.from(await res.arrayBuffer())).toEqual(png(3, 2).subarray(0, 8));
    });

    test('a missing asset is a 404', async () => {
      const orig = console.error;
      console.error = () => {};
      try {
        const {status, text} = await get('/assets/missing.png?platform=ios');
        expect(status).toBe(404);
        expect(text).toBe('Asset not found');
      } finally {
        console.error = orig;
      }
    });
  });

  describe('/symbolicate', () => {
    test('maps a bundle frame back to the original file, line and function, with a code frame', async () => {
      const bundleUrl = server.url(`/index.bundle?${Q}`);
      const {text} = await get(`/index.bundle?${Q}`);
      const lines = text.split('\n');
      const lineIdx = lines.findIndex(l => l.includes('BOOM_MARKER'));
      expect(lineIdx).toBeGreaterThan(0);
      const column = lines[lineIdx].indexOf('throw');
      const res = await fetchFresh(server.url('/symbolicate'), {
        method: 'POST',
        headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({stack: [{file: bundleUrl, lineNumber: lineIdx + 1, column, methodName: 'x'}]}),
      });
      expect(res.status).toBe(200);
      const body = await res.json();
      expect(body.stack).toHaveLength(1);
      const [frame] = body.stack;
      expect(frame.file).toBe(path.join(project.root, 'a.js'));
      expect(frame.lineNumber).toBe(3);
      expect(frame.column).toBe(2);
      expect(frame.methodName).toBe('thrower');
      expect(body.codeFrame).toEqual(
        expect.objectContaining({location: {row: 3, column: 2}, fileName: path.join(project.root, 'a.js')}),
      );
      expect(body.codeFrame.content).toContain('BOOM_MARKER');
    });

    test('frames that are not bundle URLs pass through unchanged', async () => {
      const frame = {file: 'native', lineNumber: 1, column: 1, methodName: 'nativeFn'};
      const res = await fetchFresh(server.url('/symbolicate'), {
        method: 'POST',
        body: JSON.stringify({stack: [frame]}),
      });
      const body = await res.json();
      expect(body.stack[0]).toEqual(expect.objectContaining(frame));
    });

    test('a body without a stack array is rejected with 400', async () => {
      const orig = console.error;
      console.error = () => {};
      try {
        const res = await fetchFresh(server.url('/symbolicate'), {method: 'POST', body: JSON.stringify({nope: 1})});
        expect(res.status).toBe(400);
      } finally {
        console.error = orig;
      }
    });
  });
});
