/**
 * Micro-benchmark: a cold metro-file-map crawl of the bench app and its
 * node_modules (about 24,000 files), with SHA-1s and Haste, no disk cache.
 * Uses only metro-file-map's public exports, configured the way Metro does.
 */
'use strict';

exports.run = async ({pkg, app, now}) => {
  const {default: FileMap, HastePlugin, NoopCacheManager} = require(pkg('metro-file-map'));
  const haste = new HastePlugin({
    platforms: new Set(['ios', 'android', 'native']),
    hasteImplModulePath: null,
    enableHastePackages: false,
    rootDir: app,
    failValidationOnConflicts: false,
  });
  const fileMap = new FileMap({
    cacheManagerFactory: () => new NoopCacheManager(),
    computeSha1: true,
    enableSymlinks: true,
    healthCheck: {enabled: false, interval: 30000, timeout: 5000, filePrefix: '.metro-health-check'},
    extensions: ['js', 'jsx', 'json', 'ts', 'tsx', 'cjs', 'mjs', 'png', 'jpg'],
    maxWorkers: 4,
    plugins: [haste],
    retainAllFiles: true,
    resetCache: true,
    rootDir: app,
    roots: [app],
    useWatchman: false,
    watch: false,
  });
  const start = now();
  const {fileSystem} = await fileMap.build();
  const ms = now() - start;
  await fileMap.end();
  return {ms, extra: {files: fileSystem.getAllFiles().length}};
};
