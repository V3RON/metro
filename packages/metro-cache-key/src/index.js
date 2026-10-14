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
import crypto from 'node:crypto';
import fs from 'node:fs';

export function getCacheKey(files: ReadonlyArray<string>): string {
  const binding = getRustBinding('cache-key');
  if (binding != null) {
    // $FlowFixMe[prop-missing] The binding exports more than version().
    const {key, errorIndex} = binding.getCacheKey(files);
    if (errorIndex === -1) {
      return key;
    }
    // A file could not be read: read it here so that the exception is Node's.
    fs.readFileSync(files[errorIndex]);
  }
  return getCacheKeyJs(files);
}

function getCacheKeyJs(files: ReadonlyArray<string>): string {
  return files
    .reduce(
      (hash, file) => hash.update('\0', 'utf8').update(fs.readFileSync(file)),
      crypto.createHash('md5'),
    )
    .digest('hex');
}
