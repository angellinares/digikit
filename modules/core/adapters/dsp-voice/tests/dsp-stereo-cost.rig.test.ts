/**
 * The cost of the R twins of core/dsp-voice on DN2 1.12 (docs/design/dn2-stereo.md, section 3), in instructions on the concrete core
 * (1 instruction = 1 cycle, no stalls). Port of test_dsp_stereo_cost.py (T1g phase 5b).
 *
 * A twin costs its stock call again plus the copy of the shared words and its own bookkeeping, so a stamped call stays below the stock
 * call plus the unstamped wrapper (13 instructions) plus the ceiling of the stage; an unstamped slot costs the stock call and 13
 * instructions. The ceilings are the measured extras rounded up by about 10 %; a twin that grows past them changes the budget of
 * section 3 and the cap of eight stereo slots. Original firmware is supplied at run time.
 */
import { describe, expect, it } from 'vitest';
import { DN2_112, HEARTBEAT, PyRandom, StereoRig, call, newSession, stockAvailable, stockSkipReason } from '@digi/sharc-rig-kit';
import {
  LBUF,
  R_RING,
  RING_L,
  SCRATCH,
  SLOT,
  STAGES,
  argsOf,
  blockInput,
  initRecord,
  lRecord,
  privateOf,
  rRecord,
  setupBlend,
  stageRegs,
} from './stereo-stages.ts';

// stage -> ceiling of (stamped twin call - unstamped wrapper call); the unstamped call is the stock call and 13 instructions
const CEILING: Record<string, number> = {
  'first-order': 800,
  uy: 300,
  gain: 420,
  case5: 1160,
  'shaper-early': 650,
  'shaper-late': 870,
  held: 640,
  case1: 750,
  case2: 1400,
  case3: 2520,
  ring4: 1750,
  'ring-wrap': 1680,
};
const WRAPPER_CHECK = 13;

async function instructions(rig: StereoRig, name: string, stamped: boolean): Promise<number> {
  const stage = STAGES[name]!;
  const s = await newSession(rig);
  try {
    const lrec = lRecord(stage);
    const rrec = rRecord(rig, stage);
    initRecord(s, lrec, stage.size, stage.initial, stage.extra.words);
    setupBlend(s);
    for (const [i, v] of Object.entries(privateOf(stage))) s.poke(rrec + 4 * Number(i), v);
    if (stage.extra.prime) {
      stage.extra.prime(s, 'stereo-l', RING_L);
      stage.extra.prime(s, 'stereo-r', R_RING);
    }
    s.poke(rig.data._gs_stamp! + 4 * SLOT, stamped ? HEARTBEAT : HEARTBEAT - 1);
    s.pokeWords(LBUF, blockInput(new PyRandom(1)));
    s.pokeWords(rig.data._gs_rbuf! + 128 * SLOT, blockInput(new PyRandom(2)));
    const out = stage.scratch ? SCRATCH : LBUF;
    return (await call(s, rig.sym[stage.entry]!, lrec, LBUF, out, argsOf(stage, 1), { regs: stageRegs(stage) })).instructions;
  } finally {
    await s.close();
  }
}

const enabled = stockAvailable(DN2_112);
const suite = enabled ? describe : describe.skip;
const title = 'the cost of the R twins (DN2 1.12)';

suite(enabled ? title : `${title}: ${stockSkipReason(DN2_112)}`, () => {
  const rig = enabled ? StereoRig.build() : (undefined as never);

  it.each(Object.keys(CEILING))('a twin costs the stock call again within its ceiling: %s', async (name) => {
    const unstamped = await instructions(rig, name, false);
    const stamped = await instructions(rig, name, true);
    expect(stamped, 'the R pass runs the stock stage').toBeGreaterThan(2 * (unstamped - WRAPPER_CHECK));
    expect(stamped - unstamped).toBeLessThanOrEqual(CEILING[name]!);
  });
});
