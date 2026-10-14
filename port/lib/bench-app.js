/**
 * The generated React Native bench app, installed once from the pinned
 * lockfile in port/bench/app into port/.cache/bench-app.
 */
'use strict';

const {spawnSync} = require('node:child_process');
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');

const PORT = path.resolve(__dirname, '..');
const TEMPLATE = path.join(PORT, 'bench', 'app');
const APP = path.join(PORT, '.cache', 'bench-app');

function sh(cmd, args, opts) {
  const r = spawnSync(cmd, args, {stdio: ['ignore', 'pipe', 'pipe'], encoding: 'utf8', maxBuffer: 1 << 28, ...opts});
  if (r.status !== 0) {
    throw new Error(`${cmd} ${args.join(' ')} failed (exit ${r.status}):\n${(r.stderr || '').slice(-4000)}`);
  }
  return r.stdout;
}

exports.sh = sh;

exports.ensureBenchApp = function ensureBenchApp(log = () => {}) {
  const files = ['package.json', 'package-lock.json', 'gen.js', 'babel.config.js', 'metro.config.js'];
  const h = crypto.createHash('sha1');
  for (const f of files) h.update(fs.readFileSync(path.join(TEMPLATE, f)));
  const stamp = h.digest('hex');
  const stampFile = path.join(APP, '.stamp');
  if (fs.existsSync(stampFile) && fs.readFileSync(stampFile, 'utf8') === stamp) return APP;
  log('setting up the bench app (npm ci, then generating 1,500 components)');
  fs.rmSync(APP, {recursive: true, force: true});
  fs.mkdirSync(APP, {recursive: true});
  for (const f of files) fs.copyFileSync(path.join(TEMPLATE, f), path.join(APP, f));
  sh('npm', ['ci', '--legacy-peer-deps', '--no-audit', '--no-fund'], {cwd: APP});
  sh('node', ['gen.js', '1500'], {cwd: APP});
  fs.writeFileSync(stampFile, stamp);
  return APP;
};
