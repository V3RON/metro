'use strict';
// Usage: node napi.js <metro root> <app> <cache> <napi-cost.node>
const path = require('node:path');
const [METRO, APP, CACHE, ADDON] = process.argv.slice(2).map(p => path.resolve(p));
const PKG = n => path.join(METRO, 'packages', n);
require(PKG('metro-babel-register')).unstable_registerForMetroMonorepo();
const N = require(ADDON);
const now = () => Number(process.hrtime.bigint());
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
  config.resolver.resolveRequest = (c, m, p) => { rec.push({context: {...c, resolveRequest: null}, m, p}); return c.resolveRequest(c, m, p); };
  await Metro.runBuild(config, {entry: 'index.js', platform: 'ios', dev: true, minify: false});
  const args = {fileSystemLookup: [], getPackageForModule: [], resolveHastePackage: []};
  for (const r of rec) {
    const c = {...r.context};
    for (const k of Object.keys(args)) { const f = c[k]; c[k] = a => { args[k].push(a); return f(a); }; }
    try { resolve(c, r.m, r.p); } catch {}
  }
  const ctx = rec[0].context;
  const memo = (f, list) => { const m = new Map(list.map(a => [a, f(a)])); return a => m.get(a); };
  const res = {};
  const jsLoop = (f, list, n) => { let x = 0; const t = now(); for (let i = 0; i < n; i++) { const r = f(list[i % list.length]); if (r) x++; } const ns = (now() - t) / n; return ns + x * 0; };
  const best = fn => { fn(); let b = Infinity; for (let i = 0; i < 5; i++) b = Math.min(b, fn()); return Math.round(b); };
  const n = 200000;
  res.emptyRustToJs = best(() => N.emptyCall(() => {}, n));
  const fsl = memo(ctx.fileSystemLookup, args.fileSystemLookup);
  res.fileSystemLookup = {jsToJs: best(() => jsLoop(fsl, args.fileSystemLookup, n)), rustToJs: best(() => N.lookupCalls(fsl, args.fileSystemLookup, n)), realJsCallbackJsToJs: best(() => jsLoop(ctx.fileSystemLookup, args.fileSystemLookup, n))};
  const gpm = memo(ctx.getPackageForModule, args.getPackageForModule);
  res.getPackageForModule = {jsToJs: best(() => jsLoop(gpm, args.getPackageForModule, n)), rustToJsHandleOnly: best(() => N.packageCalls(gpm, JSON.stringify, args.getPackageForModule, n, false)), rustToJsJsonConvert: best(() => N.packageCalls(gpm, JSON.stringify, args.getPackageForModule, n / 10, true))};
  const hp = memo(ctx.resolveHastePackage, args.resolveHastePackage);
  res.resolveHastePackage = {jsToJs: best(() => jsLoop(hp, args.resolveHastePackage, n)), rustToJs: best(() => N.stringOrNullCalls(hp, args.resolveHastePackage, n))};
  res.jsToRustReadContext = best(() => { const t = now(); for (let i = 0; i < 50000; i++) N.readContext(rec[i % rec.length].context); return (now() - t) / 50000; });
  res.jsToRustNoopString = best(() => { const t = now(); for (let i = 0; i < n; i++) N.noop(rec[i % rec.length].m); return (now() - t) / n; });
  console.log(JSON.stringify(res, null, 1));
  require('node:fs').writeFileSync(path.join(path.dirname(process.argv[1]), 'napi.json'), JSON.stringify(res, null, 1));
  process.exit(0);
})();
