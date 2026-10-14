/**
 * Regenerates src/oracle_data.rs from the real JS HastePlugin (the source of
 * truth). Usage, from the repo root:
 *
 *   node metro-rs/crates/haste/oracle/gen.js > metro-rs/crates/haste/src/oracle_data.rs && (cd metro-rs && cargo fmt)
 *
 * @format
 */

'use strict';

const REPO = require('path').resolve(__dirname, '../../../..');
require(REPO + '/packages/metro-babel-register').unstable_registerForMetroMonorepo();
const HastePlugin = require(REPO + '/packages/metro-file-map/src/plugins/HastePlugin').default;
const getPlatformExtension = require(REPO + '/packages/metro-file-map/src/plugins/haste/getPlatformExtension').default;
const {compareStrings} = require(REPO + '/packages/metro-file-map/src/lib/sorting');

const q = s => JSON.stringify(s);
const opt = s => (s == null ? 'None' : `Some(${q(s)})`);
const pair = ([p, id]) => `(${q(p)}, ${opt(id)})`;
const list = (items, f) => '&[' + items.map(f).join(', ') + ']';

const SCENARIOS = [
  {
    name: 'platforms',
    queryPlatforms: [null, 'ios', 'android', 'web', 'native'],
    platforms: ['ios', 'android', 'native'],
    initial: [
      ['project/Foo.js', 'Foo'],
      ['project/Foo.ios.js', 'Foo'],
      ['project/Foo.android.js', 'Foo'],
      ['project/Foo.native.js', 'Foo'],
      ['project/OnlyNative.native.js', 'OnlyNative'],
      ['project/OnlyIos.ios.js', 'OnlyIos'],
      ['project/OnlyGeneric.js', 'OnlyGeneric'],
      ['project/OnlyGeneric.web.js', 'OnlyGeneric2'],
      ['project/GenericNative.js', 'GenericNative'],
      ['project/GenericNative.native.js', 'GenericNative'],
      ['project/a.b.ios.js', 'DotName'],
      ['project/noext', 'NoExt'],
      ['project/.ios', 'DotIos'],
      ['project/Skipped.js', null],
    ],
    steps: [
      {removed: [['project/Foo.ios.js', 'Foo']], added: [], modified: []},
      {removed: [['project/Foo.native.js', 'Foo'], ['project/Foo.js', 'Foo']], added: [['project/Late.android.js', 'Late']], modified: []},
    ],
  },
  {
    name: 'no-native-platform',
    platforms: ['ios', 'android'],
    initial: [
      ['project/Foo.js', 'Foo'],
      ['project/Foo.native.js', 'Foo'],
      ['project/Foo.ios.js', 'Foo'],
      ['project/Bar.native.js', 'Bar'],
    ],
    steps: [],
  },
  {
    name: 'packages',
    platforms: ['ios', 'android', 'native'],
    enableHastePackages: true,
    initial: [
      ['project/pkg/package.json', 'my-pkg'],
      ['project/my-pkg.js', 'my-pkg'],
      ['project/pkg2/package.json', 'pkg2'],
      ['project/pkg2.ios.js', 'pkg2'],
      ['project/pkg3/package.json', 'pkg3'],
      ['../outside/pkg4/package.json', 'pkg4'],
      ['package.json', 'root-pkg'],
      ['project/Mod.js', 'Mod'],
    ],
    steps: [
      {removed: [['project/my-pkg.js', 'my-pkg']], added: [['project/pkg5/package.json', 'pkg5']], modified: []},
      {removed: [], added: [['project/pkg3/other/package.json', 'pkg3']], modified: []},
    ],
  },
  {
    name: 'packages-disabled',
    platforms: ['ios', 'android', 'native'],
    enableHastePackages: false,
    initial: [
      ['project/pkg/package.json', 'my-pkg'],
      ['project/Mod.js', 'Mod'],
    ],
    steps: [{removed: [], added: [['project/other/package.json', 'my-pkg2']], modified: []}],
  },
  {
    name: 'duplicates-recovery',
    platforms: ['ios', 'android', 'native'],
    initial: [
      ['a/Dup.js', 'Dup'],
      ['b/Dup.js', 'Dup'],
      ['a/Dup3.js', 'Dup3'],
      ['b/Dup3.js', 'Dup3'],
      ['c/Dup3.js', 'Dup3'],
      ['a/DupI.ios.js', 'DupI'],
      ['b/DupI.ios.js', 'DupI'],
      ['a/DupI.js', 'DupI'],
      ['a/Q.js', 'Q'],
      ['b/Q.js', 'Q'],
      ['a/M.js', 'M'],
    ],
    steps: [
      {removed: [['b/Dup.js', 'Dup']], added: [], modified: []},
      {removed: [['c/Dup3.js', 'Dup3']], added: [], modified: []},
      {removed: [], added: [['d/Dup3.js', 'Dup3'], ['c/Q.ios.js', 'Q'], ['c/Q.js', 'Q']], modified: []},
      {removed: [['a/Dup3.js', 'Dup3'], ['d/Dup3.js', 'Dup3']], added: [], modified: []},
      {removed: [['a/DupI.ios.js', 'DupI']], added: [], modified: []},
      {removed: [['a/Dup.js', 'Dup']], added: [['z/Dup.js', 'Dup']], modified: []},
      // A move: removals are processed first, so it is not a duplicate.
      {removed: [['a/M.js', 'M']], added: [['moved/M.js', 'M']], modified: []},
      {removed: [['a/Q.js', 'Q'], ['b/Q.js', 'Q'], ['c/Q.js', 'Q'], ['c/Q.ios.js', 'Q']], added: [], modified: []},
    ],
  },
  {
    name: 'conflicts',
    moduleOnly: true,
    queryPlatforms: [null, 'ios'],
    platforms: ['ios', 'android', 'native'],
    failValidationOnConflicts: true,
    initial: [
      ['b/Foo.js', 'Foo'],
      ['a/Foo.ios.js', 'Foo'],
      ['c/Foo.native.js', 'Foo'],
      ['x/Bar.native.js', 'Bar'],
      ['y/Bar.js', 'Bar'],
      ['same/Baz.ios.js', 'Baz'],
      ['same/Baz.js', 'Baz'],
      ['p/Dup.js', 'Dup'],
      ['q/Dup.js', 'Dup'],
      ['p/DupI.ios.js', 'DupI'],
      ['q/DupI.ios.js', 'DupI'],
      ['p/alpha.js', 'alpha'],
      ['q/alpha.js', 'alpha'],
      ['p/Beta.js', 'Beta'],
      ['q/Beta.js', 'Beta'],
      ['p/_u.js', '_u'],
      ['q/_u.js', '_u'],
      ['p/a-b.js', 'a-b'],
      ['q/a-b.js', 'a-b'],
      ['p/A1.js', 'A1'],
      ['q/A1.js', 'A1'],
      ['p/a1.js', 'a1'],
      ['q/a1.js', 'a1'],
      ['x/S.ios.js', 'S'],
      ['y/S.android.js', 'S'],
      ['z/S.native.js', 'S'],
      ['w/S.js', 'S'],
    ],
    steps: [
      {removed: [['a/Foo.ios.js', 'Foo'], ['x/Bar.native.js', 'Bar']], added: [], modified: []},
      {
        removed: ['Dup', 'DupI', 'alpha', 'Beta', '_u', 'a-b', 'A1', 'a1'].flatMap(id => [
          [`p/${id}${id === 'DupI' ? '.ios' : ''}.js`, id],
        ]),
        added: [],
        modified: [],
      },
      {removed: [['w/S.js', 'S'], ['x/S.ios.js', 'S']], added: [], modified: []},
    ],
  },
  {
    name: 'conflicts-not-failing',
    platforms: ['ios', 'native'],
    failValidationOnConflicts: false,
    initial: [
      ['p/Dup.js', 'Dup'],
      ['q/Dup.js', 'Dup'],
    ],
    steps: [],
  },
  {
    name: 'outside-root',
    rootDir: '/root/proj',
    platforms: ['ios', 'android', 'native'],
    failValidationOnConflicts: true,
    initial: [
      ['../shared/Out.js', 'Out'],
      ['../../top/Top.js', 'Top'],
      ['in/In.js', 'In'],
      ['../shared/D.js', 'D'],
      ['in/D.js', 'D'],
      ['../shared/S.ios.js', 'S'],
      ['in/S.js', 'S'],
    ],
    steps: [{removed: [['in/D.js', 'D']], added: [], modified: []}],
  },
  {
    name: 'changes-edge-cases',
    platforms: ['ios', 'android', 'native'],
    enableHastePackages: true,
    initial: [
      ['a/One.js', 'One'],
      ['a/Two.js', 'Two'],
      ['a/NoName.js', null],
    ],
    steps: [
      // Added with null id, removed with null id: no effect on the maps.
      {removed: [['a/NoName.js', null]], added: [['a/Null.js', null]], modified: []},
      // Modified files are ignored by onChanged.
      {removed: [], added: [], modified: [['a/One.js', 'Changed'], ['a/NoName.js', 'NowNamed']]},
      // Removing a path that is not registered under that id.
      {removed: [['elsewhere/One.js', 'One'], ['a/Two.js', 'Nope']], added: [], modified: []},
      // The same path added again (same id), then with a different type.
      {removed: [], added: [['a/One.js', 'One']], modified: []},
      {removed: [], added: [['a/package.json', 'One']], modified: []},
      // Same file under a new id.
      {removed: [['a/Two.js', 'Two']], added: [['a/Two.js', 'TwoRenamed']], modified: []},
    ],
  },
];

function makeCheck(scn) {
  const rootDir = scn.rootDir ?? '/root';
  const warnings = [];
  const plugin = new HastePlugin({
    console: {warn: m => warnings.push(m)},
    enableHastePackages: scn.enableHastePackages ?? false,
    hasteImplModulePath: null,
    failValidationOnConflicts: scn.failValidationOnConflicts ?? false,
    perfLogger: null,
    platforms: new Set(scn.platforms),
    rootDir,
  });
  // A stand-in for the file map's `lookup`: path -> live plugin data.
  const fsData = new Map();
  const everPaths = new Set();
  const setFs = (p, id) => {
    everPaths.add(p);
    if (id == null) {
      fsData.delete(p);
    } else {
      fsData.set(p, id);
    }
  };
  const lookup = mixed => {
    const rel = mixed.startsWith('/') ? require('path').relative(rootDir, mixed) : mixed;
    return fsData.has(rel)
      ? {exists: true, type: 'f', pluginData: fsData.get(rel)}
      : {exists: false};
  };
  const ids = new Set(['Missing']);
  for (const [, id] of [...scn.initial, ...scn.steps.flatMap(s => [...s.removed, ...s.added, ...s.modified])]) {
    if (id != null) {
      ids.add(id);
    }
  }
  const platforms = scn.queryPlatforms ?? [null, 'ios', 'native'];
  function snapshot() {
    const queries = [];
    for (const name of ids) {
      const run = (kind, platform, native, f) => {
        let expect;
        try {
          const r = f();
          expect = r == null ? 'Expect::None' : `Expect::Path(${q(r)})`;
        } catch (e) {
          if (e.hasteName == null) {
            throw e;
          }
          expect = `Expect::Dup(${q(e.platform)}, ${e.supportsNativePlatform}, ${list([...e.duplicatesSet], ([p, t]) => `(${q(p)}, ${t})`)}, ${q(e.message)})`;
        }
        queries.push(`Query(${q(name)}, ${opt(platform)}, ${native}, ${kind}, ${expect})`);
      };
      for (const platform of platforms) {
        for (const native of [false, true]) {
          run(0, platform, native, () => plugin.getModule(name, platform, native));
          if (!scn.moduleOnly) {
            run(1, platform, native, () => plugin.getModule(name, platform, native, 1));
            run(2, platform, native, () => plugin.getPackage(name, platform, native));
          }
        }
      }
    }
    const conflicts = plugin.computeConflicts().map(
      c => `Conflict(${q(c.id)}, ${opt(c.platform)}, ${q(c.type)}, ${list(c.absolutePaths, q)})`,
    );
    let valid = 'None';
    try {
      plugin.assertValid();
    } catch (e) {
      valid = `Some(Invalid(${q(e.message)}, ${q(e.getDetailedMessage())}, ${q(e.getDetailedMessage(rootDir))}))`;
    }
    const names = [];
    for (const p of [...everPaths, 'never/seen.js', '/root/never/seen.js']) {
      for (const m of [p, p.startsWith('/') ? p : require('path').resolve(rootDir, p)]) {
        const r = plugin.getModuleNameByPath(m);
        names.push(`(${q(m)}, ${opt(r)})`);
      }
    }
    const w = warnings.splice(0);
    return `State { queries: &[${queries.join(', ')}], conflicts: &[${conflicts.join(', ')}], valid: ${valid}, names: &[${names.join(', ')}], warnings: ${list(w, q)} }`;
  }
  return {plugin, setFs, lookup, snapshot, rootDir};
}

async function run(scn) {
  const c = makeCheck(scn);
  for (const [p, id] of scn.initial) {
    c.setFs(p, id);
  }
  await c.plugin.initialize({
    files: {
      fileIterator: () =>
        scn.initial.map(([canonicalPath, pluginData]) => ({
          canonicalPath,
          baseName: canonicalPath.split('/').pop(),
          pluginData,
        })),
      lookup: c.lookup,
    },
    processFile: () => {
      throw new Error('Not implemented');
    },
    pluginState: null,
  });
  const states = [c.snapshot()];
  for (const step of scn.steps) {
    for (const [p] of step.removed) {
      c.setFs(p, null);
    }
    for (const [p, id] of [...step.added, ...step.modified]) {
      c.setFs(p, id);
    }
    c.plugin.onChanged({
      addedDirectories: [],
      removedDirectories: [],
      addedFiles: step.added,
      modifiedFiles: step.modified,
      removedFiles: step.removed,
    });
    states.push(c.snapshot());
  }
  const out = [];
  out.push('Scenario {');
  out.push(`name: ${q(scn.name)}, root: ${q(c.rootDir)}, platforms: ${list(scn.platforms, q)},`);
  out.push(`enable_packages: ${scn.enableHastePackages ?? false}, fail: ${scn.failValidationOnConflicts ?? false},`);
  out.push(`initial: ${list(scn.initial, pair)},`);
  out.push(
    `steps: ${list(scn.steps, s => `Step { added: ${list(s.added, pair)}, modified: ${list(s.modified, pair)}, removed: ${list(s.removed, pair)} }`)},`,
  );
  out.push(`states: &[${states.join(',\n')}],`);
  out.push('}');
  return out.join('\n');
}

async function main() {
  const lines = [];
  lines.push('// @generated by oracle/gen.js from the JS HastePlugin. Do not edit by hand.');
  lines.push('#![allow(clippy::type_complexity)]');
  lines.push(`
pub enum Expect {
    None,
    Path(&'static str),
    /// (platform, supportsNativePlatform, duplicatesSet, message)
    Dup(&'static str, bool, &'static [(&'static str, u8)], &'static str),
}
/// (name, platform, supportsNativePlatform, kind 0 getModule | 1 getModule(PACKAGE) | 2 getPackage, expected)
pub struct Query(pub &'static str, pub Option<&'static str>, pub bool, pub u8, pub Expect);
/// (id, platform, type, absolutePaths)
pub struct Conflict(pub &'static str, pub Option<&'static str>, pub &'static str, pub &'static [&'static str]);
/// (message, getDetailedMessage(), getDetailedMessage(rootDir))
pub struct Invalid(pub &'static str, pub &'static str, pub &'static str);
pub struct State {
    pub queries: &'static [Query],
    pub conflicts: &'static [Conflict],
    pub valid: Option<Invalid>,
    pub names: &'static [(&'static str, Option<&'static str>)],
    pub warnings: &'static [&'static str],
}
pub struct Step {
    pub added: &'static [(&'static str, Option<&'static str>)],
    pub modified: &'static [(&'static str, Option<&'static str>)],
    pub removed: &'static [(&'static str, Option<&'static str>)],
}
pub struct Scenario {
    pub name: &'static str,
    pub root: &'static str,
    pub platforms: &'static [&'static str],
    pub enable_packages: bool,
    pub fail: bool,
    pub initial: &'static [(&'static str, Option<&'static str>)],
    pub steps: &'static [Step],
    /// One state after initialize and one after each step.
    pub states: &'static [State],
}`);
  const scns = [];
  for (const s of SCENARIOS) {
    scns.push(await run(s));
  }
  lines.push(`pub const SCENARIOS: &[Scenario] = &[${scns.join(',\n')}];`);

  const plats = new Set(['ios', 'android', 'native']);
  const files = ['index.ios.js', 'index.js', 'a.native.js', 'a.android.b', 'x.web.js', '.ios', '.ios.js', 'ios.js', '.', '..', 'a..js', 'a.ios.', 'a.b.c.ios', 'dir.ios/file', 'dir.ios/file.js', 'noext', '', 'a.ios', '.a', 'é.ios.js', 'p/Foo.android.js'];
  lines.push('/// (file, getPlatformExtension(file, {ios, android, native}))');
  lines.push(`pub const PLATFORM_EXTENSIONS: &[(&str, Option<&str>)] = ${list(files, f => `(${q(f)}, ${opt(getPlatformExtension(f, plats))})`)};`);

  const strings = [];
  for (let c = 32; c < 127; c++) {
    strings.push(String.fromCharCode(c));
  }
  strings.push('a', 'A', 'ab', 'aB', 'Ab', 'AB', 'a1', 'A1', 'a-b', 'a_b', 'a b', 'a.b', 'a/b', 'a10', 'a2', 'Foo', 'foo', 'FOO', 'fooBar', 'foo-bar', 'foo_bar', 'foo.bar', '@scope/pkg', 'ios', 'android', 'native', 'web');
  const sorted = [...strings].sort((a, b) => compareStrings(a, b));
  lines.push('/// Strings in `compareStrings` (localeCompare) order.');
  lines.push(`pub const COLLATION_SORTED: &[&str] = ${list(sorted, q)};`);
  process.stdout.write(lines.join('\n') + '\n');
}

main().catch(e => {
  console.error(e);
  process.exit(1);
});
