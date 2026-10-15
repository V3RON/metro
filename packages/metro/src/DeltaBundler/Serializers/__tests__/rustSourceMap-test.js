/**
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 *
 * @flow strict-local
 * @format
 * @oncall react_native
 */

'use strict';

import type {Module} from '../../types';

import CountingSet from '../../../lib/CountingSet';
import {tagSourceFile} from '../../sourceFileTag';
import {
  RustSourceMapCache,
  rustSourceMapBufferNonBlocking,
  rustSourceMapStats,
  rustSourceMapString,
  rustSourceMapStringNonBlocking,
} from '../rustSourceMap';
import {sourceMapGenerator} from '../sourceMapGenerator';
import fs from 'fs';
import {loadRustBinding} from 'metro-rust-switch';
import {vlqMapFromTuples} from 'metro-source-map';
import os from 'os';
import path from 'path';

// The binding is not typed.
// flowlint-next-line unclear-type:off
const binding: any = loadRustBinding();

type Output = {type?: string, map?: $FlowFixMe, functionMap?: $FlowFixMe};

let directory: string;
beforeAll(() => {
  directory = fs.mkdtempSync(path.join(os.tmpdir(), 'metro-rust-source-map-'));
});
afterAll(() => {
  fs.rmSync(directory, {recursive: true, force: true});
});

function makeModule(
  modulePath: string,
  code: string,
  {type = 'js/module', map, functionMap}: Output,
  getSource: () => Buffer | string = () =>
    Buffer.from('source of ' + modulePath),
): Module<> {
  return {
    path: modulePath,
    dependencies: new Map(),
    inverseDependencies: new CountingSet(),
    output: [
      {
        type,
        data: {
          code,
          lineCount: 1,
          map,
          ...(functionMap != null ? {functionMap} : null),
        },
      },
    ],
    getSource: getSource as $FlowFixMe,
  };
}

const TUPLES = [
  [1, 0],
  [1, 4, 1, 0],
  [1, 9, 1, 4, 'foo'],
  [2, 0, 2, 0, 'bar'],
  [2, 3, 2, 2, 'foo'],
  [3, 0],
];

function options(overrides: $FlowFixMe = {}): $FlowFixMe {
  return {
    excludeSource: false,
    processModuleFilter: () => true,
    shouldAddToIgnoreList: () => false,
    getSourceUrl: null,
    ...overrides,
  };
}

function jsMap(modules: Array<Module<>>, opts: $FlowFixMe): string {
  return sourceMapGenerator(modules, opts).toString(undefined, {
    excludeSource: opts.excludeSource,
  });
}

function expectSame(modules: Array<Module<>>, opts: $FlowFixMe = options()) {
  const expected = jsMap(modules, opts);
  const actual = rustSourceMapString(modules, opts);
  expect(actual).toBe(expected);
  // Twice: the second time Rust holds what the first call sent.
  expect(rustSourceMapString(modules, opts)).toBe(expected);
  return expected;
}

function fileModule(
  name: string,
  contents: string | Buffer,
  map: $FlowFixMe = TUPLES,
): Module<> {
  const filePath = path.join(directory, name);
  fs.writeFileSync(filePath, contents);
  // The closure Metro's Transformer makes, below.
  const module = makeModule(filePath, 'a;\nb;\nc;', {map}, () =>
    fs.readFileSync(filePath),
  );
  tagSourceFile(module.getSource, filePath);
  return module;
}

describe('rustSourceMapString', () => {
  test('tuple maps, VLQ maps and modules without a map', () => {
    expectSame([
      makeModule('/root/a.js', 'one\ntwo\nthree', {map: TUPLES}),
      makeModule('/root/b.js', 'x', {map: vlqMapFromTuples(TUPLES)}),
      makeModule('/root/c.js', 'no map\r\nat all\u2028and\u2029more', {
        map: null,
      }),
      makeModule('/root/d.js', 'after', {map: []}),
      makeModule('/root/e.js', 'x', {map: vlqMapFromTuples([])}),
    ]);
  });

  test('plain objects with VLQ and names', () => {
    expectSame([
      makeModule('/root/a.js', 'a', {
        map: {mappings: 'AAAA,CAAC;AACD', names: ['n"1', 'n\\2', 'é\u{1F600}']},
      }),
    ]);
  });

  test('is written by Rust, and JavaScript runs only for what Rust does not take', () => {
    const build = jest.spyOn(binding.IndexMapBuilder.prototype, 'buildString');
    try {
      rustSourceMapString(
        [
          makeModule('/root/a.js', 'one', {map: TUPLES}),
          makeModule('/root/b.js', 'x', {map: vlqMapFromTuples(TUPLES)}),
        ],
        options(),
      );
      expect(build).toHaveBeenCalledTimes(1);
      build.mockClear();
      expect(() =>
        rustSourceMapString(
          [makeModule('/root/c.js', 'x', {map: {version: 3, sections: []}})],
          options(),
        ),
      ).toThrow(/Unexpected module/);
      expect(build).not.toHaveBeenCalled();
    } finally {
      build.mockRestore();
    }
  });

  test('excludeSource', () => {
    expectSame(
      [
        makeModule('/root/a.js', 'one', {map: TUPLES}),
        makeModule('/root/b.js', 'x', {map: vlqMapFromTuples(TUPLES)}),
      ],
      options({excludeSource: true}),
    );
  });

  test('does not read sources when excludeSource', () => {
    const getSource = jest.fn(() => Buffer.from('x'));
    const module = makeModule('/root/a.js', 'one', {map: TUPLES}, getSource);
    expectSame([module], options({excludeSource: true}));
    expect(getSource).not.toHaveBeenCalled();
  });

  test('the ignore list, for both kinds of map', () => {
    expectSame(
      [
        makeModule('/root/a.js', 'one', {map: TUPLES}),
        makeModule('/root/b.js', 'x', {map: vlqMapFromTuples(TUPLES)}),
        makeModule('/root/c.js', 'x', {map: vlqMapFromTuples(TUPLES)}),
      ],
      options({
        shouldAddToIgnoreList: (m: Module<>) => m.path !== '/root/c.js',
      }),
    );
  });

  test('function maps, for both kinds of map', () => {
    const functionMap = {names: ['<global>', 'a"b'], mappings: 'AAA,CCC'};
    expectSame(
      [
        makeModule('/root/a.js', 'one', {map: TUPLES, functionMap}),
        makeModule('/root/b.js', 'x', {
          map: vlqMapFromTuples(TUPLES),
          functionMap,
        }),
        makeModule('/root/c.js', 'x', {map: vlqMapFromTuples(TUPLES)}),
      ],
      options({shouldAddToIgnoreList: () => true}),
    );
  });

  test('module filter and non-JS modules', () => {
    expectSame(
      [
        makeModule('/root/a.js', 'one', {map: TUPLES}),
        makeModule('/root/b.js', 'x', {map: TUPLES}),
        makeModule('/root/c.js', 'x', {map: TUPLES}),
      ],
      options({processModuleFilter: (m: Module<>) => m.path !== '/root/b.js'}),
    );
  });

  test('getSourceUrl, as with sourcePaths=url-server', () => {
    expectSame(
      [
        makeModule('/root/a.js', 'one', {map: TUPLES}),
        makeModule('/root/b "q".js', 'x', {map: vlqMapFromTuples(TUPLES)}),
      ],
      options({
        getSourceUrl: (m: Module<>) =>
          `http://localhost:8081/${encodeURI(path.basename(m.path))}`,
      }),
    );
  });

  test('an asset module gives an empty source without calling getSource', () => {
    const getSource = jest.fn(() => {
      throw new Error('not for assets');
    });
    expectSame([
      makeModule(
        '/root/a.png',
        'asset code',
        {type: 'js/module/asset', map: TUPLES},
        getSource,
      ),
    ]);
    expect(getSource).not.toHaveBeenCalled();
  });

  test('a virtual module with a lone surrogate in its text and its path', () => {
    const text = 'virtual \uD800 text \uDC00 "quoted"\n\t\u0001 \u{1F600} é';
    const modules = [
      makeModule('/root/virtual\uD800\\"x.js', 'v', {map: TUPLES}, () =>
        Buffer.from(text),
      ),
      makeModule(
        '/root/virtual2.js',
        'v',
        {map: vlqMapFromTuples(TUPLES)},
        () => text,
      ),
    ];
    const result = expectSame(modules);
    expect(result).toContain('\\ud800');
    // Lone surrogates inside a Buffer's text are U+FFFD by then.
    expect(result).toContain('\ufffd');
  });

  test('a path with a lone surrogate is not taken as a file', () => {
    const module = makeModule('/root/a.js', 'one', {map: TUPLES});
    tagSourceFile(module.getSource, '/root/\uD800.js');
    expectSame([module]);
  });

  test('a file with invalid UTF-8 and a BOM', () => {
    const bytes = Buffer.concat([
      Buffer.from([0xef, 0xbb, 0xbf]),
      Buffer.from('a = "é\u{1F600}";\n\tb\r\n'),
      Buffer.from([0xff, 0xc0, 0x80, 0xe2, 0x82, 0xf0, 0x9f, 0x98]),
      Buffer.from([0xed, 0xa0, 0x80, 0xed, 0xb0, 0x80, 0xf4, 0x90, 0x80]),
      Buffer.from([0x00, 0x1f, 0x7f, 0x41]),
    ]);
    const result = expectSame([fileModule('invalid.js', bytes)]);
    expect(result).toContain('\ufeff');
    expect(result).toContain('\ufffd');
  });

  test('decodes random bytes like Buffer.toString', () => {
    // Sequences that make decoders disagree: leads, continuations, truncated
    // and overlong forms, surrogates, noncharacters, controls.
    const interesting = [
      0x00, 0x09, 0x0a, 0x22, 0x5c, 0x7f, 0x80, 0x8f, 0x90, 0xa0, 0xbf, 0xc0,
      0xc1, 0xc2, 0xdf, 0xe0, 0xe1, 0xed, 0xee, 0xef, 0xf0, 0xf1, 0xf4, 0xf5,
      0xff, 0x41, 0xe2, 0x82, 0xac, 0xf0, 0x9f, 0x98, 0x80, 0xbb, 0xbf,
    ];
    let seed = 12345;
    const random = () => {
      seed = (seed * 1664525 + 1013904223) >>> 0;
      return seed / 2 ** 32;
    };
    const modules = [];
    for (let i = 0; i < 400; i++) {
      const length = Math.floor(random() * 24);
      const bytes = Buffer.alloc(length);
      for (let k = 0; k < length; k++) {
        bytes[k] =
          random() < 0.8
            ? interesting[Math.floor(random() * interesting.length)]
            : Math.floor(random() * 256);
      }
      modules.push(fileModule(`random-${i}.js`, bytes, []));
    }
    expectSame(modules);
  });

  test('a file deleted after the transform fails like getSource does', () => {
    const module = fileModule('deleted.js', 'gone');
    const other = makeModule('/root/other.js', 'x', {map: TUPLES});
    fs.rmSync(path.join(directory, 'deleted.js'));
    let expected: $FlowFixMe = null;
    try {
      jsMap([other, module], options());
    } catch (error) {
      expected = error;
    }
    expect(expected).not.toBeNull();
    let actual: $FlowFixMe = null;
    try {
      rustSourceMapString([other, module], options());
    } catch (error) {
      actual = error;
    }
    expect(actual).not.toBeNull();
    expect(actual.message).toBe(expected.message);
    expect(actual.code).toBe('ENOENT');
    expect(actual.code).toBe(expected.code);
    expect(actual.path).toBe(expected.path);
    expect(actual.syscall).toBe(expected.syscall);
  });

  test('a directory in place of a file fails like getSource does', () => {
    const module = makeModule(directory, 'x', {map: TUPLES}, () =>
      fs.readFileSync(directory),
    );
    tagSourceFile(module.getSource, directory);
    const message = (run: () => unknown): string => {
      try {
        run();
      } catch (error) {
        return `${error.code} ${error.message}`;
      }
      return 'no error';
    };
    expect(message(() => rustSourceMapString([module], options()))).toBe(
      message(() => jsMap([module], options())),
    );
    expect(message(() => jsMap([module], options()))).toMatch(/EISDIR/);
  });

  test('a module whose map is neither tuples nor VLQ throws as JavaScript does', () => {
    const modules = [
      makeModule('/root/a.js', 'one', {map: TUPLES}),
      makeModule('/root/b.js', 'x', {map: {version: 3, sections: []}}),
    ];
    const message = (run: () => unknown): string => {
      try {
        run();
      } catch (error) {
        return error.message;
      }
      return 'no error';
    };
    expect(message(() => rustSourceMapString(modules, options()))).toBe(
      message(() => jsMap(modules, options())),
    );
    expect(message(() => jsMap(modules, options()))).toMatch(
      /Unexpected module/,
    );
  });

  test('invalid tuples throw as JavaScript does', () => {
    const modules = [
      makeModule('/root/a.js', 'one', {
        map: [
          [1, 0],
          [1, 1, 2],
        ],
      }),
    ];
    const message = (run: () => unknown): string => {
      try {
        run();
      } catch (error) {
        return error.message;
      }
      return 'no error';
    };
    expect(message(() => rustSourceMapString(modules, options()))).toBe(
      message(() => jsMap(modules, options())),
    );
  });

  test('an edited module is a new output with new mappings', () => {
    const a = makeModule('/root/a.js', 'one\ntwo', {map: TUPLES});
    const b = makeModule('/root/b.js', 'x', {map: TUPLES});
    expectSame([a, b]);
    const edited = makeModule('/root/a.js', 'one\ntwo\nthree\nfour', {
      map: [
        [1, 0, 1, 0],
        [4, 2, 3, 1, 'z'],
      ],
    });
    expectSame([edited, b]);
    expectSame([a, b]);
  });

  test('the same output in two modules', () => {
    const a = makeModule('/root/a.js', 'one\ntwo', {map: TUPLES});
    const twin = {...a, path: '/root/twin.js'};
    expectSame([a, twin]);
  });

  test('all-ASCII maps are the same string as the UTF-8 decoding', () => {
    const modules = [makeModule('/root/a.js', 'one', {map: TUPLES})];
    const string = rustSourceMapString(modules, options());
    expect(/^[\x00-\x7f]*$/.test(string)).toBe(true);
    expect(string).toBe(jsMap(modules, options()));
  });
});

describe('rustSourceMapStringNonBlocking and rustSourceMapBufferNonBlocking', () => {
  test('give the same string and its bytes', async () => {
    const modules = [
      makeModule('/root/a.js', 'one', {map: TUPLES}),
      makeModule('/root/b.js', 'x', {map: vlqMapFromTuples(TUPLES)}, () =>
        Buffer.from('é \uD800'),
      ),
      fileModule('nonblocking.js', 'é\u{1F600} file'),
    ];
    const expected = jsMap(modules, options());
    expect(await rustSourceMapStringNonBlocking(modules, options())).toBe(
      expected,
    );
    const buffer = await rustSourceMapBufferNonBlocking(modules, options());
    expect(buffer.equals(Buffer.from(expected, 'utf8'))).toBe(true);
    expect(
      (
        await rustSourceMapBufferNonBlocking(
          modules,
          options({excludeSource: true}),
        )
      ).equals(
        Buffer.from(jsMap(modules, options({excludeSource: true})), 'utf8'),
      ),
    ).toBe(true);
  });

  test('a file that cannot be read rejects like getSource does', async () => {
    const module = fileModule('deleted-async.js', 'gone');
    fs.rmSync(path.join(directory, 'deleted-async.js'));
    await expect(
      rustSourceMapStringNonBlocking([module], options()),
    ).rejects.toMatchObject({code: 'ENOENT', syscall: 'open'});
  });
});

describe('what Rust holds', () => {
  const modules = () => [
    makeModule('/root/a.js', 'one', {map: TUPLES}),
    makeModule('/root/b.js', 'x', {map: vlqMapFromTuples(TUPLES)}),
  ];

  test('one-shot calls leave nothing in Rust', async () => {
    const before = rustSourceMapStats();
    rustSourceMapString(modules(), options());
    await rustSourceMapStringNonBlocking(modules(), options());
    await rustSourceMapBufferNonBlocking(modules(), options());
    expect(rustSourceMapStats()).toEqual(before);
  });

  test('a server cache keeps entries across requests until it is disposed', async () => {
    const cache = new RustSourceMapCache(true);
    const mods = modules();
    const first = await rustSourceMapBufferNonBlocking(mods, options(), cache);
    expect(rustSourceMapStats().entries).toBeGreaterThanOrEqual(2);
    const second = await rustSourceMapBufferNonBlocking(mods, options(), cache);
    expect(second.equals(first)).toBe(true);
    expect(first.equals(Buffer.from(jsMap(mods, options()), 'utf8'))).toBe(
      true,
    );
    cache.dispose();
    expect(rustSourceMapStats().entries).toBe(0);
  });
});

describe('IndexMapBuilder', () => {
  const entry = (token: number) => ({
    newTokens: new Uint32Array([token]),
    newKinds: new Uint8Array([2]),
    newLines: new Uint32Array([1]),
    newMappings: ['"AAAA"'],
    newNames: ['[]'],
    newFunctionMaps: [''],
  });
  const request = (token: number, extra: $FlowFixMe = {}) => ({
    tokens: new Uint32Array([token]),
    flags: new Uint8Array([0]),
    paths: ['"/a.js"'],
    files: [],
    literals: [],
    excludeSource: true,
    newTokens: new Uint32Array(0),
    newKinds: new Uint8Array(0),
    newLines: new Uint32Array(0),
    newMappings: [],
    newNames: [],
    newFunctionMaps: [],
    evict: new Uint32Array(0),
    ...extra,
  });

  test('holds entries until they are evicted', () => {
    const builder = new binding.IndexMapBuilder();
    const first = builder.build(request(7, entry(7)));
    expect(Buffer.from(first.buffer).toString()).toBe(
      '{"version":3,"sections":[{"offset":{"line":0,"column":0},"map":{"version":3,"sources":["/a.js"],"names":[],"mappings":"AAAA"}}]}',
    );
    expect(first.ascii).toBe(true);
    expect(builder.held()).toBe(1);
    // Held: no entry needed.
    expect(builder.build(request(7)).buffer.equals(first.buffer)).toBe(true);
    // Evicted by the next call.
    builder.build(request(8, {...entry(8), evict: new Uint32Array([7])}));
    expect(builder.held()).toBe(1);
    expect(() => builder.build(request(7))).toThrow(/not held/);
  });

  test('evicts the tokens of collected outputs', async () => {
    // Without --expose-gc, the registry cannot be driven from a test: this
    // checks what it feeds, a Rust entry per output that goes away.
    const builder = new binding.IndexMapBuilder();
    for (let token = 1; token <= 100; token++) {
      builder.build(request(token, entry(token)));
    }
    expect(builder.held()).toBe(100);
    builder.build(
      request(101, {
        ...entry(101),
        evict: Uint32Array.from(Array.from({length: 100}, (_, i) => i + 1)),
      }),
    );
    expect(builder.held()).toBe(1);
  });

  test('clear drops every entry', () => {
    const builder = new binding.IndexMapBuilder();
    builder.build(request(1, entry(1)));
    expect(builder.held()).toBe(1);
    builder.clear();
    expect(builder.held()).toBe(0);
  });

  test('buildString gives the string of the bytes, latin1 or UTF-8', () => {
    const builder = new binding.IndexMapBuilder();
    const bytes = builder.build(request(1, entry(1)));
    expect(builder.buildString(request(1))).toBe(
      bytes.buffer.toString('latin1'),
    );
    expect(
      builder.buildString(
        request(1, {
          flags: new Uint8Array([2 << 1]),
          literals: ['"é\\ud83d\\ude00"'],
          excludeSource: false,
        }),
      ),
    ).toContain('sourcesContent');
    expect(
      builder.buildString(
        request(1, {
          flags: new Uint8Array([MODE_FILE << 1]),
          files: [path.join(directory, 'does-not-exist.js')],
          excludeSource: false,
        }),
      ),
    ).toEqual([0]);
  });

  test('the async build gives the same bytes', async () => {
    const builder = new binding.IndexMapBuilder();
    const sync = builder.build(request(1, entry(1)));
    const asyncResult = await builder.buildAsync(request(1));
    expect(asyncResult.buffer.equals(sync.buffer)).toBe(true);
    expect(asyncResult.failed).toEqual([]);
  });

  test('reports files that cannot be read', () => {
    const builder = new binding.IndexMapBuilder();
    const result = builder.build(
      request(1, {
        ...entry(1),
        flags: new Uint8Array([MODE_FILE << 1]),
        files: [path.join(directory, 'does-not-exist.js')],
        excludeSource: false,
      }),
    );
    expect(result.failed).toEqual([0]);
  });
});

const MODE_FILE = 1;
