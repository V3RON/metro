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

import {getRustBinding} from 'metro-rust-switch';

/**
 * The part of the `metro-rs` native module that metro-source-map uses. Data
 * crosses in bulk (typed arrays, strings, arrays of strings), never one
 * mapping at a time.
 */
export type ModuleBatch = {
  // Five values per mapping: line, column, source line, source column and the
  // name's index in `names`, or -2 (no source position), -1 (no name) or -3
  // (not a valid tuple).
  tuples: Int32Array,
  names: Array<string>,
  // Four values per module: kind (1 tuples, 2 VLQ, 3 unexpected), first and
  // end tuple (or the VLQ map's index), lines to add.
  modules: Int32Array,
  vlqMappings: Array<string>,
  vlqNames: Array<Array<string>>,
};

export type BatchStatus = {code: number, module: number, index: number};

export type SourceMapEncoder = {
  addModules(batch: ModuleBatch): BatchStatus,
  finish(): {mappings: Buffer, names: Array<string>, state: Int32Array},
};

// Decoded mappings as flat columns: five values per mapping (generated column,
// source index, original line, original column, name index) and the index of
// the first mapping of each generated line. A mapping without a source has
// COLUMNS_NONE as its source index, one without a name has it as its name.
export type ColumnsResult = {data: Int32Array, lineStarts: Int32Array};

export type SourceMapBinding = {
  SourceMapEncoder: Class<SourceMapEncoder>,
  encodeSections(batch: ModuleBatch): {
    status: BatchStatus,
    mappings: Array<string>,
    names: Array<Array<string>>,
  },
  decodeMappings(mappings: string): ?ColumnsResult,
  decodeMappingsInto(
    mappings: string,
    data: Int32Array,
    lineStarts: Int32Array,
  ): ?{mappings: number, lines: number},
  composeMappings(
    maps: Array<{
      data: Int32Array,
      lineStarts: Int32Array,
      sources: Int32Array,
      names: Int32Array,
    }>,
  ): ?{mappings: string, sources: Int32Array, names: Int32Array},
  sourceMapStatuses(): {
    ok: number,
    precedingMapping: number,
    noFile: number,
    invalidMapping: number,
    unexpectedModule: number,
    fallback: number,
  },
  ...
};

export const COLUMNS_STRIDE = 5;
export const COLUMNS_NONE = -2147483648;
export const MODULE_TUPLES = 1;
export const MODULE_VLQ = 2;
export const MODULE_UNEXPECTED = 3;
export const NO_NAME_SIMPLE = -2;
export const NO_NAME = -1;
export const INVALID = -3;

/**
 * The Rust implementation of metro-source-map when the `source-map` component
 * is switched on (METRO_RUST), otherwise null.
 */
export function getSourceMapBinding(): ?SourceMapBinding {
  // $FlowFixMe[incompatible-type] The binding exports more than version().
  return getRustBinding('source-map');
}

const SCRATCH_MAPPINGS = 1 << 15;
const SCRATCH_LINES = 1 << 13;
let scratch: ?{data: Int32Array, lineStarts: Int32Array} = null;
let scratchInUse = false;

/**
 * Decodes `mappings` with Rust into buffers that are reused from one call to
 * the next, and passes them to `use` along with the number of mappings and
 * lines in them. The buffers are only valid until `use` returns, so nothing
 * that outlives it may refer to them. Returns false, without calling `use`, if
 * JavaScript has to decode the mappings.
 *
 * This is for passing over the mappings once. Reusing the buffers avoids
 * allocating (and having the garbage collector track) a pair of typed arrays
 * for every section of a large index map.
 */
export function withDecodedMappings(
  binding: SourceMapBinding,
  mappings: string,
  use: (
    data: Int32Array,
    mappingCount: number,
    lineStarts: Int32Array,
    lineCount: number,
  ) => void,
): boolean {
  // `use` may call back into code that decodes other mappings, in which case
  // the shared buffers are taken.
  const pooled = !scratchInUse;
  let buffers;
  if (pooled) {
    scratch ??= {
      data: new Int32Array(SCRATCH_MAPPINGS * COLUMNS_STRIDE),
      lineStarts: new Int32Array(SCRATCH_LINES),
    };
    buffers = scratch;
    scratchInUse = true;
  } else {
    buffers = {
      data: new Int32Array(SCRATCH_MAPPINGS * COLUMNS_STRIDE),
      lineStarts: new Int32Array(SCRATCH_LINES),
    };
  }
  try {
    let {data, lineStarts} = buffers;
    let counts = binding.decodeMappingsInto(mappings, data, lineStarts);
    if (counts == null) {
      return false;
    }
    if (
      counts.mappings * COLUMNS_STRIDE > data.length ||
      counts.lines > lineStarts.length
    ) {
      // Too many mappings for the shared buffers: decode again into ones that
      // fit.
      data = new Int32Array(counts.mappings * COLUMNS_STRIDE);
      lineStarts = new Int32Array(counts.lines);
      counts = binding.decodeMappingsInto(mappings, data, lineStarts);
      if (counts == null) {
        return false;
      }
    }
    use(data, counts.mappings, lineStarts, counts.lines);
    return true;
  } finally {
    if (pooled) {
      scratchInUse = false;
    }
  }
}
