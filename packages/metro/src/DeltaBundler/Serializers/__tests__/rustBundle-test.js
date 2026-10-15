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

import type {Module} from '../../types';

import bundleToString from '../../../lib/bundleToString';
import CountingSet from '../../../lib/CountingSet';
import baseJSBundle from '../baseJSBundle';
import {isRustBundleEnabled, rustBundle} from '../rustBundle';
import createModuleIdFactory from 'metro-config/private/defaults/createModuleIdFactory';
import {loadRustBinding} from 'metro-rust-switch';

// The binding is not typed.
// flowlint-next-line unclear-type:off
const binding: any = loadRustBinding();

function makeModule(
  modulePath: string,
  code: string,
  deps: Array<string> = [],
  type: string = 'js/module',
): Module<> {
  return {
    path: modulePath,
    dependencies: new Map(
      deps.map(dep => [
        dep,
        {
          absolutePath: dep,
          data: {
            data: {asyncType: null, isESMImport: false, locs: [], key: dep},
            name: dep,
          },
        },
      ]),
    ),
    inverseDependencies: new CountingSet(),
    output: [{type, data: {code, map: [], lineCount: 1}}],
    getSource: () => Buffer.from(''),
  };
}

const polyfill = makeModule('/polyfill', 'var polyfill = 1;', [], 'js/script');

function options(overrides: $FlowFixMe = {}): $FlowFixMe {
  return {
    asyncRequireModulePath: '',
    createModuleId: createModuleIdFactory(),
    dev: true,
    getRunModuleStatement: (id: number | string, prefix: string) =>
      `${prefix}__r(${JSON.stringify(id)});`,
    globalPrefix: '',
    includeAsyncPaths: false,
    inlineSourceMap: false,
    modulesOnly: false,
    processModuleFilter: () => true,
    projectRoot: '/root',
    runBeforeMainModule: [],
    runModule: true,
    serverRoot: '/root',
    shouldAddToIgnoreList: () => false,
    sourceMapUrl: 'http://localhost/bundle.map',
    sourceUrl: null,
    getSourceUrl: null,
    ...overrides,
  };
}

function graphOf(modules: Array<Module<>>): $FlowFixMe {
  return {
    dependencies: new Map(modules.map(m => [m.path, m])),
    entryPoints: new Set([modules[0].path]),
    transformOptions: {},
  };
}

function expectSame(
  entry: string,
  pre: Array<Module<>>,
  graph: $FlowFixMe,
  makeOptions: () => $FlowFixMe,
) {
  const expected = bundleToString(
    baseJSBundle(entry, pre, graph, makeOptions()),
  ).code;
  const actual = rustBundle(entry, pre, graph, makeOptions());
  expect(actual.buffer.equals(Buffer.from(expected, 'utf8'))).toBe(true);
  expect(actual.utf16Length).toBe(expected.length);
  return expected;
}

describe('BundleAssembler', () => {
  test('writes the bytes Buffer.from writes for lone surrogates', () => {
    const code = 'x\uD800 y\uDC00x \u{1F600} 􏿿 __d(function(){})';
    const assembler = new binding.BundleAssembler();
    const result = assembler.assemble(
      'p\uD800',
      '\uDC00q',
      new Uint32Array([1]),
      '',
      new Uint32Array([1]),
      [code],
      [],
    );
    const js = 'p\uD800\n' + code + '\n\uDC00q';
    expect(result.buffer.equals(Buffer.from(js, 'utf8'))).toBe(true);
    expect(result.utf16Length).toBe(js.length);
  });

  test('evicts the codes a call does not use', () => {
    const assembler = new binding.BundleAssembler();
    assembler.assemble(
      '',
      '',
      new Uint32Array([1, 2]),
      '\0',
      new Uint32Array([1, 2]),
      ['a()', 'b()'],
      [],
    );
    expect(assembler.held()).toBe(2);
    assembler.assemble(
      '',
      '',
      new Uint32Array([2]),
      '',
      new Uint32Array([]),
      [],
      [],
    );
    expect(assembler.held()).toBe(1);
    expect(() =>
      assembler.assemble(
        '',
        '',
        new Uint32Array([1]),
        '',
        new Uint32Array([]),
        [],
        [],
      ),
    ).toThrow(/not held/);
  });
});

describe('rustBundle', () => {
  const modules = (): Array<Module<>> => [
    makeModule('/root/index.js', '__d(function(){/* index */});', [
      '/root/a.js',
      '/root/b.js',
    ]),
    makeModule('/root/a.js', '__d(function(){/* a é \u{1F600} */});'),
    makeModule('/root/b.js', '__d(function(){/* b \uD800 */});', [
      '/root/a.js',
    ]),
    makeModule('/root/empty.js', ''),
    makeModule('/root/noparen.js', 'no paren here'),
    makeModule('/root/script.js', 'var s = 1;', [], 'js/script'),
  ];

  test.each([
    ['dev', {dev: true}],
    ['prod', {dev: false}],
    ['modulesOnly', {modulesOnly: true, runModule: false}],
    ['inline source map', {inlineSourceMap: true}],
    [
      'filtered',
      {processModuleFilter: (m: Module<>) => m.path !== '/root/a.js'},
    ],
    ['run before main', {runBeforeMainModule: ['/root/a.js']}],
  ])('matches baseJSBundle: %s', (_, overrides) => {
    expectSame('/root/index.js', [polyfill], graphOf(modules()), () =>
      options(overrides),
    );
  });

  test('first calls createModuleId for the same paths in the same order as baseJSBundle', () => {
    // Later calls only repeat paths; ids are given out on first use.
    const calls = (run: ($FlowFixMe, $FlowFixMe) => unknown) => {
      const seen: Array<string> = [];
      const factory = createModuleIdFactory();
      const createModuleId = (p: string) => {
        seen.push(p);
        return factory(p);
      };
      run(graphOf(modules()), options({createModuleId}));
      return [...new Set(seen)];
    };
    expect(
      calls((graph, opts) =>
        rustBundle('/root/index.js', [polyfill], graph, opts),
      ),
    ).toEqual(
      calls((graph, opts) =>
        bundleToString(baseJSBundle('/root/index.js', [polyfill], graph, opts)),
      ),
    );
  });

  test('stays identical across edits, with eviction', () => {
    const graph = graphOf(modules());
    const make = () => options();
    expectSame('/root/index.js', [polyfill], graph, make);
    // Same graph object, one module edited, one removed, one added.
    graph.dependencies.set(
      '/root/a.js',
      makeModule('/root/a.js', '__d(function(){/* a edited */});'),
    );
    graph.dependencies.delete('/root/b.js');
    graph.dependencies.set(
      '/root/c.js',
      makeModule('/root/c.js', '__d(function(){/* c */});'),
    );
    expectSame('/root/index.js', [polyfill], graph, make);
    // A module reverts to code Rust no longer holds.
    graph.dependencies.set(
      '/root/a.js',
      makeModule('/root/a.js', '__d(function(){/* a é \u{1F600} */});'),
    );
    expectSame('/root/index.js', [polyfill], graph, make);
  });

  test('identical code in two modules', () => {
    const code = '__d(function(){/* same */});';
    expectSame(
      '/root/x.js',
      [],
      graphOf([makeModule('/root/x.js', code), makeModule('/root/y.js', code)]),
      () => options(),
    );
  });

  test('an empty bundle', () => {
    expectSame('/root/x.js', [], graphOf([makeModule('/root/x.js', '')]), () =>
      options({runModule: false, sourceMapUrl: null}),
    );
  });

  describe('isRustBundleEnabled', () => {
    const config = (extra: $FlowFixMe = {}): $FlowFixMe => ({
      serializer: {},
      unstable_rust: {serializers: true},
      ...extra,
    });
    const saved = process.env.METRO_RUST;
    afterEach(() => {
      if (saved == null) {
        delete process.env.METRO_RUST;
      } else {
        process.env.METRO_RUST = saved;
      }
    });

    test('needs the component, and the default serializer', () => {
      delete process.env.METRO_RUST;
      expect(isRustBundleEnabled(config())).toBe(true);
      expect(isRustBundleEnabled(config({unstable_rust: null}))).toBe(false);
      expect(
        isRustBundleEnabled(config({serializer: {customSerializer: () => ''}})),
      ).toBe(false);
      expect(
        isRustBundleEnabled(
          config({serializer: {unstable_inlineDependencyMap: true}}),
        ),
      ).toBe(false);
      process.env.METRO_RUST = 'off';
      expect(isRustBundleEnabled(config())).toBe(false);
      process.env.METRO_RUST = 'serializers';
      expect(isRustBundleEnabled(config({unstable_rust: null}))).toBe(true);
    });
  });
});
