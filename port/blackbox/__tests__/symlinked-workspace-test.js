/**
 * Black-box: a pnpm / yarn workspace layout, where node_modules holds
 * relative symlinks to packages elsewhere in the repository.
 *
 * Bluesky's app links `node_modules/@bsky.app/<pkg>` to `../../modules/<pkg>`.
 * The bundle must resolve through those links, by package name, by deep
 * import and through `exports`, and must follow edits to the linked package.
 */
'use strict';

const {createProject, execBundle, fetchFresh, startServer, waitFor} = require('../helpers/project');
const fs = require('node:fs');
const path = require('node:path');

jest.useRealTimers();
jest.setTimeout(90000);

const BUNDLE = '/index.bundle?platform=ios&dev=true&minify=false';

function workspace() {
  const project = createProject({
    'index.js': [
      "const handler = require('@scope/handler');",
      "const deep = require('@scope/handler/src/deep');",
      "const exported = require('@scope/exported/feature');",
      "const plain = require('plain-linked');",
      'global.result = [handler.name, deep, exported, plain].join(",");',
    ].join('\n'),
    'modules/handler/package.json': JSON.stringify({name: '@scope/handler', main: 'src/index.js'}),
    'modules/handler/src/index.js': "exports.name = 'handler:' + require('./util');",
    'modules/handler/src/util.js': "module.exports = 'util-1';",
    'modules/handler/src/deep.js': "module.exports = 'deep';",
    'modules/exported/package.json': JSON.stringify({
      name: '@scope/exported',
      exports: {'./feature': './lib/feature.js'},
    }),
    'modules/exported/lib/feature.js': "module.exports = 'feature';",
    'packages/plain/package.json': JSON.stringify({name: 'plain-linked', main: 'index.js'}),
    'packages/plain/index.js': "module.exports = 'plain';",
  });
  fs.mkdirSync(path.join(project.root, 'node_modules/@scope'), {recursive: true});
  // Relative links, as pnpm and yarn workspaces create them.
  fs.symlinkSync('../../modules/handler', path.join(project.root, 'node_modules/@scope/handler'));
  fs.symlinkSync('../../modules/exported', path.join(project.root, 'node_modules/@scope/exported'));
  fs.symlinkSync('../packages/plain', path.join(project.root, 'node_modules/plain-linked'));
  return project;
}

describe('symlinked workspace packages', () => {
  let project, server;

  afterEach(async () => {
    await server?.close();
    server = null;
    project.cleanup();
  });

  test('resolve by name, deep import and exports, and follow edits inside the linked package', async () => {
    project = workspace();
    server = await startServer(await project.loadConfig({resolver: {unstable_enablePackageExports: true}}));
    const value = async () => {
      const res = await fetchFresh(server.url(BUNDLE));
      const body = await res.text();
      return res.status === 200 ? execBundle(body).result : `HTTP ${res.status}: ${body.slice(0, 500)}`;
    };
    expect(await value()).toBe('handler:util-1,deep,feature,plain');

    project.write('modules/handler/src/util.js', "module.exports = 'util-2';");
    await waitFor(async () => (await value()) === 'handler:util-2,deep,feature,plain', {label: 'edit in linked package'});
  });

  test('runBuild resolves through the links too', async () => {
    project = workspace();
    const {Metro} = require('../helpers/project');
    const {code} = await Metro.runBuild(await project.loadConfig({resolver: {unstable_enablePackageExports: true}}), {
      entry: 'index.js',
      platform: 'ios',
      dev: true,
      minify: false,
    });
    expect(execBundle(code).result).toBe('handler:util-1,deep,feature,plain');
  });
});
