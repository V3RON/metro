/**
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 *
 * @flow strict-local
 * @format
 */

import type {TransformResultDependency} from './types';

/**
 * Metro's own `resolve` function (see `getResolveDependencyFn`) carries a
 * method under this symbol when the dependencies of a module can be resolved
 * in one native call (the `incremental` component of the Rust port).
 * `buildSubgraph` uses it only when it finds the tag on the function it was
 * given, so a `resolve` that someone wrapped or replaced is called once per
 * dependency, as always.
 */
export const BATCH_RESOLVE: symbol = Symbol.for('metro.resolve.batch');

export type BatchResolution = {
  /**
   * The resolved `filePath` of the dependencies from index `start` on that
   * were answered, in order. If fewer than all remaining, the one after them
   * has to be resolved with the per-dependency function (which may throw).
   */
  resolveFrom(start: number): ReadonlyArray<string>,
};

/**
 * Called with the dependencies of one module that call `resolve` (the
 * `require.context` ones are left out). Returns null if this module is not
 * batched.
 */
export type PrepareBatchResolution = (
  from: string,
  dependencies: ReadonlyArray<TransformResultDependency>,
) => ?BatchResolution;
