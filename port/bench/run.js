/**
 * One benchmark measurement, run in a fresh process. Used by compare.js.
 *
 *   node port/bench/run.js --metro <repo root> --app <app dir> --cache <dir> --scenario <name>
 *
 * Only Metro's public API is used (loadConfig, runBuild, runServer, and each
 * package's exports), so the same script measures the JavaScript and the Rust
 * implementation. METRO_RUST is inherited from the environment.
 *
 * Scenarios:
 *   cold-build    empty transform and file-map caches (dominated by Babel until phase 6)
 *   cold-filemap  warm transform cache, empty file-map cache (crawl + resolution + graph)
 *   warm-build    both caches warm: the everyday "start Metro again" case
 *   edit          dev server on warm caches, then 6 edits of a leaf module
 *   micro:<name>  port/bench/micro/<name>.js against the chosen Metro
 *   bundle        writes a bundle and map to --out (used by port/diff/differential.js);
 *                 --dev false builds the production variant, unminified
 *
 * Prints one JSON line: {scenario, ms, mainCpuMs, peakRssMB, ...extra}.
 */
'use strict';

const fs = require('node:fs');
const http = require('node:http');
const path = require('node:path');

const args = {};
for (let i = 2; i < process.argv.length; i += 2) {
  args[process.argv[i].replace(/^--/, '')] = process.argv[i + 1];
}
const METRO = path.resolve(args.metro);
const APP = path.resolve(args.app);
const CACHE = path.resolve(args.cache);
const SCENARIO = args.scenario;
const WORKERS = Number(args.workers ?? 4);
const PKG = name => path.join(METRO, 'packages', name);

// Metro's sources are Flow; this is how its own CLI runs from a checkout.
require(PKG('metro-babel-register')).unstable_registerForMetroMonorepo();

const now = () => Number(process.hrtime.bigint()) / 1e6;
const usage = () => {
  const cpu = process.cpuUsage();
  return (cpu.user + cpu.system) / 1000;
};
const emit = (ms, extra = {}) => {
  console.log(
    JSON.stringify({
      scenario: SCENARIO,
      ms: Math.round(ms * 10) / 10,
      mainCpuMs: Math.round(usage()),
      peakRssMB: Math.round(process.resourceUsage().maxRSS / 1024),
      ...extra,
    }),
  );
};

async function loadBenchConfig(Metro) {
  const {FileStore} = require(PKG('metro-cache'));
  process.chdir(APP);
  const config = await Metro.loadConfig(
    {config: path.join(APP, 'metro.config.js')},
    {maxWorkers: WORKERS},
  );
  // Set after loading: React Native's config would otherwise keep its own
  // cacheStores (a shared directory in os.tmpdir()), and loadConfig's
  // validation does not accept the other overrides as input.
  config.cacheStores = [
    new FileStore({
      root: path.resolve(args['transform-cache'] ?? path.join(CACHE, 'transform')),
    }),
  ];
  config.reporter = {update() {}};
  config.watchFolders = [APP, PKG('metro-runtime')];
  config.transformerPath = require.resolve(PKG('metro-transform-worker'));
  config.fileMapCacheDirectory = path.join(CACHE, 'filemap');
  fs.mkdirSync(config.fileMapCacheDirectory, {recursive: true});
  return config;
}

async function build() {
  const Metro = require(PKG('metro'));
  const config = await loadBenchConfig(Metro);
  const start = now();
  const result = await Metro.runBuild(config, {
    entry: 'index.js',
    platform: 'ios',
    dev: args.dev !== 'false',
    minify: false,
    sourceMap: true,
    sourceMapUrl: 'index.map',
    out: args.out,
  });
  const ms = now() - start;
  emit(ms, {modules: (result.code.match(/__d\(function/g) || []).length});
}

function get(port, urlPath) {
  return new Promise((resolve, reject) => {
    http
      .get({host: '127.0.0.1', port, path: urlPath}, res => {
        let body = '';
        res.setEncoding('utf8');
        res.on('data', d => (body += d));
        res.on('end', () => resolve({status: res.statusCode, body}));
      })
      .on('error', reject);
  });
}

async function edit() {
  const Metro = require(PKG('metro'));
  const config = await loadBenchConfig(Metro);
  const port = 19000 + (process.pid % 1000);
  config.server.port = port;
  const {httpServer} = await Metro.runServer(config, {host: '127.0.0.1', watch: true});
  const url = '/index.js.bundle?platform=ios&dev=true&minify=false';
  const leaf = path.join(APP, 'src/feature19/Comp1499.tsx');
  const original = fs.readFileSync(leaf, 'utf8');
  const times = [];
  try {
    let t = now();
    const first = await get(port, url);
    if (first.status !== 200) throw new Error(`initial bundle failed: HTTP ${first.status}`);
    const initialMs = now() - t;
    for (let i = 0; i < 6; i++) {
      const marker = `__benchEdit${i}_${Date.now()}`;
      fs.writeFileSync(leaf, `${original}\nexport const ${marker} = ${i};\n`);
      // Give the watcher time to deliver the change, then time the rebuild.
      await new Promise(r => setTimeout(r, 500));
      t = now();
      let res = await get(port, url);
      for (let tries = 0; !res.body.includes(marker) && tries < 20; tries++) {
        res = await get(port, url);
      }
      if (!res.body.includes(marker)) throw new Error('edit never reached the bundle');
      times.push(now() - t);
    }
    const later = times.slice(1).sort((a, b) => a - b);
    emit(later[Math.floor(later.length / 2)], {
      initialMs: Math.round(initialMs),
      firstEditMs: Math.round(times[0]),
      editsMs: times.map(Math.round),
    });
  } finally {
    fs.writeFileSync(leaf, original);
    await new Promise(r => httpServer.close(r));
  }
}

async function micro(name) {
  const file = path.join(__dirname, 'micro', `${name}.js`);
  if (!fs.existsSync(file)) throw new Error(`no micro-benchmark at ${path.relative(process.cwd(), file)}`);
  const {run} = require(file);
  const res = await run({metro: METRO, pkg: PKG, app: APP, cache: CACHE, now});
  emit(res.ms, res.extra ?? {});
}

(async () => {
  if (SCENARIO.startsWith('micro:')) await micro(SCENARIO.slice(6));
  else if (SCENARIO === 'edit') await edit();
  else if (['cold-build', 'cold-filemap', 'warm-build', 'bundle'].includes(SCENARIO)) await build();
  else throw new Error(`unknown scenario ${SCENARIO}`);
  process.exit(0);
})().catch(e => {
  console.error(e && e.stack ? e.stack : e);
  process.exit(1);
});
