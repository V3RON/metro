/**
 * Micro-benchmark: metro-resolver's public `resolve()` on the bench app's real
 * resolutions (about 4,000 per iOS dev build).
 *
 * Setup, untimed: one dev build of the bench app with Metro's public API
 * (loadConfig, runBuild), with a recording `resolveRequest` that captures the
 * context Metro hands to the resolver for every resolution it asks for (cache
 * misses only, as in a real build) and delegates to the default algorithm.
 * The captured contexts keep Metro's own file-map, package-cache and Haste
 * callbacks, so the timed loop does the same lookups a build does.
 *
 * Timed: `resolve(context, name, platform)` over every recorded resolution,
 * ITERATIONS times, after one warm-up pass. Failures (optional dependencies
 * that do not exist) are part of the workload and are counted.
 */
'use strict';

const fs = require('node:fs');
const path = require('node:path');

const ITERATIONS = 5;

exports.run = async ({pkg, app, cache, now}) => {
  const Metro = require(pkg('metro'));
  const {FileStore} = require(pkg('metro-cache'));
  const {resolve} = require(pkg('metro-resolver'));

  // The same configuration as port/bench/run.js uses for builds, so the
  // transform cache warmed by the other scenarios is reused.
  process.chdir(app);
  const config = await Metro.loadConfig({config: path.join(app, 'metro.config.js')}, {maxWorkers: 4});
  config.cacheStores = [new FileStore({root: path.join(cache, 'transform')})];
  config.reporter = {update() {}};
  config.watchFolders = [app, pkg('metro-runtime')];
  config.transformerPath = require.resolve(pkg('metro-transform-worker'));
  config.fileMapCacheDirectory = path.join(cache, 'filemap');
  fs.mkdirSync(config.fileMapCacheDirectory, {recursive: true});

  const recorded = [];
  const userResolveRequest = config.resolver.resolveRequest;
  config.resolver.resolveRequest = (context, moduleName, platform) => {
    // Replayed without a custom resolver: `resolve` then runs the default
    // algorithm, which is what this records the inputs of.
    recorded.push({context: {...context, resolveRequest: null}, moduleName, platform});
    return userResolveRequest != null
      ? userResolveRequest(context, moduleName, platform)
      : context.resolveRequest(context, moduleName, platform);
  };
  await Metro.runBuild(config, {entry: 'index.js', platform: 'ios', dev: true, minify: false});

  const pass = () => {
    let failed = 0;
    for (let i = 0; i < recorded.length; i++) {
      const r = recorded[i];
      try {
        resolve(r.context, r.moduleName, r.platform);
      } catch {
        failed++;
      }
    }
    return failed;
  };

  const failed = pass();
  const start = now();
  for (let i = 0; i < ITERATIONS; i++) pass();
  const ms = now() - start;
  return {ms, extra: {resolutions: recorded.length, failed, iterations: ITERATIONS}};
};
