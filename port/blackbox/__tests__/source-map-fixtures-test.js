/**
 * Language-neutral source-map conformance runner (JS side).
 *
 * Reads ../fixtures/source-map.json and runs every case through
 * metro-source-map's public API. The cargo test in
 * metro-rs/crates/source-map/tests/fixtures.rs reads the same file.
 *
 * Inputs come from ../helpers/source-map-cases.js. UPDATE_FIXTURES=1 records
 * the JavaScript result into each case; it refuses to run while METRO_RUST
 * selects Rust, so the expectations can only come from the reference.
 *
 * Results are compared as JSON text, so key order counts.
 */
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const {PACKAGES} = require('../helpers/project');

const SM = require(path.join(PACKAGES, 'metro-source-map'));

const FIXTURES = path.join(__dirname, '..', 'fixtures', 'source-map.json');
const data = JSON.parse(fs.readFileSync(FIXTURES, 'utf8'));
const UPDATE = process.env.UPDATE_FIXTURES === '1';

if (UPDATE && !['off', '', undefined].includes(process.env.METRO_RUST)) {
  throw new Error('UPDATE_FIXTURES=1 must run with METRO_RUST=off');
}

const clone = x => JSON.parse(JSON.stringify(x));
const json = x => JSON.stringify(x);

function guard(fn) {
  try {
    return {expect: clone(fn())};
  } catch (e) {
    return {expectError: String(e && e.message)};
  }
}

function generatorResult(g, c) {
  const options = c.excludeSource ? {excludeSource: true} : undefined;
  return {
    map: g.toMap(c.file, options),
    text: g.toString(c.file, options),
  };
}

const runners = {
  fromRawMappings: c =>
    guard(() => generatorResult(SM.fromRawMappings(clone(c.modules), c.offsetLines), c)),
  fromRawMappingsIndexed: c =>
    guard(() =>
      generatorResult(SM.fromRawMappingsIndexed(clone(c.modules), c.offsetLines), c),
    ),
  vlqMapFromTuples: c => guard(() => SM.vlqMapFromTuples(clone(c.tuples))),
  vlqMapFromBabelDecodedMap: c =>
    guard(() =>
      SM.vlqMapFromBabelDecodedMap(clone(c.decodedMap), clone(c.terminatingMapping)),
    ),
  consumer: c =>
    guard(() => {
      const consumer = new SM.Consumer(clone(c.map));
      const generated = Array.from(consumer.generatedMappings());
      const sources = [
        ...new Set(generated.map(m => m.source).filter(s => s != null)),
      ];
      return {
        file: consumer.file ?? null,
        generatedMappings: generated,
        lookups: c.lookups.map(l => consumer.originalPositionFor(l)),
        sourceContent: sources.map(s => consumer.sourceContentFor(s, true) ?? null),
      };
    }),
  composeSourceMaps: c => guard(() => SM.composeSourceMaps(clone(c.maps))),
};

// Index maps are a different code path in the consumer: check each fromRawMappings
// result also reads back through Consumer the same way.
function checkCase(kind, c) {
  const got = runners[kind](c);
  if ('expectError' in c) {
    expect(got.expectError).toBe(c.expectError);
  } else {
    expect(got.expectError).toBeUndefined();
    expect(json(got.expect)).toBe(json(c.expect));
  }
}

afterAll(() => {
  if (UPDATE) {
    for (const kind of Object.keys(runners)) {
      for (const c of data[kind]) {
        delete c.expect;
        delete c.expectError;
        Object.assign(c, runners[kind](c));
      }
    }
    fs.writeFileSync(FIXTURES, JSON.stringify(data, null, 2) + '\n');
  }
});

for (const kind of Object.keys(runners)) {
  describe(`source-map fixtures: ${kind}`, () => {
    for (const c of data[kind]) {
      test(c.name, () => checkCase(kind, c));
    }
  });
}

describe('source-map fixtures: fromRawMappingsNonBlocking', () => {
  for (const c of data.fromRawMappings) {
    if ('expectError' in c) continue;
    test(c.name, async () => {
      const g = await SM.fromRawMappingsNonBlocking(clone(c.modules), c.offsetLines);
      const options = c.excludeSource ? {excludeSource: true} : undefined;
      expect(json(clone(g.toMap(c.file, options)))).toBe(json(c.expect.map));
      expect(g.toString(c.file, options)).toBe(c.expect.text);
    });
  }
});
