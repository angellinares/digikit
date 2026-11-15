/**
 * The conformance cases (port of `cases` and `fx_cases` of tools/patchkit/pk_conform.py): what every machine and every effect must
 * pass. The random walks use the generator of Python's `random.Random` with the old seeds, so the cases are the ones the modules'
 * recorded costs come from.
 */
import { PyRandom } from './prng.ts';
import { FX_PARAM_MAX, paramMid, type MachineSpec } from './spec.ts';

export const NOTES_LIMITS = [0, 24, 48, 60, 72, 96, 127];
export const NOTES_CORNERS = [0, 60, 127];
export const SETTLE = 3; // blocks run after the note-on block in the short cases
export const WALK_BLOCKS = 500;
export const LONG_BLOCKS = 2000;
export const RETRIG_EVERY = 7;
export const RETRIG_BLOCKS = 280;

/** (parameter id, word) pairs, set before a block. */
export type Words = [string, number][];

/** A run on a fresh rig: `blocks` blocks, block k with the parameter words of `words[k]` (a single entry: every block); a trigger at the blocks of `trig`. */
export interface Case {
  group: string;
  name: string;
  note: number;
  vel: number;
  words: Words[];
  blocks: number;
  trig: number[];
  /** a fixture the rig sets up before the first block ("sample-pool") */
  setup?: string;
  /** (frames, source rate in Hz, channels) of the sample the "sample-pool" setup publishes */
  sample?: [number, number, number];
}

export const wordsAt = (c: Case, k: number): Words => (c.words.length > 1 ? c.words[k]! : c.words[0]!);

const defaults = (sp: MachineSpec): Words => sp.params.map((p) => [p.id, p.default]);

/** `vals` with the word of `id` replaced (the order of the others stays). */
const withWord = (vals: Words, id: string, word: number): Words => vals.map(([i, w]) => [i, i === id ? word : w]);

/** A seeded random walk over each parameter's range, reflected at the limits; `skip` leaves one parameter out. */
function walk(sp: MachineSpec, note: number, lead: Words = [], skip?: string): Words[] {
  const rng = new PyRandom(0xc0ffee + note);
  const pos = new Map(sp.params.map((p) => [p.id, p.default]));
  const rows: Words[] = [];
  for (let b = 0; b < WALK_BLOCKS; b++) {
    const row: Words = [...lead];
    for (const p of sp.params) {
      if (p.id === skip) continue;
      let x = pos.get(p.id)! + rng.uniform(-0.6, 0.6) * (p.hi - p.lo);
      if (x < p.lo) x = 2 * p.lo - x;
      if (x > p.hi) x = 2 * p.hi - x;
      x = Math.min(Math.max(x, p.lo), p.hi);
      pos.set(p.id, x);
      row.push([p.id, Math.trunc(x)]);
    }
    rows.push(row);
  }
  return rows;
}

export function machineCases(sp: MachineSpec): Case[] {
  const out: Case[] = [];
  const dflt = defaults(sp);
  const staticCase = (group: string, name: string, note: number, vals: Words, blocks = 1 + SETTLE): Case => ({
    group,
    name,
    note,
    vel: 100,
    words: [vals],
    blocks,
    trig: [0],
  });
  // 1. limits: every parameter at min, max, default and mid, the others at default, per note
  for (const note of NOTES_LIMITS) {
    out.push(staticCase('limits', `all-default/note${note}`, note, dflt));
    for (const p of sp.params)
      for (const [tag, w] of [
        ['min', p.lo],
        ['max', p.hi],
        ['mid', paramMid(p)],
      ] as const) {
        if (w === p.default) continue;
        out.push(staticCase('limits', `${p.id}=${tag}/note${note}`, note, withWord(dflt, p.id, w)));
      }
  }
  // 2. corners: all parameters together at {min, max}, notes {0, 60, 127}
  const n = sp.params.length;
  for (const note of NOTES_CORNERS)
    for (let bits = 0; bits < 1 << n; bits++) {
      const vals: Words = sp.params.map((p, i) => [p.id, (bits >> i) & 1 ? p.hi : p.lo]);
      const tag = sp.params.map((_, i) => ((bits >> i) & 1 ? 'H' : 'L')).join('') || '-';
      out.push(staticCase('corners', `corner-${tag}/note${note}`, note, vals));
    }
  // 3. per-block modulation: every parameter moved each block, a seeded random walk over its full range, and a square wave between
  //    its limits (an LFO at the highest speed and depth)
  for (const note of [24, 100])
    out.push({
      group: 'modulation',
      name: `random-walk/note${note}`,
      note,
      vel: 100,
      words: walk(sp, note),
      blocks: WALK_BLOCKS,
      trig: [0],
    });
  const square: Words[] = Array.from({ length: WALK_BLOCKS }, (_, k) =>
    sp.params.map((p, i): [string, number] => [p.id, (k + i) % 2 ? p.hi : p.lo]),
  );
  out.push({
    group: 'modulation',
    name: 'square-every-block/note60',
    note: 60,
    vel: 100,
    words: square,
    blocks: WALK_BLOCKS,
    trig: [0],
  });
  // 4. long note, retriggers, velocity limits
  out.push({
    group: 'long',
    name: 'long-note/note60',
    note: 60,
    vel: 100,
    words: [dflt],
    blocks: LONG_BLOCKS,
    trig: [0],
  });
  for (const vel of [1, 127])
    out.push({
      group: 'retrigger',
      name: `retrigger-every-${RETRIG_EVERY}/vel${vel}`,
      note: 60,
      vel,
      words: [dflt],
      blocks: RETRIG_BLOCKS,
      trig: Array.from({ length: Math.ceil(RETRIG_BLOCKS / RETRIG_EVERY) }, (_, i) => i * RETRIG_EVERY),
    });
  // 5. a machine that plays a pool sample: the same shapes with a sample published (setup "sample-pool"), so the render path runs
  if (sp.fixtures.includes('sample-pool') && sp.sampleRecords) out.push(...sampleCases(sp, dflt));
  return out;
}

// The `sample` group. PLAY words are mode * 256 (the module README): REV 0, REV.L 1, FWD.L 2, FWD 3.
export const SAMPLE_RATE_HZ = 48000;
export const SAMPLE_FRAMES = 16000; // 8000 packed words per plane: the two test planes are 0x8000 bytes apart
const SAMPLE_PLAY = { 'rev-loop': 1 << 8, 'fwd-loop': 2 << 8, fwd: 3 << 8 };

function sampleCases(sp: MachineSpec, dflt: Words): Case[] {
  const byId = new Map(sp.params.map((p) => [p.id, p]));
  if (!['sample', 'start', 'end', 'play', 'pitch'].every((id) => byId.has(id))) return [];
  const out: Case[] = [];
  const add = (
    name: string,
    note: number,
    words: Words[],
    blocks: number,
    { vel = 100, trig = [0], channels = 2, frames = SAMPLE_FRAMES } = {},
  ): void => {
    out.push({
      group: 'sample',
      name,
      note,
      vel,
      words,
      blocks,
      trig,
      setup: 'sample-pool',
      sample: [frames, SAMPLE_RATE_HZ, channels],
    });
  };
  // sample 0 (the pool index the worker publishes), the rest of the module's parameters at their defaults
  const one = (over: Record<string, number>): Words[] => {
    const merged = new Map<string, number>([...dflt, ['sample', 0]]);
    for (const [k, v] of Object.entries(over)) merged.set(k, v);
    return [[...merged]];
  };
  const end = byId.get('end')!.hi;
  const fwd = { start: 0, end, play: SAMPLE_PLAY.fwd, pitch: 0x4000 };
  add('stereo-fwd-oneshot/note60', 60, one(fwd), 300);
  add('mono-fwd-oneshot/note60', 60, one(fwd), 300, { channels: 1 });
  // frame = round(frames * word / 0x7f00); a block is 32 frames. END 2000 is frame 984: the cursor passes it after about 31 blocks, so the end ramp runs
  add('fwd-end-reached/note60', 60, one({ ...fwd, end: 2000 }), 120);
  // START 256..END 512 is frames 126..252: the cursor wraps every 4 blocks
  const loop = { start: 256, end: 512 };
  add('fwd-loop/note60', 60, one({ ...fwd, ...loop, play: SAMPLE_PLAY['fwd-loop'] }), 300);
  add('rev-loop/note60', 60, one({ ...fwd, ...loop, play: SAMPLE_PLAY['rev-loop'] }), 300);
  const pitch = byId.get('pitch')!;
  add('pitch-min/note0', 0, one({ ...fwd, pitch: pitch.lo }), 200);
  add('pitch-max/note127', 127, one({ ...fwd, pitch: pitch.hi }), 200);
  for (const vel of [1, 127])
    add(`retrigger-every-${RETRIG_EVERY}/vel${vel}`, 60, one(fwd), RETRIG_BLOCKS, {
      vel,
      trig: Array.from({ length: Math.ceil(RETRIG_BLOCKS / RETRIG_EVERY) }, (_, i) => i * RETRIG_EVERY),
    });
  // the random walk of group 3, over every parameter but the sample index (a walk of it would leave the published pool entry)
  for (const note of [24, 100])
    add(`random-walk/note${note}`, note, walk(sp, note, [['sample', 0]], 'sample'), WALK_BLOCKS);
  return out;
}

// ---- effects ----------------------------------------------------------------------------------------------------------------

export const FX_SIGNALS = ['silence', 'dc', 'square', 'noise', 'impulse', 'sine', 'ramp'] as const;
export type FxSignal = (typeof FX_SIGNALS)[number];

/** A run on a fresh rig: `blocks` blocks of `signal`, block k with the parameter words `words[k]` (a single entry: every block); `toggle`: the effect is switched off and on every that many blocks (0: never). */
export interface FxCase {
  group: string;
  name: string;
  signal: FxSignal;
  blocks: number;
  words: number[][];
  toggle: number;
}

export const fxWordsAt = (c: FxCase, k: number): number[] => (c.words.length > 1 ? c.words[k]! : c.words[0]!);

export function fxCases(nParams: number): FxCase[] {
  const fill = (v: number): number[] => Array<number>(nParams).fill(v);
  const zero = fill(0);
  const top = fill(FX_PARAM_MAX);
  const mid = fill((FX_PARAM_MAX >> 1) & ~0xff);
  const out: FxCase[] = [];
  const add = (group: string, name: string, signal: FxSignal, blocks: number, words: number[][], toggle = 0): void => {
    out.push({ group, name, signal, blocks, words, toggle });
  };
  for (const sig of FX_SIGNALS) add('signals', sig, sig, 24, [zero]);
  for (const [tag, w] of [
    ['min', zero],
    ['max', top],
    ['mid', mid],
  ] as const)
    for (const sig of ['noise', 'square'] as const) add('params', `all-${tag}/${sig}`, sig, 24, [w]);
  for (let j = 0; j < nParams; j++)
    for (const [tag, v] of [
      ['min', 0],
      ['max', FX_PARAM_MAX],
    ] as const)
      add('params', `p${j}=${tag}/noise`, 'noise', 24, [
        Array.from({ length: nParams }, (_, i) => (i === j ? v : (FX_PARAM_MAX >> 1) & ~0xff)),
      ]);
  const rng = new PyRandom(0xfeed);
  const randomWalk = Array.from({ length: WALK_BLOCKS }, () =>
    Array.from({ length: nParams }, () => rng.randRange(0, FX_PARAM_MAX + 1)),
  );
  add('modulation', 'random-walk/noise', 'noise', WALK_BLOCKS, randomWalk);
  const square = Array.from({ length: WALK_BLOCKS }, (_, k) =>
    Array.from({ length: nParams }, (_, i) => ((k + i) % 2 ? FX_PARAM_MAX : 0)),
  );
  add('modulation', 'square-every-block/noise', 'noise', WALK_BLOCKS, square);
  add('switching', 'toggle-every-7/noise', 'noise', 300, [mid], 7);
  add('switching', 'toggle-every-1/square', 'square', 60, [mid], 1);
  add('long', 'long/noise', 'noise', LONG_BLOCKS, [mid]);
  return out;
}
