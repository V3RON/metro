/**
 * Loads the native Metro core built by `yarn --cwd metro-rs build`.
 * Throws if the binary is missing; callers must never fall back silently.
 */
'use strict';

const path = require('node:path');

let binding;
try {
  binding = require(path.join(__dirname, 'metro-rs.node'));
} catch (error) {
  const wrapped = new Error(
    'metro-rs: the native binding could not be loaded. Run `yarn --cwd metro-rs build` ' +
      `to build it. (${error.message})`,
  );
  wrapped.cause = error;
  throw wrapped;
}

module.exports = binding;
