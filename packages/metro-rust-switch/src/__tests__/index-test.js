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

import * as fs from 'node:fs';
import * as path from 'node:path';

const plan: {phases: Array<{components: Array<string>, ...}>, ...} = JSON.parse(
  fs.readFileSync(path.join(__dirname, '../../../../port/phases.json'), 'utf8'),
);

describe('metro-rust-switch', () => {
  let rust;
  beforeEach(() => {
    jest.resetModules();
    rust = require('../index');
  });
  afterEach(() => {
    jest.dontMock('metro-rs');
  });

  test('COMPONENT_IDS matches port/phases.json', () => {
    expect([...rust.COMPONENT_IDS].sort()).toEqual(
      plan.phases.flatMap(p => p.components).sort(),
    );
  });

  describe('METRO_RUST parsing', () => {
    test('unset or empty does not decide', () => {
      expect(rust.parseMetroRust(undefined)).toBeNull();
      expect(rust.parseMetroRust('')).toBeNull();
      expect(rust.parseMetroRust('  ')).toBeNull();
    });
    test('off', () => {
      expect([...nonNull(rust.parseMetroRust('off'))]).toEqual([]);
    });
    test('all', () => {
      expect([...nonNull(rust.parseMetroRust('all'))].sort()).toEqual(
        [...rust.COMPONENT_IDS].sort(),
      );
    });
    test('comma separated list, tolerant of whitespace', () => {
      expect([
        ...nonNull(rust.parseMetroRust(' resolver, file-map ,,')),
      ]).toEqual(['resolver', 'file-map']);
    });
    test('off and all cannot be mixed with ids', () => {
      expect(() => rust.parseMetroRust('off,resolver')).toThrow(
        /cannot be combined/,
      );
      expect(() => rust.parseMetroRust('resolver,all')).toThrow(
        /cannot be combined/,
      );
    });
    test('unknown ids throw and list the valid ones', () => {
      expect(() => rust.parseMetroRust('resolver,nope')).toThrow(
        /Unknown Rust component "nope" in METRO_RUST\. Valid component ids: ob1, .*resolver/,
      );
    });
  });

  describe('isRustEnabled', () => {
    test('defaults to JavaScript', () => {
      expect(rust.isRustEnabled('resolver', undefined, {})).toBe(false);
      expect(rust.isRustEnabled('resolver', null, {})).toBe(false);
      expect(rust.isRustEnabled('resolver', {}, {})).toBe(false);
    });
    test('env selects components', () => {
      const env = {METRO_RUST: 'resolver,ob1'};
      expect(rust.isRustEnabled('resolver', undefined, env)).toBe(true);
      expect(rust.isRustEnabled('ob1', undefined, env)).toBe(true);
      expect(rust.isRustEnabled('file-map', undefined, env)).toBe(false);
      expect(
        rust.isRustEnabled('file-map', undefined, {METRO_RUST: 'all'}),
      ).toBe(true);
    });
    test('config selects components', () => {
      const config = {resolver: true, 'file-map': false};
      expect(rust.isRustEnabled('resolver', config, {})).toBe(true);
      expect(rust.isRustEnabled('file-map', config, {})).toBe(false);
      expect(rust.isRustEnabled('ob1', config, {})).toBe(false);
    });
    test('env wins over config in both directions', () => {
      expect(
        rust.isRustEnabled('resolver', {resolver: true}, {METRO_RUST: 'off'}),
      ).toBe(false);
      expect(
        rust.isRustEnabled('resolver', {resolver: false}, {METRO_RUST: 'all'}),
      ).toBe(true);
      // A list decides for every component, listed or not.
      expect(
        rust.isRustEnabled('resolver', {resolver: true}, {METRO_RUST: 'ob1'}),
      ).toBe(false);
    });
    test('empty METRO_RUST falls through to config', () => {
      expect(
        rust.isRustEnabled('resolver', {resolver: true}, {METRO_RUST: ''}),
      ).toBe(true);
    });
    test('unknown ids throw, from the argument, env and config', () => {
      expect(() => rust.isRustEnabled('nope', undefined, {})).toThrow(
        /Unknown Rust component "nope".*Valid component ids/,
      );
      expect(() =>
        rust.isRustEnabled('resolver', undefined, {METRO_RUST: 'bogus'}),
      ).toThrow(/"bogus" in METRO_RUST/);
      expect(() => rust.isRustEnabled('resolver', {bogus: true}, {})).toThrow(
        /"bogus" in config\.unstable_rust/,
      );
    });
    test('reads process.env by default', () => {
      const previous = process.env.METRO_RUST;
      try {
        process.env.METRO_RUST = 'resolver';
        expect(rust.isRustEnabled('resolver')).toBe(true);
        process.env.METRO_RUST = 'off';
        expect(rust.isRustEnabled('resolver')).toBe(false);
      } finally {
        if (previous == null) {
          delete process.env.METRO_RUST;
        } else {
          process.env.METRO_RUST = previous;
        }
      }
    });
  });

  describe('getRustBinding', () => {
    test('never loads the native module when everything is off', () => {
      jest.doMock('metro-rs', () => {
        throw new Error('metro-rs must not be loaded');
      });
      expect(
        rust.getRustBinding('resolver', {}, {METRO_RUST: 'off'}),
      ).toBeNull();
      expect(rust.getRustBinding('resolver', undefined, {})).toBeNull();
      expect(
        rust.getRustBinding('resolver', {resolver: true}, {METRO_RUST: 'off'}),
      ).toBeNull();
    });
    test('fails loudly when the binding cannot be loaded', () => {
      jest.doMock('metro-rs', () => {
        throw new Error('missing metro-rs.node');
      });
      expect(() =>
        rust.getRustBinding('resolver', undefined, {METRO_RUST: 'resolver'}),
      ).toThrow(/yarn --cwd metro-rs build/);
      expect(() => rust.loadRustBinding()).toThrow(/missing metro-rs\.node/);
    });
    test('rejects a binding without version()', () => {
      jest.doMock('metro-rs', () => ({}));
      expect(() => rust.loadRustBinding()).toThrow(/does not export version/);
    });
    test('loads the real binding lazily and caches it', () => {
      const binding = nonNull(
        rust.getRustBinding('resolver', {resolver: true}, {}),
      );
      expect(typeof binding.version()).toBe('string');
      expect(rust.loadRustBinding()).toBe(binding);
    });
  });

  test('require("metro-rs").version() works when the binding is built', () => {
    // Built by `yarn --cwd metro-rs build`, which the gate runs first.
    // $FlowFixMe[untyped-import]
    const metroRs = require('metro-rs'); // eslint-disable-line import/no-extraneous-dependencies
    expect(metroRs.version()).toMatch(/^\d+\.\d+\.\d+/);
    expect(metroRs.version()).toBe(
      JSON.parse(
        fs.readFileSync(
          path.join(__dirname, '../../../../metro-rs/package.json'),
          'utf8',
        ),
      ).version,
    );
  });
});

function nonNull<T>(value: ?T): T {
  if (value == null) {
    throw new Error('unexpected null');
  }
  return value;
}
