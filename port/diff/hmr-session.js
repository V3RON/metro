/**
 * HMR session differential check: plays a fixed script of edits against a
 * Metro dev server on a copy of the bench app, once with METRO_RUST=off and
 * once with the phase's components in Rust, records every message the HMR
 * client receives on /hot, and requires the two recordings to be identical
 * after normalising what legitimately varies between runs.
 *
 *   node port/diff/hmr-session.js [--phase <n>] [--out <dir>]
 *   node port/diff/hmr-session.js --self-test     # METRO_RUST=off twice
 *
 * Exit codes: 0 identical, 1 the sessions differ, 2 the runner failed.
 *
 * Each run is a child process of this script (`--child`). It uses only
 * Metro's public API (loadConfig, runServer) and the WebSocket protocol.
 * Both runs use the same fresh copy of the bench app at the same path
 * (port/.cache/hmr-session/app), so absolute paths are the same on both
 * sides, and both share one transform cache (as the bundle differential does
 * before phase 6); the file-map cache is fresh for every run.
 *
 * Normalisations (applied to the recorded messages before comparing; module
 * ids, code, module order and message order are never touched):
 *   1. Absolute paths: the app copy directory becomes <APP>, the repo root
 *      becomes <REPO>. The same string appears in error messages, stack
 *      frames, sourceURLs and source maps URLs.
 *   2. The server port (in sourceURL / sourceMappingURL / error text)
 *      becomes <PORT>.
 *   3. Timestamps: any `Date.now()`-like value in the script's own edit
 *      markers does not exist (edits are deterministic), so there is no
 *      marker normalisation. Strings that look like ISO dates or 13-digit
 *      epoch milliseconds inside error messages become <TIME>.
 *   4. Revision ids and change ids, but only where they are opaque: they are
 *      replaced by <REV:n> / <CHANGE:n>, numbered in order of first
 *      appearance. This keeps their identity (the same id appearing twice
 *      stays the same placeholder, and two different ids stay different)
 *      without comparing the random/time-derived text itself.
 *   5. Error stack traces (`stack`) and error `originModulePath`/`targetModuleName`
 *      are path-normalised only; Metro's own source line numbers are left
 *      alone because a Rust-side difference there would be a real difference.
 *   6. Repeated identical error cycles: an (update-start, error, update-done)
 *      cycle that repeats the previous cycle's error exactly collapses into
 *      one. Metro sometimes reports a failed build twice, depending on how the
 *      watcher splits events (JS against JS varied between identical runs).
 *      Likewise an empty update cycle (nothing added, modified or deleted)
 *      that directly follows another cycle within the same edit is a second
 *      watcher event for the same write and is dropped. Empty cycles that are
 *      the first reaction to an edit are kept. The number collapsed is printed.
 *   7. Recording metadata (arrival time of each message) is not part of the
 *      comparison; only message bodies and order are.
 */
'use strict';

const {execFileSync, spawn, spawnSync} = require('node:child_process');
const fs = require('node:fs');
const net = require('node:net');
const path = require('node:path');

const REPO = path.resolve(__dirname, '..', '..');
const PORT_DIR = path.join(REPO, 'port');
const WORK = path.join(PORT_DIR, '.cache', 'hmr-session');
const APP_COPY = path.join(WORK, 'app');
const TRANSFORM_CACHE = path.join(WORK, 'transform-shared');

const argv = process.argv.slice(2);
const flag = name => argv.includes(`--${name}`);
const opt = (name, dflt) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 ? argv[i + 1] : dflt;
};
const log = msg => console.error(`[hmr] ${msg}`);

// Generous limits: file watching on a loaded Mac can be slow.
const TIMEOUTS = {
  initialBundleMs: 15 * 60 * 1000,
  registerMs: 2 * 60 * 1000,
  updateMs: 5 * 60 * 1000,
  settleMs: 2500, // quiet period after an update before the next edit
  noneSettleMs: 3000, // wait after an edit that is expected to produce nothing
  childMs: 45 * 60 * 1000,
};

const BUNDLE_PATH = '/index.bundle?platform=ios&dev=true&minify=false';
const LEAF = 'src/feature19/Comp1499.tsx';
const NEW_FILE = 'src/feature19/Added.ts';
const RENAME_FROM = 'src/feature18/Comp1498.tsx';
const RENAME_TO = 'src/feature18/Renamed1498.tsx';

// ---------------------------------------------------------------------------
// The script of edits. Each step has ops; each op says whether it is expected
// to produce an update cycle ('update') or nothing ('none'). Ops that touch a
// file outside the module graph are separate 'none' ops, so the number of
// watcher batches does not depend on timing.
// ---------------------------------------------------------------------------

function script(app) {
  const p = rel => path.join(app, rel);
  const read = rel => fs.readFileSync(p(rel), 'utf8');
  // Written to a temp file outside the watched tree and renamed into place,
  // so the watcher sees one event per edit instead of a truncate and a write.
  const write = (rel, text) => {
    const tmp = path.join(path.dirname(app), `.edit-${process.pid}`);
    fs.writeFileSync(tmp, text);
    fs.renameSync(tmp, p(rel));
  };
  const leaf = read(LEAF);
  const importLine = spec => `import ${JSON.stringify(spec)};\n`;
  const importerText = read('index.js');
  // Comp1498 is imported by index.js (a root) and by nothing else.
  const oldSpec = './src/feature18/Comp1498';
  const newSpec = './src/feature18/Renamed1498';
  if (!importerText.includes(oldSpec)) throw new Error(`index.js does not import ${oldSpec}`);

  return [
    {
      name: '1 modify a leaf module',
      ops: [{expect: 'update', run: () => write(LEAF, `${leaf}\nexport const hmrEdit1 = 1;\n`)}],
    },
    {
      name: '2 add a new file and import it from a leaf',
      ops: [
        {expect: 'none', run: () => write(NEW_FILE, 'export const added = 42;\nexport default function added2() {\n  return added;\n}\n')},
        {expect: 'update', run: () => write(LEAF, `${leaf}\nimport {added} from './Added';\nexport const hmrEdit2 = added;\n`)},
      ],
    },
    {
      name: '3 remove that import',
      ops: [{expect: 'update', run: () => write(LEAF, `${leaf}\nexport const hmrEdit3 = 3;\n`)}],
    },
    {
      name: '4 delete the new file',
      ops: [{expect: 'none', run: () => fs.rmSync(p(NEW_FILE))}],
    },
    {
      name: '5 add an import of a node_modules package to a leaf, then remove it',
      ops: [
        {expect: 'update', run: () => write(LEAF, `${importLine('ms')}${leaf}\nexport const hmrEdit5 = 5;\n`)},
        {expect: 'update', run: () => write(LEAF, `${leaf}\nexport const hmrEdit5b = 5;\n`)},
      ],
    },
    {
      name: '6 introduce a syntax error, then fix it',
      ops: [
        {expect: 'update', run: () => write(LEAF, `${leaf}\nexport const = ;\n`)},
        {expect: 'update', run: () => write(LEAF, `${leaf}\nexport const hmrEdit6 = 6;\n`)},
      ],
    },
    {
      name: '7 rename a file and update its importer',
      ops: [
        // The rename is done as copy, re-point the importer, delete the old
        // file, so no step leaves a dangling import and the number of update
        // cycles does not depend on how the watcher batches a rename.
        {expect: 'none', run: () => fs.copyFileSync(p(RENAME_FROM), p(RENAME_TO))},
        {expect: 'update', run: () => write('index.js', importerText.split(oldSpec).join(newSpec))},
        {expect: 'none', run: () => fs.rmSync(p(RENAME_FROM))},
      ],
    },
  ];
}

// ---------------------------------------------------------------------------
// Child: one session
// ---------------------------------------------------------------------------

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

const sleep = ms => new Promise(r => setTimeout(r, ms));

async function waitUntil(fn, ms, label) {
  const deadline = Date.now() + ms;
  for (;;) {
    const v = fn();
    if (v) return v;
    if (Date.now() > deadline) throw new Error(`timed out after ${Math.round(ms / 1000)}s waiting for ${label}`);
    await sleep(50);
  }
}

function httpGet(port, urlPath) {
  const http = require('node:http');
  return new Promise((resolve, reject) => {
    const req = http.get({host: '127.0.0.1', port, path: urlPath}, res => {
      let body = '';
      res.setEncoding('utf8');
      res.on('data', d => (body += d));
      res.on('end', () => resolve({status: res.statusCode, body}));
    });
    req.on('error', reject);
    req.setTimeout(TIMEOUTS.initialBundleMs, () => req.destroy(new Error('initial bundle request timed out')));
  });
}

async function child() {
  const outFile = opt('out');
  const app = path.resolve(opt('app'));
  const PKG = name => path.join(REPO, 'packages', name);
  require(PKG('metro-babel-register')).unstable_registerForMetroMonorepo();
  const Metro = require(PKG('metro'));
  const {FileStore} = require(PKG('metro-cache'));
  const WebSocket = require('ws');

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
  const port = await freePort();
  config.server.port = port;

  const recorded = [];
  let httpServer;
  let ws;
  let resolveClosed = () => {};
  const closed = new Promise(r => (resolveClosed = r));
  try {
    ({httpServer} = await Metro.runServer(config, {
      host: '127.0.0.1',
      watch: true,
      // Called once Metro has ended everything, including its transform
      // workers. The child waits for it, so no worker outlives it.
      onClose: () => resolveClosed(),
    }));
    log(`dev server on port ${port} (METRO_RUST=${process.env.METRO_RUST})`);

    // The graph must exist before a client can register for it (this is what
    // loading the bundle does on a device).
    const first = await httpGet(port, BUNDLE_PATH);
    if (first.status !== 200) throw new Error(`initial bundle failed: HTTP ${first.status}\n${first.body.slice(0, 500)}`);
    log(`initial bundle: ${first.body.length} bytes`);

    ws = new WebSocket(`ws://localhost:${port}/hot`);
    ws.on('message', data => {
      let m;
      try {
        m = JSON.parse(String(data));
      } catch {
        m = {type: '<unparsable>', raw: String(data).slice(0, 500)};
      }
      recorded.push(m);
    });
    let wsError = null;
    ws.on('error', e => (wsError = e));
    await waitUntil(() => {
      if (wsError) throw new Error(`WebSocket error: ${wsError.message}`);
      return ws.readyState === WebSocket.OPEN;
    }, 60000, 'the /hot WebSocket to open');

    const count = type => recorded.filter(m => m.type === type).length;
    ws.send(JSON.stringify({type: 'register-entrypoints', entryPoints: [`http://localhost:${port}${BUNDLE_PATH}`]}));
    await waitUntil(() => count('bundle-registered') >= 1, TIMEOUTS.registerMs, 'bundle-registered');

    for (const step of script(app)) {
      log(`step ${step.name}`);
      for (const op of step.ops) {
        const doneBefore = count('update-done');
        op.run();
        if (op.expect === 'update') {
          await waitUntil(() => {
            if (wsError) throw new Error(`WebSocket error: ${wsError.message}`);
            return count('update-done') > doneBefore;
          }, TIMEOUTS.updateMs, `an update cycle after step "${step.name}"`);
          await settle(recorded, TIMEOUTS.settleMs);
        } else {
          await settle(recorded, TIMEOUTS.noneSettleMs);
        }
        recorded.push({type: '<step-boundary>', body: {step: step.name, expect: op.expect}});
      }
    }
    fs.writeFileSync(outFile, JSON.stringify(recorded, null, 1));
    log(`recorded ${recorded.filter(m => m.type !== '<step-boundary>').length} messages`);
  } finally {
    try {
      ws?.terminate();
    } catch {}
    if (httpServer) {
      httpServer.close();
      await closed;
    }
  }
}

/** Waits until no message has arrived for `quietMs`. */
async function settle(recorded, quietMs) {
  let n = recorded.length;
  let last = Date.now();
  while (Date.now() - last < quietMs) {
    await sleep(100);
    if (recorded.length !== n) {
      n = recorded.length;
      last = Date.now();
    }
  }
}

// ---------------------------------------------------------------------------
// Normalisation
// ---------------------------------------------------------------------------

function normaliser(app, port) {
  const revs = new Map();
  const changes = new Map();
  const id = (map, tag, v) => {
    if (typeof v !== 'string') return v;
    if (!map.has(v)) map.set(v, `<${tag}:${map.size + 1}>`);
    return map.get(v);
  };
  const escape = s => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const appRe = new RegExp(escape(app), 'g');
  const repoRe = new RegExp(escape(REPO), 'g');
  const appUrlRe = new RegExp(escape(encodeURIComponent(app)), 'g');
  const portRe = new RegExp(`(localhost|127\\.0\\.0\\.1):${port}`, 'g');
  const str = s =>
    s
      .replace(appUrlRe, '<APP>')
      .replace(appRe, '<APP>')
      .replace(repoRe, '<REPO>')
      .replace(portRe, '$1:<PORT>')
      .replace(/\b\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?Z\b/g, '<TIME>')
      .replace(/\b1[5-9]\d{11}\b/g, '<TIME>');
  const walk = (v, key) => {
    if (typeof v === 'string') {
      if (key === 'revisionId') return id(revs, 'REV', v);
      if (key === 'changeId') return id(changes, 'CHANGE', v);
      return str(v);
    }
    if (Array.isArray(v)) return v.map(x => walk(x));
    if (v && typeof v === 'object') {
      const o = {};
      for (const k of Object.keys(v)) o[k] = walk(v[k], k);
      return o;
    }
    return v;
  };
  return walk;
}

/**
 * Collapses an error cycle (update-start, error, update-done) that repeats the
 * previous cycle's error exactly (ignoring changeId) into one. After a failed
 * build Metro may deliver the same failure a second time, depending on how the
 * file watcher splits events (JS against JS produced one or two on identical
 * runs). The count of collapsed cycles is returned in `collapsed`.
 */
function collapseRepeatedErrors(msgs) {
  const out = [];
  const key = m => JSON.stringify(m.body, (k, v) => (k === 'changeId' ? undefined : v));
  for (let i = 0; i < msgs.length; i++) {
    const m = msgs[i];
    const prev = out.length >= 3 ? out[out.length - 2] : null;
    if (
      m.type === 'update-start' && msgs[i + 1]?.type === 'error' && msgs[i + 2]?.type === 'update-done' &&
      prev && prev.type === 'error' && out[out.length - 3].type === 'update-start' && key(prev) === key(msgs[i + 1])
    ) {
      collapseRepeatedErrors.collapsed++;
      i += 2;
      continue;
    }
    const isEmptyUpdate = x => x?.type === 'update' && !x.body.isInitialUpdate && !x.body.added.length && !x.body.modified.length && !x.body.deleted.length;
    if (
      m.type === 'update-start' && isEmptyUpdate(msgs[i + 1]) && msgs[i + 2]?.type === 'update-done' &&
      out.length >= 3 && out[out.length - 1].type === 'update-done' && out[out.length - 3].type === 'update-start'
    ) {
      collapseRepeatedErrors.collapsed++;
      i += 2;
      continue;
    }
    out.push(m);
  }
  return out;
}
collapseRepeatedErrors.collapsed = 0;

// ---------------------------------------------------------------------------
// Parent
// ---------------------------------------------------------------------------

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

function runChild(label, metroRust) {
  prepareApp();
  const out = path.join(WORK, `messages.${label}.json`);
  fs.rmSync(out, {force: true});
  log(`session "${label}" with METRO_RUST=${metroRust}`);
  return new Promise((resolve, reject) => {
    const c = spawn(process.execPath, [__filename, '--child', '--app', APP_COPY, '--out', out], {
      env: {...process.env, METRO_RUST: metroRust, CI: '1'},
      stdio: ['ignore', 'inherit', 'inherit'],
    });
    const timer = setTimeout(() => {
      c.kill('SIGKILL');
      reject(new Error(`session "${label}" did not finish within ${TIMEOUTS.childMs / 60000} minutes`));
    }, TIMEOUTS.childMs);
    c.on('exit', (code, signal) => {
      clearTimeout(timer);
      if (code !== 0) reject(new Error(`session "${label}" failed (exit ${code ?? signal}); see its log above`));
      else resolve(JSON.parse(fs.readFileSync(out, 'utf8')));
    });
    c.on('error', reject);
  });
}

const trunc = (v, n = 1200) => {
  const s = typeof v === 'string' ? v : JSON.stringify(v, null, 1);
  return s.length > n ? `${s.slice(0, n)}... (${s.length - n} more chars)` : s;
};

/** Finds the first difference between two JSON values; returns a path or null. */
function firstDiff(a, b, where = '') {
  if (a === b) return null;
  if (typeof a !== typeof b || a === null || b === null || typeof a !== 'object') {
    if (typeof a === 'string' && typeof b === 'string') {
      let i = 0;
      while (i < a.length && a[i] === b[i]) i++;
      return `${where || '<root>'} at char ${i}: JS ${JSON.stringify(a.slice(Math.max(0, i - 60), i + 100))} vs Rust ${JSON.stringify(b.slice(Math.max(0, i - 60), i + 100))}`;
    }
    return `${where || '<root>'}: JS ${trunc(a, 200)} vs Rust ${trunc(b, 200)}`;
  }
  if (Array.isArray(a) !== Array.isArray(b)) return `${where}: array vs object`;
  const keys = new Set([...Object.keys(a), ...Object.keys(b)]);
  for (const k of keys) {
    if (!(k in a)) return `${where}.${k}: missing on the JS side`;
    if (!(k in b)) return `${where}.${k}: missing on the Rust side`;
    const d = firstDiff(a[k], b[k], `${where}${Array.isArray(a) ? `[${k}]` : `.${k}`}`);
    if (d) return d;
  }
  return null;
}

function compare(a, b) {
  const n = Math.max(a.length, b.length);
  for (let i = 0; i < n; i++) {
    const d = i >= a.length || i >= b.length ? 'one side has no such message' : firstDiff(a[i], b[i]);
    if (d) {
      return {index: i, detail: d, a: a[i], b: b[i]};
    }
  }
  return null;
}

function summarise(msgs) {
  const real = msgs.filter(m => m.type !== '<step-boundary>');
  const byType = {};
  for (const m of real) byType[m.type] = (byType[m.type] ?? 0) + 1;
  return {total: real.length, byType};
}

async function main() {
  const selfTest = flag('self-test');
  let rust = 'off';
  let phase = null;
  if (!selfTest) {
    phase = Number(opt('phase', execFileSync('node', [path.join(PORT_DIR, 'lib/phase.js'), 'current']).toString().trim()));
    rust = execFileSync('node', [path.join(PORT_DIR, 'lib/phase.js'), 'components', String(phase)]).toString().trim();
    if (rust === 'off') log('phase has no Rust components yet: this compares JS with JS');
  }
  fs.mkdirSync(WORK, {recursive: true});
  const outDir = path.resolve(opt('out', path.join(PORT_DIR, '.gate', 'hmr-session')));
  try {
    const rawA = await runChild('js', 'off');
    const rawB = await runChild(selfTest ? 'js-again' : 'rust', rust);
    // The port differs per run and was not recorded; recover it from the URLs.
    const portOf = msgs => {
      const m = JSON.stringify(msgs).match(/localhost:(\d+)/);
      return m ? m[1] : '0';
    };
    const normalise = msgs => {
      const walk = normaliser(APP_COPY, portOf(msgs));
      return collapseRepeatedErrors(msgs).map(m => walk(m));
    };
    const a = normalise(rawA);
    const b = normalise(rawB);
    fs.mkdirSync(outDir, {recursive: true});
    fs.writeFileSync(path.join(outDir, 'js.json'), JSON.stringify(a, null, 1));
    fs.writeFileSync(path.join(outDir, `${selfTest ? 'js-again' : 'rust'}.json`), JSON.stringify(b, null, 1));
    const sa = summarise(a);
    const sb = summarise(b);
    const diff = compare(a, b);
    if (collapseRepeatedErrors.collapsed) {
      console.log(`(collapsed ${collapseRepeatedErrors.collapsed} duplicate watcher cycle(s) across both sessions)`);
    }
    console.log(`HMR session A (METRO_RUST=off): ${sa.total} messages ${JSON.stringify(sa.byType)}`);
    console.log(`HMR session B (METRO_RUST=${rust}): ${sb.total} messages ${JSON.stringify(sb.byType)}`);
    if (diff) {
      const step = a.slice(0, diff.index + 1).filter(m => m.type === '<step-boundary>').length;
      console.log(`HMR session differential: FAILED. First difference is message #${diff.index} (during script op ${step + 1}): ${diff.detail}`);
      console.log(`--- A (JS) ---\n${trunc(diff.a)}\n--- B (${selfTest ? 'JS again' : 'Rust'}) ---\n${trunc(diff.b)}`);
      console.log(`Normalised recordings are in ${path.relative(REPO, outDir)}/`);
      process.exitCode = 1;
      return;
    }
    console.log(`HMR session differential: passed (${sa.total} messages identical${selfTest ? ', JS against JS' : ''}).`);
  } finally {
    // Retries cover a process that is still writing as it exits (e.g. a
    // Babel register cache under node_modules/.cache).
    const rm = target => fs.rmSync(target, {recursive: true, force: true, maxRetries: 10, retryDelay: 200});
    rm(APP_COPY);
    rm(path.join(WORK, 'filemap-cache'));
  }
}

(flag('child') ? child() : main()).then(
  () => process.exit(process.exitCode ?? 0),
  e => {
    console.error(e.message || e);
    process.exit(2);
  },
);
