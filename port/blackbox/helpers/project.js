/**
 * Black-box test helpers for the Metro → Rust port.
 *
 * Everything here goes through Metro's public API and a real temporary
 * directory, so the same tests can run unchanged against a Rust-backed Metro.
 */
'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const REPO_ROOT = path.resolve(__dirname, '..', '..', '..');
const PACKAGES = path.join(REPO_ROOT, 'packages');
const Metro = require(path.join(PACKAGES, 'metro'));

/**
 * Creates a throwaway project on disk. `files` maps relative paths to contents.
 * Returns helpers to edit files and to load a Metro config pointing at it.
 */
function createProject(files = {}) {
  const root = fs.realpathSync(
    fs.mkdtempSync(path.join(os.tmpdir(), 'metro-bb-')),
  );
  const write = (rel, content) => {
    const abs = path.join(root, rel);
    fs.mkdirSync(path.dirname(abs), {recursive: true});
    fs.writeFileSync(abs, content);
    return abs;
  };
  for (const [rel, content] of Object.entries(files)) {
    write(rel, content);
  }
  return {
    root,
    write,
    read: rel => fs.readFileSync(path.join(root, rel), 'utf8'),
    exists: rel => fs.existsSync(path.join(root, rel)),
    remove: rel => fs.rmSync(path.join(root, rel), {recursive: true, force: true}),
    cleanup: () => fs.rmSync(root, {recursive: true, force: true}),
    /** Writes metro.config.js equivalent to loadConfig()'s base, for CLI tests. */
    writeConfigFile(extra = '') {
      return write(
        'metro.config.js',
        `const path = require('path');
module.exports = {
  projectRoot: __dirname,
  watchFolders: [__dirname, ${JSON.stringify(PACKAGES)}],
  cacheStores: [],
  maxWorkers: 1,
  resolver: {useWatchman: false},
  transformer: {
    babelTransformerPath: ${JSON.stringify(require.resolve('@react-native/metro-babel-transformer'))},
    enableBabelRCLookup: false,
    enableBabelRuntime: false,
    asyncRequireModulePath: ${JSON.stringify(require.resolve('metro-runtime/src/modules/asyncRequire'))},
  },
  ${extra}
};
`,
      );
    },
    /** Loads a Metro config for this project; `overrides` is merged on top. */
    async loadConfig(overrides = {}) {
      const base = {
        projectRoot: root,
        // metro-runtime's polyfills and async-require must be inside watchFolders.
        watchFolders: [root, PACKAGES],
        cacheStores: [],
        maxWorkers: 1,
        reporter: {update() {}},
        server: {port: 0},
        resolver: {useWatchman: false},
        transformer: {
          babelTransformerPath: require.resolve(
            '@react-native/metro-babel-transformer',
          ),
          enableBabelRCLookup: false,
          enableBabelRuntime: false,
          asyncRequireModulePath: require.resolve(
            'metro-runtime/src/modules/asyncRequire',
          ),
        },
      };
      return Metro.mergeConfig(
        await Metro.loadConfig({config: write('.metro-bb.config.js', 'module.exports = {};')}, base),
        overrides,
      );
    },
  };
}

/** Starts a dev server; returns {url, port, close}. */
async function startServer(config, opts = {}) {
  let onClosed;
  const closed = new Promise(resolve => (onClosed = resolve));
  const {httpServer} = await Metro.runServer(config, {
    reporter: {update() {}},
    onClose: () => onClosed(),
    ...opts,
  });
  const port = httpServer.address().port;
  return {
    port,
    url: p => `http://localhost:${port}${p}`,
    async close() {
      httpServer.close();
      await closed;
    },
  };
}

/** fetch() that does not reuse connections (nodejs/node#54484). */
function fetchFresh(url, init = {}) {
  return fetch(url, {...init, headers: {Connection: 'close', ...init.headers}});
}

/** Runs a bundle in a fresh VM context and returns that context. */
function execBundle(code, context = {}) {
  const vm = require('node:vm');
  vm.runInNewContext(code, context);
  return context;
}

/** Polls `fn` until it returns a truthy value or the timeout elapses. */
async function waitFor(fn, {timeout = 20000, interval = 100, label = 'condition'} = {}) {
  const start = Date.now();
  let lastError;
  while (Date.now() - start < timeout) {
    try {
      const v = await fn();
      if (v) {
        return v;
      }
    } catch (e) {
      lastError = e;
    }
    await new Promise(r => setTimeout(r, interval));
  }
  throw new Error(`Timed out waiting for ${label}${lastError ? `: ${lastError.message}` : ''}`);
}

module.exports = {Metro, REPO_ROOT, PACKAGES, createProject, startServer, fetchFresh, execBundle, waitFor};
