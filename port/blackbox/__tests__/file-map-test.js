/**
 * Black-box: metro-file-map on a real directory.
 *
 * Builds FileMap through its public constructor (the same options Metro's
 * createFileMap passes), with the Node crawler and the default watcher, and
 * asserts only on the returned FileSystem, Haste lookups, change events and
 * cache reuse. No fs mocks, so a Rust FileMap can be dropped in behind the
 * same API.
 */
'use strict';

const crypto = require('node:crypto');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {PACKAGES, createProject, waitFor} = require('../helpers/project');

const fileMapPkg = require(path.join(PACKAGES, 'metro-file-map'));
const FileMap = fileMapPkg.default;
const {DiskCacheManager, HastePlugin, HasteConflictsError} = fileMapPkg;

jest.useRealTimers();
jest.setTimeout(60000);

const sha1 = s => crypto.createHash('sha1').update(s).digest('hex');

function makeFileMap(root, {watch = false, cacheDir, haste = false, ignorePattern, extensions} = {}) {
  const plugins = [];
  let hastePlugin = null;
  if (haste) {
    hastePlugin = new HastePlugin({
      platforms: new Set(['ios', 'android', 'native']),
      hasteImplModulePath: path.join(root, 'hasteImpl.js'),
      enableHastePackages: true,
      rootDir: root,
      failValidationOnConflicts: true,
    });
    plugins.push(hastePlugin);
  }
  const fileMap = new FileMap({
    cacheManagerFactory: factoryParams =>
      new DiskCacheManager(factoryParams, {
        cacheDirectory: cacheDir ?? fs.mkdtempSync(path.join(os.tmpdir(), 'metro-bb-cache-')),
        cacheFilePrefix: 'bb',
      }),
    computeSha1: true,
    enableSymlinks: true,
    extensions: extensions ?? ['js', 'json', 'png'],
    healthCheck: {enabled: false, interval: 30000, timeout: 5000, filePrefix: '.metro-health-check'},
    ignorePattern: ignorePattern ?? /\/__ignored__\//,
    maxWorkers: 1,
    plugins,
    retainAllFiles: true,
    resetCache: false,
    rootDir: root,
    roots: [root],
    useWatchman: false,
    watch,
  });
  return {fileMap, hastePlugin};
}

const HASTE_IMPL = `
module.exports = {
  getCacheKey() {
    return 'bb-haste-v1';
  },
  getHasteName(filePath) {
    const m = /@haste ([A-Za-z0-9_]+)/.exec(require('fs').readFileSync(filePath, 'utf8'));
    return m ? m[1] : null;
  },
};`;

describe('metro-file-map on a real directory', () => {
  let project;
  const ends = [];
  afterEach(async () => {
    await Promise.all(ends.splice(0).map(fm => fm.end()));
    project?.cleanup();
  });
  const build = async (opts) => {
    const r = makeFileMap(project.root, opts);
    ends.push(r.fileMap);
    const {fileSystem} = await r.fileMap.build();
    return {...r, fileSystem};
  };
  const rel = files => files.map(f => path.relative(project.root, f).split(path.sep).join('/')).sort();

  test('crawls files with the configured extensions only', async () => {
    project = createProject({
      'a.js': 'a', 'dir/b.js': 'b', 'data.json': '{}', 'img.png': 'png',
      'readme.md': 'not crawled', 'noext': 'not crawled',
    });
    const {fileSystem} = await build();
    expect(rel(fileSystem.getAllFiles())).toEqual(['a.js', 'data.json', 'dir/b.js', 'img.png']);
  });

  test('skips files matching ignorePattern and VCS directories', async () => {
    project = createProject({
      'a.js': 'a', '__ignored__/b.js': 'b', '.git/hooks/c.js': 'c', '.hg/d.js': 'd', 'node_modules/pkg/index.js': 'kept',
    });
    const {fileSystem} = await build();
    expect(rel(fileSystem.getAllFiles())).toEqual(['a.js', 'node_modules/pkg/index.js']);
  });

  test('computes SHA-1 of file contents', async () => {
    project = createProject({'a.js': 'hello world'});
    const {fileSystem} = await build();
    expect(fileSystem.getSha1(path.join(project.root, 'a.js'))).toBe(sha1('hello world'));
  });

  test('lookup reports files, directories and missing paths', async () => {
    project = createProject({'dir/a.js': 'a'});
    const {fileSystem} = await build();
    expect(fileSystem.lookup(path.join(project.root, 'dir/a.js'))).toEqual(
      expect.objectContaining({exists: true, type: 'f', realPath: path.join(project.root, 'dir/a.js')}),
    );
    expect(fileSystem.lookup(path.join(project.root, 'dir'))).toEqual(
      expect.objectContaining({exists: true, type: 'd', realPath: path.join(project.root, 'dir')}),
    );
    expect(fileSystem.lookup(path.join(project.root, 'nope.js'))).toEqual(expect.objectContaining({exists: false}));
    expect(fileSystem.exists(path.join(project.root, 'dir/a.js'))).toBe(true);
  });

  test('follows symlinks to files and directories in lookup', async () => {
    project = createProject({'real/a.js': 'a', 'real/b.js': 'b'});
    fs.symlinkSync(path.join(project.root, 'real/a.js'), path.join(project.root, 'link.js'));
    fs.symlinkSync(path.join(project.root, 'real'), path.join(project.root, 'linkdir'));
    const {fileSystem} = await build();
    expect(fileSystem.lookup(path.join(project.root, 'link.js'))).toEqual(
      expect.objectContaining({exists: true, type: 'f', realPath: path.join(project.root, 'real/a.js')}),
    );
    expect(fileSystem.lookup(path.join(project.root, 'linkdir/b.js'))).toEqual(
      expect.objectContaining({exists: true, type: 'f', realPath: path.join(project.root, 'real/b.js')}),
    );
    expect(fileSystem.linkStats(path.join(project.root, 'link.js'))).toEqual(expect.objectContaining({fileType: 'l'}));
  });

  test('matchFiles filters by regex and subdirectory, recursively or not', async () => {
    project = createProject({'src/a.js': '', 'src/a.test.js': '', 'src/deep/b.test.js': '', 'other/c.test.js': ''});
    const {fileSystem} = await build();
    const m = opts => rel([...fileSystem.matchFiles(opts)]);
    expect(m({filter: /\.test\.js$/, rootDir: path.join(project.root, 'src')})).toEqual(['src/a.test.js', 'src/deep/b.test.js']);
    expect(m({filter: /\.test\.js$/, rootDir: path.join(project.root, 'src'), recursive: false})).toEqual(['src/a.test.js']);
    expect(m({filter: /\.test\.js$/, rootDir: null})).toEqual(['other/c.test.js', 'src/a.test.js', 'src/deep/b.test.js']);
  });

  test('hierarchicalLookup finds the closest package.json, stopping at node_modules', async () => {
    project = createProject({
      'package.json': '{}', 'src/deep/a.js': '', 'node_modules/pkg/lib/x.js': '', 'node_modules/pkg/package.json': '{}',
    });
    const {fileSystem} = await build({extensions: ['js', 'json']});
    const r = fileSystem.hierarchicalLookup(path.join(project.root, 'src/deep/a.js'), 'package.json', {
      breakOnSegment: 'node_modules', subpathType: 'f',
    });
    expect(r).toEqual({absolutePath: path.join(project.root, 'package.json'), containerRelativePath: 'src/deep/a.js'});
    const inPkg = fileSystem.hierarchicalLookup(path.join(project.root, 'node_modules/pkg/lib/x.js'), 'package.json', {
      breakOnSegment: 'node_modules', subpathType: 'f',
    });
    expect(inPkg?.absolutePath).toBe(path.join(project.root, 'node_modules/pkg/package.json'));
  });

  describe('Haste', () => {
    test('maps Haste names to files, per platform', async () => {
      project = createProject({
        'hasteImpl.js': HASTE_IMPL,
        'Foo.js': '// @haste Foo',
        'Bar.ios.js': '// @haste Bar',
        'Bar.android.js': '// @haste Bar',
        'plain.js': 'no haste name',
      });
      const {hastePlugin} = await build({haste: true});
      expect(hastePlugin.getModule('Foo')).toBe(path.join(project.root, 'Foo.js'));
      expect(hastePlugin.getModule('Bar', 'ios')).toBe(path.join(project.root, 'Bar.ios.js'));
      expect(hastePlugin.getModule('Bar', 'android')).toBe(path.join(project.root, 'Bar.android.js'));
      expect(hastePlugin.getModule('Bar')).toBe(null);
      expect(hastePlugin.getModule('plain')).toBe(null);
      expect(hastePlugin.getModuleNameByPath(path.join(project.root, 'Foo.js'))).toBe('Foo');
    });

    test('maps Haste packages by package.json name', async () => {
      project = createProject({'hasteImpl.js': HASTE_IMPL, 'libs/thing/package.json': JSON.stringify({name: 'thing'})});
      const {hastePlugin} = await build({haste: true, extensions: ['js', 'json']});
      expect(hastePlugin.getPackage('thing')).toBe(path.join(project.root, 'libs/thing/package.json'));
    });

    test('duplicate Haste names fail the build with HasteConflictsError', async () => {
      project = createProject({'hasteImpl.js': HASTE_IMPL, 'a/Foo.js': '// @haste Foo', 'b/Foo.js': '// @haste Foo'});
      const {fileMap} = makeFileMap(project.root, {haste: true});
      ends.push(fileMap);
      await expect(fileMap.build()).rejects.toBeInstanceOf(HasteConflictsError);
    });
  });

  describe('disk cache', () => {
    test('a second FileMap on the same cache sees edits, additions and deletions made between runs', async () => {
      project = createProject({'a.js': 'v1', 'b.js': 'b'});
      const cacheDir = fs.mkdtempSync(path.join(os.tmpdir(), 'metro-bb-cache-'));
      const first = await build({cacheDir});
      await first.fileMap.end();
      // Make the mtime change observable even on coarse-grained file systems.
      await new Promise(r => setTimeout(r, 20));
      project.write('a.js', 'v2 longer');
      project.write('c.js', 'c');
      project.remove('b.js');
      const second = await build({cacheDir});
      expect(rel(second.fileSystem.getAllFiles())).toEqual(['a.js', 'c.js']);
      expect(second.fileSystem.getSha1(path.join(project.root, 'a.js'))).toBe(sha1('v2 longer'));
      fs.rmSync(cacheDir, {recursive: true, force: true});
    });
  });

  describe('watch mode', () => {
    async function watching(files) {
      project = createProject(files);
      const r = await build({watch: true});
      const events = [];
      r.fileMap.on('change', e => {
        const toRel = ([p]) => p.split(path.sep).join('/');
        events.push({
          added: [...e.changes.addedFiles].map(toRel),
          modified: [...e.changes.modifiedFiles].map(toRel),
          removed: [...e.changes.removedFiles].map(toRel),
        });
      });
      const seen = () => ({
        added: events.flatMap(e => e.added),
        modified: events.flatMap(e => e.modified),
        removed: events.flatMap(e => e.removed),
      });
      return {...r, events, seen};
    }

    test('emits added, modified and removed files, and the FileSystem follows', async () => {
      const w = await watching({'a.js': 'a', 'b.js': 'b'});
      project.write('new.js', 'n');
      await waitFor(() => w.seen().added.includes('new.js'), {label: 'add event'});
      project.write('a.js', 'a changed');
      await waitFor(() => w.seen().modified.includes('a.js'), {label: 'modify event'});
      project.remove('b.js');
      await waitFor(() => w.seen().removed.includes('b.js'), {label: 'remove event'});
      expect(rel(w.fileSystem.getAllFiles())).toEqual(['a.js', 'new.js']);
      await waitFor(() => w.fileSystem.getSha1(path.join(project.root, 'a.js')) === sha1('a changed'), {label: 'sha1 updated'});
    });

    test('does not emit events for ignored files or unwatched extensions', async () => {
      const w = await watching({'a.js': 'a'});
      project.write('__ignored__/x.js', 'x');
      project.write('notes.md', 'x');
      project.write('sentinel.js', 's');
      await waitFor(() => w.seen().added.includes('sentinel.js'), {label: 'sentinel'});
      expect(w.seen().added).not.toContain('__ignored__/x.js');
      expect(w.seen().added).not.toContain('notes.md');
    });

    test('files in a newly created directory are reported as added', async () => {
      const w = await watching({'a.js': 'a'});
      project.write('newdir/sub/x.js', 'x');
      await waitFor(() => w.seen().added.includes('newdir/sub/x.js'), {label: 'nested add'});
      expect(w.fileSystem.exists(path.join(project.root, 'newdir/sub/x.js'))).toBe(true);
    });

    test('removing a directory reports every file in it as removed', async () => {
      const w = await watching({'a.js': 'a', 'dir/x.js': 'x', 'dir/y.js': 'y'});
      project.remove('dir');
      await waitFor(() => ['dir/x.js', 'dir/y.js'].every(f => w.seen().removed.includes(f)), {label: 'dir removal'});
      expect(rel(w.fileSystem.getAllFiles())).toEqual(['a.js']);
    });

    test('rewriting a file with identical content is still reported as modified (mtime-based)', async () => {
      // Current behaviour: change detection is by mtime, not content.
      const w = await watching({'a.js': 'same'});
      await new Promise(r => setTimeout(r, 20));
      project.write('a.js', 'same');
      await waitFor(() => w.seen().modified.includes('a.js'), {label: 'modify event'});
      expect(w.fileSystem.getSha1(path.join(project.root, 'a.js'))).toBe(sha1('same'));
    });
  });
});
