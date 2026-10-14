/**
 * Phase bookkeeping for gate.sh. Reads port/phases.json and port/approvals/.
 *
 *   node port/lib/phase.js current            first phase without an approval
 *   node port/lib/phase.js check <n>          fails if an earlier phase is not approved
 *   node port/lib/phase.js components <n>     METRO_RUST value for phase n (cumulative)
 *   node port/lib/phase.js js-only <list>     JS-only test files to skip for a METRO_RUST list
 *   node port/lib/phase.js json <n>           the phase entry as JSON
 */
'use strict';

const fs = require('node:fs');
const path = require('node:path');

const PORT = path.resolve(__dirname, '..');
const plan = JSON.parse(fs.readFileSync(path.join(PORT, 'phases.json'), 'utf8'));
const approved = n => fs.existsSync(path.join(PORT, 'approvals', `phase-${n}.md`));
const phase = n => {
  const p = plan.phases.find(x => x.id === Number(n));
  if (p == null) {
    console.error(`Unknown phase ${n}. Phases are 0 to ${plan.phases.length - 1}.`);
    process.exit(2);
  }
  return p;
};

const [cmd, arg] = process.argv.slice(2);
switch (cmd) {
  case 'current': {
    const next = plan.phases.find(p => !approved(p.id));
    console.log(next ? next.id : 'done');
    break;
  }
  case 'check': {
    const missing = plan.phases.filter(p => p.id < Number(phase(arg).id) && !approved(p.id));
    if (missing.length > 0) {
      console.error(
        `Phase ${arg} cannot start: ${missing.length > 1 ? 'phases' : 'phase'} ${missing.map(p => p.id).join(', ')} ${missing.length > 1 ? 'are' : 'is'} not approved yet.\n` +
          'Only Szymon approves a phase, by adding port/approvals/phase-<n>.md. Stop and wait for that.',
      );
      process.exit(1);
    }
    break;
  }
  case 'components': {
    const list = plan.phases.filter(p => p.id <= phase(arg).id).flatMap(p => p.components);
    console.log(list.length ? list.join(',') : 'off');
    break;
  }
  case 'js-only': {
    const active = new Set((arg ?? '').split(',').filter(x => x && x !== 'off'));
    const {tests} = JSON.parse(fs.readFileSync(path.join(PORT, 'js-only-tests.json'), 'utf8'));
    for (const [file, t] of Object.entries(tests)) {
      if (t.components.some(c => active.has(c))) console.log(file);
    }
    break;
  }
  case 'json':
    console.log(JSON.stringify({...phase(arg), baselineRef: plan.baselineRef, maxRegression: plan.maxRegression}));
    break;
  default:
    console.error('usage: phase.js current | check <n> | components <n> | js-only <list> | json <n>');
    process.exit(2);
}
