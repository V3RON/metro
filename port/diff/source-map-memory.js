/**
 * Memory check for the Rust source map writer: runBuild on the bench app with
 * a source map, then two forced garbage collections (as port/bench/run.js
 * does), then the resident memory and what the writer still holds in Rust.
 *
 *   node --expose-gc port/diff/source-map-memory.js [--cache <dir>]
 *
 * METRO_RUST is inherited. Run it with the phase's components, and again
 * without `serializers`, to compare. With `serializers` on, `rustEntries`
 * must be 0: runBuild's maps are one-shot and leave nothing in Rust.
 * Prints one JSON line. Uses the bench app and the caches of the benchmark
 * (default port/.cache/bench/mem-p5); run it once first to warm them.
 */
'use strict';

const fs = require('node:fs');
const path = require('node:path');

const REPO = path.resolve(__dirname, '..', '..');
const argv = process.argv.slice(2);
const opt = (name, dflt) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 ? argv[i + 1] : dflt;
};
const CACHE = path.resolve(opt('cache', path.join(REPO, 'port', '.cache', 'bench', 'mem-p5')));
const PKG = name => path.join(REPO, 'packages', name);

require(PKG('metro-babel-register')).unstable_registerForMetroMonorepo();

(async () => {
  if (typeof global.gc !== 'function') throw new Error('run with node --expose-gc');
  const {ensureBenchApp} = require('../lib/bench-app');
  const app = ensureBenchApp(() => {});
  const Metro = require(PKG('metro'));
  const {FileStore} = require(PKG('metro-cache'));
  process.chdir(app);
  const config = await Metro.loadConfig({config: path.join(app, 'metro.config.js')}, {maxWorkers: 4});
  config.cacheStores = [new FileStore({root: path.join(CACHE, 'transform')})];
  config.reporter = {update() {}};
  config.watchFolders = [app, PKG('metro-runtime')];
  config.transformerPath = require.resolve(PKG('metro-transform-worker'));
  config.fileMapCacheDirectory = path.join(CACHE, 'filemap');
  fs.mkdirSync(config.fileMapCacheDirectory, {recursive: true});

  const result = await Metro.runBuild(config, {
    entry: 'index.js',
    platform: 'ios',
    dev: true,
    minify: false,
    sourceMap: true,
    sourceMapUrl: 'index.map',
  });
  const mb = bytes => Math.round(bytes / 1048576);
  const before = mb(process.memoryUsage.rss());
  global.gc();
  global.gc();
  const {rustSourceMapStats} = require(PKG('metro/src/DeltaBundler/Serializers/rustSourceMap'));
  const stats = rustSourceMapStats();
  console.log(
    JSON.stringify({
      metroRust: process.env.METRO_RUST,
      mapBytes: result.map.length,
      rssBeforeGcMB: before,
      afterGcRssMB: mb(process.memoryUsage.rss()),
      rustCaches: stats.caches,
      rustEntries: stats.entries,
    }),
  );
  process.exit(0);
})().catch(e => {
  console.error(e);
  process.exit(1);
});
