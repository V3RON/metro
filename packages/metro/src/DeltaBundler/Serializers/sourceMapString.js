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

import type {Module} from '../types';
import type {RustSourceMapCache} from './rustSourceMap';
import type {SourceMapGeneratorOptions} from './sourceMapGenerator';

import {
  isRustSourceMapEnabled,
  rustSourceMapBufferNonBlocking,
  rustSourceMapString,
  rustSourceMapStringNonBlocking,
} from './rustSourceMap';
import {
  sourceMapGenerator,
  sourceMapGeneratorNonBlocking,
} from './sourceMapGenerator';

function sourceMapString(
  modules: ReadonlyArray<Module<>>,
  options: SourceMapGeneratorOptions,
): string {
  if (isRustSourceMapEnabled(options)) {
    return rustSourceMapString(modules, options);
  }
  return sourceMapGenerator(modules, options).toString(undefined, {
    excludeSource: options.excludeSource,
  });
}

async function sourceMapStringNonBlocking(
  modules: ReadonlyArray<Module<>>,
  options: SourceMapGeneratorOptions,
): Promise<string> {
  if (isRustSourceMapEnabled(options)) {
    return rustSourceMapStringNonBlocking(modules, options);
  }
  const generator = await sourceMapGeneratorNonBlocking(modules, options);
  return generator.toString(undefined, {
    excludeSource: options.excludeSource,
  });
}

/**
 * The bytes of `sourceMapStringNonBlocking`'s string, for responses that send
 * a Buffer. Internal: only for use when `isRustSourceMapEnabled`; the
 * JavaScript implementation makes the string and encodes it.
 */
async function sourceMapBufferNonBlocking(
  modules: ReadonlyArray<Module<>>,
  options: SourceMapGeneratorOptions,
  cache?: ?RustSourceMapCache,
): Promise<Buffer> {
  if (isRustSourceMapEnabled(options)) {
    return rustSourceMapBufferNonBlocking(modules, options, cache);
  }
  return Buffer.from(
    await sourceMapStringNonBlocking(modules, options),
    'utf8',
  );
}

export {
  sourceMapBufferNonBlocking,
  sourceMapString,
  sourceMapStringNonBlocking,
};
