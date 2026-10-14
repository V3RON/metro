/**
 * Copies the cdylib produced by `cargo build` (profile given as the first
 * argument: release or dev-fast; default release) to metro-rs/metro-rs.node,
 * where index.js loads it.
 */
'use strict';

const fs = require('node:fs');
const path = require('node:path');

const PROFILE = process.argv[2] ?? 'release';
const PREFIX = process.platform === 'win32' ? '' : 'lib';
const EXT = {win32: 'dll', darwin: 'dylib'}[process.platform] ?? 'so';
const source = path.join(
  __dirname,
  '..',
  'target',
  PROFILE,
  `${PREFIX}metro_rs.${EXT}`,
);
const target = path.join(__dirname, '..', 'metro-rs.node');

if (!fs.existsSync(source)) {
  process.stderr.write(`Expected cargo to have built ${source}\n`);
  process.exit(1);
}
fs.copyFileSync(source, target);
process.stdout.write(`metro-rs: wrote ${path.relative(process.cwd(), target)}\n`);
