/**
 * Micro-benchmark: decode the bench app's dev source map (about 2,700 sources)
 * with metro-source-map's Consumer, then re-encode it per module with
 * fromRawMappings, which is what Metro's serializers do for every bundle.
 * compare.js writes the input map to <cache>/bench.map before running this.
 */
'use strict';

const fs = require('node:fs');
const path = require('node:path');

exports.run = async ({pkg, cache, now}) => {
  const {Consumer, fromRawMappings} = require(pkg('metro-source-map'));
  const map = JSON.parse(fs.readFileSync(path.join(cache, '..', 'bench.map'), 'utf8'));
  const start = now();
  const consumer = new Consumer(map);
  const bySource = new Map();
  consumer.eachMapping(m => {
    if (m.source == null) return;
    let list = bySource.get(m.source);
    if (list == null) bySource.set(m.source, (list = []));
    list.push(m);
  });
  const modules = [];
  for (const [source, list] of bySource) {
    const firstLine = list[0].generatedLine;
    let lines = 1;
    const tuples = list.map(m => {
      const line = m.generatedLine - firstLine + 1;
      lines = Math.max(lines, line);
      return m.originalLine == null
        ? [line, m.generatedColumn]
        : [line, m.generatedColumn, m.originalLine, m.originalColumn];
    });
    modules.push({
      map: tuples,
      functionMap: null,
      path: source,
      source: '',
      code: '\n'.repeat(lines - 1),
      isIgnored: false,
    });
  }
  const out = fromRawMappings(modules).toString(undefined, {excludeSource: true});
  return {ms: now() - start, extra: {sources: modules.length, outKB: Math.round(out.length / 1024)}};
};
