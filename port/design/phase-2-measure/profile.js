'use strict';
// Usage: node profile.js <metro root> <app> <cache>
const path = require('node:path');
const fs = require('node:fs');
const [METRO, APP, CACHE] = process.argv.slice(2).map(p => path.resolve(p));
const PKG = n => path.join(METRO, 'packages', n);
require(PKG('metro-babel-register')).unstable_registerForMetroMonorepo();
const now = () => Number(process.hrtime.bigint()) / 1e6;
const CB = ['fileSystemLookup', 'doesFileExist', 'getPackage', 'getPackageForModule', 'resolveHasteModule', 'resolveHastePackage', 'resolveAsset', 'redirectModulePath', 'unstable_logWarning'];
(async () => {
  const Metro = require(PKG('metro'));
  const {FileStore} = require(PKG('metro-cache'));
  const {resolve} = require(PKG('metro-resolver'));
  process.chdir(APP);
  const config = await Metro.loadConfig({config: path.join(APP, 'metro.config.js')}, {maxWorkers: 4});
  config.cacheStores = [new FileStore({root: path.join(CACHE, 'transform')})];
  config.reporter = {update() {}};
  config.watchFolders = [APP, PKG('metro-runtime')];
  config.transformerPath = require.resolve(PKG('metro-transform-worker'));
  config.fileMapCacheDirectory = path.join(CACHE, 'filemap');
  const rec = [];
  const user = config.resolver.resolveRequest;
  console.log('user resolveRequest set:', user != null);
  config.resolver.resolveRequest = (c, m, p) => { rec.push({context: {...c, resolveRequest: null}, m, p}); return user ? user(c, m, p) : c.resolveRequest(c, m, p); };
  await Metro.runBuild(config, {entry: 'index.js', platform: 'ios', dev: true, minify: false});
  const pass = () => { for (const r of rec) { try { resolve(r.context, r.m, r.p); } catch {} } };
  pass(); pass();
  let t = now(); for (let i = 0; i < 5; i++) pass(); const totalMs = (now() - t) / 5;
  // Count callbacks and record args.
  const counts = {}, args = {};
  const wrapped = rec.map(r => {
    const c = {...r.context};
    for (const k of CB) if (typeof c[k] === 'function') { const f = c[k]; c[k] = (...a) => { counts[k] = (counts[k] ?? 0) + 1; (args[k] ??= []).push({f, a}); return f(...a); }; }
    return {...r, context: c};
  });
  for (const r of wrapped) { try { resolve(r.context, r.m, r.p); } catch {} }
  // Time replaying each callback's calls alone.
  const cbMs = {};
  for (const k of Object.keys(args)) {
    const list = args[k];
    for (let w = 0; w < 2; w++) for (const {f, a} of list) f(...a);
    const t0 = now(); for (let i = 0; i < 5; i++) for (const {f, a} of list) f(...a); cbMs[k] = (now() - t0) / 5;
  }
  // Context fields that are not callbacks.
  const ctx0 = rec[0].context;
  const fields = Object.keys(ctx0).filter(k => typeof ctx0[k] !== 'function');
  // Distinct values of the callback args
  const distinct = Object.fromEntries(Object.entries(args).map(([k, l]) => [k, new Set(l.map(x => JSON.stringify(x.a))).size]));
  // Distinct package.json objects
  const pkgs = new Set(); for (const {f, a} of args.getPackageForModule ?? []) { const r = f(...a); if (r) pkgs.add(r.packageJson); }
  const kinds = {relative: 0, absolute: 0, bare: 0, other: 0};
  for (const r of rec) { if (/^\.\.?(\/|$)/.test(r.m)) kinds.relative++; else if (r.m.startsWith('/')) kinds.absolute++; else if (/^[@a-zA-Z0-9_]/.test(r.m)) kinds.bare++; else kinds.other++; }
  const out = {resolutions: rec.length, kinds, msPerPass: totalMs, usPerResolve: totalMs * 1000 / rec.length, counts, perResolve: Object.fromEntries(Object.entries(counts).map(([k, v]) => [k, +(v / rec.length).toFixed(2)])), callbackMsPerPass: cbMs, callbackNsPerCall: Object.fromEntries(Object.entries(cbMs).map(([k, v]) => [k, Math.round(v * 1e6 / counts[k])])), distinctArgs: distinct, distinctPackageJsonObjects: pkgs.size, nonCallbackFields: fields, enablePackageExports: ctx0.unstable_enablePackageExports, mainFields: ctx0.mainFields, sourceExts: ctx0.sourceExts};
  out.callbackShare = Object.values(cbMs).reduce((a, b) => a + b, 0) / totalMs;
  console.log(JSON.stringify(out, null, 1));
  fs.writeFileSync(path.join(__dirname, 'profile.json'), JSON.stringify(out, null, 1));
  // Dump sample package.json sizes
  const sizes = [...pkgs].map(p => JSON.stringify(p).length).sort((a, b) => a - b);
  console.log('package.json bytes: median', sizes[sizes.length >> 1], 'max', sizes[sizes.length - 1], 'sum', sizes.reduce((a, b) => a + b, 0));
  process.exit(0);
})();
