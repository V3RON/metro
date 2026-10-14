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

import type {MetroSourceMapSegmentTuple, VlqMap} from './source-map';

const newline = /\r\n?|\n|\u2028|\u2029/g;

export const countLines = (string: string): number =>
  (string.match(newline) || []).length + 1;

export function isVlqMap(
  map: ?ReadonlyArray<MetroSourceMapSegmentTuple> | VlqMap,
): implies map is VlqMap {
  return map != null && !Array.isArray(map) && typeof map.mappings === 'string';
}
