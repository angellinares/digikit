/**
 * The stage table and the harness of the DN2 R twins, for dsp-voice-stereo-twins.rig.test.ts,
 * dsp-voice-stereo-identity.rig.test.ts and dsp-voice-stereo-cost.rig.test.ts.
 *
 * A `Stage` names a twin, the stock stage behind it, the L record, the words the stage shares between the channels (`controls`, copied
 * before the L call) and the words each channel keeps (`histories`). `runChain` runs the twin on a stamped slot next to two reference
 * cores that run the stock function alone, one for each channel; `variantR` computes R by hand with the stock function (the negative
 * controls). The original firmware is read at run time, never copied into a fixture.
 */
import { readFileSync } from 'node:fs';
import path from 'node:path';
import {
  BACK,
  BUFFERS,
  ENGINE,
  STEREO_FRAME,
  HEARTBEAT,
  REGS,
  ROOT,
  STOCK,
  bits,
  call,
  filled,
  f32,
  newSession,
  PyRandom,
  value,
  type StereoRig,
  type StereoSession,
} from '@digi/sharc-rig-kit';

export const SLOT = 5;
// ADM_RING_R_BASE of the generated adapter constants ([adapter.sample_pool] ring_r_base): the 16 R rings of 32 KiB
const inc = readFileSync(path.join(ROOT, 'modules/core/adapters/dsp-voice/build/gen/adapter-dn2.inc'), 'utf8');
export const RING_R_BASE = Number(/#define ADM_RING_R_BASE (\d+)/.exec(inc)![1]);
export const R_RING = RING_R_BASE + 0x8000 * SLOT;
export const RING_L = 0x80100000; // the L ring of the L record in the tests (any DDR address of the model)
export const RING_VARIANT = 0x80200000; // the R ring of a hand-made R pass
export const RING_WORDS = 8192;
export const LBUF = BUFFERS + 0x80 * SLOT;
export const SCRATCH = BUFFERS + 0x1000; // the L scratch block of case 5

export type Prime = (session: StereoSession, who: string, address: number) => void;

export interface StageExtra {
  rrec: string;
  /** control word -> [base, spread per block] */
  values?: Record<number, [number, number]>;
  words?: readonly number[];
  private?: Record<number, number>;
  prime?: Prime;
  ringWord?: number;
  dry?: boolean;
}

export interface Stage {
  name: string;
  entry: string;
  stock: string;
  /** L record of slot v: record + stride * v (byte address) */
  record: number;
  stride: number;
  /** words */
  size: number;
  /** shared control words, rewritten before every block */
  controls: number[];
  /** words the stage keeps per channel */
  histories: number[];
  /** word -> float: the L record at the start (and the R record's histories are zero) */
  initial: Record<number, number>;
  args: number[];
  /** R12 is a scratch output block and a blend follows (case 5) */
  scratch: boolean;
  blocks: number;
  /** gain: the first stack argument of each block (1 restarts the attack) */
  controlsPerBlock: number[];
  /** words of the R record compared with the reference (default: controls and histories) */
  checked: number[];
  /** the late shaper: the engine scales L by 16 before the stage and by 1/16 after it, the twin does the same on R */
  rScale: boolean;
  extra: StageExtra;
}

const keys = (o: Record<number, unknown>): number[] => Object.keys(o).map(Number);

function stage(s: Partial<Stage> & Pick<Stage, 'name' | 'entry' | 'stock' | 'record' | 'stride' | 'size' | 'controls' | 'histories' | 'initial' | 'extra'>): Stage {
  return { args: [32], scratch: false, blocks: 4, controlsPerBlock: [], checked: [], rScale: false, ...s };
}

export function argsOf(s: Stage, block: number): number[] {
  if (s.controlsPerBlock.length === 0) return s.args;
  return [s.controlsPerBlock[block % s.controlsPerBlock.length]!, ...s.args.slice(1)];
}

/** The private start of the R record (word -> raw value) and of the right reference: zero histories, or the stage's own `private` words. */
export function privateOf(s: Stage): Record<number, number> {
  return s.extra.private ?? Object.fromEntries(s.histories.map((i) => [i, 0]));
}

export const lRecord = (s: Stage, v = SLOT): number => s.record + s.stride * v;
export const rRecord = (rig: StereoRig, s: Stage, v = SLOT): number => rig.data[s.extra.rrec]! + s.stride * v;

export const STAGES: Record<string, Stage> = {};
const add = (s: Stage): Stage => (STAGES[s.name] = s);

add(
  stage({
    name: 'first-order',
    entry: '_gs_fo',
    stock: '_gs_fo_stock',
    record: ENGINE + 0xe088,
    stride: 112,
    size: 28,
    controls: [21, 22, 23],
    histories: [24, 25],
    initial: { 21: 0.7, 22: 0.2, 23: 0.1, 24: 0.1, 25: -0.05, 26: 0.3, 27: -0.2 },
    extra: { rrec: '_gs_fo_rec', values: { 21: [0.7, -0.05], 22: [0.2, 0.01], 23: [0.1, 0.02] } },
  }),
);
add(
  stage({
    name: 'uy',
    entry: '_gs_uy',
    stock: '_gs_uy_stock',
    record: ENGINE + 0x12148,
    stride: 32,
    size: 8,
    controls: [2, 3, 4, 5],
    histories: [6, 7],
    initial: { 2: 0.6, 3: 0.3, 4: 0.2, 5: 0.4, 6: 0.02, 7: -0.03 },
    extra: { rrec: '_gs_uy_rec', values: { 2: [0.6, 0.02], 3: [0.3, -0.01], 4: [0.2, 0.0], 5: [0.4, 0.03] } },
  }),
);

export const BLEND = ENGINE + 0x13e9c + 0x38 * SLOT; // the blend's 14-word ramp record (I0 of the engine at the case 5 call)
export const BLEND_ENTRY = 0x1c9c12;
export const BLEND_STOP = 0x1c98b9;
const C5_BASE = [0.6, 0.1, 0.15, 0.2, 0.1, 0.08];
const CASE5: Record<number, number> = { 9: 0.02, 10: -0.03 };
const CASE5_VALUES: Record<number, [number, number]> = {};
for (let k = 0; k < 6; k++)
  for (const [d, v] of [
    [0, C5_BASE[k]!],
    [2, 0.0001],
  ] as const) {
    CASE5[11 + 7 * k + d] = v;
    CASE5_VALUES[11 + 7 * k + d] = [v * (1 + 0.1 * k), v * 0.05];
  }
add(
  stage({
    name: 'gain',
    entry: '_gs_gain',
    stock: '_gs_gain_stock',
    record: ENGINE + 0x13508,
    stride: 44,
    size: 11,
    controls: [],
    histories: [],
    initial: {},
    args: [0, 0, 32],
    blocks: 9,
    controlsPerBlock: [1, 0, 0, 0, 0, 1, 0, 0, 0],
    extra: { rrec: '_gs_gain_rec', words: [7, 1, 7, bits(0.5), 7, 1, 0, 5, 0, 0, 0] },
  }),
);
add(
  stage({
    name: 'case5',
    entry: '_gs_c5',
    stock: '_gs_c5_stock',
    record: ENGINE + 0x127c8,
    stride: 212,
    size: 53,
    controls: keys(CASE5_VALUES),
    histories: [9, 10],
    initial: CASE5,
    scratch: true,
    extra: { rrec: '_gs_c5_rec', values: CASE5_VALUES },
  }),
);

/** The engine's scaling of a block by a power of two (exact, so the float32 product is the core's). */
export function scaled(block: readonly number[], factor: number): number[] {
  return block.map((w) => bits(f32(value(w) * factor)));
}

const SHAPER_VALUES: Record<number, [number, number]> = { 0: [0.6, 0.05], 1: [0.2, 0.0], 2: [0.5, 0.04] };
const SHAPER_INITIAL = { 0: 0.6, 1: 0.2, 2: 0.5, 5: 0.4, 12: 0.3 };
for (const late of [false, true])
  add(
    stage({
      name: late ? 'shaper-late' : 'shaper-early',
      entry: late ? '_gs_shaper_late' : '_gs_shaper_early',
      stock: '_gs_shaper_stock',
      record: ENGINE + 0xe788,
      stride: 96,
      size: 24,
      controls: [0, 1, 2],
      histories: [],
      initial: SHAPER_INITIAL,
      checked: [0, 1, 2, 4, 5, 6, 11, 12, 13],
      rScale: late,
      blocks: 6,
      extra: { rrec: '_gs_sh_rec', values: SHAPER_VALUES },
    }),
  );
add(
  stage({
    name: 'held',
    entry: '_gs_held_early',
    stock: '_gs_held_stock',
    record: ENGINE + 0xeec8,
    stride: 20,
    size: 5,
    controls: [0],
    histories: [1, 2],
    initial: { 0: 0.25 },
    checked: [0, 1, 2, 3, 4],
    blocks: 5,
    extra: { rrec: '_gs_hd_rec', values: { 0: [32 / 127, 16 / 127] }, words: [0, 0, 0, 1, 1] },
  }),
);
add({ ...STAGES.held!, name: 'held-late', entry: '_gs_held_late' });

// dispatch cases 1, 2 and 3: the controls and values of the replay fixtures (small finite coefficients, a nonzero step each block)
const C1_CONTROLS = [5, 6, 7, 12, 14, 26, 28, 33, 35, 40, 42];
const C1_BASE = [0.2, 0.3, 0.1, 0.4, 0.0002, 0.2, 0.0001, 0.1, 0.0001, 0.1, 0.0001];
const zip = <T>(a: readonly number[], b: readonly T[]): Record<number, T> => Object.fromEntries(a.map((k, i) => [k, b[i]!]));
add(
  stage({
    name: 'case1',
    entry: '_gs_case1',
    stock: '_gs_case1_stock',
    record: ENGINE + 0xf308,
    stride: 204,
    size: 51,
    controls: C1_CONTROLS,
    histories: [3, 4, 48, 49, 50],
    initial: { ...zip(C1_CONTROLS, C1_BASE), 3: 0.01, 4: -0.02 },
    extra: { rrec: '_gs_c1_rec', values: zip(C1_CONTROLS, C1_BASE.map((v) => [v, v / 16] as [number, number])) },
  }),
);
const C2_CONTROLS: number[] = [];
for (let k = 0; k < 9; k++) for (const d of [0, 2]) C2_CONTROLS.push(12 + 7 * k + d);
const C2_BASE = [0.1, 0.2, 0.1, 0.1, 0.05, 0.05, 0.1, 0.1, 0.1].flatMap((v0) => [v0, 0.0001]);
add(
  stage({
    name: 'case2',
    entry: '_gs_case2',
    stock: '_gs_case2_stock',
    record: ENGINE + 0xffc8,
    stride: 300,
    size: 75,
    controls: C2_CONTROLS,
    histories: [10, 11],
    initial: { ...zip(C2_CONTROLS, C2_BASE), 10: 0.01, 11: -0.02 },
    extra: { rrec: '_gs_c2_rec', values: zip(C2_CONTROLS, C2_BASE.map((v) => [v, v / 16] as [number, number])) },
  }),
);
const C3_CONTROLS: Record<number, number> = { 0: 0.12, 1: 0.32, 2: 0.1, 3: 0.12, 4: 0.65, 14: 0.22 };
const C3_WRITES: number[] = [];
for (let k = 0; k < 6; k++) for (const d of [0, 1, 2]) C3_WRITES.push(17 + 7 * k + d);
const C3_CURRENT = [0.32, 0.12, 0.1 / 4, 0.12, 0.65, -0.22 / 8];
const c3Initial: Record<number, number> = { ...C3_CONTROLS };
C3_CURRENT.forEach((v, k) => {
  for (const d of [0, 1]) c3Initial[17 + 7 * k + d] = v;
});
Object.assign(c3Initial, { 5: 0.004, 9: 0.003, 10: -0.002, 11: 0.001, 12: -0.003 });
add(
  stage({
    name: 'case3',
    entry: '_gs_case3',
    stock: '_gs_case3_stock',
    record: ENGINE + 0x11288,
    stride: 236,
    size: 59,
    controls: keys(C3_CONTROLS),
    histories: [5, 9, 10, 11, 12],
    initial: c3Initial,
    checked: [...keys(C3_CONTROLS), 5, 9, 10, 11, 12, ...C3_WRITES],
    blocks: 3,
    extra: {
      rrec: '_gs_c3_rec',
      values: Object.fromEntries(Object.entries(C3_CONTROLS).map(([i, v]) => [i, [v, v / 16]])),
    },
  }),
);

// the ring stage (cases 4 and 6): the records of a ring after the stock reset (history and cursor 0, fill status 1) or of a filled ring near its
// wrap. Word 16 is the byte pointer of the ring; the L ring is at RING_L, the twin's R ring at R_RING.
export const ringPattern = (channel: number): number[] =>
  Array.from({ length: RING_WORDS }, (_, i) => bits(((i % 97) - 48) / (512 + 97 * channel)));

/** Prime function of a ring stage: who is stereo-l, stereo-r, left, right (the reference cores) or variant-l, variant-r (a hand-made pass); a
 * ring is a pattern (unequal for the two channels) or zeros. `referenceFill` is the right reference's ring when it differs from the twin's R ring. */
export function ringPrime(lFill = 'pattern', rFill = 'pattern', referenceFill?: string): Prime {
  return (session, who, address) => {
    const rightSide = ['stereo-r', 'right', 'variant-r'].includes(who);
    const fill = who === 'stereo-r' ? rFill : rightSide ? (referenceFill ?? rFill) : lFill;
    session.pokeWords(address, fill === 'pattern' ? ringPattern(rightSide ? 1 : 0) : filled(RING_WORDS, 0));
  };
}

function ringWords(cursor: number, coordinate: number, status: number): number[] {
  const w = filled(18, 0);
  [w[6], w[7], w[8], w[9]] = [0x3f7eb852, 0x20, 0x20, 0x3d000000];
  [w[11], w[12]] = [0x44fa0000, 0x473b8000];
  [w[13], w[14], w[15]] = [bits(coordinate), cursor, bits(32.0)];
  [w[16], w[17]] = [RING_L, status];
  return w;
}

function ringStage(name: string, entry: string, cursor: number, coordinate: number, status: number, prime: Prime): Stage {
  return stage({
    name,
    entry,
    stock: '_gs_ring_stock',
    record: ENGINE + 0x12348,
    stride: 72,
    size: 18,
    controls: [2, 10],
    histories: [0, 13, 14, 16, 17],
    initial: { 2: 0.5, 10: 0.5, 3: 0.5, 4: 0.5 },
    checked: [0, 2, 3, 4, 5, 10, 13, 14, 17],
    blocks: 4,
    extra: {
      rrec: '_gs_rg_rec',
      values: { 2: [0.5, 0.0], 10: [0.5, 1 / 32] },
      words: ringWords(cursor, coordinate, status),
      private: { 0: 0, 13: bits(coordinate), 14: cursor, 17: status },
      prime,
      ringWord: 16,
      // a ring with fill status 1 has not been read yet: the block passes dry
      dry: status === 1,
    },
  });
}

add(ringStage('ring4', '_gs_ring4', 0, 8160.0, 1, ringPrime('pattern', 'pattern')));
add({ ...STAGES.ring4!, name: 'ring6', entry: '_gs_ring6' });
add(ringStage('ring-wrap', '_gs_ring4', 8190, 8158.0, 0, ringPrime('pattern', 'pattern')));
// a dirty R ring against a clean reference ring: only the fill status can hide the difference
add(ringStage('ring-dirty-r', '_gs_ring4', 0, 8160.0, 1, ringPrime('pattern', 'pattern', 'zero')));
// the same with a ring that has been written once (fill status 0): the dirty backing is audible, so R differs from the clean reference
export const DIRTY_FILLED = ringStage('ring-dirty-r-filled', '_gs_ring4', 8190, 8158.0, 0, ringPrime('pattern', 'pattern', 'zero'));

export function initRecord(
  s: StereoSession,
  address: number,
  size: number,
  initial: Record<number, number>,
  words?: readonly number[],
): void {
  s.pokeWords(address, words && words.length ? words : filled(size, 0));
  for (const [i, v] of Object.entries(initial)) s.poke(address + 4 * Number(i), bits(v));
}

/** The caller's continuation after the case 5 call: the stock blend 0x1c9c12 to 0x1c98b9 on the buffer and the scratch block. */
export async function runBlend(s: StereoSession, buffer: number, scratch = SCRATCH): Promise<void> {
  s.poke(ENGINE + 0x13808, scratch);
  const regs = { ...REGS, I0: BLEND, I1: ENGINE, I5: buffer, R9: 32, I6: STEREO_FRAME, I7: STEREO_FRAME - 8 };
  const result = await s.raw(BLEND_ENTRY, regs, { maxSteps: 4000, breaks: [BACK, BLEND_STOP] });
  if (result.halt !== 'breakpoint' || result.final_pc !== BLEND_STOP) throw new Error(`the blend did not stop: ${JSON.stringify(result)}`);
}

export const blockInput = (rng: PyRandom, scale = 0.4): number[] => Array.from({ length: 32 }, () => bits(rng.uniform(-scale, scale)));

/** The registers of the caller that the stage's call keeps (case 5: the engine's I0, I1, I5 and R9 at the call, read by the blend that follows). */
export const stageRegs = (s: Stage): Record<string, number> => (s.scratch ? { I0: BLEND, I1: ENGINE, I5: LBUF, R9: 32 } : {});

export function setupBlend(s: StereoSession): void {
  s.pokeWords(BLEND, filled(14, 0));
  for (const [i, v] of Object.entries({ 0: 1.0, 2: -0.01, 7: 0.0, 9: 0.01 })) s.poke(BLEND + 4 * Number(i), bits(v));
}

export interface Observation {
  l: number[];
  l_ref: number[];
  r: number[];
  r_ref: number[];
  l_rec: number[];
  l_rec_ref: number[];
  r_rec: number[];
  r_rec_ref: number[];
  l_in: number[];
  r_in: number[];
  ctrl: number[];
  ring_l?: number[];
  ring_l_ref?: number[];
  ring_r?: number[];
  ring_r_ref?: number[];
}

/** Stereo core with the wrapper, left and right reference cores with the stock function. Returns the three cores' observations per block; the caller closes `stereo`. */
export async function runChain(
  rig: StereoRig,
  st: Stage,
  blocks = st.blocks,
  seed = 1,
): Promise<{ history: Observation[]; stereo: StereoSession }> {
  const rng = new PyRandom(seed);
  const stereo = await newSession(rig);
  const left = await newSession(rig);
  const right = await newSession(rig);
  try {
    const lrec = lRecord(st);
    const rrec = rRecord(rig, st);
    const stockPc = STOCK[st.stock]!;
    const out = st.scratch ? SCRATCH : LBUF;
    const regs = stageRegs(st);
    for (const session of [stereo, left, right]) {
      initRecord(session, lrec, st.size, st.initial, st.extra.words);
      setupBlend(session);
    }
    // R state of the stereo core: what the adapter's clear leaves (histories), unwritten (NaN) words elsewhere
    stereo.pokeWords(rrec, filled(st.size, 0xffffffff));
    const priv = privateOf(st);
    for (const [i, v] of Object.entries(priv)) stereo.poke(rrec + 4 * Number(i), v);
    // the right reference's own record: the same private start, controls as the left ones
    for (const [i, v] of Object.entries(priv)) right.poke(lrec + 4 * Number(i), v);
    const prime = st.extra.prime;
    if (prime) {
      prime(stereo, 'stereo-l', RING_L);
      prime(stereo, 'stereo-r', R_RING);
      prime(left, 'left', RING_L);
      prime(right, 'right', RING_L);
    }
    stereo.poke(rig.data._gs_stamp! + 4 * SLOT, HEARTBEAT);
    const rbuf = rig.data._gs_rbuf! + 128 * SLOT;
    const history: Observation[] = [];
    for (let block = 0; block < blocks; block++) {
      const lIn = blockInput(rng);
      const rIn = blockInput(rng, 0.3);
      for (const session of [stereo, left, right])
        for (const i of st.controls) {
          const [base, spread] = st.extra.values![i]!;
          session.poke(lrec + 4 * i, bits(base + spread * block));
        }
      stereo.pokeWords(LBUF, lIn);
      left.pokeWords(LBUF, lIn);
      stereo.pokeWords(rbuf, rIn);
      right.pokeWords(LBUF, st.rScale ? scaled(rIn, 16.0) : rIn);
      const beforeLCtrl = await stereo.words(lrec, st.size);
      const args = argsOf(st, block);
      await call(stereo, rig.sym[st.entry]!, lrec, LBUF, out, args, { regs });
      await call(left, stockPc, lrec, LBUF, out, args, { regs });
      await call(right, stockPc, lrec, LBUF, out, args, { regs });
      if (st.scratch) for (const session of [stereo, left, right]) await runBlend(session, LBUF);
      const rRef = await right.words(LBUF, 32);
      const h: Observation = {
        l: await stereo.words(LBUF, 32),
        l_ref: await left.words(LBUF, 32),
        r: await stereo.words(rbuf, 32),
        r_ref: st.rScale ? scaled(rRef, 1 / 16) : rRef,
        l_rec: await stereo.words(lrec, st.size),
        l_rec_ref: await left.words(lrec, st.size),
        r_rec: await stereo.words(rrec, st.size),
        r_rec_ref: await right.words(lrec, st.size),
        l_in: lIn,
        r_in: rIn,
        ctrl: beforeLCtrl,
      };
      if (prime) {
        h.ring_l = await stereo.words(RING_L, RING_WORDS);
        h.ring_l_ref = await left.words(RING_L, RING_WORDS);
        h.ring_r = await stereo.words(R_RING, RING_WORDS);
        h.ring_r_ref = await right.words(RING_L, RING_WORDS);
      }
      history.push(h);
    }
    return { history, stereo };
  } catch (error) {
    await stereo.close();
    throw error;
  } finally {
    await Promise.all([left.close(), right.close()]);
  }
}

const SCRATCH_R = SCRATCH + 0x200;
const RREC = 0x20190000; // a separate record for the variants

/** R by hand with the stock function: `early` copies the control words from the L record before the L call (what the twin does, so it must give the twin's R),
 * `late` copies them after the L call, `shared` runs R on the L record. */
export async function variantR(
  rig: StereoRig,
  st: Stage,
  history: readonly Observation[],
  mode: 'early' | 'late' | 'shared',
  omitScale = false,
): Promise<number[][]> {
  const session = await newSession(rig);
  try {
    const lrec = lRecord(st);
    const rrec = mode === 'shared' ? lrec : RREC;
    const regs = stageRegs(st);
    const rScale = st.rScale && !omitScale;
    initRecord(session, lrec, st.size, st.initial, st.extra.words);
    const prime = st.extra.prime;
    if (mode !== 'shared') {
      initRecord(session, rrec, st.size, {}, filled(st.size, 0));
      for (const [i, v] of Object.entries(privateOf(st))) session.poke(rrec + 4 * Number(i), v);
      if (prime) session.poke(rrec + 4 * st.extra.ringWord!, RING_VARIANT);
    }
    if (prime) {
      prime(session, 'variant-l', RING_L);
      prime(session, 'variant-r', RING_VARIANT);
    }
    setupBlend(session);
    const out = st.scratch ? SCRATCH : LBUF;
    const outputs: number[][] = [];
    for (const [block, h] of history.entries()) {
      for (const i of st.controls) {
        const [base, spread] = st.extra.values![i]!;
        session.poke(lrec + 4 * i, bits(base + spread * block));
      }
      session.pokeWords(LBUF, h.l_in);
      let copy = Array.from({ length: st.size }, (_, i) => i).filter((i) => !st.histories.includes(i));
      if (st.size === 5) copy = [0, 3, 4]; // the held stage: the counters are advanced by the call
      const copyControls = async (): Promise<void> => {
        for (const i of copy) session.poke(rrec + 4 * i, (await session.peek(lrec + 4 * i))!);
      };
      if (mode === 'early') await copyControls();
      await call(session, STOCK[st.stock]!, lrec, LBUF, out, argsOf(st, block), { regs });
      if (st.scratch) await runBlend(session, LBUF);
      if (mode === 'late') await copyControls();
      session.pokeWords(LBUF, rScale ? scaled(h.r_in, 16.0) : h.r_in);
      await call(session, STOCK[st.stock]!, rrec, LBUF, st.scratch ? SCRATCH_R : LBUF, argsOf(st, block), { regs });
      if (st.scratch) await runBlend(session, LBUF, SCRATCH_R);
      const outR = await session.words(LBUF, 32);
      outputs.push(rScale ? scaled(outR, 1 / 16) : outR);
    }
    return outputs;
  } finally {
    await session.close();
  }
}
