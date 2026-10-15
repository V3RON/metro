/**
 * Black-box: the exact payload of an HMR update for a modified module.
 *
 * hmr-protocol-test pins the message flow. This suite pins what phase 5's HMR
 * serializer produces for one module: the define call with the module's
 * inverse dependencies up to the entry point, and the per-module source and
 * map URLs (path encoding, JSC-safe form, the options added and removed).
 */
'use strict';

const {createProject, fetchFresh, startServer, waitFor} = require('../helpers/project');
const WebSocket = require('ws');

jest.useRealTimers();
jest.setTimeout(90000);

const QUERY = 'platform=ios&dev=true&minify=false';

// Module ids by module name, read from the bundle's define calls.
function moduleIds(bundle) {
  const ids = {};
  for (const m of bundle.matchAll(/^},(\d+),(?:\[[\d,]*\]|null),"([^"]+)"\);$/gm)) {
    ids[m[2]] = Number(m[1]);
  }
  return ids;
}

describe('HMR update payload', () => {
  let project, server, ws, messages;

  beforeEach(async () => {
    project = createProject({
      'index.js': "global.result = require('./lib/mid').value + require('./lib/other');",
      'lib/mid.js': "module.exports = {value: require('./deep name')};",
      'lib/other.js': "module.exports = require('./deep name');",
      'lib/deep name.js': "module.exports = 'leaf-1';",
    });
    server = await startServer(await project.loadConfig());
    messages = [];
  });

  afterEach(async () => {
    ws?.close();
    await server.close();
    project.cleanup();
  });

  // Loads `bundlePath`, registers it over /hot, edits the leaf and returns the
  // module ids and the first non-initial update.
  async function editLeaf(bundlePath) {
    const bundle = await (await fetchFresh(server.url(bundlePath))).text();
    ws = new WebSocket(`ws://localhost:${server.port}/hot`);
    ws.on('message', data => messages.push(JSON.parse(String(data))));
    await new Promise((resolve, reject) => {
      ws.once('open', resolve);
      ws.once('error', reject);
    });
    ws.send(JSON.stringify({type: 'register-entrypoints', entryPoints: [server.url(bundlePath)]}));
    await waitFor(() => messages.some(m => m.type === 'bundle-registered'), {label: 'bundle-registered'});
    project.write('lib/deep name.js', "module.exports = 'leaf-2';");
    const update = await waitFor(() => messages.find(m => m.type === 'update' && !m.body.isInitialUpdate), {
      label: 'update',
    });
    return {ids: moduleIds(bundle), update: update.body};
  }

  test('the define call carries the inverse dependencies of the module up to the entry point', async () => {
    const {ids, update} = await editLeaf(`/index.bundle?${QUERY}`);
    expect(Object.keys(ids).sort()).toEqual(['index.js', 'lib/deep name.js', 'lib/mid.js', 'lib/other.js']);
    expect(update.added).toEqual([]);
    expect(update.deleted).toEqual([]);
    expect(update.modified).toHaveLength(1);

    const [id, code] = update.modified[0].module;
    const leaf = ids['lib/deep name.js'];
    expect(id).toBe(leaf);
    const inverse = {
      [ids['index.js']]: [],
      [ids['lib/mid.js']]: [ids['index.js']],
      [ids['lib/other.js']]: [ids['index.js']],
      [leaf]: [ids['lib/mid.js'], ids['lib/other.js']],
    };
    expect(code.split('\n').slice(0, 3)).toEqual([
      '__d(function (global, require, _$$_IMPORT_DEFAULT, _$$_IMPORT_ALL, module, exports, _dependencyMap) {',
      "  module.exports = 'leaf-2';",
      `},${leaf},null,"lib/deep name.js",${JSON.stringify(inverse)});`,
    ]);
  });

  test('module URLs encode the path, drop excludeSource and ask for a shallow, modules-only bundle', async () => {
    const {update} = await editLeaf(`/index.bundle?${QUERY}&excludeSource=true`);
    const {module, sourceMappingURL, sourceURL} = update.modified[0];
    const origin = `http://localhost:${server.port}`;
    const options = `${QUERY}&modulesOnly=true&runModule=false&shallow=true`;
    expect(sourceMappingURL).toBe(`${origin}/lib/deep%20name.map?${options}`);
    // The JSC-safe form: the query string follows '//&'.
    expect(sourceURL).toBe(`${origin}/lib/deep%20name.bundle//&${options}`);
    expect(module[1].endsWith(`\n//# sourceMappingURL=${sourceMappingURL}\n//# sourceURL=${sourceURL}\n`)).toBe(true);
  });
});
