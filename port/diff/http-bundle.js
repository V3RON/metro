/**
 * HTTP bundle differential check: the dev server's `.bundle` responses with
 * METRO_RUST=off and with the phase's components in Rust must be identical,
 * byte for byte. `differential.js` builds through runBuild and the CLI, which
 * do not take the path of a dev server response (from phase 5 the server's
 * response is assembled by Rust and sent as a Buffer).
 *
 *   node port/diff/http-bundle.js [--phase <n>] [--out <dir>]
 *   node port/diff/http-bundle.js --self-test     # METRO_RUST=off twice
 *
 * Exit codes: 0 identical, 1 the bundles differ, 2 the runner failed.
 *
 * Each side is a child process of this script (`--child`) that starts Metro's
 * dev server (the public `runServer`) on a fresh copy of the bench app, at the
 * same path and on the same port for both sides, and fetches these URLs:
 *
 *   ios dev, ios dev lazy, modulesOnly without running the module,
 *   inlineSourceMap, android production,
 *
 * then edits a leaf module (like the `edit` scenario of the benchmark), waits
 * until the server answers with the edit, and fetches the first URL again.
 * Both sides share one transform cache, as the other differentials do; the
 * file-map cache is fresh for each. The Content-Length of every response must
 * be the length of its body.
 */
'use strict';

const {execFileSync, spawn, spawnSync} = require('node:child_process');
const fs = require('node:fs');
const net = require('node:net');
const path = require('node:path');

const REPO = path.resolve(__dirname, '..', '..');
const PORT_DIR = path.join(REPO, 'port');
const WORK = path.join(PORT_DIR, '.cache', 'http-bundle');
const APP_COPY = path.join(WORK, 'app');
const TRANSFORM_CACHE = path.join(WORK, 'transform-shared');

const argv = process.argv.slice(2);
const flag = name => argv.includes(`--${name}`);
const opt = (name, dflt) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 ? argv[i + 1] : dflt;
};
const log = msg => console.error(`[http-bundle] ${msg}`);

const TIMEOUTS = {
  requestMs: 15 * 60 * 1000,
  editMs: 5 * 60 * 1000,
  childMs: 45 * 60 * 1000,
};

const FIRST = '/index.bundle?platform=ios&dev=true&minify=false';
const URLS = [
  FIRST,
  `${FIRST}&lazy=true`,
  `${FIRST}&modulesOnly=true&runModule=false`,
  `${FIRST}&inlineSourceMap=true`,
  '/index.bundle?platform=android&dev=false&minify=false',
];
const LEAF = 'src/feature19/Comp1499.tsx';
const MARKER = 'httpBundleEdit';

const sleep = ms => new Promise(r => setTimeout(r, ms));

function freePort() {
  return new Promise((resolve, reject) => {
    const s = net.createServer();
    s.once('error', reject);
    s.listen(0, '127.0.0.1', () => {
      const {port} = s.address();
      s.close(() => resolve(port));
    });
  });
}

function httpGet(port, urlPath) {
  const http = require('node:http');
  return new Promise((resolve, reject) => {
    const req = http.get({host: '127.0.0.1', port, path: urlPath}, res => {
      const chunks = [];
      res.on('data', d => chunks.push(d));
      res.on('end', () => resolve({status: res.statusCode, headers: res.headers, body: Buffer.concat(chunks)}));
    });
    req.on('error', reject);
    req.setTimeout(TIMEOUTS.requestMs, () => req.destroy(new Error(`request for ${urlPath} timed out`)));
  });
}

async function child() {
  const outDir = path.resolve(opt('out'));
  const app = path.resolve(opt('app'));
  const port = Number(opt('port'));
  const PKG = name => path.join(REPO, 'packages', name);
  require(PKG('metro-babel-register')).unstable_registerForMetroMonorepo();
  const Metro = require(PKG('metro'));
  const {FileStore} = require(PKG('metro-cache'));

  process.chdir(app);
  const config = await Metro.loadConfig({config: path.join(app, 'metro.config.js')}, {maxWorkers: 4});
  // Same overrides as port/bench/run.js; see loadBenchConfig there.
  config.cacheStores = [new FileStore({root: TRANSFORM_CACHE})];
  config.reporter = {update() {}};
  config.watchFolders = [app, PKG('metro-runtime')];
  config.transformerPath = require.resolve(PKG('metro-transform-worker'));
  config.fileMapCacheDirectory = path.join(path.dirname(app), 'filemap-cache');
  fs.rmSync(config.fileMapCacheDirectory, {recursive: true, force: true});
  fs.mkdirSync(config.fileMapCacheDirectory, {recursive: true});
  config.server.port = port;

  fs.rmSync(outDir, {recursive: true, force: true});
  fs.mkdirSync(outDir, {recursive: true});
  const fetchTo = async (urlPath, name) => {
    const res = await httpGet(port, urlPath);
    if (res.status !== 200) throw new Error(`${urlPath}: HTTP ${res.status}\n${res.body.toString('utf8', 0, 500)}`);
    if (Number(res.headers['content-length']) !== res.body.length) {
      throw new Error(`${urlPath}: Content-Length ${res.headers['content-length']} but the body is ${res.body.length} bytes`);
    }
    fs.writeFileSync(path.join(outDir, name), res.body);
    log(`${name}: ${res.body.length} bytes (${urlPath})`);
    return res.body;
  };

  let httpServer;
  let resolveClosed = () => {};
  const closed = new Promise(r => (resolveClosed = r));
  const leaf = path.join(app, LEAF);
  const original = fs.readFileSync(leaf, 'utf8');
  try {
    ({httpServer} = await Metro.runServer(config, {
      host: '127.0.0.1',
      watch: true,
      // Called once Metro has ended everything, including its transform workers.
      onClose: () => resolveClosed(),
    }));
    log(`dev server on port ${port} (METRO_RUST=${process.env.METRO_RUST})`);

    for (const [i, urlPath] of URLS.entries()) {
      await fetchTo(urlPath, `${i}.js`);
    }

    // Written to a temp file outside the watched tree and renamed into place,
    // so the watcher sees one event.
    const tmp = path.join(path.dirname(app), `.edit-${process.pid}`);
    fs.writeFileSync(tmp, `${original}\nexport const ${MARKER} = 1;\n`);
    fs.renameSync(tmp, leaf);
    const deadline = Date.now() + TIMEOUTS.editMs;
    for (;;) {
      const res = await httpGet(port, FIRST);
      if (res.status === 200 && res.body.includes(MARKER)) break;
      if (Date.now() > deadline) throw new Error('the edit never reached the bundle');
      await sleep(100);
    }
    await fetchTo(FIRST, `${URLS.length}.js`);
  } finally {
    fs.writeFileSync(leaf, original);
    if (httpServer) {
      httpServer.close();
      await closed;
    }
  }
}

function prepareApp() {
  const {ensureBenchApp} = require('../lib/bench-app');
  const source = ensureBenchApp(log);
  fs.rmSync(APP_COPY, {recursive: true, force: true});
  fs.mkdirSync(path.dirname(APP_COPY), {recursive: true});
  // APFS clones make the 500 MB node_modules copy instant; fall back to a
  // normal copy elsewhere.
  const tryCp = args => spawnSync('cp', args, {stdio: 'pipe'}).status === 0;
  if (!(process.platform === 'darwin' && tryCp(['-cR', source, APP_COPY])) && !tryCp(['-R', source, APP_COPY])) {
    throw new Error(`could not copy ${source} to ${APP_COPY}`);
  }
}

function runChild(label, metroRust, port, outDir) {
  prepareApp();
  log(`side "${label}" with METRO_RUST=${metroRust}`);
  return new Promise((resolve, reject) => {
    const c = spawn(
      process.execPath,
      [__filename, '--child', '--app', APP_COPY, '--out', outDir, '--port', String(port)],
      {env: {...process.env, METRO_RUST: metroRust, CI: '1'}, stdio: ['ignore', 'inherit', 'inherit']},
    );
    const timer = setTimeout(() => {
      c.kill('SIGKILL');
      reject(new Error(`side "${label}" did not finish within ${TIMEOUTS.childMs / 60000} minutes`));
    }, TIMEOUTS.childMs);
    c.on('exit', (code, signal) => {
      clearTimeout(timer);
      if (code !== 0) reject(new Error(`side "${label}" failed (exit ${code ?? signal}); see its log above`));
      else resolve();
    });
    c.on('error', reject);
  });
}

async function main() {
  const selfTest = flag('self-test');
  let rust = 'off';
  if (!selfTest) {
    const phase = Number(opt('phase', execFileSync('node', [path.join(PORT_DIR, 'lib/phase.js'), 'current']).toString().trim()));
    rust = execFileSync('node', [path.join(PORT_DIR, 'lib/phase.js'), 'components', String(phase)]).toString().trim();
    if (rust === 'off') log('phase has no Rust components yet: this compares JS with JS');
  }
  fs.mkdirSync(WORK, {recursive: true});
  const outDir = path.resolve(opt('out', path.join(PORT_DIR, '.gate', 'http-bundle')));
  const dirA = path.join(outDir, 'js');
  const dirB = path.join(outDir, selfTest ? 'js-again' : 'rust');
  // The same port on both sides: it is in the bundles' source map URLs.
  const port = await freePort();
  try {
    await runChild('js', 'off', port, dirA);
    await runChild(selfTest ? 'js-again' : 'rust', rust, port, dirB);
  } finally {
    const rm = target => fs.rmSync(target, {recursive: true, force: true, maxRetries: 10, retryDelay: 200});
    rm(APP_COPY);
    rm(path.join(WORK, 'filemap-cache'));
  }

  let failed = false;
  for (let i = 0; i <= URLS.length; i++) {
    const url = URLS[i] ?? `${FIRST} (after the edit)`;
    const a = fs.readFileSync(path.join(dirA, `${i}.js`));
    const b = fs.readFileSync(path.join(dirB, `${i}.js`));
    if (a.equals(b)) {
      console.log(`  identical (${a.length} bytes): ${url}`);
      continue;
    }
    failed = true;
    let offset = 0;
    while (offset < a.length && offset < b.length && a[offset] === b[offset]) offset++;
    const ctx = buf => JSON.stringify(buf.toString('utf8', Math.max(0, offset - 80), offset + 120));
    console.log(`  DIFFERENT: ${url}\n    JS ${a.length} bytes, Rust ${b.length} bytes; first difference at byte ${offset}\n    JS:   ${ctx(a)}\n    Rust: ${ctx(b)}`);
  }
  if (failed) {
    console.log(`HTTP bundle differential: FAILED. Responses are in ${path.relative(REPO, outDir)}/`);
    process.exitCode = 1;
    return;
  }
  console.log(`HTTP bundle differential: passed (${URLS.length + 1} responses identical${selfTest ? ', JS against JS' : ''}).`);
}

(flag('child') ? child() : main()).then(
  () => process.exit(process.exitCode ?? 0),
  e => {
    console.error(e.message || e);
    process.exit(2);
  },
);
