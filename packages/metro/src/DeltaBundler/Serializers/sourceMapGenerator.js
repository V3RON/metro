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

import type {Module} from '../types';

import getSourceMapInfo from './helpers/getSourceMapInfo';
import {isJsModule} from './helpers/js';
import {fromRawMappingsIndexed} from 'metro-source-map';

export type SourceMapGeneratorOptions = Readonly<{
  excludeSource: boolean,
  processModuleFilter: (module: Module<>) => boolean,
  shouldAddToIgnoreList: (module: Module<>) => boolean,
  getSourceUrl?: ?(module: Module<>) => string,
  // The `unstable_rust` option of Metro config, for the `serializers`
  // component, if the caller has the config (see rustSourceMap.js).
  unstable_rust?: ?{[string]: boolean, ...},
}>;

function getSourceMapInfosImpl<T>(
  isBlocking: boolean,
  onDone: (ReadonlyArray<T>) => void,
  modules: ReadonlyArray<Module<>>,
  options: SourceMapGeneratorOptions,
  getInfo: (Module<>) => T,
): void {
  const sourceMapInfos: Array<T> = [];
  const modulesToProcess = modules
    .filter(isJsModule)
    .filter(options.processModuleFilter);

  function processNextModule() {
    if (modulesToProcess.length === 0) {
      return true;
    }

    const mod = modulesToProcess.shift();
    // $FlowFixMe[incompatible-type]
    const info = getInfo(mod);
    sourceMapInfos.push(info);
    return false;
  }

  function workLoop() {
    const time = process.hrtime();
    while (true) {
      const isDone = processNextModule();
      if (isDone) {
        onDone(sourceMapInfos);
        break;
      }
      if (!isBlocking) {
        // Keep the loop running but try to avoid blocking
        // for too long because this is not in a worker yet.
        const diff = process.hrtime(time);
        const NS_IN_MS = 1000000;
        if (diff[1] > 50 * NS_IN_MS) {
          // We've blocked for more than 50ms.
          // This code currently runs on the main thread,
          // so let's give Metro an opportunity to handle requests.
          setImmediate(workLoop);
          break;
        }
      }
    }
  }
  workLoop();
}

function getInfoOf(
  options: SourceMapGeneratorOptions,
): (Module<>) => ReturnType<typeof getSourceMapInfo> {
  return mod =>
    getSourceMapInfo(mod, {
      excludeSource: options.excludeSource,
      shouldAddToIgnoreList: options.shouldAddToIgnoreList,
      getSourceUrl: options.getSourceUrl,
    });
}

function sourceMapGenerator(
  modules: ReadonlyArray<Module<>>,
  options: SourceMapGeneratorOptions,
): ReturnType<typeof fromRawMappingsIndexed> {
  let sourceMapInfos;
  getSourceMapInfosImpl(
    true,
    infos => {
      sourceMapInfos = infos;
    },
    modules,
    options,
    getInfoOf(options),
  );
  if (sourceMapInfos == null) {
    throw new Error(
      'Expected getSourceMapInfosImpl() to finish synchronously.',
    );
  }
  return fromRawMappingsIndexed(sourceMapInfos);
}

async function sourceMapGeneratorNonBlocking(
  modules: ReadonlyArray<Module<>>,
  options: SourceMapGeneratorOptions,
): Promise<ReturnType<typeof fromRawMappingsIndexed>> {
  const sourceMapInfos = await new Promise<
    ReadonlyArray<ReturnType<typeof getSourceMapInfo>>,
  >(resolve => {
    getSourceMapInfosImpl(false, resolve, modules, options, getInfoOf(options));
  });
  // Building an index map is a cheap synchronous passthrough (VLQ maps carry
  // over verbatim); only gathering the per-module info above needs to yield.
  return fromRawMappingsIndexed(sourceMapInfos);
}

export {
  getSourceMapInfosImpl,
  sourceMapGenerator,
  sourceMapGeneratorNonBlocking,
};
