/**
 * Black-box test of metro-cache-key's getCacheKey with real files (the
 * package's own test mocks `fs`, so it only covers the JavaScript path).
 * The expected keys are computed here with node:crypto, independently of the
 * implementation under test, so the test passes in JS and in Rust mode.
 */
'use strict';

const crypto = require('node:crypto');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {PACKAGES} = require('../helpers/project');

const {getCacheKey} = require(path.join(PACKAGES, 'metro-cache-key'));

const reference = buffers => {
  const hash = crypto.createHash('md5');
  for (const b of buffers) {
    hash.update('\0', 'utf8').update(b);
  }
  return hash.digest('hex');
};

describe('getCacheKey', () => {
  let dir;
  const file = (name, content) => {
    const p = path.join(dir, name);
    fs.writeFileSync(p, content);
    return p;
  };

  beforeEach(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'metro-cache-key-'));
  });
  afterEach(() => {
    fs.rmSync(dir, {recursive: true, force: true});
  });

  test('no files', () => {
    expect(getCacheKey([])).toBe(reference([]));
  });

  test('text, empty and binary files', () => {
    const files = [
      file('a.js', 'const a = 1;\n'),
      file('empty', ''),
      file('bin', Buffer.from([0, 255, 128, 10, 13, 0])),
      file('unicode.txt', 'héllo \u{1F600}'),
    ];
    expect(getCacheKey(files)).toBe(
      reference([
        'const a = 1;\n',
        '',
        Buffer.from([0, 255, 128, 10, 13, 0]),
        'héllo \u{1F600}',
      ]),
    );
  });

  test('order and repetition matter, and so does the separator', () => {
    const a = file('a', 'ab');
    const b = file('b', 'c');
    const c = file('c', 'a');
    const d = file('d', 'bc');
    expect(getCacheKey([a, b])).not.toBe(getCacheKey([b, a]));
    expect(getCacheKey([a, b])).not.toBe(getCacheKey([c, d]));
    expect(getCacheKey([a, a])).toBe(reference(['ab', 'ab']));
  });

  test('a big file', () => {
    const content = Buffer.alloc(5 * 1024 * 1024, 'xyz');
    expect(getCacheKey([file('big', content)])).toBe(reference([content]));
  });

  test('a missing file throws the error of fs.readFileSync', () => {
    const good = file('good', 'x');
    const missing = path.join(dir, 'missing');
    let expected;
    try {
      fs.readFileSync(missing);
    } catch (e) {
      expected = e;
    }
    let actual;
    try {
      getCacheKey([good, missing]);
    } catch (e) {
      actual = e;
    }
    expect(actual).toBeDefined();
    expect(actual.code).toBe(expected.code);
    expect(actual.message).toBe(expected.message);
    expect(actual.syscall).toBe(expected.syscall);
  });

  test('a directory throws the error of fs.readFileSync', () => {
    let expected;
    try {
      fs.readFileSync(dir);
    } catch (e) {
      expected = e;
    }
    expect(() => getCacheKey([dir])).toThrow(expected.message);
  });
});
