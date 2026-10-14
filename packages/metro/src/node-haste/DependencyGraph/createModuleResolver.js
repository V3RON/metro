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

import type {PackageCache} from '../PackageCache';
import type {FileStoreHolder} from 'metro-resolver/private/rust';
import type {ConfigT} from 'metro-config';
import type {FileSystem, HasteMap} from 'metro-file-map';
import type {FileSystemLookup} from 'metro-resolver';

import getSchemeResolvers from '../../lib/getSchemeResolvers';
import {ModuleResolver} from './ModuleResolution';
import {FILE_STORE} from 'metro-resolver/private/rust';
import path from 'node:path';

export type CreateModuleResolverOptions = Readonly<{
  config: ConfigT,
  fileSystem: FileSystem,
  hasteMap: HasteMap,
  packageCache: PackageCache,
  // The Rust store that answers the callbacks natively, when the resolver
  // runs in Rust (METRO_RUST / unstable_rust): the replica of `fileSystem` and
  // `packageCache` (RustFileStore), or `fileSystem` itself when the file map
  // is in Rust (RustFileSystem).
  rustFileStore?: ?FileStoreHolder,
  // Whether `hasteMap` is the Rust file map's own, so that the Rust resolver
  // answers Haste natively too.
  nativeHaste?: boolean,
}>;

export default function createModuleResolver({
  config,
  fileSystem,
  hasteMap,
  packageCache,
  rustFileStore,
  nativeHaste,
}: CreateModuleResolverOptions): ModuleResolver {
  const fileSystemLookup = (filePath: string): ReturnType<FileSystemLookup> => {
    const result = fileSystem.lookup(filePath);
    if (result.exists) {
      return {
        exists: true,
        realPath: result.realPath,
        type: result.type,
      };
    }
    return {exists: false};
  };
  const doesFileExist = (filePath: string) => fileSystem.exists(filePath);
  const getPackage = (packageJsonPath: string) => {
    try {
      return packageCache.getPackage(packageJsonPath).packageJson ?? null;
    } catch {
      // Non-existence or malformed JSON, we treat both as non-existent
      return null;
    }
  };
  const getPackageForModule = (absolutePath: string) =>
    packageCache.getPackageForModule(absolutePath);

  if (rustFileStore != null) {
    // The Rust resolver answers these natively from the store, and calls
    // them only when the store defers (see metro-resolver/src/rust.js).
    for (const callback of [
      fileSystemLookup,
      doesFileExist,
      getPackage,
      getPackageForModule,
    ]) {
      // $FlowFixMe[invalid-computed-prop]
      // $FlowFixMe[prop-missing]
      callback[FILE_STORE] = rustFileStore;
    }
  }

  const getHasteModulePath = (name: string, platform: ?string) =>
    hasteMap.getModule(name, platform, true);
  const getHastePackagePath = (name: string, platform: ?string) =>
    hasteMap.getPackage(name, platform, true);
  if (rustFileStore != null && nativeHaste === true) {
    for (const callback of [getHasteModulePath, getHastePackagePath]) {
      // $FlowFixMe[invalid-computed-prop]
      // $FlowFixMe[prop-missing]
      callback[FILE_STORE] = rustFileStore;
    }
  }

  return new ModuleResolver({
    assetExts: new Set(config.resolver.assetExts),
    disableHierarchicalLookup: config.resolver.disableHierarchicalLookup,
    doesFileExist,
    emptyModulePath: config.resolver.emptyModulePath,
    extraNodeModules: config.resolver.extraNodeModules,
    fileSystemLookup,
    getHasteModulePath,
    getHastePackagePath,
    getPackage,
    getPackageForModule,
    mainFields: config.resolver.resolverMainFields,
    nodeModulesPaths: config.resolver.nodeModulesPaths,
    preferNativePlatform: true,
    projectRoot: config.projectRoot,
    reporter: config.reporter,
    resolveAsset: (dirPath: string, assetName: string, extension: string) => {
      const basePath = dirPath + path.sep + assetName;
      const assets = [
        basePath + extension,
        ...config.resolver.assetResolutions.map(
          resolution => basePath + '@' + resolution + 'x' + extension,
        ),
      ]
        .map(assetPath => {
          const result = fileSystemLookup(assetPath);
          // A directory such as `node_modules/lodash.zip` is not an asset, and
          // must fall through to directory (package) resolution.
          return result.exists && result.type === 'f' ? result.realPath : null;
        })
        .filter(Boolean);

      return assets.length ? assets : null;
    },
    resolveRequest: config.resolver.resolveRequest,
    schemeResolvers: {
      ...getSchemeResolvers(),
      ...config.resolver.schemeResolvers,
    },
    sourceExts: config.resolver.sourceExts,
    unstable_conditionNames: config.resolver.unstable_conditionNames,
    unstable_conditionsByPlatform:
      config.resolver.unstable_conditionsByPlatform,
    unstable_enablePackageExports:
      config.resolver.unstable_enablePackageExports,
    unstable_incrementalResolution:
      config.resolver.unstable_incrementalResolution,
  });
}
