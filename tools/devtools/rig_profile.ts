// Instructions per symbol and per run for one conformance case of a machine module, on the SHARC-only rig (no emulator).
//
//   node tools/devtools/rig_profile.ts MODULE --device dt2|dn2 [--case GROUP/NAME] [--blocks 0,1,50] [--top 20] [--runs] [--json FILE]
//
// MODULE is a module id (`<author/name>`). --case is the case as `group/name` (`sample/stereo-fwd-oneshot/note60`) or only the name
// (`stereo-fwd-oneshot/note60`); default the first case of the module. --blocks lists the blocks to print (default 0,1: the note-on
// block and the one after it; DN2 stages the note one block after the trig). The case runs from block 0 up to the last block asked, with
// the case's parameter words, so a block holds the state the case builds up. --runs adds the table of equal-count runs (loop bodies).
// --json writes every printed block as `{device, module, case, block, instructions, symbols, runs}`.
// The rig binary and patchkit come from DIGI_SHARC_RIG and DIGI_PATCHKIT, else dist/ (`nx run sharc-rig:build`, `nx run patchkit:cli`).
// DIGI_RIG_ROOT=PATH reads the module sources, SDK and device files of another checkout (read only), for example a lane branch.
import { writeFileSync } from 'node:fs';
import {
  machineCases,
  machineSpec,
  MachineRig,
  formatRuns,
  formatSymbols,
  profileBySymbol,
  profileRuns,
  publishSample,
  totalOf,
  wordsAt,
  type Device,
} from '../../packages/sharc-rig-kit/src/index.ts';

function arg(name: string): string | undefined {
  const i = process.argv.indexOf(name);
  return i < 0 ? undefined : process.argv[i + 1];
}

const module = process.argv[2];
const device = arg('--device') as Device | undefined;
if (!module || module.startsWith('--') || (device !== 'dt2' && device !== 'dn2')) {
  console.error(
    'usage: node tools/devtools/rig_profile.ts MODULE --device dt2|dn2 [--case GROUP/NAME] [--blocks 0,1,50] [--top 20] [--runs] [--json FILE]',
  );
  process.exit(2);
}
const sp = machineSpec(module, device);
if (!sp) {
  console.error(`${module} is no machine with a DSP object for ${device}`);
  process.exit(2);
}
const cases = machineCases(sp);
const want = arg('--case');
const c = want ? cases.find((k) => `${k.group}/${k.name}` === want || k.name === want) : cases[0];
if (!c) {
  console.error(`no case ${want}; cases: ${cases.map((k) => `${k.group}/${k.name}`).join(', ')}`);
  process.exit(2);
}
const blocks = (arg('--blocks') ?? '0,1').split(',').map(Number);
const top = Number(arg('--top') ?? 20);
const last = Math.max(...blocks);
if (last >= c.blocks) {
  console.error(`case ${c.name} has ${c.blocks} blocks`);
  process.exit(2);
}

const rig = await MachineRig.open(sp, { profile: true });
const json: unknown[] = [];
try {
  if (c.setup === 'sample-pool') publishSample(rig, sp, c);
  rig.setNote(c.note, c.vel);
  const trig = new Set(c.trig);
  for (let k = 0; k <= last; k++) {
    rig.setParams(wordsAt(c, k));
    if (trig.has(k)) rig.trigger();
    const b = await rig.block();
    if (!b.returned) throw new Error(`block ${k} did not return (${b.halt})`);
    if (!blocks.includes(k)) continue;
    const symbols = profileBySymbol(b.profile, rig.built.symbols);
    const runs = profileRuns(b.profile, rig.built.symbols);
    console.log(`\n== ${module} ${device} ${c.group}/${c.name} block ${k}`);
    console.log(formatSymbols(symbols, totalOf(b.profile), top));
    if (process.argv.includes('--runs')) console.log(`\n${formatRuns(runs, top)}`);
    json.push({ device, module, case: `${c.group}/${c.name}`, block: k, instructions: b.instructions, symbols, runs });
  }
} finally {
  await rig.close();
}
const out = arg('--json');
if (out) writeFileSync(out, `${JSON.stringify(json, null, 1)}\n`);
