/**
 * Black-box: the bundle output format's `save`, through the package's
 * `metro/private/shared/output/bundle` export, which React Native's CLI uses to
 * write bundles and source maps. Phase 5 may move these writes to Rust.
 */
'use strict';

const {createProject} = require('../helpers/project');
const outputBundle = require('metro/private/shared/output/bundle');
const fs = require('node:fs');
const path = require('node:path');

describe('output/bundle save', () => {
  let project;
  beforeEach(() => {
    project = createProject({});
  });
  afterEach(() => project.cleanup());

  const map = root =>
    JSON.stringify({
      version: 3,
      sources: [path.join(root, 'src', 'a.js'), path.join(root, 'b.js')],
      sourcesContent: ['a', 'b'],
      names: [],
      mappings: 'AAAA;ACAA',
    });

  test('writes the code and the map as they are, and logs each write', async () => {
    const logs = [];
    const bundleOutput = path.join(project.root, 'main.jsbundle');
    const sourcemapOutput = path.join(project.root, 'main.map');
    const code = '__d(function(){/* zażółć 🚀 */});\n';
    await outputBundle.save({code, map: map(project.root)}, {bundleOutput, sourcemapOutput}, m => logs.push(m));
    expect(fs.readFileSync(bundleOutput, 'utf8')).toBe(code);
    expect(fs.readFileSync(sourcemapOutput, 'utf8')).toBe(map(project.root));
    expect(logs).toEqual(
      expect.arrayContaining([
        `Writing bundle output to: ${bundleOutput}`,
        'Done writing bundle output',
        `Writing sourcemap output to: ${sourcemapOutput}`,
        'Done writing sourcemap output',
      ]),
    );
  });

  test('without sourcemapOutput only the bundle is written', async () => {
    const bundleOutput = path.join(project.root, 'main.jsbundle');
    await outputBundle.save({code: 'x', map: map(project.root)}, {bundleOutput}, () => {});
    expect(fs.readdirSync(project.root).filter(f => !f.startsWith('.'))).toEqual(['main.jsbundle']);
  });

  test('sourcemapSourcesRoot makes the map sources relative to it', async () => {
    const sourcemapOutput = path.join(project.root, 'main.map');
    await outputBundle.save(
      {code: 'x', map: map(project.root)},
      {bundleOutput: path.join(project.root, 'main.jsbundle'), sourcemapOutput, sourcemapSourcesRoot: project.root},
      () => {},
    );
    const written = JSON.parse(fs.readFileSync(sourcemapOutput, 'utf8'));
    expect(written.sources).toEqual([path.join('src', 'a.js'), 'b.js']);
    expect(written.sourcesContent).toEqual(['a', 'b']);
  });

  test('bundleEncoding is the encoding the bundle is written in', async () => {
    const bundleOutput = path.join(project.root, 'main.jsbundle');
    const code = 'global.x = "Ä";';
    await outputBundle.save({code, map: '{}'}, {bundleOutput, bundleEncoding: 'utf16le'}, () => {});
    expect(fs.readFileSync(bundleOutput)).toEqual(Buffer.from(code, 'utf16le'));
  });

  test('a missing output directory is an error', async () => {
    const bundleOutput = path.join(project.root, 'missing', 'main.jsbundle');
    await expect(outputBundle.save({code: 'x', map: '{}'}, {bundleOutput}, () => {})).rejects.toMatchObject({
      code: 'ENOENT',
    });
  });
});
