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
 * The source map part of the `serializers` component of the Rust port: the
 * bytes of `JSON.stringify(fromRawMappingsIndexed(getSourceMapInfos(...))
 * .toMap(undefined, {excludeSource}))`, written by Rust in one call per map.
 *
 * What it takes over from JavaScript:
 *
 * - Reading `sourcesContent`. `module.getSource()` is a closure; Metro's own
 *   (Transformer.js) is tagged with its file (sourceFileTag.js), and Rust
 *   reads those files in parallel and decodes them like `Buffer.toString()`.
 *   Any other `getSource` (Expo's virtual modules, prepended scripts, ...) is
 *   called here and its text passed in. If Rust cannot read a file,
 *   `getSource()` is called for that module, so the error is Node's own.
 * - Building the map and `JSON.stringify` of it.
 * - Work per module that does not change between requests: the encoded
 *   `mappings`, the `names`, the function map and the number of lines are held
 *   by Rust, keyed by a token per module output (`output.data`, by identity). A
 *   FinalizationRegistry sends the tokens of collected outputs for eviction
 *   with the next call.
 *
 * Strings that JavaScript sends cross as JSON text (`JSON.stringify`), because
 * a JavaScript string may hold a lone surrogate, which cannot cross as UTF-8
 * and which `JSON.stringify` writes as `\udxxx`. Text that Rust decoded itself
 * is escaped by Rust, exactly like `JSON.stringify`.
 *
 * Whenever something is not what Rust handles (a module map that is neither
 * tuples nor VLQ, tuples the encoder rejects, or any error while preparing),
 * the JavaScript implementation runs instead, so that its errors and their
 * order are the same as always.
 */

import type {Module} from '../types';
import type {SourceMapGeneratorOptions} from './sourceMapGenerator';

import {getTaggedSourceFile} from '../sourceFileTag';
import {getJsOutput} from './helpers/js';
import {
  getSourceMapInfosImpl,
  sourceMapGenerator,
  sourceMapGeneratorNonBlocking,
} from './sourceMapGenerator';
import {isRustEnabled, loadRustBinding} from 'metro-rust-switch';
import {fromRawMappingsIndexed} from 'metro-source-map';
import {countLines, isVlqMap} from 'metro-source-map/private/rawMappings';
import {encodeSectionsRust} from 'metro-source-map/private/rustMappings';

// The binding is not typed.
// flowlint-next-line unclear-type:off
type Loose = any;

const MODE_EMPTY = 0;
const MODE_FILE = 1;
const MODE_LITERAL = 2;

const KIND_NONE = 0;
const KIND_TUPLES = 1;
const KIND_VLQ = 2;

type ModuleInfo = {
  module: Module<>,
  data: Loose,
  isIgnored: boolean,
  path: unknown,
  // Where the source comes from (MODE_*), and the file or the text.
  mode: number,
  file: ?string,
  text: ?string,
};

type NewEntry = {
  data: Loose,
  token: number,
  kind: number,
  lines: number,
  mappings: string,
  names: string,
  functionMap: string,
};

type Result = {buffer: Buffer, ascii: boolean, failed: Array<number>};

// What a call gives: the bytes, whether they are all ASCII, and the string if
// the JavaScript implementation made it.
type Built = {buffer: Buffer, ascii: boolean, string: ?string};

let builder: ?Loose = null;
// Output data -> token of the entry Rust holds for it.
let tokens: WeakMap<interface {}, number> = new WeakMap();
// Tokens of collected outputs, to be dropped by Rust with the next call.
let pendingEvictions: Array<number> = [];
const registry: FinalizationRegistry<number> = new FinalizationRegistry(
  token => {
    pendingEvictions.push(token);
  },
);
// Tokens are never reused within a process.
let lastToken = 0;

/**
 * Whether index source maps are written in Rust: the `serializers` component
 * is on. `sourceMapString` and its callers get no config, so, like the
 * `source-map` component's binding getter, this follows `METRO_RUST`; callers
 * that have the config pass its `unstable_rust` in the options.
 */
export function isRustSourceMapEnabled(
  options: ?SourceMapGeneratorOptions,
): boolean {
  return isRustEnabled('serializers', options?.unstable_rust);
}

let lastVerifyValue: ?string;
let lastVerify = false;

/** Whether METRO_RUST_VERIFY lists `serializers`. */
function isVerifyEnabled(): boolean {
  const verify = process.env.METRO_RUST_VERIFY;
  if (verify !== lastVerifyValue) {
    // Cached only after isRustEnabled succeeds (it throws on an invalid list).
    lastVerify =
      verify != null &&
      verify !== '' &&
      isRustEnabled('serializers', null, {METRO_RUST: verify});
    lastVerifyValue = verify;
  }
  return lastVerify;
}

function getBuilder(): Loose {
  if (builder == null) {
    builder = new (loadRustBinding() as Loose).IndexMapBuilder();
  }
  return builder;
}

/** Starts again: Rust holds nothing JavaScript believes it does. */
function reset(): void {
  builder = null;
  tokens = new WeakMap();
  pendingEvictions = [];
}

function nextToken(): number {
  lastToken++;
  // 2^32 - 1 and up do not fit a Uint32Array.
  return lastToken;
}

const SAFE_JSON_STRING = /^[\x20\x21\x23-\x5b\x5d-\x7e]*$/;

/** `JSON.stringify(text)` for a string; cheap for plain ASCII. */
function jsonString(text: string): string {
  return SAFE_JSON_STRING.test(text) ? `"${text}"` : JSON.stringify(text);
}

function infoOf(options: SourceMapGeneratorOptions): (Module<>) => ModuleInfo {
  return module => {
    // Calls the hooks in the order getSourceMapInfo does.
    const output = getJsOutput(module);
    const isIgnored = options.shouldAddToIgnoreList(module);
    const path = options.getSourceUrl?.(module) ?? module.path;
    let mode = MODE_EMPTY;
    let file = null;
    let text = null;
    if (!options.excludeSource && output.type !== 'js/module/asset') {
      file = getTaggedSourceFile(module.getSource);
      if (file != null) {
        mode = MODE_FILE;
      } else {
        mode = MODE_LITERAL;
        text = module.getSource().toString();
      }
    }
    return {module, data: output.data, isIgnored, path, mode, file, text};
  };
}

/** The entry that Rust should hold for a module output. */
function describe(data: Loose, path: unknown): NewEntry {
  const {map} = data;
  let kind;
  if (map == null) {
    kind = KIND_NONE;
  } else if (isVlqMap(map)) {
    kind = KIND_VLQ;
  } else if (Array.isArray(map)) {
    kind = KIND_TUPLES;
  } else {
    throw new Error(`Unexpected module with full source map: ${String(path)}`);
  }
  let functionMap = '';
  if (kind !== KIND_NONE && data.functionMap) {
    const json = JSON.stringify(data.functionMap);
    if (typeof json !== 'string') {
      throw new Error('function map is not JSON');
    }
    functionMap = json;
  }
  return {
    data,
    token: nextToken(),
    kind,
    lines: countLines(data.code),
    mappings: kind === KIND_VLQ ? jsonString(map.mappings) : '""',
    names: kind === KIND_VLQ ? JSON.stringify([...map.names]) : '[]',
    functionMap,
  };
}

/**
 * The request for Rust, and the entries it does not hold yet (to be
 * committed to `tokens` once Rust accepted them). Returns null if the
 * JavaScript implementation has to run: something here is not what Rust
 * handles, or throws as the JavaScript implementation would.
 */
function prepare(
  infos: ReadonlyArray<ModuleInfo>,
  options: SourceMapGeneratorOptions,
): ?{request: Loose, newEntries: Array<NewEntry>} {
  try {
    const pending: Map<interface {}, NewEntry> = new Map();
    const tuples: Array<NewEntry> = [];
    const entryTokens = new Uint32Array(infos.length);
    for (let i = 0; i < infos.length; i++) {
      const {data, path} = infos[i];
      let token = tokens.get(data);
      if (token == null) {
        let entry = pending.get(data);
        if (entry == null) {
          entry = describe(data, path);
          pending.set(data, entry);
          if (entry.kind === KIND_TUPLES) {
            tuples.push(entry);
          }
        }
        token = entry.token;
      }
      entryTokens[i] = token;
    }
    if (tuples.length > 0) {
      // One batch for all new tuple maps (the encoder of the source-map
      // component). It throws for the same invalid mappings as JavaScript.
      const encoded = encodeSectionsRust(
        loadRustBinding() as Loose,
        tuples.map(entry => ({map: entry.data.map, path: ''}) as Loose),
      );
      if (encoded == null) {
        return null;
      }
      tuples.forEach((entry, k) => {
        entry.mappings = jsonString(encoded.mappings[k]);
        entry.names = JSON.stringify(encoded.names[k]);
      });
    }
    const newEntries = [...pending.values()];
    return {
      request: makeRequest(infos, entryTokens, newEntries, options),
      newEntries,
    };
  } catch {
    // The JavaScript implementation throws what it always throws, in the
    // order it always does.
    return null;
  }
}

function makeRequest(
  infos: ReadonlyArray<ModuleInfo>,
  entryTokens: Uint32Array,
  newEntries: ReadonlyArray<NewEntry>,
  options: SourceMapGeneratorOptions,
): Loose {
  const flags = new Uint8Array(infos.length);
  const paths: Array<string> = [];
  const files: Array<string> = [];
  const literals: Array<string> = [];
  for (let i = 0; i < infos.length; i++) {
    const info = infos[i];
    flags[i] = (info.isIgnored ? 1 : 0) | (info.mode << 1);
    const path = info.path;
    if (typeof path !== 'string') {
      throw new Error('source path is not a string');
    }
    paths.push(jsonString(path));
    if (info.mode === MODE_FILE) {
      files.push(String(info.file));
    } else if (info.mode === MODE_LITERAL) {
      literals.push(jsonString(String(info.text)));
    }
  }
  return {
    tokens: entryTokens,
    flags,
    paths,
    files,
    literals,
    excludeSource: options.excludeSource === true,
    newTokens: Uint32Array.from(newEntries.map(e => e.token)),
    newKinds: Uint8Array.from(newEntries.map(e => e.kind)),
    newLines: Uint32Array.from(newEntries.map(e => e.lines)),
    newMappings: newEntries.map(e => e.mappings),
    newNames: newEntries.map(e => e.names),
    newFunctionMaps: newEntries.map(e => e.functionMap),
    evict: Uint32Array.from(pendingEvictions),
  };
}

/**
 * Records what Rust now holds. Called right after the call that sent the
 * entries and the evictions, in the same synchronous stretch.
 */
function commit(newEntries: ReadonlyArray<NewEntry>): void {
  pendingEvictions = [];
  for (const entry of newEntries) {
    tokens.set(entry.data, entry.token);
    registry.register(entry.data, entry.token);
  }
}

/**
 * The JavaScript implementation, given the infos gathered so far. It reads
 * the files that Rust was to read, in module order, so a file that cannot be
 * read throws Node's own error, from `getSource()`.
 */
function fallback(
  infos: ReadonlyArray<ModuleInfo>,
  options: SourceMapGeneratorOptions,
): Built {
  const modules = infos.map(info => {
    const source =
      info.mode === MODE_FILE
        ? info.module.getSource().toString()
        : (info.text ?? '');
    return {...info.data, isIgnored: info.isIgnored, path: info.path, source};
  });
  const string = fromRawMappingsIndexed(modules).toString(undefined, {
    excludeSource: options.excludeSource,
  });
  return {buffer: Buffer.from(string, 'utf8'), ascii: false, string};
}

/** Sends the request; calls `fallback` when the build left files unread. */
function afterBuild(
  result: Result,
  infos: ReadonlyArray<ModuleInfo>,
  options: SourceMapGeneratorOptions,
): Built {
  return result.failed.length === 0
    ? {buffer: result.buffer, ascii: result.ascii, string: null}
    : fallback(infos, options);
}

function stringOf(built: Built): string {
  return built.string ?? built.buffer.toString(built.ascii ? 'latin1' : 'utf8');
}

function verify(built: Built, js: string): void {
  const expected = Buffer.from(js, 'utf8');
  const actual = built.buffer;
  if (expected.equals(actual) && stringOf(built) === js) {
    return;
  }
  let offset = 0;
  const n = Math.min(expected.length, actual.length);
  while (offset < n && expected[offset] === actual[offset]) {
    offset++;
  }
  const context = (buffer: Buffer): string =>
    JSON.stringify(
      buffer.subarray(Math.max(0, offset - 80), offset + 80).toString('utf8'),
    );
  throw new Error(
    `METRO_RUST_VERIFY=serializers: the source map bytes differ at byte ${offset} ` +
      `(JavaScript ${expected.length} bytes, Rust ${actual.length} bytes).\n` +
      `JavaScript: ${context(expected)}\nRust:       ${context(actual)}`,
  );
}

function gather(
  modules: ReadonlyArray<Module<>>,
  options: SourceMapGeneratorOptions,
): ReadonlyArray<ModuleInfo> {
  let infos = null;
  getSourceMapInfosImpl(
    true,
    result => {
      infos = result;
    },
    modules,
    options,
    infoOf(options),
  );
  if (infos == null) {
    throw new Error(
      'Expected getSourceMapInfosImpl() to finish synchronously.',
    );
  }
  return infos;
}

function gatherNonBlocking(
  modules: ReadonlyArray<Module<>>,
  options: SourceMapGeneratorOptions,
): Promise<ReadonlyArray<ModuleInfo>> {
  return new Promise(resolve => {
    getSourceMapInfosImpl(false, resolve, modules, options, infoOf(options));
  });
}

function build(
  infos: ReadonlyArray<ModuleInfo>,
  options: SourceMapGeneratorOptions,
): Built {
  const prepared = prepare(infos, options);
  if (prepared == null) {
    return fallback(infos, options);
  }
  let result;
  try {
    result = getBuilder().build(prepared.request);
  } catch (error) {
    reset();
    throw error;
  }
  commit(prepared.newEntries);
  return afterBuild(result, infos, options);
}

async function buildNonBlocking(
  infos: ReadonlyArray<ModuleInfo>,
  options: SourceMapGeneratorOptions,
): Promise<Built> {
  const prepared = prepare(infos, options);
  if (prepared == null) {
    return fallback(infos, options);
  }
  let promise;
  try {
    promise = getBuilder().buildAsync(prepared.request);
  } catch (error) {
    reset();
    throw error;
  }
  commit(prepared.newEntries);
  let result;
  try {
    result = await promise;
  } catch (error) {
    reset();
    throw error;
  }
  return afterBuild(result, infos, options);
}

/**
 * `sourceMapString(modules, options)` with the map written in Rust. The
 * result is the same string. When every byte is ASCII the string is made with
 * `latin1`, which is much cheaper than decoding UTF-8.
 */
export function rustSourceMapString(
  modules: ReadonlyArray<Module<>>,
  options: SourceMapGeneratorOptions,
): string {
  const built = build(gather(modules, options), options);
  if (isVerifyEnabled()) {
    verify(
      built,
      sourceMapGenerator(modules, options).toString(undefined, {
        excludeSource: options.excludeSource,
      }),
    );
  }
  return stringOf(built);
}

async function builtNonBlocking(
  modules: ReadonlyArray<Module<>>,
  options: SourceMapGeneratorOptions,
): Promise<Built> {
  const built = await buildNonBlocking(
    await gatherNonBlocking(modules, options),
    options,
  );
  if (isVerifyEnabled()) {
    const generator = await sourceMapGeneratorNonBlocking(modules, options);
    verify(
      built,
      generator.toString(undefined, {excludeSource: options.excludeSource}),
    );
  }
  return built;
}

export async function rustSourceMapStringNonBlocking(
  modules: ReadonlyArray<Module<>>,
  options: SourceMapGeneratorOptions,
): Promise<string> {
  return stringOf(await builtNonBlocking(modules, options));
}

/** The map's bytes, for the dev server's `.map` response. */
export async function rustSourceMapBufferNonBlocking(
  modules: ReadonlyArray<Module<>>,
  options: SourceMapGeneratorOptions,
): Promise<Buffer> {
  return (await builtNonBlocking(modules, options)).buffer;
}
