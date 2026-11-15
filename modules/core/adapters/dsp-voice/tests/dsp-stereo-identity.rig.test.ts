/**
 * With the same audio in L and R, R must equal L bit for bit through every twin of core/dsp-voice on DN2 1.12 (docs/design/dn2-stereo.md,
 * section 3). Port of test_dsp_stereo_identity.py (T1g phase 5b).
 *
 * The stock DT2 engine holds this by construction for a mono sample: both voices of a pair run the same code on the same data. Here it is
 * the whole-chain check on top of the per-twin tests: each stage runs on a stamped slot over several blocks in which its controls change
 * every block, with a reset of the stage in the middle of the note (the engine's reset after a filter change; the reset wrappers mirror it
 * for R), starting from equal private state, and the twin's R block is compared with its L block. A twin that shares a history, copies a
 * control after the L call, or lets a reset reach one channel only, fails. Original firmware is supplied at run time.
 */
import { describe, expect, it } from 'vitest';
import {
  DN2_112,
  HEARTBEAT,
  PyRandom,
  STOCK,
  StereoRig,
  bits,
  call,
  filled,
  newSession,
  stockAvailable,
  stockSkipReason,
} from '@digi/sharc-rig-kit';
import {
  LBUF,
  R_RING,
  RING_L,
  RING_WORDS,
  SCRATCH,
  SLOT,
  STAGES,
  argsOf,
  blockInput,
  initRecord,
  lRecord,
  privateOf,
  rRecord,
  ringPattern,
  runBlend,
  scaled,
  setupBlend,
  stageRegs,
} from './stereo-stages.ts';

// the stages that reset in the middle of a note, and the wrapper that mirrors the reset
const RESET: Record<string, string> = {
  'first-order': '_gs_fo_reset',
  uy: '_gs_uy_reset',
  case5: '_gs_case5_reset',
  held: '_gs_held_reset',
  'held-late': '_gs_held_reset',
  case1: '_gs_case1_reset',
  case2: '_gs_case2_reset',
  case3: '_gs_case3_reset',
  ring4: '_gs_ring_reset',
  ring6: '_gs_ring_reset',
  'ring-wrap': '_gs_ring_reset',
  'ring-dirty-r': '_gs_ring_reset',
};
const BLOCKS = 7;
const RESET_AT = 3; // the reset comes before this block

/** The stage on a stamped slot with equal L and R input and equal private state; returns (L block, R block, audio) per block. */
async function identicalRun(rig: StereoRig, name: string, resetPc: number | undefined, seed = 4) {
  const stage = STAGES[name]!;
  const rng = new PyRandom(seed);
  const session = await newSession(rig);
  try {
    const lrec = lRecord(stage);
    const rrec = rRecord(rig, stage);
    const out = stage.scratch ? SCRATCH : LBUF;
    const regs = stageRegs(stage);
    initRecord(session, lrec, stage.size, stage.initial, stage.extra.words);
    setupBlend(session);
    session.pokeWords(rrec, filled(stage.size, 0xffffffff));
    for (const [i, v] of Object.entries(privateOf(stage))) {
      // equal private start in both records
      session.poke(rrec + 4 * Number(i), v);
      session.poke(lrec + 4 * Number(i), v);
    }
    if (stage.extra.prime) {
      // equal rings
      session.pokeWords(RING_L, ringPattern(0));
      session.pokeWords(R_RING, ringPattern(0));
    }
    session.poke(rig.data._gs_stamp! + 4 * SLOT, HEARTBEAT);
    const rbuf = rig.data._gs_rbuf! + 128 * SLOT;
    const blocks: { left: number[]; right: number[]; audio: number[] }[] = [];
    for (let block = 0; block < BLOCKS; block++) {
      if (resetPc !== undefined && block === RESET_AT) await call(session, resetPc, lrec, 0, 0, []);
      for (const i of stage.controls) {
        const [base, spread] = stage.extra.values![i]!;
        session.poke(lrec + 4 * i, bits(base + spread * block));
      }
      const audio = blockInput(rng);
      session.pokeWords(LBUF, stage.rScale ? scaled(audio, 16.0) : audio);
      session.pokeWords(rbuf, audio);
      await call(session, rig.sym[stage.entry]!, lrec, LBUF, out, argsOf(stage, block), { regs });
      if (stage.scratch) await runBlend(session, LBUF);
      const left = await session.words(LBUF, 32);
      blocks.push({ left: stage.rScale ? scaled(left, 1 / 16) : left, right: await session.words(rbuf, 32), audio });
    }
    return blocks;
  } finally {
    await session.close();
  }
}

const enabled = stockAvailable(DN2_112);
const suite = enabled ? describe : describe.skip;
const title = 'R equals L bit for bit through every twin (DN2 1.12)';

suite(enabled ? title : `${title}: ${stockSkipReason(DN2_112)}`, () => {
  const rig = enabled ? StereoRig.build() : (undefined as never);

  it.each(Object.keys(STAGES))('with equal input R equals L bit for bit: %s', async (name) => {
    const stage = STAGES[name]!;
    const blocks = await identicalRun(rig, name, RESET[name] ? rig.sym[RESET[name]!] : undefined);
    blocks.forEach(({ left, right }, n) => {
      expect(left, `block ${n}`).toEqual(right);
      expect(
        left.some((w) => (w & 0x7fffffff) !== 0),
        `block ${n} is silent`,
      ).toBe(true);
      expect(left.every((w) => (w & 0x7f800000) >>> 0 !== 0x7f800000)).toBe(true);
    });
    // the stage did something: the mid-note reset and the control changes were not a bypass
    expect(blocks.some(({ left, audio }) => left.some((w, i) => w !== audio[i])) || stage.extra.dry === true).toBe(true);
  });

  // the held-sample stage reads its input again in the first sample after a reset (its counters are 0), so stale held audio never sounds: its mirror is hygiene
  it.each(Object.keys(STAGES).filter((n) => n in RESET && !n.startsWith('held')))(
    'a reset that misses R breaks the identity (negative control): %s',
    async (name) => {
      // the stock reset on L alone (no mirror) leaves R's history from before the reset
      const stock = {
        _gs_fo_reset: '_gs_fo_reset_stock',
        _gs_uy_reset: '_gs_uy_reset_stock',
        _gs_held_reset: '_gs_held_reset_stock',
        _gs_case1_reset: '_gs_case1_reset_stock',
        _gs_case2_reset: '_gs_case2_reset_stock',
        _gs_case3_reset: '_gs_case3_reset_stock',
        _gs_case5_reset: '_gs_case5_reset_stock',
        _gs_ring_reset: '_gs_ring_reset_stock',
      }[RESET[name]!]!;
      const blocks = await identicalRun(rig, name, STOCK[stock]);
      expect(blocks.some(({ left, right }) => left.some((w, i) => w !== right[i]))).toBe(true);
    },
  );

  it('every stage with a reset is in the table', () => {
    // the gain stage restarts through its argument and the shaper has no state; every other stage has a reset mirror
    expect(Object.keys(STAGES).filter((n) => !(n in RESET)).sort()).toEqual(['gain', 'shaper-early', 'shaper-late']);
    expect(RING_WORDS).toBe(8192);
  });
});
