/**
 * Black-box: module paths that are not file system paths.
 *
 * Frameworks put virtual modules in the graph: Expo resolves its polyfills to
 * paths such as `\0polyfill:assets-registry` and supplies their contents
 * itself. The graph must hold any string as a module path. Driven through the
 * Graph class (`metro/private/DeltaBundler/Graph`) with a custom resolve and
 * transform, which is what such frameworks plug in.
 */
'use strict';

const {Graph} = require('metro/private/DeltaBundler/Graph');

const VIRTUAL = '\0polyfill:assets-registry';
const ILL_FORMED = '/virtual/lone-\uD800-surrogate.js';

// A project as a map from path to its dependencies ({name → path}).
function createProject(tree) {
  const revisions = new Map();
  const transform = async path => ({
    dependencies: Object.keys(tree[path] ?? {}).map(name => ({
      name,
      data: {asyncType: null, isESMImport: false, locs: [], key: name},
    })),
    getSource: () => Buffer.from(`// ${path}`),
    output: [{data: {code: `// ${path}`, lineCount: 1, map: []}, type: 'js/module'}],
    unstable_transformResultKey: `${path} (revision ${revisions.get(path) ?? 0})`,
  });
  const resolve = (from, dependency) => {
    const to = tree[from]?.[dependency.name];
    if (to == null) {
      throw new Error(`Cannot resolve ${dependency.name} from ${from}`);
    }
    return {type: 'sourceFile', filePath: to};
  };
  return {
    tree,
    edit(path, deps) {
      tree[path] = deps;
      revisions.set(path, (revisions.get(path) ?? 0) + 1);
    },
    options: {
      lazy: false,
      onProgress: null,
      resolve,
      shallow: false,
      transform,
      transformOptions: {dev: true, minify: false, platform: 'ios', type: 'module', unstable_transformProfile: 'default'},
      unstable_allowRequireContext: false,
      unstable_enablePackageExports: false,
      unstable_incrementalResolution: false,
    },
  };
}

const paths = result => [...result].sort();

describe('virtual module paths in the graph', () => {
  test.each([
    ['a NUL-prefixed virtual module', VIRTUAL],
    ['a path with a lone surrogate', ILL_FORMED],
  ])('%s is held like any other module, edited and removed', async (_, virtualPath) => {
    const project = createProject({
      '/app/index.js': {virtual: virtualPath, './a': '/app/a.js'},
      [virtualPath]: {'./registry': '/app/registry.js'},
      '/app/a.js': {},
      '/app/registry.js': {},
    });
    const graph = new Graph({entryPoints: new Set(['/app/index.js']), transformOptions: project.options.transformOptions});

    const initial = await graph.initialTraverseDependencies(project.options);
    expect(paths(initial.added.keys())).toEqual(paths(['/app/index.js', '/app/a.js', virtualPath, '/app/registry.js']));
    expect([...graph.dependencies.get(virtualPath).inverseDependencies]).toEqual(['/app/index.js']);
    expect(graph.dependencies.get('/app/index.js').dependencies.get('virtual').absolutePath).toBe(virtualPath);

    // The virtual module drops its dependency.
    project.edit(virtualPath, {});
    const edited = await graph.traverseDependencies([virtualPath], project.options);
    expect(paths(edited.modified.keys())).toEqual([virtualPath]);
    expect(paths(edited.deleted)).toEqual(['/app/registry.js']);

    // The entry point stops using the virtual module.
    project.edit('/app/index.js', {'./a': '/app/a.js'});
    const removed = await graph.traverseDependencies(['/app/index.js'], project.options);
    expect(paths(removed.deleted)).toEqual([virtualPath]);
    expect(graph.dependencies.has(virtualPath)).toBe(false);

    // And uses it again.
    project.edit('/app/index.js', {virtual: virtualPath, './a': '/app/a.js'});
    const readded = await graph.traverseDependencies(['/app/index.js'], project.options);
    expect(paths(readded.added.keys())).toEqual([virtualPath]);
  });

  test('paths that differ only in a lone surrogate are distinct modules', async () => {
    const first = '/virtual/\uD800.js';
    const second = '/virtual/\uD801.js';
    const project = createProject({'/app/index.js': {first, second}, [first]: {}, [second]: {'./a': '/app/a.js'}, '/app/a.js': {}});
    const graph = new Graph({entryPoints: new Set(['/app/index.js']), transformOptions: project.options.transformOptions});
    const initial = await graph.initialTraverseDependencies(project.options);
    expect(paths(initial.added.keys())).toEqual(paths(['/app/index.js', first, second, '/app/a.js']));
    project.edit('/app/index.js', {second});
    const edited = await graph.traverseDependencies(['/app/index.js'], project.options);
    expect(paths(edited.deleted)).toEqual([first]);
    expect(graph.dependencies.has(second)).toBe(true);
  });

  test('a virtual entry point', async () => {
    const project = createProject({[VIRTUAL]: {'./a': '/app/a.js'}, '/app/a.js': {}});
    const graph = new Graph({entryPoints: new Set([VIRTUAL]), transformOptions: project.options.transformOptions});
    const initial = await graph.initialTraverseDependencies(project.options);
    expect(paths(initial.added.keys())).toEqual(paths([VIRTUAL, '/app/a.js']));
  });
});
