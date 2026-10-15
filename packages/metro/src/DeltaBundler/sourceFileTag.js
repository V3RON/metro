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

/**
 * Marks the `getSource` closures that Metro itself creates for files on disk
 * (see Transformer.js: `fs.readFileSync(filePath)`), so that the Rust source
 * map writer can read those files itself and decode them like
 * `getSource().toString()` would (RULES.md section 7, rule 7: native or
 * JavaScript is decided per callback, by function identity). A `getSource`
 * that something else made or wrapped is not in the table and is called.
 */

const files: WeakMap<() => Buffer, string> = new WeakMap();

/**
 * Marks `getSource` as "reads this file". The path is only taken if it
 * crosses to Rust unchanged: a string with a lone surrogate would not.
 */
export function tagSourceFile<T extends () => Buffer>(
  getSource: T,
  filePath: string,
): T {
  // $FlowFixMe[prop-missing] ES2024, in every supported Node.
  if (typeof filePath === 'string' && filePath.isWellFormed()) {
    files.set(getSource, filePath);
  }
  return getSource;
}

/** The file that `getSource` reads, if Metro made it. */
export function getTaggedSourceFile(getSource: () => Buffer): ?string {
  return typeof getSource === 'function' ? files.get(getSource) : null;
}
