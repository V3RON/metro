/**
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 *
 * @flow
 * @format
 * @oncall react_native
 */

import type {ColumnsResult} from '../rust';
import type {BasicSourceMap} from '../source-map';
import type {
  GeneratedPositionLookup,
  IConsumer,
  IterationOrder,
  Mapping,
  SourcePosition,
} from './types';
import type {Number0, Number1} from 'ob1';

import AbstractConsumer from './AbstractConsumer';
import {
  COLUMNS_NONE,
  COLUMNS_STRIDE,
  getSourceMapBinding,
  withDecodedMappings,
} from '../rust';
import {
  EMPTY_POSITION,
  FIRST_COLUMN,
  FIRST_LINE,
  GENERATED_ORDER,
  GREATEST_LOWER_BOUND,
  lookupBiasToString,
} from './constants';
import normalizeSourcePath from './normalizeSourcePath';
import invariant from 'invariant';
import {add, add0, get0, get1, inc} from 'ob1';

/* eslint-disable no-bitwise */

const COMMA = 44; // ','
const SEMICOLON = 59; // ';'
const VLQ_BASE_SHIFT = 5;
const VLQ_BASE_MASK = (1 << VLQ_BASE_SHIFT) - 1;
const VLQ_CONTINUATION_BIT = 1 << VLQ_BASE_SHIFT;
const BASE64_DECODE: Int8Array = (() => {
  const table = new Int8Array(128).fill(-1);
  const chars =
    'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
  for (let i = 0; i < chars.length; i++) {
    table[chars.charCodeAt(i)] = i;
  }
  return table;
})();

/**
 * A source map consumer that supports "basic" source maps (that have a
 * `mappings` field and no sections).
 */
export default class MappingsConsumer
  extends AbstractConsumer
  implements IConsumer
{
  _sourceMap: BasicSourceMap;
  _decodedMappings: ?ReadonlyArray<Mapping>;
  // The index in `_decodedMappings` of the first mapping on each generated
  // line (0-based), so a lookup only searches the mappings on its line.
  _lineStarts: ?ReadonlyArray<number>;
  _normalizedSources: ?ReadonlyArray<string>;
  // The mappings as flat columns when the Rust decoder is on and has decoded
  // them for good (see ColumnsResult). `undefined` until then, null when
  // JavaScript decodes.
  _columns: ?ColumnsResult | void;

  constructor(sourceMap: BasicSourceMap) {
    super(sourceMap);
    this._sourceMap = sourceMap;
    this._decodedMappings = null;
    this._lineStarts = null;
    this._normalizedSources = null;
    this._columns = undefined;
  }

  /**
   * Decodes the mappings with Rust if that is switched on, and keeps the
   * result. Returns null when the JavaScript decoder has to run: Rust is off,
   * or the mappings are ones for which JavaScript throws or wraps around.
   */
  _rustColumns(): ?ColumnsResult {
    let columns = this._columns;
    if (columns === undefined) {
      columns = null;
      const binding = getSourceMapBinding();
      if (binding != null && typeof this._sourceMap.mappings === 'string') {
        // Like `_decodeMappings`, fail on bad sources before the mappings.
        this._normalizeAndCacheSources();
        columns = binding.decodeMappings(this._sourceMap.mappings);
      }
      this._columns = columns;
    }
    return columns;
  }

  /**
   * The decoded mappings, with the normalized sources and the names they
   * index, for `composeSourceMaps`. Null when JavaScript decodes.
   */
  rustMappingsAndTables(): ?{
    columns: ColumnsResult,
    sources: ReadonlyArray<string>,
    names: ReadonlyArray<unknown>,
  } {
    const columns = this._rustColumns();
    if (columns == null) {
      return null;
    }
    return {
      columns,
      sources: this._normalizeAndCacheSources(),
      names: this._sourceMap.names ?? [],
    };
  }

  /**
   * Calls `callback` with every mapping, from the columns, in the order they
   * were encoded, moved by an offset the way `SectionsConsumer` yields the
   * mappings of a section (`startsSection`, which also adds the empty mapping
   * that starts a section whose first mapping is not at its origin). Mappings
   * that have not been decoded for good are decoded into shared buffers and
   * not kept: a pass over the mappings costs no memory afterwards.
   *
   * Returns false, without calling back, if JavaScript has to decode.
   */
  eachMappingFromColumns(
    callback: (mapping: Mapping) => unknown,
    context: unknown,
    offsetLines: number,
    offsetColumns: number,
    startsSection: boolean,
  ): boolean {
    if (this._decodedMappings != null) {
      return false;
    }
    const emit = (
      data: Int32Array,
      mappingCount: number,
      lineStarts: Int32Array,
      lineCount: number,
    ) => {
      const sources = this._normalizeAndCacheSources();
      const names = this._sourceMap.names;
      let first = startsSection;
      for (let line = 0; line < lineCount; line++) {
        const end = line + 1 < lineCount ? lineStarts[line + 1] : mappingCount;
        const generatedLine = line + 1 + offsetLines;
        for (let i = lineStarts[line]; i < end; i++) {
          const base = i * COLUMNS_STRIDE;
          const column = data[base];
          if (first) {
            first = false;
            if (line > 0 || column > 0) {
              callback.call(context, {
                generatedLine: (1 + offsetLines) as $FlowFixMe,
                generatedColumn: (0 + offsetColumns) as $FlowFixMe,
                source: null,
                name: null,
                originalLine: null,
                originalColumn: null,
              });
            }
          }
          const generatedColumn = line === 0 ? column + offsetColumns : column;
          const source = data[base + 1];
          const name = data[base + 4];
          callback.call(
            context,
            (source === COLUMNS_NONE
              ? {
                  generatedLine,
                  generatedColumn,
                  source: null,
                  name: null,
                  originalLine: null,
                  originalColumn: null,
                }
              : {
                  generatedLine,
                  generatedColumn,
                  source: sources[source],
                  name: name === COLUMNS_NONE ? null : names[name],
                  originalLine: data[base + 2],
                  originalColumn: data[base + 3],
                }) as $FlowFixMe,
          );
        }
      }
    };
    const columns = this._columns;
    if (columns === null) {
      return false;
    }
    if (columns !== undefined) {
      emit(
        columns.data,
        columns.data.length / COLUMNS_STRIDE,
        columns.lineStarts,
        columns.lineStarts.length,
      );
      return true;
    }
    const binding = getSourceMapBinding();
    if (binding == null || typeof this._sourceMap.mappings !== 'string') {
      return false;
    }
    // Like `_decodeMappings`, fail on bad sources before the mappings.
    this._normalizeAndCacheSources();
    if (withDecodedMappings(binding, this._sourceMap.mappings, emit)) {
      return true;
    }
    this._columns = null;
    return false;
  }

  _originalPositionFromColumns(
    columns: ColumnsResult,
    line: Number1,
    column: Number0,
  ): SourcePosition {
    const {data, lineStarts} = columns;
    const line0 = get1(line) - 1;
    if (line0 < 0 || line0 >= lineStarts.length) {
      return {...EMPTY_POSITION};
    }
    // The last mapping on the line at or before `column`.
    let low = lineStarts[line0];
    let high =
      line0 + 1 < lineStarts.length
        ? lineStarts[line0 + 1]
        : data.length / COLUMNS_STRIDE;
    const lineStart = low;
    const column0 = get0(column);
    while (low < high) {
      const mid = (low + high) >>> 1;
      if (data[mid * COLUMNS_STRIDE] <= column0) {
        low = mid + 1;
      } else {
        high = mid;
      }
    }
    if (low > lineStart) {
      const base = (low - 1) * COLUMNS_STRIDE;
      const source = data[base + 1];
      if (source === COLUMNS_NONE) {
        return {source: null, name: null, line: null, column: null};
      }
      const name = data[base + 4];
      return {
        source: this._normalizeAndCacheSources()[source],
        name: name === COLUMNS_NONE ? null : this._sourceMap.names[name],
        line: data[base + 2] as $FlowFixMe,
        column: data[base + 3] as $FlowFixMe,
      };
    }
    return {...EMPTY_POSITION};
  }

  eachMapping(
    callback: (mapping: Mapping) => unknown,
    context?: unknown = null,
    order?: IterationOrder = GENERATED_ORDER,
  ): void {
    if (
      order === GENERATED_ORDER &&
      this.eachMappingFromColumns(callback, context, 0, 0, false)
    ) {
      return;
    }
    super.eachMapping(callback, context, order);
  }

  originalPositionFor(
    generatedPosition: GeneratedPositionLookup,
  ): SourcePosition {
    const {line, column} = generatedPosition;
    if (line == null || column == null) {
      return {...EMPTY_POSITION};
    }
    if (generatedPosition.bias != null) {
      invariant(
        generatedPosition.bias === GREATEST_LOWER_BOUND,
        `Unimplemented lookup bias: ${lookupBiasToString(
          // $FlowFixMe[incompatible-type]
          generatedPosition.bias,
        )}`,
      );
    }
    const columns = this._rustColumns();
    if (columns != null) {
      return this._originalPositionFromColumns(columns, line, column);
    }
    const mappings = this._decodeAndCacheMappings();
    const lineStarts = this._lineStarts;
    invariant(lineStarts != null, 'Expected line starts to be decoded');
    const line0 = get1(line) - 1;
    if (line0 < 0 || line0 >= lineStarts.length) {
      return {...EMPTY_POSITION};
    }
    // The last mapping on the line at or before `column`.
    let low = lineStarts[line0];
    let high =
      line0 + 1 < lineStarts.length ? lineStarts[line0 + 1] : mappings.length;
    const lineStart = low;
    while (low < high) {
      const mid = (low + high) >>> 1;
      if (get0(mappings[mid].generatedColumn) <= get0(column)) {
        low = mid + 1;
      } else {
        high = mid;
      }
    }
    if (low > lineStart) {
      const mapping = mappings[low - 1];
      return {
        source: mapping.source,
        name: mapping.name,
        line: mapping.originalLine,
        column: mapping.originalColumn,
      };
    }
    return {...EMPTY_POSITION};
  }

  _decodeMappings(): Array<Mapping> {
    const columns = this._rustColumns();
    if (columns != null) {
      const result: Array<Mapping> = [];
      this.eachMappingFromColumns(
        mapping => {
          result.push(mapping);
        },
        null,
        0,
        0,
        false,
      );
      return result;
    }
    const normalizedSources = this._normalizeAndCacheSources();
    const {mappings: mappingsRaw, names} = this._sourceMap;
    const result: Array<Mapping> = [];
    const lineStarts = [0];

    let generatedLine = FIRST_LINE;
    let generatedColumn = FIRST_COLUMN;
    let originalLine = FIRST_LINE;
    let originalColumn = FIRST_COLUMN;
    let nameIndex = add0(0);
    let sourceIndex = add0(0);

    // The VLQ fields of the segment being decoded, decoded in place from
    // character codes rather than by slicing out each segment.
    const fields = [0, 0, 0, 0, 0];
    let fieldCount = 0;
    let value = 0;
    let shift = 0;

    const length = mappingsRaw.length;
    for (let i = 0; i <= length; i++) {
      const charCode = i < length ? mappingsRaw.charCodeAt(i) : SEMICOLON;
      if (charCode === COMMA || charCode === SEMICOLON) {
        // A trailing field cut off mid-VLQ is dropped, as `vlq` does.
        invariant(
          fieldCount > 0 || shift === 0,
          'Invalid generated column delta',
        );
        value = 0;
        shift = 0;
        if (fieldCount > 0) {
          invariant(fieldCount !== 2, 'Invalid original line delta');
          invariant(fieldCount !== 3, 'Invalid original column delta');
          generatedColumn = add(generatedColumn, fields[0]);
          let source = null;
          let name = null;
          let mappingOriginalLine = null;
          let mappingOriginalColumn = null;
          if (fieldCount >= 4) {
            sourceIndex = add(sourceIndex, fields[1]);
            source = normalizedSources[get0(sourceIndex)];
            originalLine = add(originalLine, fields[2]);
            originalColumn = add(originalColumn, fields[3]);
            mappingOriginalLine = originalLine;
            mappingOriginalColumn = originalColumn;
            if (fieldCount >= 5) {
              nameIndex = add(nameIndex, fields[4]);
              name = names[get0(nameIndex)];
            }
          }
          result.push({
            generatedLine,
            generatedColumn,
            source,
            name,
            originalLine: mappingOriginalLine,
            originalColumn: mappingOriginalColumn,
          });
          fieldCount = 0;
        }
        if (charCode === SEMICOLON && i < length) {
          lineStarts.push(result.length);
          generatedLine = inc(generatedLine);
          generatedColumn = FIRST_COLUMN;
        }
        continue;
      }
      const digit = charCode < 128 ? BASE64_DECODE[charCode] : -1;
      invariant(digit !== -1, 'Invalid character in source map mappings');
      value = value + ((digit & VLQ_BASE_MASK) << shift);
      if (digit & VLQ_CONTINUATION_BIT) {
        shift = shift + VLQ_BASE_SHIFT;
      } else {
        const negate = value & 1;
        value = value >>> 1;
        if (fieldCount < 5) {
          fields[fieldCount] = negate ? -value : value;
        }
        fieldCount++;
        value = 0;
        shift = 0;
      }
    }
    this._lineStarts = lineStarts;
    return result;
  }

  _normalizeAndCacheSources(): ReadonlyArray<string> {
    if (!this._normalizedSources) {
      this._normalizedSources = this._sourceMap.sources.map(source =>
        normalizeSourcePath(source, this._sourceMap),
      );
    }
    return this._normalizedSources;
  }

  _decodeAndCacheMappings(): ReadonlyArray<Mapping> {
    if (!this._decodedMappings) {
      this._decodedMappings = this._decodeMappings();
    }
    return this._decodedMappings;
  }

  generatedMappings(): Iterable<Mapping> {
    return this._decodeAndCacheMappings();
  }

  _indexOfSource(source: string): ?Number0 {
    const idx = this._normalizeAndCacheSources().indexOf(
      normalizeSourcePath(source, this._sourceMap),
    );
    if (idx === -1) {
      return null;
    }
    return add0(idx);
  }

  sourceContentFor(source: string, nullOnMissing: true): ?string {
    const {sourcesContent} = this._sourceMap;
    if (!sourcesContent) {
      return null;
    }
    const idx = this._indexOfSource(source);
    if (idx == null) {
      return null;
    }
    return sourcesContent[get0(idx)] ?? null;
  }
}
