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

import {createResolutionContext} from './utils';
import path from 'node:path';

const Resolver = require('../index');

// `extraNodeModules` is read per package name, so objects that compute their
// entries (a common monorepo pattern) work like plain objects.
describe('extraNodeModules', () => {
  const fileMap = {
    '/root/project/foo.js': '',
    '/root/shared/package.json': JSON.stringify({name: 'shared'}),
    '/root/shared/index.js': '',
    '/root/shared/lib/util.js': '',
  };

  const resolveWith = (
    extraNodeModules: {[packageName: string]: string, ...},
    specifier: string,
  ) =>
    Resolver.resolve(
      {
        ...createResolutionContext(fileMap),
        originModulePath: '/root/project/foo.js',
        extraNodeModules,
      },
      specifier,
      null,
    );

  test('a plain object maps a package name to a directory', () => {
    expect(resolveWith({shared: '/root/shared'}, 'shared/lib/util')).toEqual({
      type: 'sourceFile',
      filePath: '/root/shared/lib/util.js',
    });
  });

  test('a Proxy with no own keys is read per package name', () => {
    const extraNodeModules = new Proxy(
      {},
      {get: (_target, name) => path.join('/root', String(name))},
    );
    expect(resolveWith(extraNodeModules, 'shared')).toEqual({
      type: 'sourceFile',
      filePath: '/root/shared/index.js',
    });
    expect(resolveWith(extraNodeModules, 'shared/lib/util')).toEqual({
      type: 'sourceFile',
      filePath: '/root/shared/lib/util.js',
    });
  });

  test('a getter that returns undefined does not resolve', () => {
    const extraNodeModules = new Proxy({}, {get: () => undefined});
    expect(() => resolveWith(extraNodeModules, 'shared')).toThrow(
      Resolver.FailedToResolveNameError,
    );
  });
});
