/**
 * Black-box: the HMR WebSocket protocol on /hot.
 *
 * A client registers the bundle URL it loaded, then receives
 * update-start / update | error / update-done after every relevant change.
 * Pins message order, payload shape and recovery, observed only on the wire.
 */
'use strict';

const WebSocket = require('ws');
const {createProject, startServer, fetchFresh, waitFor} = require('../helpers/project');

jest.useRealTimers();
jest.setTimeout(90000);

const BUNDLE = '/index.bundle?platform=ios&dev=true&minify=false';

function connect(port) {
  const ws = new WebSocket(`ws://localhost:${port}/hot`);
  const messages = [];
  ws.on('message', data => messages.push(JSON.parse(String(data))));
  const opened = new Promise((resolve, reject) => {
    ws.once('open', resolve);
    ws.once('error', reject);
  });
  return {
    ws,
    messages,
    opened,
    send: msg => ws.send(typeof msg === 'string' ? msg : JSON.stringify(msg)),
    /** Waits until a message of `type` arrives after index `from`; returns its index. */
    async next(type, from = 0) {
      return waitFor(() => {
        const i = messages.findIndex((m, idx) => idx >= from && m.type === type);
        return i >= 0 ? i + 1 : 0;
      }, {label: `HMR message "${type}"`}).then(i => i - 1);
    },
    close: () => ws.close(),
  };
}

describe('HMR protocol', () => {
  let project, server, client;

  beforeEach(async () => {
    project = createProject({
      'index.js': "global.result = require('./a').value;",
      'a.js': 'module.exports = {value: 1};',
    });
    server = await startServer(await project.loadConfig());
    // The graph must exist before a client can register for it.
    expect((await fetchFresh(server.url(BUNDLE))).status).toBe(200);
    client = connect(server.port);
    await client.opened;
  });

  afterEach(async () => {
    client.close();
    await server.close();
    project.cleanup();
  });

  async function register() {
    client.send({type: 'register-entrypoints', entryPoints: [server.url(BUNDLE)]});
    return client.next('bundle-registered');
  }

  test('registering sends an initial empty update, then bundle-registered', async () => {
    const registeredAt = await register();
    const types = client.messages.slice(0, registeredAt + 1).map(m => m.type);
    expect(types).toEqual(['update-start', 'update', 'update-done', 'bundle-registered']);
    expect(client.messages[0].body).toEqual({isInitialUpdate: true});
    const update = client.messages[1].body;
    expect(update.isInitialUpdate).toBe(true);
    expect(update.added).toEqual([]);
    expect(update.modified).toEqual([]);
    expect(update.deleted).toEqual([]);
    expect(typeof update.revisionId).toBe('string');
  });

  test('editing a module sends one modified module with code, sourceURL and sourceMappingURL', async () => {
    const start = (await register()) + 1;
    project.write('a.js', 'module.exports = {value: 2};');
    const doneAt = await client.next('update-done', start);
    const batch = client.messages.slice(start, doneAt + 1);
    expect(batch.map(m => m.type)).toEqual(['update-start', 'update', 'update-done']);
    expect(batch[0].body).toEqual({isInitialUpdate: false});
    const {added, modified, deleted, isInitialUpdate} = batch[1].body;
    expect(isInitialUpdate).toBe(false);
    expect(added).toEqual([]);
    expect(deleted).toEqual([]);
    expect(modified).toHaveLength(1);
    const [id, code] = modified[0].module;
    expect(typeof id).toBe('number');
    expect(code).toContain('__d(');
    expect(code).toContain('value: 2');
    expect(modified[0].sourceURL).toContain('a.bundle');
    expect(modified[0].sourceMappingURL).toContain('a.map');
    // Module URLs are shallow, modules-only versions of the registered URL.
    expect(modified[0].sourceURL).toContain('modulesOnly=true');
    expect(modified[0].sourceURL).toContain('platform=ios');
  });

  test('the revisionId changes with every update', async () => {
    const start = (await register()) + 1;
    const initial = client.messages[1].body.revisionId;
    project.write('a.js', 'module.exports = {value: 2};');
    const u = await client.next('update', start);
    expect(client.messages[u].body.revisionId).not.toBe(initial);
  });

  test('adding a dependency sends it as added, removing it sends its id as deleted', async () => {
    let from = (await register()) + 1;
    project.write('b.js', "module.exports = 'b';");
    project.write('a.js', "module.exports = {value: require('./b')};");
    let u = await waitFor(async () => {
      const i = await client.next('update', from);
      from = i + 1;
      return client.messages[i].body.added.length > 0 ? i : 0;
    }, {label: 'update with added module'});
    const added = client.messages[u].body.added;
    expect(added).toHaveLength(1);
    expect(added[0].module[1]).toContain("'b'");
    const bId = added[0].module[0];

    project.write('a.js', 'module.exports = {value: 3};');
    u = await waitFor(async () => {
      const i = await client.next('update', from);
      from = i + 1;
      return client.messages[i].body.deleted.length > 0 ? i : 0;
    }, {label: 'update with deleted module'});
    expect(client.messages[u].body.deleted).toEqual([bId]);
  });

  test('a syntax error sends an error message inside the update batch, and fixing it sends a normal update', async () => {
    const start = (await register()) + 1;
    project.write('a.js', 'module.exports = {value: ;');
    const errAt = await client.next('error', start);
    expect(client.messages[errAt - 1].type).toBe('update-start');
    expect(await client.next('update-done', errAt)).toBe(errAt + 1);
    const err = client.messages[errAt].body;
    expect(err.type).toBe('TransformError');
    expect(err.message).toContain('a.js');

    project.write('a.js', 'module.exports = {value: 4};');
    const u = await client.next('update', errAt);
    expect(client.messages[u].body.modified[0].module[1]).toContain('value: 4');
  });

  test('registering an entry point that was never bundled returns a GraphNotFoundError', async () => {
    client.send({
      type: 'register-entrypoints',
      entryPoints: [server.url('/index.bundle?platform=android&dev=true&minify=false')],
    });
    const e = await client.next('error');
    expect(client.messages[e].body.type).toBe('GraphNotFoundError');
  });

  test('invalid JSON is answered with an error message', async () => {
    client.send('not json');
    const e = await client.next('error');
    expect(client.messages[e].body.message).toMatch(/JSON/i);
  });

  test('heartbeat messages are echoed back unchanged', async () => {
    client.send({type: 'heartbeat', id: 'abc'});
    const h = await client.next('heartbeat');
    expect(client.messages[h]).toEqual({type: 'heartbeat', id: 'abc'});
  });

  test('two clients on the same bundle both receive the update', async () => {
    await register();
    const second = connect(server.port);
    await second.opened;
    try {
      second.send({type: 'register-entrypoints', entryPoints: [server.url(BUNDLE)]});
      const reg2 = await second.next('bundle-registered');
      const from1 = client.messages.length;
      project.write('a.js', 'module.exports = {value: 5};');
      const u1 = await client.next('update', from1);
      const u2 = await second.next('update', reg2 + 1);
      expect(second.messages[u2].body.modified).toEqual(client.messages[u1].body.modified);
    } finally {
      second.close();
    }
  });
});
