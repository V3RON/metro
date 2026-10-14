/**
 * Copies the cdylib produced by `cargo build --release` to metro-rs/metro-rs.node,
 * where index.js loads it.
 */
'use strict';

const fs = require('node:fs');
const path = require('node:path');

const PREFIX = process.platform === 'win32' ? '' : 'lib';
const EXT = {win32: 'dll', darwin: 'dylib'}[process.platform] ?? 'so';
const source = path.join(
  __dirname,
  '..',
  'target',
  'release',
  `${PREFIX}metro_rs.${EXT}`,
);
const target = path.join(__dirname, '..', 'metro-rs.node');

if (!fs.existsSync(source)) {
  process.stderr.write(`Expected cargo to have built ${source}\n`);
  process.exit(1);
}
fs.copyFileSync(source, target);
process.stdout.write(`metro-rs: wrote ${path.relative(process.cwd(), target)}\n`);
