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
 * The JavaScript half of the Rust implementation of mapping generation
 * (`METRO_RUST=source-map`). Modules' raw mappings are flattened into typed
 * arrays here, encoded by `metro-rs` in one call per batch, and the result is
 * put back into the objects the JavaScript implementation builds.
 *
 * Every function returns null (or false) when the Rust side cannot reproduce
 * what the JavaScript implementation does, for example for a malformed VLQ map:
 * the caller then runs the JavaScript implementation, so that the result, or
 * the exception, is the one it has always been.
 */

import type {
  BatchStatus,
  ModuleBatch,
  SourceMapBinding,
  SourceMapEncoder,
} from './rust';
import type {
  BabelDecodedMap,
  MetroSourceMapSegmentTuple,
  RawMappingsModule,
  VlqMap,
} from './source-map';

import Generator from './Generator';
import {countLines, isVlqMap} from './rawMappings';
import {
  INVALID,
  MODULE_TUPLES,
  MODULE_UNEXPECTED,
  MODULE_VLQ,
  NO_NAME,
  NO_NAME_SIMPLE,
} from './rust';

const TUPLE_STRIDE = 5;
// How many mappings go to Rust in one call, at least. Small enough to stay in
// the CPU cache and to let the non-blocking variant check the clock often,
// large enough that the cost of a call does not show.
const BATCH_MAPPINGS = 1 << 15;

/**
 * Flattens modules into a `ModuleBatch`.
 */
class BatchBuilder {
  _tuples: Int32Array;
  _tupleCount: number = 0;
  _names: Array<string> = [];
  _nameIndex: Map<string, number> = new Map();
  _modules: Array<number> = [];
  _vlqMappings: Array<string> = [];
  _vlqNames: Array<Array<string>> = [];

  /** `tuples` is overwritten; it may be bigger than the batch needs. */
  constructor(tuples: Int32Array) {
    this._tuples = tuples;
  }

  /** Returns false if the tuples have something the Rust side cannot take. */
  addTuples(
    map: ReadonlyArray<MetroSourceMapSegmentTuple>,
    carryOver: number,
  ): boolean {
    const start = this._tupleCount;
    const out = this._tuples;
    const names = this._names;
    const nameIndex = this._nameIndex;
    let o = start * TUPLE_STRIDE;
    for (let i = 0, n = map.length; i < n; i++) {
      const mapping = map[i];
      switch (mapping.length) {
        case 2:
          out[o] = mapping[0];
          out[o + 1] = mapping[1];
          out[o + 2] = 0;
          out[o + 3] = 0;
          out[o + 4] = NO_NAME_SIMPLE;
          break;
        case 4:
          out[o] = mapping[0];
          out[o + 1] = mapping[1];
          out[o + 2] = mapping[2];
          out[o + 3] = mapping[3];
          out[o + 4] = NO_NAME;
          break;
        case 5: {
          const name = mapping[4];
          if (typeof name !== 'string') {
            return false;
          }
          let index = nameIndex.get(name);
          if (index === undefined) {
            index = names.length;
            names.push(name);
            nameIndex.set(name, index);
          }
          out[o] = mapping[0];
          out[o + 1] = mapping[1];
          out[o + 2] = mapping[2];
          out[o + 3] = mapping[3];
          out[o + 4] = index;
          break;
        }
        default:
          out[o + 4] = INVALID;
      }
      o = o + TUPLE_STRIDE;
    }
    this._tupleCount = o / TUPLE_STRIDE;
    this._modules.push(MODULE_TUPLES, start, this._tupleCount, carryOver);
    return true;
  }

  addVlq(map: VlqMap, carryOver: number): void {
    this._modules.push(MODULE_VLQ, this._vlqMappings.length, 0, carryOver);
    this._vlqMappings.push(map.mappings);
    this._vlqNames.push([...map.names]);
  }

  addUnexpected(): void {
    this._modules.push(MODULE_UNEXPECTED, 0, 0, 0);
  }

  toBatch(): ModuleBatch {
    return {
      tuples: this._tuples,
      names: this._names,
      modules: Int32Array.from(this._modules),
      vlqMappings: this._vlqMappings,
      vlqNames: this._vlqNames,
    };
  }
}

/**
 * Throws the exception the JavaScript implementation throws for `status`.
 * Returns false for the status that asks for the JavaScript implementation.
 */
function throwForStatus(
  binding: SourceMapBinding,
  status: BatchStatus,
  modules: ReadonlyArray<RawMappingsModule>,
): false {
  const codes = binding.sourceMapStatuses();
  const mod = modules[status.module];
  switch (status.code) {
    case codes.precedingMapping:
      throw new Error('Mapping is for a position preceding an earlier mapping');
    case codes.noFile:
      throw new Error(
        'Cannot add mapping before starting a file with `addFile()`',
      );
    case codes.invalidMapping: {
      const mapping: ReadonlyArray<unknown> = (mod.map as $FlowFixMe)[
        status.index
      ];
      throw new Error(`Invalid mapping: [${mapping.join(', ')}]`);
    }
    case codes.unexpectedModule:
      throw new Error(
        `Unexpected module with full source map found: ${mod.path}`,
      );
    default:
      return false;
  }
}

/**
 * The Rust version of `fromRawMappingsImpl`. Calls `onDone` with the
 * generator, or, if the Rust side cannot reproduce what JavaScript does for
 * these modules, `runJavaScript` instead (before `onDone` was called).
 */
export function fromRawMappingsRust(
  binding: SourceMapBinding,
  isBlocking: boolean,
  onDone: Generator => void,
  modules: ReadonlyArray<RawMappingsModule>,
  offsetLines: number,
  runJavaScript: () => void,
): void {
  const generator = new Generator();
  const encoder: SourceMapEncoder = new binding.SourceMapEncoder();
  const codes = binding.sourceMapStatuses();
  let next = 0;
  let carryOver = offsetLines;
  // The flattened tuples, reused by every batch.
  let tuples = new Int32Array(0);

  // Encodes the next batch of modules. Returns false to fall back.
  function processBatch(): boolean {
    // Find the modules of the batch and how many mappings they have.
    const first = next;
    let mappings = 0;
    let end = first;
    while (end < modules.length && mappings < BATCH_MAPPINGS) {
      const map = modules[end].map;
      if (Array.isArray(map)) {
        mappings = mappings + map.length;
      }
      end++;
    }
    if (tuples.length < mappings * TUPLE_STRIDE) {
      tuples = new Int32Array(
        Math.max(mappings, BATCH_MAPPINGS) * TUPLE_STRIDE,
      );
    }
    const builder = new BatchBuilder(tuples);
    const inBatch: Array<RawMappingsModule> = [];
    for (let i = first; i < end; i++) {
      const mod = modules[i];
      const {code, map} = mod;
      if (isVlqMap(map)) {
        builder.addVlq(map, carryOver);
        inBatch.push(mod);
      } else if (Array.isArray(map)) {
        if (!builder.addTuples(map, carryOver)) {
          return false;
        }
        inBatch.push(mod);
      } else if (map != null) {
        builder.addUnexpected();
        inBatch.push(mod);
      }
      carryOver = carryOver + countLines(code);
    }
    const status = encoder.addModules(builder.toBatch());
    if (status.code !== codes.ok) {
      return throwForStatus(binding, status, inBatch);
    }
    for (const mod of inBatch) {
      generator.startFile(mod.path, mod.source, mod.functionMap, {
        addToIgnoreList: mod.isIgnored,
      });
      generator.endFile();
    }
    next = end;
    return true;
  }

  function workLoop(): void {
    const start = process.hrtime();
    while (true) {
      if (next >= modules.length) {
        const result = encoder.finish();
        generator.restoreFromRust(result.mappings, result.names, result.state);
        onDone(generator);
        return;
      }
      if (!processBatch()) {
        runJavaScript();
        return;
      }
      if (!isBlocking) {
        const [seconds, nanoseconds] = process.hrtime(start);
        if (seconds > 0 || nanoseconds > 50 * 1e6) {
          // We've blocked for more than 50ms. This code runs on the main
          // thread, so give Metro an opportunity to handle requests.
          setImmediate(workLoop);
          return;
        }
      }
    }
  }

  workLoop();
}

/**
 * Encodes the tuples modules of an indexed source map, each with a generator
 * of its own. Calls `onModule(index, mappings, names)` for each module in
 * order. Returns false to fall back to JavaScript. Modules that are not
 * tuples must be filtered by the caller.
 */
export function encodeSectionsRust(
  binding: SourceMapBinding,
  modules: ReadonlyArray<RawMappingsModule>,
): ?{
  readonly mappings: Array<string>,
  readonly names: Array<Array<string>>,
  ...
} {
  let mappings = 0;
  for (const mod of modules) {
    mappings = mappings + (mod.map as $FlowFixMe).length;
  }
  const builder = new BatchBuilder(new Int32Array(mappings * TUPLE_STRIDE));
  for (const mod of modules) {
    if (!builder.addTuples(mod.map as $FlowFixMe, 0)) {
      return null;
    }
  }
  const result = binding.encodeSections(builder.toBatch());
  if (result.status.code !== binding.sourceMapStatuses().ok) {
    throwForStatus(binding, result.status, modules);
    return null;
  }
  return result;
}

/** `vlqMapFromTuples`: null to fall back to JavaScript. */
export function vlqMapFromTuplesRust(
  binding: SourceMapBinding,
  tuples: ReadonlyArray<MetroSourceMapSegmentTuple>,
): ?VlqMap {
  const encoded = encodeSectionsRust(binding, [
    {
      map: tuples,
      functionMap: null,
      path: '',
      source: '',
      code: '',
      isIgnored: false,
    },
  ]);
  return encoded == null
    ? null
    : {mappings: encoded.mappings[0], names: encoded.names[0]};
}

/** `vlqMapFromBabelDecodedMap`: null to fall back to JavaScript. */
export function vlqMapFromBabelDecodedMapRust(
  binding: SourceMapBinding,
  decodedMap: BabelDecodedMap,
  terminatingMapping: [number, number],
): ?VlqMap {
  const {mappings, names} = decodedMap;
  let count = 1;
  for (let line = 0; line < mappings.length; line++) {
    count = count + mappings[line].length;
  }
  const tuples = new Int32Array(count * TUPLE_STRIDE);
  const nameTable: Array<string> = [];
  const nameIndex: Map<string, number> = new Map();
  const invalid: Map<number, ReadonlyArray<number>> = new Map();
  let o = 0;
  let lastGeneratedLine = -1;
  let lastGeneratedColumn = -1;
  for (let line = 0, n = mappings.length; line < n; ++line) {
    // Decoded mappings are grouped by generated line (0-based); the
    // generator expects 1-based generated lines.
    const generatedLine = line + 1;
    const segments = mappings[line];
    for (let i = 0, m = segments.length; i < m; ++i) {
      const segment = segments[i];
      const generatedColumn = segment[0];
      tuples[o] = generatedLine;
      tuples[o + 1] = generatedColumn;
      switch (segment.length) {
        case 1:
          tuples[o + 2] = 0;
          tuples[o + 3] = 0;
          tuples[o + 4] = NO_NAME_SIMPLE;
          break;
        case 4:
          // Decoded source lines are 0-based; the generator expects 1-based.
          tuples[o + 2] = segment[2] + 1;
          tuples[o + 3] = segment[3];
          tuples[o + 4] = NO_NAME;
          break;
        case 5: {
          const name = names[segment[4]];
          if (typeof name !== 'string') {
            return null;
          }
          let index = nameIndex.get(name);
          if (index === undefined) {
            index = nameTable.length;
            nameTable.push(name);
            nameIndex.set(name, index);
          }
          tuples[o + 2] = segment[2] + 1;
          tuples[o + 3] = segment[3];
          tuples[o + 4] = index;
          break;
        }
        default:
          tuples[o + 4] = INVALID;
          invalid.set(o / TUPLE_STRIDE, segment);
      }
      o = o + TUPLE_STRIDE;
      lastGeneratedLine = generatedLine;
      lastGeneratedColumn = generatedColumn;
    }
  }
  if (
    lastGeneratedLine !== terminatingMapping[0] ||
    lastGeneratedColumn !== terminatingMapping[1]
  ) {
    tuples[o] = terminatingMapping[0];
    tuples[o + 1] = terminatingMapping[1];
    tuples[o + 2] = 0;
    tuples[o + 3] = 0;
    tuples[o + 4] = NO_NAME_SIMPLE;
    o = o + TUPLE_STRIDE;
  }
  const total = o / TUPLE_STRIDE;
  const result = binding.encodeSections({
    tuples: tuples.subarray(0, total * TUPLE_STRIDE),
    names: nameTable,
    modules: Int32Array.of(MODULE_TUPLES, 0, total, 0),
    vlqMappings: [],
    vlqNames: [],
  });
  const codes = binding.sourceMapStatuses();
  if (result.status.code === codes.invalidMapping) {
    const segment = invalid.get(result.status.index);
    throw new Error(`Invalid mapping: [${(segment ?? []).join(', ')}]`);
  }
  if (result.status.code !== codes.ok) {
    throwForStatus(binding, result.status, []);
    return null;
  }
  return {mappings: result.mappings[0], names: result.names[0]};
}
