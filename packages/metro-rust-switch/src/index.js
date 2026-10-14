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
 * Decides, per component, whether Metro runs its JavaScript implementation or
 * the Rust one (`metro-rs`). Components call `isRustEnabled` or
 * `getRustBinding` at their public boundary.
 *
 * Sources, strongest first:
 *   1. The `METRO_RUST` environment variable: `off`, `all`, or a comma
 *      separated list of component ids. When it is set (and not empty) it
 *      decides for every component: components that are not listed are off.
 *   2. `unstable_rust` in Metro config, `{[componentId]: boolean}`, passed in
 *      by callers that have a config.
 *   3. Otherwise JavaScript.
 *
 * The native module is only loaded once a component is actually switched on,
 * and a failure to load it is an error, never a silent fallback to JavaScript.
 */

/**
 * Every component that can be ported. Keep in sync with the `components` of
 * all phases in port/phases.json (checked by this package's tests).
 */
export const COMPONENT_IDS: ReadonlyArray<string> = [
  'ob1',
  'source-map',
  'cache-key',
  'core',
  'resolver',
  'file-map',
  'node-haste',
  'cache',
  'delta-core',
  'incremental',
  'serializers',
  'output',
  'assets',
  'hmr',
  'server',
  'lib',
  'module-graph',
  'transform-plugins',
  'transform-worker',
  'minify',
];

export type RustConfig = ?Readonly<{[componentId: string]: boolean}>;

export type RustBinding = {
  version(): string,
  ...
};

type Env = Readonly<{[string]: ?string}>;

function assertKnown(ids: ReadonlyArray<string>, source: string): void {
  const unknown = ids.filter(id => !COMPONENT_IDS.includes(id));
  if (unknown.length > 0) {
    throw new Error(
      `Unknown Rust component ${unknown.map(id => `"${id}"`).join(', ')} in ${source}. ` +
        `Valid component ids: ${COMPONENT_IDS.join(', ')}.`,
    );
  }
}

/**
 * Parses a METRO_RUST value. Returns null when the variable does not decide
 * (unset or empty), otherwise the set of components that are on.
 */
export function parseMetroRust(value: ?string): ?ReadonlySet<string> {
  const trimmed = value?.trim();
  if (trimmed == null || trimmed === '') {
    return null;
  }
  if (trimmed === 'off') {
    return new Set();
  }
  if (trimmed === 'all') {
    return new Set(COMPONENT_IDS);
  }
  const ids = trimmed
    .split(',')
    .map(id => id.trim())
    .filter(id => id !== '');
  if (ids.includes('off') || ids.includes('all')) {
    throw new Error(
      `Invalid METRO_RUST="${trimmed}": "off" and "all" cannot be combined with component ids.`,
    );
  }
  assertKnown(ids, 'METRO_RUST');
  return new Set(ids);
}

/**
 * Whether `componentId` should run in Rust. `config` is the `unstable_rust`
 * option of Metro config, if the caller has one. Does not load the binding.
 */
export function isRustEnabled(
  componentId: string,
  config?: RustConfig,
  env: Env = process.env,
): boolean {
  assertKnown([componentId], 'the requested component');
  const fromEnv = parseMetroRust(env.METRO_RUST);
  if (fromEnv != null) {
    return fromEnv.has(componentId);
  }
  if (config != null) {
    assertKnown(Object.keys(config), 'config.unstable_rust');
    return config[componentId] === true;
  }
  return false;
}

let cachedBinding: ?RustBinding;

/**
 * Loads `metro-rs` (once). Throws if it cannot be loaded; a Rust switch that
 * quietly ran JavaScript would make benchmarks and tests meaningless.
 */
export function loadRustBinding(): RustBinding {
  if (cachedBinding != null) {
    return cachedBinding;
  }
  let binding;
  try {
    // metro-rs is a private native package, linked at the repo root and not a
    // dependency of this published package.
    // $FlowFixMe[untyped-import]
    binding = require('metro-rs'); // eslint-disable-line import/no-extraneous-dependencies
  } catch (error) {
    throw new Error(
      `Metro was asked to use its Rust implementation but the "metro-rs" native module could not be loaded: ${error.message}\n` +
        'Build it with `yarn --cwd metro-rs build`, or turn Rust off with METRO_RUST=off.',
      {cause: error},
    );
  }
  if (binding == null || typeof binding.version !== 'function') {
    throw new Error(
      'The "metro-rs" native module loaded but does not export version(); rebuild it with `yarn --cwd metro-rs build`.',
    );
  }
  cachedBinding = binding;
  return binding;
}

/**
 * The binding if `componentId` is switched on, otherwise null (use the
 * JavaScript implementation). Throws if it is on and the binding is missing.
 */
export function getRustBinding(
  componentId: string,
  config?: RustConfig,
  env?: Env,
): ?RustBinding {
  return isRustEnabled(componentId, config, env) ? loadRustBinding() : null;
}
