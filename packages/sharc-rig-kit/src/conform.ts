/**
 * Runs the conformance cases of one machine or effect module and returns the report (the port of the retired Python workers, docs/plans/ledgers/t1g-retirement.md). The cases come from cases.ts and run on a fresh rig each; the rules of every block are in checks.ts.
 * A case stops at its first failure.
 */
import {
  describeFx,
  describeMachine,
  fxBlockFailure,
  fxBudgetFailure,
  machineBlockFailure,
  machineBudgetFailure,
  signalBlock,
} from './checks.ts';
import { fxCases, fxWordsAt, machineCases, wordsAt, type Case, type FxCase } from './cases.ts';
import { P0, FxRig, type FxRigOptions } from './fx.ts';
import { MachineRig, type MachineRigOptions } from './machine.ts';
import type { RigOpener } from './ops.ts';
import { buildFx, buildMachine } from './place.ts';
import { PyRandom } from './prng.ts';
import { RUNNER, round2, type CaseResult, type FxReport, type MachineReport } from './report.ts';
import { noise, publish, writePlanes } from './samplepool.ts';
import {
  FX_ADAPTER_MAX,
  budget,
  fxBudget,
  fxLabel,
  fxWord,
  machineLabel,
  type Budget,
  type FxSpec,
  type MachineSpec,
} from './spec.ts';
import { nw } from './context.ts';

export interface ConformOptions {
  /** run only these case groups (all when empty) */
  groups?: string[];
  seed?: number;
  open?: RigOpener;
}

const SEED = 0xd1617;
const seconds = (t0: number): number => round2((performance.now() - t0) / 1000);

/** The "sample-pool" setup: the PCM planes of the case's sample (a fixed seed) and its record at pool index 0. The case's words select index 0. */
export function publishSample(rig: MachineRig, sp: MachineSpec, c: Case): void {
  const [frames, rate, channels] = c.sample!;
  const left = noise(1, frames);
  const right = channels === 1 ? left : noise(2, frames);
  const poke = (a: number, w: number) => rig.poke(a, w);
  writePlanes(poke, left, right);
  publish(poke, nw(rig.built.data[sp.sampleRecords!]!), 0, frames, rate, channels);
}

/** `RigTest`: a rig's transport and built code, made up by a host test. */
export type RigTest = Pick<MachineRigOptions, 'open' | 'built'>;

export async function runMachineCase(
  sp: MachineSpec,
  c: Case,
  bud: Budget,
  seed: number,
  test: RigTest = {},
): Promise<CaseResult> {
  const t0 = performance.now();
  const rig = await MachineRig.open(sp, { seed, recordStores: true, ...test });
  try {
    if (c.setup === 'sample-pool') publishSample(rig, sp, c);
    rig.setNote(c.note, c.vel);
    let worstSteady = 0;
    let worstNoteon = 0;
    const trig = new Set(c.trig);
    const noteonBlocks = new Set(trig);
    if (sp.device === 'dn2') for (const k of trig) noteonBlocks.add(k + 1);
    let failure: string | null = null;
    let k = 0;
    for (; k < c.blocks; k++) {
      const words = wordsAt(c, k);
      rig.setParams(words);
      if (trig.has(k)) rig.trigger();
      const b = await rig.block();
      const where = describeMachine(c.group, c.name, k, words, c.note, c.vel);
      failure = machineBlockFailure(where, b, rig.stepLimit, rig.lastCtx);
      if (failure === null) {
        const noteOn = noteonBlocks.has(k);
        if (noteOn) worstNoteon = Math.max(worstNoteon, b.instructions);
        else worstSteady = Math.max(worstSteady, b.instructions);
        failure = machineBudgetFailure(where, b.instructions, noteOn, bud);
      }
      if (failure) break;
    }
    return {
      group: c.group,
      name: c.name,
      blocks: Math.min(k, c.blocks - 1) + 1,
      steady: worstSteady,
      noteon: worstNoteon,
      failure: failure ? `${machineLabel(sp)} ${failure}` : null,
      seconds: seconds(t0),
    };
  } finally {
    await rig.close();
  }
}

export const POLY_BLOCKS = 12;
export const POLY_NOTES: [number, number][] = [
  [24, 60],
  [60, 100],
  [100, 127],
];

/**
 * Two voices on one core, notes `notes` (voice 1 starts four blocks late), all parameters at their defaults, then at their maxima from
 * block 6, then at their minima from block 9. A store of one voice into the other's state, or past either, fails.
 */
export async function runPoly(
  sp: MachineSpec,
  notes: [number, number],
  seed: number,
  test: RigTest = {},
): Promise<CaseResult> {
  const t0 = performance.now();
  const rig = await MachineRig.open(sp, { seed, recordStores: true, ...test });
  try {
    rig.select(0);
    rig.select(1);
    let failure: string | null = null;
    let k = 0;
    for (; k < POLY_BLOCKS && !failure; k++)
      for (const t of [0, 1]) {
        const level = k >= 6 && k < 9 ? 'hi' : k >= 9 ? 'lo' : null;
        rig.use(t);
        if (level) rig.setParams(sp.params.map((p) => [p.id, level === 'hi' ? p.hi : p.lo] as const));
        if (k === 4 * t) {
          rig.setNote(notes[t]!, 100);
          rig.trigger();
        }
        if (k < 4 * t) continue; // voice 1 has not started
        const b = await rig.block();
        if (!b.returned || b.foreign.length) {
          const why = b.foreign.join('; ') || 'the call did not return';
          failure = `${machineLabel(sp)} case writes/two-voices/notes${notes[0]},${notes[1]} block ${k} voice ${t}: the module wrote memory it does not own (${why})`;
          break;
        }
      }
    return {
      group: 'writes',
      name: `two-voices/notes${notes[0]},${notes[1]}`,
      blocks: k,
      steady: 0,
      noteon: 0,
      failure,
      seconds: seconds(t0),
    };
  } finally {
    await rig.close();
  }
}

/** The cases of one machine module on one device. */
export async function conformMachine(sp: MachineSpec, o: ConformOptions = {}): Promise<MachineReport> {
  const bud = budget(sp.device);
  const seed = o.seed ?? SEED;
  const t0 = performance.now();
  buildMachine(sp); // assembles and links once; the cases reuse it
  const built = (performance.now() - t0) / 1000;
  const groups = o.groups ?? [];
  const cases: CaseResult[] = [];
  for (const c of machineCases(sp))
    if (!groups.length || groups.includes(c.group))
      cases.push(await runMachineCase(sp, c, bud, seed, { open: o.open }));
  if (!groups.length || groups.includes('writes'))
    for (const notes of POLY_NOTES) cases.push(await runPoly(sp, notes, seed, { open: o.open }));
  return {
    label: machineLabel(sp),
    runner: RUNNER,
    build_seconds: Math.round(built * 10) / 10,
    budget: { voice_block: bud.voiceBlock, note_on_block: bud.noteOnBlock, headroom: bud.headroom },
    params: sp.params.map((p) => p.id),
    cases,
  };
}

export type FxRigTest = Pick<FxRigOptions, 'open' | 'built'>;

export async function runFxCase(
  sp: FxSpec,
  c: FxCase,
  effectBudget: number,
  desc: number[],
  seed: number,
  test: FxRigTest = {},
): Promise<CaseResult> {
  const t0 = performance.now();
  const rig = await FxRig.open([{ pc: sp.process, state: sp.state, desc: sp.desc }], {
    sources: sp.sources,
    seed,
    recordStores: true,
    ...test,
  });
  try {
    const rng = new PyRandom(seed);
    const stack = desc[5]!;
    let worst = 0;
    let failure: string | null = null;
    let on = true;
    let faults = 0; // the row's fault count before the block (a fresh table holds 0)
    let k = 0;
    for (; k < c.blocks; k++) {
      const raw = fxWordsAt(c, k);
      const words = raw.map((w, j) => fxWord(sp, j, w));
      words.forEach((w, j) => rig.setRowWord(0, P0 + j, w));
      if (c.toggle && k && k % c.toggle === 0) {
        on = !on;
        rig.setEnabled(0, on);
      }
      const b = await rig.block(signalBlock(c.signal, k, rng));
      const where = describeFx(c.group, c.name, k, c.signal, words);
      failure = fxBlockFailure(where, b, rig.stepLimit, faults, () => rig.foreignStores(stack, b.stores));
      faults = b.faults[0]!;
      if (failure === null) {
        worst = Math.max(worst, b.instructions);
        failure = fxBudgetFailure(where, b.instructions, effectBudget);
      }
      if (failure) break;
    }
    return {
      group: c.group,
      name: c.name,
      blocks: Math.min(k, c.blocks - 1) + 1,
      steady: worst,
      noteon: 0,
      failure: failure ? `${fxLabel(sp)} ${failure}` : null,
      seconds: seconds(t0),
    };
  } finally {
    await rig.close();
  }
}

/** The problems of an effect's description (`dk_fx_desc` words): magic, contract, parameter count, state size. */
export function descriptionProblems(desc: readonly number[]): string[] {
  const problems: string[] = [];
  if (desc[0] !== 0x444b4658) problems.push('the description has no DK_FX_MAGIC');
  if (desc[1] !== 1) problems.push(`contract ${desc[1]} is not DK_FX_V1`);
  if (desc[2]! > 8) problems.push(`${desc[2]} parameters, at most 8`);
  if (desc[3]! % 4) problems.push(`state_bytes ${desc[3]} is not a multiple of 4`);
  return problems;
}

/** The cases of one effect module. */
export async function conformFx(sp: FxSpec, o: ConformOptions = {}): Promise<FxReport> {
  const t0 = performance.now();
  const built = buildFx(sp.sources);
  const desc = descriptionOf(built, sp.desc);
  const buildSeconds = (performance.now() - t0) / 1000;
  const problems = descriptionProblems(desc);
  const effectBudget = fxBudget();
  const seed = o.seed ?? SEED;
  const groups = o.groups ?? [];
  const cases: CaseResult[] = [];
  if (problems.length)
    cases.push({
      group: 'signals',
      name: 'description',
      blocks: 0,
      steady: 0,
      noteon: 0,
      failure: `${fxLabel(sp)} ${problems.join('; ')}`,
      seconds: 0,
    });
  else
    for (const c of fxCases(desc[2]!))
      if (!groups.length || groups.includes(c.group))
        cases.push(await runFxCase(sp, c, effectBudget, desc, seed, { open: o.open }));
  return {
    label: fxLabel(sp),
    runner: RUNNER,
    build_seconds: Math.round(buildSeconds * 10) / 10,
    budget: { effect: effectBudget, adapter: FX_ADAPTER_MAX },
    n_params: desc[2]!,
    cases,
  };
}

function descriptionOf(built: ReturnType<typeof buildFx>, symbol: string, n = 6): number[] {
  const at = built.data[symbol];
  for (const init of built.inits)
    if (at !== undefined && init.address <= at && at < init.address + init.bytes.length) {
      const view = new DataView(init.bytes.buffer, init.bytes.byteOffset, init.bytes.byteLength);
      return Array.from({ length: n }, (_, k) => view.getUint32(at - init.address + 4 * k, true));
    }
  throw new Error(`no data symbol ${symbol}`);
}
