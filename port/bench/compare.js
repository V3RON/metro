/**
 * Performance gate: pure-JS Metro (baselineRef, METRO_RUST=off) against this
 * checkout with the phase's components switched to Rust, on the same machine,
 * runs interleaved so drift hits both sides equally.
 *
 *   node port/bench/compare.js --phase <n> [--runs 3] [--quick] [--out port/.gate/perf]
 *
 * Writes perf.json and perf.md to --out. Exit code 0 when every scenario meets
 * its target and nothing regresses beyond tolerance, 1 otherwise.
 */
'use strict';

const {execFileSync, spawnSync} = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const REPO = path.resolve(__dirname, '..', '..');
const PORT = path.join(REPO, 'port');
const CACHE = path.join(PORT, '.cache');

const argv = process.argv.slice(2);
const flag = name => argv.includes(`--${name}`);
const opt = (name, dflt) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 ? argv[i + 1] : dflt;
};
const PHASE = Number(opt('phase', execFileSync('node', [path.join(PORT, 'lib/phase.js'), 'current']).toString().trim()));
const QUICK = flag('quick');
const RUNS = Number(opt('runs', QUICK ? 1 : 3));
const OUT = path.resolve(opt('out', path.join(PORT, '.gate', 'perf')));
const plan = JSON.parse(execFileSync('node', [path.join(PORT, 'lib/phase.js'), 'json', String(PHASE)]).toString());
const COMPONENTS = execFileSync('node', [path.join(PORT, 'lib/phase.js'), 'components', String(PHASE)]).toString().trim();

const log = msg => console.error(`[perf] ${msg}`);
const {ensureBenchApp, sh} = require('../lib/bench-app');

/** A clean checkout of baselineRef with its own node_modules. */
function ensureBaseline() {
  const override = opt('baseline-root');
  if (override) return path.resolve(override);
  const ref = plan.baselineRef;
  const dir = path.join(CACHE, `baseline-${ref.slice(0, 12)}`);
  if (fs.existsSync(path.join(dir, '.ready'))) return dir;
  log(`creating the pure-JS baseline checkout at ${ref.slice(0, 12)}`);
  fs.rmSync(dir, {recursive: true, force: true});
  sh('git', ['-C', REPO, 'worktree', 'prune']);
  try {
    sh('git', ['-C', REPO, 'cat-file', '-e', `${ref}^{commit}`]);
  } catch {
    sh('git', ['-C', REPO, 'fetch', '--depth', '1', 'origin', ref]);
  }
  sh('git', ['-C', REPO, 'worktree', 'add', '--detach', dir, ref]);
  sh('yarn', ['install', '--frozen-lockfile', '--prefer-offline', '--ignore-scripts'], {cwd: dir});
  fs.writeFileSync(path.join(dir, '.ready'), ref);
  return dir;
}

function runOnce(side, scenario, app) {
  const cache = path.join(CACHE, 'bench', side.name);
  if (scenario === 'cold-build') fs.rmSync(cache, {recursive: true, force: true});
  if (scenario === 'cold-filemap') fs.rmSync(path.join(cache, 'filemap'), {recursive: true, force: true});
  fs.mkdirSync(cache, {recursive: true});
  const args = [path.join(__dirname, 'run.js'), '--metro', side.root, '--app', app, '--cache', cache, '--scenario', scenario];
  // The baseline's cold build also writes the source map the source-map micro-benchmark reads.
  if (scenario === 'cold-build' && side.name === 'baseline') args.push('--out', path.join(CACHE, 'bench', 'bench.js'));
  const start = process.hrtime.bigint();
  const r = spawnSync(process.execPath, args, {
    env: {...process.env, METRO_RUST: side.metroRust, CI: '1'},
    encoding: 'utf8',
    timeout: 15 * 60 * 1000,
    maxBuffer: 1 << 26,
  });
  const wallMs = Number(process.hrtime.bigint() - start) / 1e6;
  const line = (r.stdout || '').trim().split('\n').filter(l => l.startsWith('{')).pop();
  if (r.status !== 0 || !line) {
    throw new Error(`${scenario} on ${side.name} failed (exit ${r.status}):\n${(r.stderr || '').slice(-4000)}`);
  }
  return {...JSON.parse(line), wallMs: Math.round(wallMs)};
}

const median = xs => {
  const s = [...xs].sort((a, b) => a - b);
  const m = Math.floor(s.length / 2);
  return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2;
};
const spread = xs => (xs.length < 2 ? 0 : (Math.max(...xs) - Math.min(...xs)) / median(xs) / 2);

function main() {
  const app = ensureBenchApp(log);
  const baselineRoot = ensureBaseline();
  const sides = [
    {name: 'baseline', root: baselineRoot, metroRust: 'off'},
    {name: 'candidate', root: REPO, metroRust: COMPONENTS},
  ];
  const targets = plan.perf.targets ?? {};
  const informational = new Set(plan.perf.informational ?? []);
  const micro = fs.readdirSync(path.join(__dirname, 'micro')).filter(f => f.endsWith('.js')).map(f => `micro:${f.slice(0, -3)}`);
  // Quick runs skip cold builds once both sides' caches and the source map
  // the micro-benchmark reads exist; they only warm the caches otherwise.
  const warm =
    sides.every(side => fs.existsSync(path.join(CACHE, 'bench', side.name, 'transform'))) &&
    fs.existsSync(path.join(CACHE, 'bench', 'bench.map'));
  const scenarios = [...(QUICK && warm ? [] : ['cold-build']), 'cold-filemap', 'warm-build', 'edit', ...micro];
  const missing = Object.keys(targets).filter(s => !scenarios.includes(s));
  const runsFor = s => (s === 'cold-build' ? (QUICK ? 1 : Math.max(1, RUNS - 1)) : RUNS);

  const samples = {};
  for (const s of scenarios) samples[s] = {baseline: [], candidate: []};
  for (const s of scenarios) {
    for (let i = 0; i < runsFor(s); i++) {
      // Alternate which side goes first so slow drift does not favour one side.
      const order = i % 2 ? [...sides].reverse() : sides;
      for (const side of order) {
        log(`${s} ${side.name} run ${i + 1}/${runsFor(s)}`);
        samples[s][side.name].push(runOnce(side, s, app));
      }
    }
  }

  const tol = plan.maxRegression;
  const rows = [];
  for (const s of scenarios) {
    const b = samples[s].baseline;
    const c = samples[s].candidate;
    const bMs = median(b.map(x => x.ms));
    const cMs = median(c.map(x => x.ms));
    const speedup = bMs / cMs;
    const noise = Math.max(spread(b.map(x => x.ms)), spread(c.map(x => x.ms)));
    const allowed = Math.max(tol, noise);
    let status = 'ok';
    let note = '';
    if (targets[s] != null) {
      status = speedup >= targets[s] ? 'ok' : 'fail';
      note = `target ${targets[s]}x`;
    } else if (informational.has(s) || (QUICK && s === 'cold-build')) {
      status = 'info';
    } else if (speedup < 1 / (1 + allowed)) {
      status = 'fail';
      note = `regressed more than ${Math.round(allowed * 100)}%`;
    }
    const bRss = median(b.map(x => x.peakRssMB));
    const cRss = median(c.map(x => x.peakRssMB));
    if (status === 'ok' && !s.startsWith('micro:') && cRss > bRss * 1.1) {
      status = 'fail';
      note = `peak memory up ${Math.round((cRss / bRss - 1) * 100)}%`;
    }
    // A single quick run has no noise estimate, so a regression there is only
    // a warning; full runs and --report enforce it. Missed targets still fail.
    if (QUICK && status === 'fail' && targets[s] == null) {
      status = 'warn';
      note += '; single run, confirm with a full ./gate.sh';
    }
    rows.push({
      scenario: s,
      baselineMs: Math.round(bMs),
      candidateMs: Math.round(cMs),
      speedup: Math.round(speedup * 100) / 100,
      noisePct: Math.round(noise * 1000) / 10,
      baselineRssMB: bRss,
      candidateRssMB: cRss,
      runs: b.length,
      status,
      note,
      extra: {baseline: b.map(({scenario, ...x}) => x), candidate: c.map(({scenario, ...x}) => x)},
    });
  }
  for (const s of missing) {
    rows.push({scenario: s, status: 'fail', note: `target ${targets[s]}x but port/bench/micro/${s.replace('micro:', '')}.js does not exist`});
  }

  const git = (...a) => sh('git', ['-C', REPO, ...a]).trim();
  const result = {
    phase: PHASE,
    title: plan.title,
    date: new Date().toISOString(),
    quick: QUICK,
    passed: rows.every(r => r.status !== 'fail'),
    baseline: {ref: plan.baselineRef, metroRust: 'off'},
    candidate: {commit: git('rev-parse', 'HEAD'), dirty: git('status', '--porcelain', '--untracked-files=no') !== '', metroRust: COMPONENTS},
    machine: {cpu: os.cpus()[0]?.model, cores: os.cpus().length, memGB: Math.round(os.totalmem() / 2 ** 30), os: `${os.type()} ${os.release()}`, node: process.version},
    rows,
  };
  fs.mkdirSync(OUT, {recursive: true});
  fs.writeFileSync(path.join(OUT, 'perf.json'), JSON.stringify(result, null, 2) + '\n');
  fs.writeFileSync(path.join(OUT, 'perf.md'), toMarkdown(result));
  process.stdout.write(toMarkdown(result));
  process.exit(result.passed ? 0 : 1);
}

function toMarkdown(r) {
  const icon = {ok: 'pass', fail: '**FAIL**', info: 'info', warn: 'warning'};
  const lines = [
    `### Performance, phase ${r.phase} (${r.title})${r.quick ? ' — quick run, not valid for a phase report' : ''}`,
    '',
    `Pure JS at \`${r.baseline.ref.slice(0, 12)}\` against \`${r.candidate.commit.slice(0, 12)}\`${r.candidate.dirty ? ' (uncommitted changes)' : ''} with \`METRO_RUST=${r.candidate.metroRust}\`. ` +
      `${r.machine.cores} × ${r.machine.cpu}, ${r.machine.memGB} GB, ${r.machine.os}, Node ${r.machine.node}. Medians of ${Math.max(...r.rows.map(x => x.runs || 0))} interleaved runs.`,
    '',
    '| Scenario | Pure JS | With Rust | Speed-up | Noise | Peak RSS (JS → Rust) | Result |',
    '|---|---:|---:|---:|---:|---|---|',
  ];
  for (const x of r.rows) {
    if (x.baselineMs == null) {
      lines.push(`| ${x.scenario} | | | | | | ${icon[x.status]} ${x.note} |`);
      continue;
    }
    lines.push(
      `| ${x.scenario} | ${x.baselineMs} ms | ${x.candidateMs} ms | ${x.speedup}× | ±${x.noisePct}% | ${x.baselineRssMB} → ${x.candidateRssMB} MB | ${icon[x.status]}${x.note ? ` (${x.note})` : ''} |`,
    );
  }
  const edit = r.rows.find(x => x.scenario === 'edit');
  if (edit?.extra) {
    const first = side => median(edit.extra[side].map(x => x.firstEditMs));
    lines.push('', `First edit after a warm start: ${Math.round(first('baseline'))} ms → ${Math.round(first('candidate'))} ms.`);
  }
  lines.push('', r.passed ? 'Performance gate: **passed**.' : 'Performance gate: **failed**.', '');
  return lines.join('\n');
}

try {
  main();
} catch (e) {
  console.error(e.message || e);
  process.exit(2);
}
