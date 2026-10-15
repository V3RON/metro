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

import {getRustBinding, isRustEnabled} from 'metro-rust-switch';
import fs from 'node:fs';
import path from 'node:path';

const NULL_BYTE = 0x00;
const NULL_BYTE_BUFFER = Buffer.from([NULL_BYTE]);

export type Options = Readonly<{
  root: string,
}>;

export default class FileStore<T> {
  readonly #root: string;

  constructor(options: Options) {
    this.#root = options.root;
  }

  async get(key: Buffer): Promise<?T> {
    try {
      const data = await readEntry(this.#getFilePath(key));

      if (data[0] === NULL_BYTE) {
        return data.slice(1) as any;
      }

      return JSON.parse(data.toString('utf8'));
    } catch (err) {
      if (err.code === 'ENOENT' || err instanceof SyntaxError) {
        return null;
      }

      throw err;
    }
  }

  async set(key: Buffer, value: T): Promise<void> {
    const filePath = this.#getFilePath(key);
    try {
      await this.#set(filePath, value);
    } catch (err) {
      if (err.code === 'ENOENT') {
        fs.mkdirSync(path.dirname(filePath), {recursive: true});
        await this.#set(filePath, value);
      } else {
        throw err;
      }
    }
  }

  async #set(filePath: string, value: T): Promise<void> {
    let content;
    if (value instanceof Buffer) {
      content = Buffer.concat([NULL_BYTE_BUFFER, value]);
    } else {
      content = JSON.stringify(value) ?? JSON.stringify(null);
    }
    await fs.promises.writeFile(filePath, content);
  }

  clear() {
    this.#removeDirs();
  }

  #getFilePath(key: Buffer): string {
    return path.join(
      this.#root,
      key.slice(0, 1).toString('hex'),
      key.slice(1).toString('hex'),
    );
  }

  #removeDirs() {
    for (let i = 0; i < 256; i++) {
      fs.rmSync(path.join(this.#root, ('0' + i.toString(16)).slice(-2)), {
        force: true,
        recursive: true,
      });
    }
  }
}

type TransformCacheBinding = {
  transformCacheRead(
    filePath: string,
  ): Promise<Buffer | null | {errorCode?: string, errorMessage: string}>,
  ...
};

let lastMetroRust: ?string;
let lastBinding: ?TransformCacheBinding = null;
let lastVerify: ?string;
let lastVerifyEnabled = false;

/**
 * The `transform-cache` component of the Rust port (METRO_RUST): read an entry
 * with one read on a Rust worker thread (port/design/phase-4.md). Resolves to
 * the same Buffer `fs.promises.readFile` would, and rejects with an Error
 * carrying the same `code` and message. With the component off, it is
 * `fs.promises.readFile`.
 */
async function readEntry(filePath: string): Promise<Buffer> {
  const metroRust = process.env.METRO_RUST;
  if (metroRust !== lastMetroRust) {
    // $FlowFixMe[incompatible-type] The binding exports more than version().
    const binding: ?TransformCacheBinding = getRustBinding('transform-cache');
    lastBinding = binding;
    lastMetroRust = metroRust;
  }
  const binding = lastBinding;
  if (binding == null) {
    return fs.promises.readFile(filePath);
  }
  const result: any = await binding.transformCacheRead(filePath);
  if (result == null) {
    const error: any = new Error(
      `ENOENT: no such file or directory, open '${filePath}'`,
    );
    error.code = 'ENOENT';
    throw error;
  }
  if (!Buffer.isBuffer(result)) {
    const error: any = new Error(result.errorMessage);
    error.code = result.errorCode;
    throw error;
  }
  const data: Buffer = result;
  if (isVerifyEnabled()) {
    const expected = await fs.promises.readFile(filePath);
    if (!expected.equals(data)) {
      throw new Error(
        `METRO_RUST_VERIFY: transform-cache read of ${filePath} differs from fs.promises.readFile`,
      );
    }
  }
  return data;
}

function isVerifyEnabled(): boolean {
  const verify = process.env.METRO_RUST_VERIFY;
  if (verify !== lastVerify) {
    lastVerifyEnabled =
      verify != null &&
      verify !== '' &&
      isRustEnabled('transform-cache', null, {METRO_RUST: verify});
    lastVerify = verify;
  }
  return lastVerifyEnabled;
}
