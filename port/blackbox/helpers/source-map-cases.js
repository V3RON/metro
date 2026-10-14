/**
 * Inputs of the source-map conformance fixtures (../fixtures/source-map.json).
 *
 * The cases are deterministic: handwritten edge cases, the maps in
 * metro-source-map's own __fixtures__, and pseudo-random ones from a fixed
 * seed. Only the inputs are made here. The expected outputs are recorded from
 * the JavaScript implementation by the runner:
 *
 *   node port/blackbox/helpers/source-map-cases.js     # rewrites the inputs
 *   UPDATE_FIXTURES=1 METRO_RUST=off yarn jest port/blackbox/__tests__/source-map-fixtures-test.js
 *
 * Expectations are never edited by hand and never recorded from Rust.
 */
'use strict';

const fs = require('node:fs');
const path = require('node:path');

const FIXTURES = path.join(__dirname, '..', 'fixtures', 'source-map.json');
const SM_FIXTURES = path.join(
  __dirname,
  '..',
  '..',
  '..',
  'packages',
  'metro-source-map',
  'src',
  '__tests__',
  '__fixtures__',
);

function rng(seed) {
  // mulberry32
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const NAMES = ['a', 'b', 'foo', 'bar', 'x', 'y', 'render', 'useState', '', 'ünï', '日本'];

function makeRandom(seed) {
  const r = rng(seed);
  const int = (lo, hi) => lo + Math.floor(r() * (hi - lo + 1));
  const pick = list => list[int(0, list.length - 1)];

  /** Sorted raw mappings (1-based lines) for a module of `lines` lines. */
  function tuples({lines, perLine = 4, named = true, sourceLines = 40, big = false}) {
    const out = [];
    for (let line = 1; line <= lines; line++) {
      const count = int(0, perLine);
      let column = 0;
      for (let i = 0; i < count; i++) {
        column += int(i === 0 ? 0 : 1, big ? 4000 : 12);
        const kind = int(0, 9);
        if (kind < 2) {
          out.push([line, column]);
        } else if (kind < 6 || !named) {
          out.push([line, column, int(1, sourceLines), int(0, big ? 70000 : 60)]);
        } else {
          out.push([line, column, int(1, sourceLines), int(0, 60), pick(NAMES)]);
        }
      }
    }
    return out;
  }

  const lineCount = map => (map.length === 0 ? 1 : Math.max(...map.map(m => m[0])));

  function module(index, opts = {}) {
    const lines = opts.lines ?? int(1, 8);
    const map = tuples({lines, ...opts});
    const code = Array.from({length: lines}, (_, i) => `line ${index}.${i}`).join(
      pick(['\n', '\n', '\r\n']),
    );
    const mod = {
      path: `/src/mod${index}.js`,
      source: `// module ${index}\n`,
      code,
      isIgnored: int(0, 5) === 0,
      functionMap: int(0, 3) === 0 ? {names: ['<global>', 'f' + index], mappings: 'AAA,CCC'} : null,
      map,
    };
    return mod;
  }

  return {r, int, pick, tuples, module, lineCount};
}

function toVlq(mod, vlqMapFromTuples) {
  return {...mod, map: vlqMapFromTuples(mod.map)};
}

function generate() {
  require(path.join(__dirname, '..', '..', '..', 'packages', 'metro-babel-register')).unstable_registerForMetroMonorepo();
  const SM = require(path.join(__dirname, '..', '..', '..', 'packages', 'metro-source-map'));
  const g = makeRandom(20260514);

  const fromRawMappings = [];
  const indexed = [];
  const handwritten = [
    {
      name: 'no modules',
      modules: [],
    },
    {
      name: 'a module without a map takes up lines',
      modules: [
        {path: '/a.js', source: 's', code: 'x\ny\nz', isIgnored: false, functionMap: null, map: null},
        {path: '/b.js', source: 't', code: 'q', isIgnored: false, functionMap: null, map: [[1, 0, 1, 0]]},
      ],
    },
    {
      name: 'offsetLines shifts everything',
      offsetLines: 5,
      modules: [
        {
          path: '/a.js',
          source: 'var a;',
          code: 'var a;',
          isIgnored: false,
          functionMap: null,
          map: [[1, 0, 1, 0], [1, 4, 1, 4, 'a']],
        },
      ],
    },
    {
      name: 'ignored module and function map',
      modules: [
        {
          path: '/node_modules/x.js',
          source: 'x',
          code: 'x',
          isIgnored: true,
          functionMap: {names: ['<global>'], mappings: 'AAA'},
          map: [[1, 0, 1, 0]],
        },
        {path: '/y.js', source: 'y', code: 'y', isIgnored: false, functionMap: null, map: [[1, 0, 1, 0]]},
      ],
    },
    {
      name: 'excludeSource',
      excludeSource: true,
      file: 'bundle.js',
      modules: [
        {path: '/a.js', source: 'content', code: 'c', isIgnored: false, functionMap: null, map: [[1, 2, 3, 4]]},
      ],
    },
    {
      name: 'names are shared between modules',
      modules: [
        {path: '/a.js', source: '', code: 'a\nb', isIgnored: false, functionMap: null, map: [[1, 0, 1, 0, 'x'], [2, 0, 2, 0, 'y']]},
        {path: '/b.js', source: '', code: 'a\nb', isIgnored: false, functionMap: null, map: [[1, 0, 1, 0, 'y'], [2, 0, 2, 0, 'z']]},
      ],
    },
    {
      name: 'generated-only mappings at the end of a line',
      modules: [
        {path: '/a.js', source: '', code: 'abc\n', isIgnored: false, functionMap: null, map: [[1, 0, 1, 0], [1, 3], [2, 0]]},
      ],
    },
    {
      name: 'vlq module is decoded and re-encoded',
      modules: [
        {
          path: '/a.js',
          source: 'a',
          code: 'a\nb',
          isIgnored: false,
          functionMap: null,
          map: SM.vlqMapFromTuples([[1, 0, 1, 0], [1, 5, 2, 3, 'n'], [2, 1, 3, 3]]),
        },
        {path: '/b.js', source: 'b', code: 'c', isIgnored: false, functionMap: null, map: [[1, 0, 9, 9, 'n']]},
      ],
    },
    {
      name: 'vlq module with unsorted mappings is sorted',
      modules: [
        {
          path: '/a.js',
          source: 'a',
          code: 'a',
          isIgnored: false,
          functionMap: null,
          map: {mappings: 'KAAA,DAAA', names: []},
        },
      ],
    },
    {
      name: 'vlq module with an unterminated field throws',
      modules: [
        {path: '/a.js', source: 'a', code: 'a', isIgnored: false, functionMap: null, map: {mappings: 'AAAA,Cg', names: []}},
      ],
    },
    {
      name: 'vlq module with a mapping that has two fields throws',
      modules: [
        {path: '/a.js', source: 'a', code: 'a', isIgnored: false, functionMap: null, map: {mappings: 'AA', names: []}},
      ],
    },
    {
      name: 'vlq module naming a missing name throws',
      modules: [
        {path: '/a.js', source: 'a', code: 'a', isIgnored: false, functionMap: null, map: {mappings: 'AAAAC', names: ['only']}},
      ],
    },
    {
      name: 'out of order mappings throw',
      modules: [
        {path: '/a.js', source: 'a', code: 'a\nb', isIgnored: false, functionMap: null, map: [[2, 0, 1, 0], [1, 0, 1, 0]]},
      ],
    },
    {
      name: 'a mapping that goes backwards in a line throws',
      modules: [
        {path: '/a.js', source: 'a', code: 'a', isIgnored: false, functionMap: null, map: [[1, 5, 1, 0], [1, 4, 1, 0]]},
      ],
    },
    {
      name: 'modules whose lines overlap throw',
      modules: [
        {path: '/a.js', source: 'a', code: 'a', isIgnored: false, functionMap: null, map: [[3, 0, 1, 0]]},
        {path: '/b.js', source: 'b', code: 'b', isIgnored: false, functionMap: null, map: [[1, 0, 1, 0]]},
      ],
    },
    {
      name: 'a mapping with three elements throws',
      modules: [
        {path: '/a.js', source: 'a', code: 'a', isIgnored: false, functionMap: null, map: [[1, 0, 1]]},
      ],
    },
    {
      name: 'an earlier error wins over an invalid mapping',
      modules: [
        {path: '/a.js', source: 'a', code: 'a\nb', isIgnored: false, functionMap: null, map: [[2, 0, 1, 0], [1, 0, 1, 0], [1, 1, 1]]},
      ],
    },
    {
      name: 'a full source map as a module map throws',
      modules: [
        {path: '/a.js', source: 'a', code: 'a', isIgnored: false, functionMap: null, map: {version: 3, sources: [], names: [], mappings: 1}},
      ],
    },
    {
      name: 'large columns and lines',
      modules: [
        {path: '/a.js', source: '', code: 'a', isIgnored: false, functionMap: null, map: [[1, 0, 1, 0], [1, 100000, 90000, 123456], [1, 100001, 1, 0, 'n']]},
      ],
    },
    {
      name: 'unicode in names, paths and sources',
      modules: [
        {path: '/\u65e5\u672c/\u00fcn\u00ef.js', source: '\u00e7\u00e9\u2028\u2029"quoted"', code: 'x\u2028y', isIgnored: false, functionMap: null, map: [[1, 0, 1, 0, '\u00fcn\u00ef'], [1, 1, 1, 1, '\u65e5\u672c']]},
      ],
    },
  ];
  for (const c of handwritten) {
    fromRawMappings.push(c);
    // The indexed variant takes the same modules, except for the invalid ones
    // (it does not decode VLQ).
    indexed.push({...c, name: `${c.name} (indexed)`});
  }
  for (let n = 0; n < 40; n++) {
    const count = g.int(1, 6);
    const modules = [];
    for (let i = 0; i < count; i++) {
      const mod = g.module(i, {big: g.int(0, 6) === 0});
      const kind = g.int(0, 5);
      if (kind === 0) {
        modules.push({...mod, map: null});
      } else if (kind === 1) {
        modules.push(toVlq(mod, SM.vlqMapFromTuples));
      } else {
        modules.push(mod);
      }
    }
    const c = {
      name: `random ${n}`,
      offsetLines: g.int(0, 3) === 0 ? g.int(1, 20) : 0,
      excludeSource: g.int(0, 3) === 0,
      file: g.int(0, 2) === 0 ? 'out.js' : undefined,
      modules,
    };
    fromRawMappings.push(c);
    indexed.push({...c, name: `random ${n} (indexed)`});
  }
  // Modules in several of the random cases are VLQ: those are the ones that
  // exercise decoding. A few cases with many mappings.
  for (let n = 0; n < 4; n++) {
    const lines = g.int(150, 400);
    const mod = g.module(0, {lines, perLine: 12, big: true});
    const c = {
      name: `many mappings ${n}`,
      modules: [n % 2 === 0 ? mod : toVlq(mod, SM.vlqMapFromTuples)],
    };
    fromRawMappings.push(c);
    indexed.push({...c, name: `many mappings ${n} (indexed)`});
  }

  const vlqMapFromTuples = [
    {name: 'empty', tuples: []},
    {name: 'generated only', tuples: [[1, 0], [1, 7], [3, 2]]},
    {name: 'all kinds', tuples: [[1, 0, 1, 0], [1, 3, 2, 8, 'a'], [2, 0], [2, 4, 1, 1, 'a'], [2, 9, 5, 5, 'b']]},
    {name: 'big deltas', tuples: [[1, 0, 1, 0], [1, 1 << 20, 1 << 18, 1 << 19], [9000, 3, 2, 2]]},
    {name: 'backwards source positions', tuples: [[1, 0, 10, 10], [1, 1, 3, 2], [1, 2, 1, 0]]},
    {name: 'out of order throws', tuples: [[2, 0, 1, 0], [1, 0, 1, 0]]},
    {name: 'invalid length throws', tuples: [[1, 0, 1, 0], [1, 1, 1]]},
  ];
  for (let n = 0; n < 12; n++) {
    vlqMapFromTuples.push({
      name: `random ${n}`,
      tuples: g.tuples({lines: g.int(1, 30), perLine: g.int(1, 8), big: n % 4 === 0}),
    });
  }

  // Babel's decoded maps: 0-based lines of both kinds, segment per generated line.
  const babel = [];
  function babelCase(name, mappings, names, terminating) {
    return {name, decodedMap: {mappings, names}, terminatingMapping: terminating};
  }
  babel.push(babelCase('empty', [], [], [1, 0]));
  babel.push(babelCase('one segment, terminating at its end', [[[0, 0, 0, 0]]], [], [1, 0]));
  babel.push(babelCase('terminating mapping appended', [[[0, 0, 0, 0], [4]]], [], [2, 3]));
  babel.push(babelCase('names', [[[0, 0, 0, 0, 1], [3, 0, 1, 2, 0]], [], [[1, 0, 4, 4, 1]]], ['x', 'y'], [3, 9]));
  babel.push(babelCase('invalid segment length throws', [[[0, 0, 0]]], [], [1, 0]));
  babel.push(babelCase('out of order throws', [[[5], [2]]], [], [1, 9]));
  babel.push(babelCase('terminating mapping before the last throws', [[[5], [9]]], [], [1, 2]));
  for (let n = 0; n < 8; n++) {
    const mappings = [];
    const names = ['n0', 'n1', 'n2', 'n3'];
    const lines = g.int(1, 20);
    for (let l = 0; l < lines; l++) {
      const line = [];
      let col = 0;
      const count = g.int(0, 6);
      for (let i = 0; i < count; i++) {
        col += g.int(i === 0 ? 0 : 1, 10);
        const k = g.int(0, 2);
        if (k === 0) line.push([col]);
        else if (k === 1) line.push([col, 0, g.int(0, 50), g.int(0, 50)]);
        else line.push([col, 0, g.int(0, 50), g.int(0, 50), g.int(0, 3)]);
      }
      mappings.push(line);
    }
    babel.push(babelCase(`random ${n}`, mappings, names, g.int(0, 1) === 0 ? [lines + g.int(1, 2), g.int(0, 5)] : [lines, 200 + g.int(0, 5)]));
  }

  // Consumers.
  const consumer = [];
  const probe = (mapLines, n) =>
    Array.from({length: n}, () => ({line: g.int(0, mapLines + 3), column: g.int(0, 90)}));
  function consumerCase(name, map, extra = {}) {
    consumer.push({name, map, lookups: probe(extra.lines ?? 12, 24), ...extra});
  }
  for (const f of ['1.json', '2.json', 'merged_1_2.json']) {
    consumerCase(`fixture ${f}`, JSON.parse(fs.readFileSync(path.join(SM_FIXTURES, f), 'utf8')), {lines: 12});
  }
  consumerCase('empty mappings', {version: 3, sources: [], names: [], mappings: ''});
  consumerCase('generated-only mappings', {version: 3, sources: [], names: [], mappings: 'AAAA,CAAC;;EAAE'.replace(/AAAA/, 'A')});
  consumerCase('sourceRoot and relative sources', {
    version: 3,
    sourceRoot: '/root/',
    sources: ['./a.js', 'b/../c.js', '/root/d.js'],
    sourcesContent: ['A', 'C', null],
    names: ['n'],
    mappings: 'AAAA,CCCAA;ADAA,CEAC',
    file: 'out.js',
  });
  consumerCase('mappings with names out of range', {version: 3, sources: ['a'], names: ['n'], mappings: 'AAAAC,CAAAC'});
  consumerCase('source index out of range', {version: 3, sources: ['a'], names: [], mappings: 'AAAA,CCAA'});
  consumerCase('mappings of an unterminated field throw', {version: 3, sources: ['a'], names: [], mappings: 'AAAA,Cg'});
  consumerCase('mappings with a bad character throw', {version: 3, sources: ['a'], names: [], mappings: 'AAAA,C?'});
  consumerCase('mappings with two fields throw', {version: 3, sources: ['a'], names: [], mappings: 'AAAA,CC'});
  consumerCase('unsupported version throws', {version: 4, sources: [], names: [], mappings: ''});
  for (let n = 0; n < 10; n++) {
    const mods = Array.from({length: g.int(1, 4)}, (_, i) => g.module(i));
    const gen = SM.fromRawMappings(mods);
    const map = gen.toMap('out.js');
    if (n % 3 === 0) map.sourceRoot = '/src';
    consumerCase(`random basic ${n}`, JSON.parse(JSON.stringify(map)), {lines: 40});
  }
  for (let n = 0; n < 8; n++) {
    const mods = Array.from({length: g.int(1, 5)}, (_, i) => g.module(i));
    const idx = SM.fromRawMappingsIndexed(mods.map(m => (g.int(0, 2) === 0 ? toVlq(m, SM.vlqMapFromTuples) : m)), g.int(0, 3));
    consumerCase(`random indexed ${n}`, JSON.parse(JSON.stringify(idx.toMap('out.js'))), {lines: 40});
  }
  consumerCase('indexed with sections that start late', {
    version: 3,
    sections: [
      {offset: {line: 0, column: 0}, map: {version: 3, sources: ['a'], names: [], mappings: 'EAAA'}},
      {offset: {line: 2, column: 4}, map: {version: 3, sources: ['b'], names: ['k'], mappings: ';AAAAA,CAAC'}},
      {offset: {line: 5, column: 0}, map: {version: 3, sources: ['c'], names: [], mappings: ''}},
    ],
  });

  // composeSourceMaps.
  const compose = [];
  function randomMap(lines, sourceName, from) {
    const mod = g.module(0, {lines});
    mod.path = sourceName;
    mod.isIgnored = false;
    mod.functionMap = null;
    if (from) {
      // Point into positions that exist in `from`.
      mod.map = mod.map.map(m => (m.length >= 4 ? [m[0], m[1], g.pick(from)[0] ?? m[2], g.int(0, 12), ...m.slice(4)] : m));
    }
    return SM.fromRawMappings([mod]).toMap('out.js');
  }
  const base = randomMap(6, 'orig.js', null);
  compose.push({name: 'a single map', maps: [JSON.parse(JSON.stringify(base))]});
  for (let n = 0; n < 14; n++) {
    const first = randomMap(g.int(3, 10), `orig${n}.js`, null);
    const second = randomMap(g.int(3, 10), `mid${n}.js`, Array.from({length: 10}, (_, i) => [i + 1]));
    const maps = [first, second];
    if (n % 3 === 0) {
      maps.push(randomMap(g.int(3, 10), `gen${n}.js`, Array.from({length: 10}, (_, i) => [i + 1])));
    }
    compose.push({name: `random chain ${n}`, maps: JSON.parse(JSON.stringify(maps))});
  }
  compose.push({
    name: 'fixtures 1 and 2',
    maps: [
      JSON.parse(fs.readFileSync(path.join(SM_FIXTURES, '1.json'), 'utf8')),
      JSON.parse(fs.readFileSync(path.join(SM_FIXTURES, '2.json'), 'utf8')),
    ],
  });
  compose.push({
    name: 'ignore lists',
    maps: [
      JSON.parse(fs.readFileSync(path.join(SM_FIXTURES, 'ignore_1.json'), 'utf8')),
      JSON.parse(fs.readFileSync(path.join(SM_FIXTURES, 'ignore_2.json'), 'utf8')),
    ],
  });
  compose.push({name: 'no maps throws', maps: []});
  compose.push({
    name: 'unsorted generated positions',
    maps: [
      {version: 3, sources: ['a.js'], names: [], mappings: 'AAAA,CAAC'},
      {version: 3, sources: ['b.js'], names: [], mappings: 'KAAA,DAAC'},
    ],
  });
  compose.push({
    name: 'indexed map as the last map',
    maps: [
      {version: 3, sources: ['a.js'], names: ['n'], mappings: 'AAAAA,CAAC;AACA'},
      {
        version: 3,
        sections: [
          {offset: {line: 0, column: 0}, map: {version: 3, sources: ['b.js'], names: [], mappings: 'AAAA,CAAC;AACA'}},
        ],
      },
    ],
  });
  compose.push({
    name: 'mapping with an empty source throws',
    maps: [{version: 3, sources: [''], names: [], mappings: 'AAAA'}],
  });

  return {fromRawMappings, fromRawMappingsIndexed: indexed, vlqMapFromTuples, vlqMapFromBabelDecodedMap: babel, consumer, composeSourceMaps: compose};
}

module.exports = {generate, FIXTURES};

if (require.main === module) {
  const NOTES =
    'Language-neutral source-map conformance fixtures. Inputs are made by port/blackbox/helpers/source-map-cases.js; expectations are recorded from the JavaScript implementation (UPDATE_FIXTURES=1 METRO_RUST=off) and never edited by hand. Each case has an input and either expect (the JSON of the result) or expectError (the message of the exception). fromRawMappings*: modules (RawMappingsModule; map is raw tuples, a {mappings, names} VLQ map or null), optional offsetLines, file and excludeSource; expect has the toMap() object and the toString() text. vlqMapFromTuples: tuples. vlqMapFromBabelDecodedMap: decodedMap and terminatingMapping. consumer: a source map, expect has generatedMappings, file, per-lookup originalPositionFor results and sourceContentFor of every source. composeSourceMaps: maps, expect is the composed map. Positions in tuples are [generated line (1-based), generated column, original line (1-based), original column, name]. Both the Jest runner (port/blackbox/__tests__/source-map-fixtures-test.js) and cargo test (metro-rs/crates/source-map/tests/fixtures.rs) read this file.';
  const existing = fs.existsSync(FIXTURES) ? JSON.parse(fs.readFileSync(FIXTURES, 'utf8')) : {};
  const cases = generate();
  // Keep recorded expectations of cases whose input did not change.
  const recorded = new Map();
  for (const [kind, list] of Object.entries(existing)) {
    if (Array.isArray(list)) for (const c of list) recorded.set(kind + '\0' + JSON.stringify(strip(c)), c);
  }
  function strip(c) {
    const {expect: _e, expectError: _x, ...rest} = c;
    return rest;
  }
  const out = {$schema_notes: NOTES};
  for (const [kind, list] of Object.entries(cases)) {
    out[kind] = list.map(c => recorded.get(kind + '\0' + JSON.stringify(strip(c))) ?? c);
  }
  fs.writeFileSync(FIXTURES, JSON.stringify(out, null, 2) + '\n');
  console.log(`wrote ${Object.values(cases).reduce((n, l) => n + l.length, 0)} cases to ${path.relative(process.cwd(), FIXTURES)}`);
}
