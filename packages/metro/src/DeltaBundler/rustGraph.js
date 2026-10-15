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
 * The `delta-core` component of the Rust port: the topology of `Graph` (which
 * module imports which, the inverse dependencies, the cycle collector and the
 * added / modified / deleted sets) lives in Rust (`metro-rs/crates/graph`).
 * The module objects, `graph.dependencies` and everything the serializers
 * read stay real JavaScript objects (RULES.md section 7, rule 9): after each
 * traversal Rust returns one delta, and this file applies it in exactly the
 * order, and to exactly the objects, that the JavaScript algorithm in
 * Graph.js would have.
 *
 * The layout of the arrays that cross is documented in
 * `metro-rs/crates/graph/src/wire.rs`. Paths and dependency keys cross as
 * ids; the `id -> string` arrays below are append-only (rule 4).
 */

import type {RequireContext} from '../lib/contextModule';
import type {Dependency, Module, ModuleData} from './types';

import CountingSet from '../lib/CountingSet';
import {isRustEnabled, loadRustBinding} from 'metro-rust-switch';

// The binding is not typed.
// flowlint-next-line unclear-type:off
type Loose = any;

const NONE = 0xffffffff;

const STATUS_OK = 0;
const STATUS_MODULE = 1;
const STATUS_NULL_THROWS = 2;
const STATUS_INVARIANT = 3;
const STATUS_DEPTH = 4;

const INVARIANT_MESSAGES = [
  'attempted to roll back a graph commit but there were still changes',
  'Failed to add the correct dependencies',
  'lazy: import bundle inverse references',
];

export type GraphView<T> = {
  dependencies: Map<string, Module<T>>,
  resolvedContexts: Map<string, RequireContext>,
  importBundleNodes: Map<
    string,
    Readonly<{inverseDependencies: CountingSet<string>}>,
  >,
};

// ------------------------------------------------------------------ ids

// The ids come from Rust's process-wide table. This module may be loaded
// more than once in a process (Jest does), each time with its own maps, so an
// id is never assumed to be the next one.
const pathIds: Map<string, number> = new Map();
const pathList: Array<string> = [];
let nativeBinding: Loose = null;

// A string that crosses to Rust unchanged. napi converts strings to UTF-8 and
// replaces lone surrogates, so two such paths could share an id. Those, and
// any path starting with ESCAPE, cross as ESCAPE + their JSON (which escapes
// lone surrogates), so different paths always reach Rust as different
// strings.
const ESCAPE = '\0\x01';
const LONE_SURROGATE =
  /[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/;

function rustKey(path: string): string {
  return path.startsWith(ESCAPE) || LONE_SURROGATE.test(path)
    ? ESCAPE + JSON.stringify(path)
    : path;
}

function pathId(path: string): number {
  let id = pathIds.get(path);
  if (id == null) {
    id = nativeBinding.graphInternPath(rustKey(path)) as number;
    pathIds.set(path, id);
    pathList[id] = path;
  }
  return id;
}

const keyIds: Map<string, number> = new Map();
const keyList: Array<string> = [];

// Identity of `require.context` parameters; 0 is "none".
const contextSignatures: Map<string, number> = new Map();

// `asyncType` codes; 0 is "none" and 1 is "weak".
const asyncCodes: Map<string, number> = new Map([['weak', 1]]);

function keyId(key: string): number {
  let id = keyIds.get(key);
  if (id == null) {
    id = keyList.length;
    keyIds.set(key, id);
    keyList.push(key);
  }
  return id;
}

function asyncCode(asyncType: ?string): number {
  if (asyncType == null) {
    return 0;
  }
  let code = asyncCodes.get(asyncType);
  if (code == null) {
    code = asyncCodes.size + 1;
    asyncCodes.set(asyncType, code);
  }
  return code;
}

function contextSignature(dependency: Dependency): number {
  const params = dependency.data.data.contextParams;
  if (params == null) {
    return 0;
  }
  const signature = JSON.stringify([
    params.recursive,
    params.filter.pattern,
    params.filter.flags,
    params.mode,
  ]);
  let id = contextSignatures.get(signature);
  if (id == null) {
    id = contextSignatures.size + 1;
    contextSignatures.set(signature, id);
  }
  return id;
}

// ------------------------------------------------------------ switch

let lastVerifyValue: ?string;
let lastVerify = false;

/** Whether METRO_RUST_VERIFY lists `delta-core`. */
export function isVerifyEnabled(): boolean {
  const verify = process.env.METRO_RUST_VERIFY;
  if (verify !== lastVerifyValue) {
    // Cached only after isRustEnabled succeeds (it throws on an invalid list).
    lastVerify =
      verify != null &&
      verify !== '' &&
      isRustEnabled('delta-core', null, {METRO_RUST: verify});
    lastVerifyValue = verify;
  }
  return lastVerify;
}

/**
 * The Rust topology for a new graph, or null when `delta-core` is off.
 *
 * Rust holds any module path, including virtual ones (`\0polyfill:…`).
 */
export function createRustGraph(
  entryPoints: ReadonlySet<string>,
  config: ?Readonly<{[componentId: string]: boolean}>,
): ?RustGraph {
  if (!isRustEnabled('delta-core', config)) {
    return null;
  }
  return new RustGraph(loadRustBinding() as Loose, entryPoints);
}

export type RustOutcome = {
  status: number,
  /** The path whose error is thrown (`STATUS_MODULE`). */
  errorPath: ?string,
  invariantMessage: ?string,
  added: Array<string>,
  deleted: Array<string>,
  modified: Array<string>,
};

export const Status = {
  OK: STATUS_OK,
  MODULE: STATUS_MODULE,
  NULL_THROWS: STATUS_NULL_THROWS,
  INVARIANT: STATUS_INVARIANT,
  DEPTH: STATUS_DEPTH,
};

export class RustGraph {
  #binding: Loose;
  #graph: Loose;

  constructor(binding: Loose, entryPoints: ReadonlySet<string>) {
    nativeBinding = binding;
    this.#binding = binding;
    const ids = [...entryPoints].map(pathId);
    this.#graph = new binding.DeltaGraph(Uint32Array.from(ids));
  }

  moduleCount(): number {
    return this.#graph.moduleCount();
  }

  /**
   * One traversal: Rust decides what is added, modified and deleted and in
   * which order, then this applies the result to `view`. Returns what the
   * caller needs to build `Result` or to throw.
   */
  traverse<T>(
    initial: boolean,
    view: GraphView<T>,
    modified: ReadonlySet<string>,
    errors: ReadonlyMap<string, Error>,
    updated: ReadonlyMap<string, ModuleData<T>>,
    options: Readonly<{lazy: boolean, shallow: boolean, ...}>,
  ): RustOutcome {
    const words = encodeInput(view, modified, errors, updated, options);
    const out: Uint32Array = initial
      ? this.#graph.initial(words)
      : this.#graph.incremental(words);
    return decodeAndApply(out, view, updated);
  }
}

function encodeInput<T>(
  view: GraphView<T>,
  modified: ReadonlySet<string>,
  errors: ReadonlyMap<string, Error>,
  updated: ReadonlyMap<string, ModuleData<T>>,
  options: Readonly<{lazy: boolean, shallow: boolean, ...}>,
): Uint32Array {
  let size = 4 + modified.size + errors.size;
  for (const data of updated.values()) {
    size += 3 + 5 * data.dependencies.size;
  }
  const w = new Uint32Array(size);
  let n = 0;
  w[n++] = (options.lazy ? 1 : 0) + (options.shallow ? 2 : 0);
  w[n++] = modified.size;
  for (const path of modified) {
    w[n++] = pathId(path);
  }
  w[n++] = errors.size;
  for (const path of errors.keys()) {
    w[n++] = pathId(path);
  }
  w[n++] = updated.size;
  for (const [path, data] of updated) {
    const original = view.dependencies.get(path);
    // `{...previousModule, ...transformResult}` keeps the previous key when
    // the transform result has no such property.
    let keyIsNull;
    let keyEquals;
    if ('unstable_transformResultKey' in data) {
      keyIsNull = data.unstable_transformResultKey == null;
      keyEquals =
        original != null &&
        original.unstable_transformResultKey ===
          data.unstable_transformResultKey;
    } else {
      keyIsNull =
        original == null || original.unstable_transformResultKey == null;
      keyEquals = true;
    }
    w[n++] = pathId(path);
    w[n++] = (keyIsNull ? 1 : 0) + (keyEquals ? 2 : 0);
    w[n++] = data.dependencies.size;
    for (const [key, dependency] of data.dependencies) {
      w[n++] = keyId(key);
      w[n++] =
        dependency.absolutePath != null
          ? pathId(dependency.absolutePath)
          : NONE;
      w[n++] = asyncCode(dependency.data.data.asyncType);
      w[n++] = contextSignature(dependency);
      w[n++] = data.resolvedContexts.has(key) ? 1 : 0;
    }
  }
  return w;
}

type Descriptor = {
  path: string,
  kind: number,
  fromTransform: boolean,
  inverseIsOriginal: boolean,
  edges: Array<[string, boolean]>,
  inverse: ?Array<[string, number]>,
};

type ContextEntry = {
  path: string,
  fromTransform: boolean,
  module: string,
  key: string,
};

function decodeAndApply<T>(
  out: Uint32Array,
  view: GraphView<T>,
  updated: ReadonlyMap<string, ModuleData<T>>,
): RustOutcome {
  let pos = 0;
  const next = () => out[pos++];
  const paths = (): Array<string> => {
    const n = next();
    const list = [];
    for (let i = 0; i < n; i++) {
      list.push(pathList[next()]);
    }
    return list;
  };
  const counts = (): Array<[string, number]> => {
    const n = next();
    const list: Array<[string, number]> = [];
    for (let i = 0; i < n; i++) {
      const parent = pathList[next()];
      list.push([parent, next()]);
    }
    return list;
  };

  const status = next();
  const arg = next();
  const hasState = next() === 1;
  const outcome: RustOutcome = {
    status,
    errorPath: status === STATUS_MODULE ? pathList[arg] : null,
    invariantMessage:
      status === STATUS_INVARIANT ? INVARIANT_MESSAGES[arg] : null,
    added: [],
    deleted: [],
    modified: [],
  };
  if (!hasState) {
    return outcome;
  }

  // 1. Read everything.
  const removed = paths();
  const descriptors = (): Array<Descriptor> => {
    const n = next();
    const list = [];
    for (let i = 0; i < n; i++) {
      const path = pathList[next()];
      const kind = next();
      const d: Descriptor = {
        path,
        kind,
        fromTransform: false,
        inverseIsOriginal: true,
        edges: [],
        inverse: null,
      };
      const edges = () => {
        const m = next();
        for (let j = 0; j < m; j++) {
          const key = keyList[next()];
          d.edges.push([key, next() === 1]);
        }
      };
      if (kind === 1) {
        edges();
      } else if (kind === 2) {
        d.fromTransform = next() === 1;
        d.inverseIsOriginal = next() === 1;
        edges();
        if (!d.inverseIsOriginal) {
          d.inverse = counts();
        }
      }
      list.push(d);
    }
    return list;
  };
  const inPlace = descriptors();
  const fresh = descriptors();
  const inverseChanges: Array<[string, Array<[string, number]>]> = [];
  for (let n = next(), i = 0; i < n; i++) {
    const owner = pathList[next()];
    inverseChanges.push([owner, counts()]);
  }
  const importCleared = next() === 1;
  const importNodes: Array<[string, ?Array<[string, number]>]> = [];
  for (let n = next(), i = 0; i < n; i++) {
    const path = pathList[next()];
    importNodes.push([path, next() === 1 ? counts() : null]);
  }
  const removedContexts = paths();
  const contexts = (): Array<ContextEntry> => {
    const n = next();
    const list = [];
    for (let i = 0; i < n; i++) {
      const path = pathList[next()];
      const kind = next();
      const module = next();
      const key = next();
      list.push({
        path,
        fromTransform: kind === 1,
        module: pathList[module] ?? '',
        key: keyList[key] ?? '',
      });
    }
    return list;
  };
  const contextsInPlace = contexts();
  const contextsAppended = contexts();
  outcome.added = paths();
  outcome.deleted = paths();
  outcome.modified = paths();

  // 2. Build the objects, before anything is changed: they take their
  // dependencies from the modules that are in the graph now.
  const dependencies: Map<string, Module<T>> = view.dependencies;
  const build = (d: Descriptor): Loose => {
    const original: Loose = dependencies.get(d.path);
    const transform: ?ModuleData<T> = updated.get(d.path);
    const map = new Map<string, Dependency>();
    for (const [key, fromTransform] of d.edges) {
      const dependency = fromTransform
        ? transform?.dependencies.get(key)
        : original?.dependencies.get(key);
      if (dependency == null) {
        throw new Error(
          `The Rust graph (delta-core) refers to a dependency of ${d.path} that does not exist (key ${key})`,
        );
      }
      map.set(key, dependency);
    }
    if (d.kind === 1) {
      return {module: original, map};
    }
    const inverseDependencies = d.inverseIsOriginal
      ? original.inverseDependencies
      : countingSet(nullToEmpty(d.inverse));
    const base = d.inverseIsOriginal
      ? original
      : {inverseDependencies, path: d.path};
    let fields;
    if (d.fromTransform) {
      // eslint-disable-next-line no-unused-vars
      const {
        dependencies: _d,
        resolvedContexts: _c,
        ...rest
      } = nullthrows(transform);
      fields = rest;
    } else {
      fields = {
        getSource: original.getSource,
        output: original.output,
        unstable_transformResultKey: original.unstable_transformResultKey,
      };
    }
    return {module: {...base, ...fields, dependencies: map}};
  };
  const built = new Map<string, Loose>();
  for (const d of inPlace) {
    if (d.kind !== 0) {
      built.set(d.path, build(d));
    }
  }
  const builtFresh: Array<Loose> = [];
  for (const d of fresh) {
    builtFresh.push(
      d.kind === 0 ? {module: dependencies.get(d.path)} : build(d),
    );
  }
  const contextValue = (c: ContextEntry): RequireContext =>
    nullthrows(
      c.fromTransform
        ? updated.get(c.module)?.resolvedContexts.get(c.key)
        : view.resolvedContexts.get(c.path),
    );
  const inPlaceContexts = contextsInPlace.map(c => [c.path, contextValue(c)]);
  const appendedContexts = contextsAppended.map(c => [c.path, contextValue(c)]);

  // 3. Apply.
  for (const path of removed) {
    dependencies.delete(path);
  }
  for (const d of inPlace) {
    const b = built.get(d.path);
    if (b == null) {
      continue;
    }
    if (d.kind === 1) {
      replaceContents(b.module.dependencies, b.map);
    } else {
      dependencies.set(d.path, b.module);
    }
  }
  fresh.forEach((d, i) => {
    const b = builtFresh[i];
    if (d.kind === 1) {
      replaceContents(b.module.dependencies, b.map);
    }
    dependencies.set(d.path, b.module);
  });
  for (const [owner, content] of inverseChanges) {
    const module = nullthrows(dependencies.get(owner));
    fillCountingSet(module.inverseDependencies, content);
  }
  if (importCleared) {
    view.importBundleNodes.clear();
  }
  for (const [path, content] of importNodes) {
    if (content == null) {
      view.importBundleNodes.delete(path);
      continue;
    }
    let node = view.importBundleNodes.get(path);
    if (node == null) {
      node = {inverseDependencies: new CountingSet()};
      view.importBundleNodes.set(path, node);
    }
    fillCountingSet(node.inverseDependencies, content);
  }
  for (const path of removedContexts) {
    view.resolvedContexts.delete(path);
  }
  for (const [path, context] of inPlaceContexts) {
    view.resolvedContexts.set(path, context);
  }
  for (const [path, context] of appendedContexts) {
    view.resolvedContexts.set(path, context);
  }
  return outcome;
}

function nullthrows<V>(value: ?V): V {
  if (value == null) {
    throw new Error('Got unexpected null or undefined');
  }
  return value;
}

function nullToEmpty(value: ?Array<[string, number]>): Array<[string, number]> {
  return value ?? [];
}

function countingSet(content: Array<[string, number]>): CountingSet<string> {
  const set = new CountingSet<string>();
  fillCountingSet(set, content);
  return set;
}

function fillCountingSet(
  set: CountingSet<string>,
  content: Array<[string, number]>,
): void {
  set.clear();
  for (const [item, count] of content) {
    for (let i = 0; i < count; i++) {
      set.add(item);
    }
  }
}

function replaceContents<K, V>(target: Map<K, V>, source: Map<K, V>): void {
  target.clear();
  for (const [k, v] of source) {
    target.set(k, v);
  }
}

// ------------------------------------------------------------ verify

function describe(value: unknown): string {
  const text = JSON.stringify(value);
  return text != null && text.length > 400
    ? text.slice(0, 400) + '...'
    : String(text);
}

function fail(what: string, detail: string): empty {
  throw new Error(`METRO_RUST_VERIFY=delta-core: ${what} differs.\n${detail}`);
}

function sameList(
  what: string,
  a: ReadonlyArray<unknown>,
  b: ReadonlyArray<unknown>,
): void {
  const n = Math.max(a.length, b.length);
  for (let i = 0; i < n; i++) {
    if (a[i] !== b[i]) {
      fail(
        what,
        `  first difference at index ${i} (Rust has ${a.length} entries, JS has ${b.length})\n  Rust: ${describe(a[i])}\n  JS:   ${describe(b[i])}`,
      );
    }
  }
}

function inverseList(set: CountingSet<string>): Array<unknown> {
  return [...set].map(item => `${item} x${set.count(item)}`);
}

/**
 * METRO_RUST_VERIFY=delta-core: the graph the Rust topology produced must be
 * the graph the JavaScript implementation produced from the same inputs, down
 * to the order of every map and the identity of every object they hold.
 */
export function compareGraphs<T>(
  rust: GraphView<T>,
  js: GraphView<T>,
  rustResult: ?RustOutcome,
  jsResult: ?{
    added: Array<string>,
    deleted: Array<string>,
    modified: Array<string>,
  },
): void {
  sameList(
    'graph.dependencies keys (or their order)',
    [...rust.dependencies.keys()],
    [...js.dependencies.keys()],
  );
  for (const [path, a] of rust.dependencies) {
    const b = nullthrows(js.dependencies.get(path));
    const where = `module ${path}`;
    sameList(`${where}: properties`, Object.keys(a), Object.keys(b));
    for (const prop of Object.keys(a)) {
      if (prop === 'dependencies' || prop === 'inverseDependencies') {
        continue;
      }
      // $FlowFixMe[prop-missing]
      if (a[prop] !== b[prop]) {
        fail(
          `${where}: property ${prop}`,
          '  the values are not the same object',
        );
      }
    }
    sameList(
      `${where}: dependency keys (or their order)`,
      [...a.dependencies.keys()],
      [...b.dependencies.keys()],
    );
    for (const [key, dependency] of a.dependencies) {
      if (dependency !== b.dependencies.get(key)) {
        fail(
          `${where}: dependency ${key}`,
          `  Rust: ${describe(dependency.absolutePath)}\n  JS:   ${describe(b.dependencies.get(key)?.absolutePath)} (not the same object, or different)`,
        );
      }
    }
    sameList(
      `${where}: inverse dependencies (or their order, or counts)`,
      inverseList(a.inverseDependencies),
      inverseList(b.inverseDependencies),
    );
  }
  sameList(
    'resolved contexts (keys or their order)',
    [...rust.resolvedContexts.keys()],
    [...js.resolvedContexts.keys()],
  );
  for (const [path, context] of rust.resolvedContexts) {
    if (context !== js.resolvedContexts.get(path)) {
      fail(`resolved context of ${path}`, '  not the same object');
    }
  }
  sameList(
    'import bundle nodes',
    [...rust.importBundleNodes.keys()].sort(),
    [...js.importBundleNodes.keys()].sort(),
  );
  for (const [path, node] of rust.importBundleNodes) {
    sameList(
      `import bundle node ${path}`,
      inverseList(node.inverseDependencies),
      inverseList(
        nullthrows(js.importBundleNodes.get(path)).inverseDependencies,
      ),
    );
  }
  if (rustResult != null && jsResult != null) {
    sameList('the added modules', rustResult.added, jsResult.added);
    sameList('the modified modules', rustResult.modified, jsResult.modified);
    sameList('the deleted modules', rustResult.deleted, jsResult.deleted);
  }
}
