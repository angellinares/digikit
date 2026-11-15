// Instructions per function and per DO loop for one conformance case of a machine module, on the SHARC-only rig (no emulator): the rig is
// a producer for `wb profile report` (packages/dkprof), the profiler of the emulator and the device.
//
//   node tools/devtools/rig_profile.ts MODULE --device dt2|dn2 [--case GROUP/NAME] [--blocks 40,41] [--out DIR] [--top 20] [--print]
//
// MODULE is a module id (`<author/name>`). --case is the case as `group/name` (`sample/stereo-fwd-oneshot/note60`) or only the name; default
// the first case of the module. The case runs block by block with its parameter words, as the conformance run does. The counts of the
// steady blocks (the blocks that are no note-on block; DN2 stages the note one block after the trig) are added; --blocks names the blocks to
// add instead. The tool writes to DIR (default out/runs/rig-profile/<module>/<device>/):
//   profile.json       `dk.profile/1`: raw SHARC counters per pc, metric sharc.blocks, the load of the module's DSP image
//   rig.addrmap.json   the labels of the placed objects (module labels relative to its DSP image)
//   module.dkm         `patchkit pack` of the module for the device (its DSP section is what dksharc analyses)
// and runs `node tools/devtools/wb.mjs profile report profile.json --map rig.addrmap.json --analyse ID=module.dkm:DSP.dko.json`, so functions
// come from the map and loops from dksharc. --print shows that command instead of running it (needs `mise run wasm:dkprof` to run).
// The rig binary and patchkit come from DIGI_SHARC_RIG and DIGI_PATCHKIT, else dist/ (`nx run sharc-rig:build`, `nx run patchkit:cli`).
// DIGI_RIG_ROOT=PATH reads the module sources, SDK and device files of another checkout (read only), for example a lane branch.
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  machineCases,
  machineSpec,
  MachineRig,
  moduleBaseOf,
  patchkit,
  publishSample,
  rigAddrMap,
  rigBinary,
  rigProfile,
  sumCounts,
  wordsAt,
  type Device,
  type PcCounts,
} from '../../packages/sharc-rig-kit/src/index.ts';

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, '../..');

function arg(name: string): string | undefined {
  const i = process.argv.indexOf(name);
  return i < 0 ? undefined : process.argv[i + 1];
}

const module = process.argv[2];
const device = arg('--device') as Device | undefined;
if (!module || module.startsWith('--') || (device !== 'dt2' && device !== 'dn2')) {
  console.error(
    'usage: node tools/devtools/rig_profile.ts MODULE --device dt2|dn2 [--case GROUP/NAME] [--blocks 40,41] [--out DIR] [--top 20] [--print]',
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
const trig = new Set(c.trig);
const noteOn = new Set(trig);
if (device === 'dn2') for (const k of trig) noteOn.add(k + 1);
const asked = arg('--blocks')?.split(',').map(Number);
const wanted = (k: number): boolean => (asked ? asked.includes(k) : !noteOn.has(k));
const last = asked ? Math.max(...asked) : c.blocks - 1;
if (last >= c.blocks) {
  console.error(`case ${c.name} has ${c.blocks} blocks`);
  process.exit(2);
}

const rig = await MachineRig.open(sp, { profile: true });
const calls: PcCounts[] = [];
let moduleBase: number;
try {
  moduleBase = moduleBaseOf(rig.built);
  if (c.setup === 'sample-pool') publishSample(rig, sp, c);
  rig.setNote(c.note, c.vel);
  for (let k = 0; k <= last; k++) {
    rig.setParams(wordsAt(c, k));
    if (trig.has(k)) rig.trigger();
    const b = await rig.block();
    if (!b.returned) throw new Error(`block ${k} did not return (${b.halt})`);
    if (wanted(k)) calls.push(b.profile);
  }
} finally {
  await rig.close();
}
if (calls.length === 0) {
  console.error(`case ${c.name}: no block to add`);
  process.exit(2);
}

const caseName = `${c.group}/${c.name}`;
const dir = path.resolve(arg('--out') ?? path.join(repo, 'out/runs/rig-profile', module, device));
mkdirSync(dir, { recursive: true });
const profileFile = path.join(dir, 'profile.json');
const mapFile = path.join(dir, 'rig.addrmap.json');
const dkmFile = path.join(dir, 'module.dkm');
const binarySha256 = createHash('sha256').update(readFileSync(rigBinary())).digest('hex');
writeFileSync(
  profileFile,
  `${JSON.stringify(rigProfile({ module, device, caseName, blocks: calls.length, binarySha256 }, sumCounts(calls), moduleBase))}\n`,
);
writeFileSync(mapFile, `${JSON.stringify(rigAddrMap(rig.built, module, device))}\n`);

// The module's DSP section and object for the loops: the pack of the module, and its DSP object in the build cache.
const report = [
  path.join(here, 'wb.mjs'),
  'profile',
  'report',
  profileFile,
  '--map',
  mapFile,
  '--top',
  String(arg('--top') ?? 20),
];
try {
  patchkit(['pack', module, '-o', dkmFile, '--device', device]);
  const objects =
    (JSON.parse(patchkit(['build', '--paths', module])) as Record<string, Record<string, string>>)[module] ?? {};
  const dko = Object.entries(objects).find(([k]) => k.endsWith('-dsp.dko.json'))?.[1];
  if (!dko) throw new Error(`${module} has no DSP object`);
  report.push('--analyse', `${module}=${dkmFile}:${dko}`);
} catch (error) {
  console.error(`no loop rows: ${(error as Error).message}`);
}
console.error(
  `${caseName} on ${device}: ${calls.length} block${calls.length === 1 ? '' : 's'} added; wrote ${profileFile}`,
);
if (process.argv.includes('--print')) {
  console.log(['node', ...report].join(' '));
} else {
  const r = spawnSync('node', report, { stdio: 'inherit', cwd: repo });
  process.exit(r.status ?? 1);
}
