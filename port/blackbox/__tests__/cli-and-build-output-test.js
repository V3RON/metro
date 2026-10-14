/**
 * Black-box: the `metro` CLI and the files runBuild writes.
 *
 * Runs the real CLI in a child process (exit codes, stdout, files on disk)
 * and calls runBuild/buildGraph through the public API. A Rust-backed Metro
 * keeps the same CLI and JS API, so these tests apply unchanged.
 */
'use strict';

const {spawnSync} = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const {Metro, PACKAGES, createProject, execBundle} = require('../helpers/project');

jest.useRealTimers();
jest.setTimeout(120000);

const CLI = path.join(PACKAGES, 'metro', 'src', 'cli.js');

function metro(args, cwd) {
  const r = spawnSync(process.execPath, [CLI, ...args], {cwd, encoding: 'utf8', env: {...process.env, CI: '1'}});
  return {code: r.status, stdout: r.stdout, stderr: r.stderr};
}

const FILES = {
  'index.js': "global.result = require('./a').value + (__DEV__ ? '-dev' : '-prod');",
  'a.js': "module.exports = {value: 'A'};",
  'unused.js': "module.exports = 'unused';",
};

describe('metro CLI', () => {
  let project;
  beforeEach(() => {
    project = createProject(FILES);
    project.writeConfigFile();
  });
  afterEach(() => project.cleanup());

  test('`metro build` writes a runnable production bundle and exits 0', () => {
    const out = path.join(project.root, 'out', 'bundle.js');
    fs.mkdirSync(path.dirname(out));
    const r = metro(['build', 'index.js', '--out', out, '--platform', 'ios', '--config', path.join(project.root, 'metro.config.js')], project.root);
    expect(r.code).toBe(0);
    const code = fs.readFileSync(out, 'utf8');
    expect(execBundle(code).result).toBe('A-prod');
    // Production builds are minified by default.
    expect(code.split('\n').length).toBeLessThan(40);
  });

  test('`metro build --dev --no-minify --source-map` writes bundle and map side by side', () => {
    const out = path.join(project.root, 'bundle.js');
    const r = metro(['build', 'index.js', '--out', out, '--dev', '--no-minify', '--source-map', '--platform', 'ios', '-c', 'metro.config.js'], project.root);
    expect(r.code).toBe(0);
    expect(execBundle(fs.readFileSync(out, 'utf8')).result).toBe('A-dev');
    const map = JSON.parse(fs.readFileSync(out.replace(/\.js$/, '.map'), 'utf8'));
    expect(map.version).toBe(3);
  });

  test('`metro build` exits non-zero and names the missing module when resolution fails', () => {
    project.write('index.js', "require('./does-not-exist');");
    const r = metro(['build', 'index.js', '--out', path.join(project.root, 'b.js'), '-c', 'metro.config.js'], project.root);
    expect(r.code).not.toBe(0);
    expect(r.stdout + r.stderr).toContain('does-not-exist');
    expect(fs.existsSync(path.join(project.root, 'b.js'))).toBe(false);
  });

  test('`metro build` without --out fails argument validation', () => {
    const r = metro(['build', 'index.js', '-c', 'metro.config.js'], project.root);
    expect(r.code).not.toBe(0);
    expect(r.stderr).toMatch(/out/);
  });

  test('`metro get-dependencies` prints the files in the graph, one per line', () => {
    // Takes --entry-file (required) and finds metro.config.js from the cwd.
    const r = metro(['get-dependencies', '--entry-file', path.join(project.root, 'index.js'), '--platform', 'ios'], project.root);
    expect(r.code).toBe(0);
    const files = r.stdout.trim().split('\n');
    expect(files).toEqual(expect.arrayContaining([path.join(project.root, 'index.js'), path.join(project.root, 'a.js')]));
    expect(files).not.toContain(path.join(project.root, 'unused.js'));
  });

  test('`metro get-dependencies --output` writes the list to a file', () => {
    const out = path.join(project.root, 'deps.txt');
    const r = metro(['get-dependencies', '--entry-file', path.join(project.root, 'index.js'), '--platform', 'ios', '--output', out], project.root);
    expect(r.code).toBe(0);
    expect(fs.readFileSync(out, 'utf8').trim().split('\n')).toContain(path.join(project.root, 'a.js'));
  });

  test('`metro get-dependencies` rejects --config (strict options) but accepts the entry file positionally', () => {
    const withConfig = metro(['get-dependencies', '--entry-file', path.join(project.root, 'index.js'), '--config', 'metro.config.js'], project.root);
    expect(withConfig.code).not.toBe(0);
    expect(withConfig.stderr).toContain('Unknown argument: config');
    const positional = metro(['get-dependencies', path.join(project.root, 'index.js'), '--platform', 'ios'], project.root);
    expect(positional.code).toBe(0);
    expect(positional.stdout).toContain(path.join(project.root, 'a.js'));
  });

  test('an unknown command exits non-zero', () => {
    expect(metro(['frobnicate'], project.root).code).not.toBe(0);
  });
});

describe('runBuild / buildGraph API', () => {
  let project, config;
  beforeEach(async () => {
    project = createProject(FILES);
    config = await project.loadConfig();
  });
  afterEach(() => project.cleanup());

  test('returns code and map and writes bundleOut and sourceMapOut', async () => {
    const bundleOut = path.join(project.root, 'dist/main.jsbundle');
    const sourceMapOut = path.join(project.root, 'dist/main.map');
    fs.mkdirSync(path.dirname(bundleOut));
    const result = await Metro.runBuild(config, {
      entry: 'index.js', platform: 'ios', dev: false, minify: false, sourceMap: true,
      sourceMapUrl: 'main.map', bundleOut, sourceMapOut,
    });
    expect(typeof result.code).toBe('string');
    expect(fs.readFileSync(bundleOut, 'utf8')).toBe(result.code);
    expect(JSON.parse(fs.readFileSync(sourceMapOut, 'utf8')).version).toBe(3);
    expect(result.code).toContain('//# sourceMappingURL=main.map');
    expect(execBundle(result.code).result).toBe('A-prod');
  });

  test('onBegin, onProgress and onComplete are called in order', async () => {
    const calls = [];
    await Metro.runBuild(config, {
      entry: 'index.js', platform: 'ios', dev: true, minify: false,
      onBegin: () => calls.push('begin'),
      onProgress: (done, total) => calls.push(done <= total ? 'progress' : 'bad-progress'),
      onComplete: () => calls.push('complete'),
    });
    expect(calls[0]).toBe('begin');
    expect(calls[calls.length - 1]).toBe('complete');
    expect(calls).toContain('progress');
    expect(calls).not.toContain('bad-progress');
  });

  test('builds succeed with different customTransformOptions values', async () => {
    const a = await Metro.runBuild(config, {entry: 'index.js', platform: 'ios', minify: false, customTransformOptions: {flavour: 'a'}});
    const b = await Metro.runBuild(config, {entry: 'index.js', platform: 'ios', minify: false, customTransformOptions: {flavour: 'b'}});
    expect(execBundle(a.code).result).toBe(execBundle(b.code).result);
  });

  test('buildGraph returns the module graph with dependencies', async () => {
    const graph = await Metro.buildGraph(config, {entries: [path.join(project.root, 'index.js')], platform: 'ios', dev: true, minify: false});
    const paths = [...graph.dependencies.keys()];
    expect(paths).toEqual(expect.arrayContaining([path.join(project.root, 'index.js'), path.join(project.root, 'a.js')]));
    expect(paths).not.toContain(path.join(project.root, 'unused.js'));
    const index = graph.dependencies.get(path.join(project.root, 'index.js'));
    expect([...index.dependencies.values()].map(d => d.absolutePath)).toContain(path.join(project.root, 'a.js'));
  });
});
