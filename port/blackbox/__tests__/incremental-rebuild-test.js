/**
 * Black-box: incremental rebuilds through the dev server.
 *
 * Pins how a running server reacts to files being edited, added, removed and
 * renamed on disk. Covers file-map watching, DeltaCalculator, Graph and
 * IncrementalBundler together, observed only through HTTP responses.
 */
'use strict';

const {createProject, startServer, fetchFresh, execBundle, waitFor} = require('../helpers/project');

jest.useRealTimers();
jest.setTimeout(90000);

const BUNDLE = '/index.bundle?platform=ios&dev=true&minify=false';

describe('incremental rebuilds', () => {
  let project, server;

  async function get(p = BUNDLE) {
    const res = await fetchFresh(server.url(p));
    return {status: res.status, body: await res.text(), headers: res.headers};
  }
  async function result(p = BUNDLE) {
    const {status, body} = await get(p);
    if (status !== 200) {
      return {status, body};
    }
    return {status, value: execBundle(body).result};
  }
  const waitForValue = (expected, label) =>
    waitFor(async () => {
      const r = await result();
      return r.status === 200 && JSON.stringify(r.value) === JSON.stringify(expected);
    }, {label});
  const waitForStatus = (status, label) =>
    waitFor(async () => (await get()).status === status, {label});

  beforeEach(async () => {
    project = createProject({
      'index.js': "global.result = require('./a').value;",
      'a.js': 'module.exports = {value: 1};',
    });
    server = await startServer(await project.loadConfig());
    expect(await result()).toEqual({status: 200, value: 1});
  });

  afterEach(async () => {
    await server.close();
    project.cleanup();
  });

  test('editing a dependency is reflected in the next bundle', async () => {
    project.write('a.js', 'module.exports = {value: 2};');
    await waitForValue(2, 'edited value');
  });

  test('adding a new dependency pulls the new file into the bundle', async () => {
    project.write('b.js', "module.exports = 'from b';");
    project.write('a.js', "module.exports = {value: [1, require('./b')]};");
    await waitForValue([1, 'from b'], 'new dependency');
  });

  test('a module dropped from the graph is no longer in the bundle', async () => {
    project.write('b.js', "module.exports = 'MARKER_B_MODULE';");
    project.write('a.js', "module.exports = {value: require('./b')};");
    await waitForValue('MARKER_B_MODULE', 'b added');
    project.write('a.js', 'module.exports = {value: 3};');
    await waitForValue(3, 'b removed');
    expect((await get()).body).not.toContain('MARKER_B_MODULE');
  });

  test('deleting a required file turns the bundle into a 500 resolution error, and restoring it recovers', async () => {
    project.remove('a.js');
    await waitForStatus(500, 'resolution error');
    const {body} = await get();
    const error = JSON.parse(body);
    expect(error.type).toBe('UnableToResolveError');
    expect(error.message).toContain('./a');
    project.write('a.js', 'module.exports = {value: 4};');
    await waitForValue(4, 'recovery');
  });

  test('a syntax error is reported as a 500 with the file name, and fixing it recovers', async () => {
    project.write('a.js', 'module.exports = {value: ;');
    await waitForStatus(500, 'syntax error');
    const error = JSON.parse((await get()).body);
    expect(error.type).toBe('TransformError');
    expect(error.message).toContain('a.js');
    project.write('a.js', 'module.exports = {value: 5};');
    await waitForValue(5, 'recovery');
  });

  test('renaming a file and updating the import keeps the bundle working', async () => {
    project.write('renamed.js', 'module.exports = {value: 6};');
    project.remove('a.js');
    project.write('index.js', "global.result = require('./renamed').value;");
    await waitForValue(6, 'rename');
  });

  test('a new platform-specific file is only picked up once the importer is re-resolved', async () => {
    // Current behaviour (unstable_incrementalResolution: false): adding a file
    // that would shadow an existing resolution does not invalidate the importer.
    project.write('a.ios.js', "module.exports = {value: 'ios'};");
    await new Promise(r => setTimeout(r, 1500));
    expect(await result()).toEqual({status: 200, value: 1});
    // Touching the importer re-resolves './a' and picks the platform file.
    project.write('index.js', "global.result = require('./a').value; // touched");
    await waitForValue('ios', 'platform file');
    // The android bundle keeps resolving the generic file.
    const android = await result(BUNDLE.replace('platform=ios', 'platform=android'));
    expect(android).toEqual({status: 200, value: 1});
  });

  test('editing a file that is not part of the bundle does not change the bundle', async () => {
    const before = (await get()).body;
    project.write('unrelated.js', 'module.exports = 123;');
    // Give the watcher time to observe the change.
    await new Promise(r => setTimeout(r, 1500));
    expect((await get()).body).toBe(before);
  });

  test('bundles for different entry points are tracked independently', async () => {
    project.write('other.js', "global.result = 'other:' + require('./a').value;");
    const other = '/other.bundle?platform=ios&dev=true&minify=false';
    // A brand-new entry file is a 404 until the file map has seen it.
    await waitFor(async () => (await result(other)).value === 'other:1', {label: 'other entry'});
    project.write('a.js', 'module.exports = {value: 7};');
    await waitForValue(7, 'index updated');
    await waitFor(async () => (await result(other)).value === 'other:7', {label: 'other updated'});
  });
});
