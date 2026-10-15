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
 * The bundle part of the `serializers` component of the Rust port: the bytes
 * of `bundleToString(baseJSBundle(...))`, assembled in Rust and returned as a
 * `Buffer`, so that the dev server never makes a JavaScript string of the
 * whole bundle (RULES.md section 7).
 *
 * JavaScript still decides everything that user hooks can influence, in the
 * order `baseJSBundle` does (module ids, the module filter, the run-module
 * statements, the polyfills), and passes the results in. Rust holds each
 * distinct module code once, keyed by a token, and one call per bundle sends
 * only the codes it does not hold yet, the token and `__d(...)` parameters of
 * every module in id order, and the small pre and post parts (rules 1 and 2).
 */

import type {Module, ReadOnlyGraph, SerializerOptions} from '../types';

import bundleToString from '../../lib/bundleToString';
import getAppendScripts from '../../lib/getAppendScripts';
import baseJSBundle from './baseJSBundle';
import {
  getJsOutput,
  getModuleParams,
  isJsModule,
  wrapModule,
} from './helpers/js';
import processModules from './helpers/processModules';
import {isRustEnabled, loadRustBinding} from 'metro-rust-switch';

// The binding is not typed.
// flowlint-next-line unclear-type:off
type Loose = any;

type RustConfig = Readonly<{
  serializer: Readonly<{
    customSerializer?: ?unknown,
    unstable_inlineDependencyMap?: boolean,
    ...
  }>,
  unstable_rust?: ?{[string]: boolean, ...},
  ...
}>;

/** What goes in `tokens` for "the next literal". */
const LITERAL = 0xffffffff;

type GraphState = {
  assembler: Loose,
  // Code string -> token, for the codes Rust holds after the last call.
  tokens: Map<string, number>,
};

const states: WeakMap<interface {}, GraphState> = new WeakMap();
// Tokens are never reused within a process.
let lastToken = 0;

function nextToken(): number {
  lastToken++;
  if (lastToken === LITERAL) {
    lastToken++;
  }
  return lastToken;
}

/**
 * Whether the dev server's bundle responses are assembled in Rust: the
 * `serializers` component is on, and nothing replaces the default serializer
 * or changes how modules are wrapped.
 */
export function isRustBundleEnabled(config: RustConfig): boolean {
  return (
    isRustEnabled('serializers', config.unstable_rust) &&
    config.serializer.customSerializer == null &&
    config.serializer.unstable_inlineDependencyMap !== true
  );
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

/**
 * `Buffer.from(bundleToString(baseJSBundle(...)).code, 'utf8')` and the
 * length of that string, with the same calls to the hooks in `options`, in the
 * same order.
 */
export function rustBundle(
  entryPoint: string,
  preModules: ReadonlyArray<Module<>>,
  graph: ReadOnlyGraph<>,
  options: SerializerOptions,
): {buffer: Buffer, utf16Length: number} {
  const result = assembleBundle(entryPoint, preModules, graph, options);
  if (isVerifyEnabled()) {
    verify(entryPoint, preModules, graph, options, result);
  }
  return result;
}

function assembleBundle(
  entryPoint: string,
  preModules: ReadonlyArray<Module<>>,
  graph: ReadOnlyGraph<>,
  options: SerializerOptions,
): {buffer: Buffer, utf16Length: number} {
  // This follows baseJSBundle step by step: the default createModuleId gives
  // out ids on first use, so the order of these calls can be seen.
  const graphModules = [...graph.dependencies.values()];
  for (const module of graphModules) {
    options.createModuleId(module.path);
  }

  const processModulesOptions = {
    filter: options.processModuleFilter,
    createModuleId: options.createModuleId,
    dev: options.dev,
    includeAsyncPaths: options.includeAsyncPaths,
    projectRoot: options.projectRoot,
    serverRoot: options.serverRoot,
    sourceUrl: options.sourceUrl,
    dependencyMapReservedName: options.dependencyMapReservedName,
    unstable_inlineDependencyMap: options.unstable_inlineDependencyMap,
    unstable_getAsyncDependencyPath: options.unstable_getAsyncDependencyPath,
  };

  // Do not prepend polyfills or the require runtime when only modules are
  // requested.
  const pre = options.modulesOnly ? [] : preModules;

  const preCode = processModules(pre, processModulesOptions)
    .map(([_, code]) => code)
    .join('\n');

  const sortedModules = graphModules
    .slice()
    .sort(
      (a, b) => options.createModuleId(a.path) - options.createModuleId(b.path),
    );

  const postCode = processModules(
    getAppendScripts(entryPoint, [...pre, ...sortedModules], {
      asyncRequireModulePath: options.asyncRequireModulePath,
      createModuleId: options.createModuleId,
      getRunModuleStatement: options.getRunModuleStatement,
      globalPrefix: options.globalPrefix,
      inlineSourceMap: options.inlineSourceMap,
      runBeforeMainModule: options.runBeforeMainModule,
      runModule: options.runModule,
      shouldAddToIgnoreList: options.shouldAddToIgnoreList,
      sourceMapUrl: options.sourceMapUrl,
      sourceUrl: options.sourceUrl,
      getSourceUrl: options.getSourceUrl,
    }),
    processModulesOptions,
  )
    .map(([_, code]) => code)
    .join('\n');

  // processModules over the graph: filter all, then wrap each.
  const included = graphModules
    .filter(isJsModule)
    .filter(options.processModuleFilter ?? (() => true));

  let state = states.get(graph);
  if (state == null) {
    state = {
      assembler: new (loadRustBinding() as Loose).BundleAssembler(),
      tokens: new Map(),
    };
    states.set(graph, state);
  }
  const previous = state.tokens;
  const next: Map<string, number> = new Map();
  const newTokens: Array<number> = [];
  const newCodes: Array<string> = [];

  function tokenFor(code: string): number {
    let token = next.get(code);
    if (token == null) {
      token = previous.get(code);
      if (token == null) {
        token = nextToken();
        newTokens.push(token);
        newCodes.push(code);
      }
      next.set(code, token);
    }
    return token;
  }

  const entries: Array<{
    id: number,
    token: number,
    params: string,
    literal: string,
  }> = [];
  for (const module of included) {
    const output = getJsOutput(module);
    const code = output.data.code;
    let params = '';
    let literal = '';
    let token;
    if (output.type.startsWith('js/script')) {
      token = tokenFor(code);
    } else if (code.lastIndexOf(')') === -1) {
      // There is no call to add parameters to; wrapModule does what it does.
      token = LITERAL;
      literal = wrapModule(module, processModulesOptions);
    } else {
      params = getModuleParams(module, processModulesOptions)
        .map(param =>
          // Distinguish between `undefined` and `null`, as
          // addParamsToDefineCall does: `undefined` is not JSON.
          // eslint-disable-next-line lint/strictly-null
          param !== undefined ? JSON.stringify(param) : 'undefined',
        )
        .join(',');
      token = tokenFor(code);
    }
    entries.push({
      id: options.createModuleId(module.path),
      token,
      params,
      literal,
    });
  }

  // bundleToString sorts the modules by id (the sort is stable).
  entries.sort((a, b) => a.id - b.id);
  const tokens = new Uint32Array(entries.length);
  const params: Array<string> = [];
  const literals: Array<string> = [];
  entries.forEach((entry, k) => {
    tokens[k] = entry.token;
    params.push(entry.params);
    if (entry.token === LITERAL) {
      literals.push(entry.literal);
    }
  });

  let assembled;
  try {
    assembled = state.assembler.assemble(
      preCode,
      postCode,
      tokens,
      params.join('\0'),
      Uint32Array.from(newTokens),
      newCodes,
      literals,
    );
  } catch (error) {
    // Rust may have evicted what it held; start again next time.
    states.delete(graph);
    throw error;
  }
  state.tokens = next;
  return {buffer: assembled.buffer, utf16Length: assembled.utf16Length};
}

function verify(
  entryPoint: string,
  preModules: ReadonlyArray<Module<>>,
  graph: ReadOnlyGraph<>,
  options: SerializerOptions,
  result: {buffer: Buffer, utf16Length: number},
): void {
  const js = bundleToString(
    baseJSBundle(entryPoint, preModules, graph, options),
  ).code;
  const expected = Buffer.from(js, 'utf8');
  const actual = result.buffer;
  if (!expected.equals(actual)) {
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
      `METRO_RUST_VERIFY=serializers: the bundle bytes differ at byte ${offset} ` +
        `(JavaScript ${expected.length} bytes, Rust ${actual.length} bytes).\n` +
        `JavaScript: ${context(expected)}\nRust:       ${context(actual)}`,
    );
  }
  if (js.length !== result.utf16Length) {
    throw new Error(
      'METRO_RUST_VERIFY=serializers: the bundle length differs ' +
        `(JavaScript ${js.length} UTF-16 units, Rust ${result.utf16Length}).`,
    );
  }
}
