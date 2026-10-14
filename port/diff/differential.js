/**
 * Differential check: builds the same apps with METRO_RUST=off and with the
 * phase's components in Rust, and requires byte-identical bundles and maps.
 *
 *   node port/diff/differential.js --phase <n> [--out port/.gate/diff]
 *
 * Apps: the generated bench app (dev and production, unminified) and Metro's
 * own integration-test bundle built through the CLI. Before phase 6 the
 * transform cache is shared by both sides, because transforms are still the
 * same JavaScript; the file map, graph and serializers run separately.
 */
'use strict';

const {execFileSync, spawnSync} = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const {ensureBenchApp} = require('../lib/bench-app');

const REPO = path.resolve(__dirname, '..', '..');
const PORT = path.join(REPO, 'port');
const argv = process.argv.slice(2);
const opt = (name, dflt) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 ? argv[i + 1] : dflt;
};
const phaseJs = (...a) => execFileSync('node', [path.join(PORT, 'lib/phase.js'), ...a]).toString().trim();
const PHASE = Number(opt('phase', phaseJs('current')));
const RUST = phaseJs('components', String(PHASE));
const OUT = path.resolve(opt('out', path.join(PORT, '.gate', 'diff')));
const WORK = path.join(PORT, '.cache', 'diff');
const log = msg => console.error(`[diff] ${msg}`);

function run(cmd, args, env, cwd = REPO) {
  const r = spawnSync(cmd, args, {cwd, env: {...process.env, CI: '1', ...env}, encoding: 'utf8', maxBuffer: 1 << 26, timeout: 20 * 60 * 1000});
  if (r.status !== 0) throw new Error(`${path.basename(args[0] ?? cmd)} failed with METRO_RUST=${env.METRO_RUST}:\n${(r.stderr || r.stdout || '').slice(-4000)}`);
}

function buildAll(side, metroRust, app) {
  const dir = path.join(WORK, side);
  fs.rmSync(path.join(dir, 'out'), {recursive: true, force: true});
  fs.mkdirSync(path.join(dir, 'out'), {recursive: true});
  const shared = PHASE < 6 ? path.join(WORK, 'transform-shared') : path.join(dir, 'transform');
  const env = {METRO_RUST: metroRust};
  for (const dev of ['true', 'false']) {
    log(`bench app, dev=${dev}, METRO_RUST=${metroRust}`);
    // A fresh file-map cache each time, so the file map really runs.
    fs.rmSync(path.join(dir, 'cache'), {recursive: true, force: true});
    run(process.execPath, [
      path.join(PORT, 'bench', 'run.js'), '--metro', REPO, '--app', app, '--cache', path.join(dir, 'cache'),
      '--transform-cache', shared, '--scenario', 'bundle', '--dev', dev,
      '--out', path.join(dir, 'out', `bench.${dev === 'true' ? 'dev' : 'prod'}.js`),
    ], env);
  }
  log(`integration-test bundle via the CLI, METRO_RUST=${metroRust}`);
  run(process.execPath, [
    path.join(REPO, 'packages/metro/src/cli.js'), 'build', 'TestBundle.js',
    '--config', path.join(REPO, 'packages/metro/src/integration_tests/metro.config.js'),
    '--platform', 'ios', '--dev', 'true', '--minify', 'false', '--reset-cache',
    '--out', path.join(dir, 'out', 'TestBundle.js'), '--source-map', 'true',
  ], env);
  return path.join(dir, 'out');
}

/** Describes where two bundles first differ, by module where possible. */
function explain(a, b) {
  let i = 0;
  while (i < a.length && i < b.length && a[i] === b[i]) i++;
  const before = a.lastIndexOf('__d(', i);
  const line = a.slice(0, i).split('\n').length;
  const snippet = s => JSON.stringify(s.slice(Math.max(0, i - 120), i + 200));
  let module = '';
  if (before >= 0) {
    const end = a.indexOf('\n__d(', i);
    const tail = a.slice(before, end < 0 ? undefined : end);
    const name = tail.match(/,\s*"([^"]+)"\);?\s*$/);
    if (name) module = ` in module "${name[1]}"`;
  }
  return `first difference at byte ${i} (line ${line})${module}\n      JS:   ${snippet(a)}\n      Rust: ${snippet(b)}`;
}

function main() {
  const app = ensureBenchApp(log);
  if (RUST === 'off') log('phase has no Rust components yet: this compares JS with JS, which checks that builds are deterministic');
  const js = buildAll('js', 'off', app);
  const rust = buildAll('rust', RUST, app);
  const results = [];
  for (const f of fs.readdirSync(js).sort()) {
    const a = fs.readFileSync(path.join(js, f), 'utf8');
    const bPath = path.join(rust, f);
    if (!fs.existsSync(bPath)) {
      results.push({file: f, same: false, detail: 'missing on the Rust side'});
      continue;
    }
    const b = fs.readFileSync(bPath, 'utf8');
    results.push({file: f, same: a === b, bytes: a.length, detail: a === b ? '' : explain(a, b)});
  }
  const passed = results.length > 0 && results.every(r => r.same);
  fs.mkdirSync(OUT, {recursive: true});
  fs.writeFileSync(path.join(OUT, 'diff.json'), JSON.stringify({phase: PHASE, metroRust: RUST, passed, results}, null, 2) + '\n');
  for (const r of results) console.log(`${r.same ? 'same' : 'DIFF'}  ${r.file}${r.same ? ` (${r.bytes} bytes)` : `\n      ${r.detail}`}`);
  console.log(passed ? 'Differential check: passed.' : `Differential check: failed. Outputs are in ${path.relative(REPO, WORK)}/{js,rust}/out.`);
  process.exit(passed ? 0 : 1);
}

try {
  main();
} catch (e) {
  console.error(e.message || e);
  process.exit(2);
}
