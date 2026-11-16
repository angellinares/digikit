/**
 * The R twins of core/dsp-voice on DN2 1.12 (docs/design/dn2-stereo.md, section 2). Port of test_dsp_stereo_twins.py (T1g phase 5b).
 *
 * A twin wrapper (`_gs_fo`, `_gs_uy`, `_gs_gain`, `_gs_c5`, `_gs_shaper_*`, `_gs_held_*`, `_gs_case1` to `_gs_case3`, `_gs_ring4`,
 * `_gs_ring6`) is called the way the engine calls the stock stage: a linked call, R4 = the L record, R8 = R12 = the voice buffer, the
 * arguments on the stack. For a stamped slot it runs the stock function on L, then on a private R record and the R buffer. Reference
 * channels run the stock function on independent concrete cores with private state; the control words of each block are the same for all
 * three. Negative controls: a shared history, and the stage's control words copied after the L call, give a different R. Original
 * firmware is supplied at run time, never copied into a fixture.
 */
import { describe, expect, it } from 'vitest';
import {
  DN2_112,
  HEARTBEAT,
  PyRandom,
  STOCK,
  StereoRig,
  call,
  filled,
  newSession,
  stockAvailable,
  stockSkipReason,
} from '@digi/sharc-rig-kit';
import {
  DIRTY_FILLED,
  LBUF,
  R_RING,
  RING_L,
  RING_WORDS,
  SCRATCH,
  SLOT,
  STAGES,
  blockInput,
  initRecord,
  lRecord,
  rRecord,
  runChain,
  stageRegs,
  variantR,
} from './stereo-stages.ts';

const finite = (words: readonly number[]): boolean => words.every((w) => (w & 0x7f800000) >>> 0 !== 0x7f800000);
const differ = (a: readonly number[], b: readonly number[]): boolean => a.some((w, i) => w !== b[i]);
const rOf = (history: { r: number[] }[]): number[][] => history.map((h) => h.r);

const enabled = stockAvailable(DN2_112);
const suite = enabled ? describe : describe.skip;
const title = 'the R twins of core/dsp-voice (DN2 1.12)';

suite(enabled ? title : `${title}: ${stockSkipReason(DN2_112)}`, () => {
  const rig = enabled ? StereoRig.build() : (undefined as never);
  const names = Object.keys(STAGES);

  it.each(names)('the twin matches two isolated mono channels: %s', async (name) => {
    const stage = STAGES[name]!;
    const { history, stereo } = await runChain(rig, stage);
    await stereo.close();
    for (const h of history) {
      expect(h.l, 'L is the stock path').toEqual(h.l_ref);
      expect(h.l_rec).toEqual(h.l_rec_ref);
      expect(h.r, 'R is the stock stage on independent state').toEqual(h.r_ref);
      const checked =
        stage.name === 'gain'
          ? Array.from({ length: stage.size }, (_, i) => i)
          : stage.checked.length
            ? stage.checked
            : [...stage.controls, ...stage.histories];
      for (const i of checked) expect(h.r_rec[i], `R record word ${i}`).toBe(h.r_rec_ref[i]);
      expect(h.l).not.toEqual(h.r);
      if (!stage.extra.dry) {
        expect(h.r).not.toEqual(h.r_in);
        expect(h.l).not.toEqual(h.l_in);
      }
      expect(finite([...h.l, ...h.r])).toBe(true);
    }
    // the histories are independent: both are live and different
    const last = history.at(-1)!;
    expect(stage.histories.length === 0 || stage.histories.some((i) => last.l_rec[i] !== last.r_rec[i])).toBe(true);
  });

  it.each(names)('a slot that is not stamped is the plain stock call: %s', async (name) => {
    const stage = STAGES[name]!;
    const stereo = await newSession(rig);
    const plain = await newSession(rig);
    try {
      const lrec = lRecord(stage);
      const rrec = rRecord(rig, stage);
      for (const s of [stereo, plain]) {
        initRecord(s, lrec, stage.size, stage.initial);
        s.pokeWords(LBUF, blockInput(new PyRandom(3)));
      }
      stereo.pokeWords(rrec, filled(stage.size, 0x12345678));
      stereo.poke(rig.data._gs_stamp! + 4 * SLOT, HEARTBEAT - 1);
      stereo.poke(rig.data._gs_rbuf! + 128 * SLOT, 0x12345678);
      const out = stage.scratch ? SCRATCH : LBUF;
      const regs = stageRegs(stage);
      await call(stereo, rig.sym[stage.entry]!, lrec, LBUF, out, stage.args, { regs });
      await call(plain, STOCK[stage.stock]!, lrec, LBUF, out, stage.args, { regs });
      expect(await stereo.words(LBUF, 32)).toEqual(await plain.words(LBUF, 32));
      expect(await stereo.words(lrec, stage.size)).toEqual(await plain.words(lrec, stage.size));
      expect(await stereo.words(rrec, stage.size), 'R state untouched').toEqual(filled(stage.size, 0x12345678));
      expect(await stereo.peek(rig.data._gs_rbuf! + 128 * SLOT)).toBe(0x12345678);
    } finally {
      await Promise.all([stereo.close(), plain.close()]);
    }
  });

  // positive control for the two negative controls below
  it.each(names)('the harness reproduces the twin with the early capture: %s', async (name) => {
    const stage = STAGES[name]!;
    const { history, stereo } = await runChain(rig, stage);
    await stereo.close();
    expect(await variantR(rig, stage, history, 'early')).toEqual(rOf(history));
  });

  it.each(names)('a shared history gives a different R: %s', async (name) => {
    const stage = STAGES[name]!;
    const { history, stereo } = await runChain(rig, stage);
    await stereo.close();
    expect(await variantR(rig, stage, history, 'shared')).not.toEqual(rOf(history));
  });

  it('controls copied after the L call give a different R for the gain stage', async () => {
    // the L call advances the stage's control words once per block; an R pass that starts from the advanced words is a different gain trajectory
    const stage = STAGES.gain!;
    const { history, stereo } = await runChain(rig, stage);
    await stereo.close();
    expect(await variantR(rig, stage, history, 'late')).not.toEqual(rOf(history));
  });

  it('the late shaper without the scalings on R gives a different R', async () => {
    // the engine scales L by 16 before the late stage and by 1/16 after it; R without that is another point on the curve
    const stage = STAGES['shaper-late']!;
    const { history, stereo } = await runChain(rig, stage);
    await stereo.close();
    expect(await variantR(rig, stage, history, 'early', true)).not.toEqual(rOf(history));
  });

  it('the callee-saved registers survive the stereo path (hostile context registers)', async () => {
    // `call` checks them for every stage above; this one holds the identity under the context registers the engine keeps
    const stage = STAGES['first-order']!;
    const s = await newSession(rig);
    try {
      initRecord(s, lRecord(stage), stage.size, stage.initial);
      s.poke(rig.data._gs_stamp! + 4 * SLOT, HEARTBEAT);
      for (const i of stage.histories) s.poke(rRecord(rig, stage) + 4 * i, 0);
      s.pokeWords(LBUF, blockInput(new PyRandom(2)));
      await call(s, rig.sym[stage.entry]!, lRecord(stage), LBUF, LBUF, stage.args, {
        regs: { M6: 1, M5: 0, M14: 1, M7: 0xffffffff },
      });
    } finally {
      await s.close();
    }
  });

  // ---- the ring stage: its own ring in the DSP's DDR ----

  it('the R pass of the ring stage uses its own ring at the profile base', async () => {
    const stage = STAGES.ring4!;
    const { history, stereo } = await runChain(rig, stage);
    try {
      expect(await stereo.peek(rRecord(rig, stage) + 4 * 16), 'word 16 of the R record, not the L record').toBe(R_RING);
      expect(await stereo.peek(lRecord(stage) + 4 * 16)).toBe(RING_L);
      for (const h of history) {
        expect(h.ring_l, 'the R pass never writes the L ring').toEqual(h.ring_l_ref);
        expect(h.ring_r, 'the R ring is the mono channel ring').toEqual(h.ring_r_ref);
      }
      const first = history[0]!;
      const last = history.at(-1)!;
      const changed = (a: number[], b: number[]): boolean => Array.from({ length: RING_WORDS }, (_, i) => a[i] !== b[i]).some(Boolean);
      expect(changed(first.ring_l!, last.ring_l!) && changed(first.ring_r!, last.ring_r!), 'both rings were written').toBe(true);
    } finally {
      await stereo.close();
    }
  });

  it('a dirty R ring is hidden by the fill status of a fresh note', async () => {
    // the stock reset leaves the backing of a ring as it is and sets the fill status; R starts the same way, so its ring needs no clear: a
    // ring full of old audio gives the same R as a ring of zeros until the write cursor has wrapped once
    const { history, stereo } = await runChain(rig, STAGES['ring-dirty-r']!);
    await stereo.close();
    expect(history.every((h) => !differ(h.r, h.r_ref))).toBe(true);
    expect(history[0]!.ring_r!.slice(0, 4000).some((w) => w !== 0), 'the R ring was dirty').toBe(true);
    expect(history.some((h) => differ(h.r, h.l))).toBe(true);
  });

  it('a dirty R ring is audible once the ring is filled', async () => {
    // negative control for the test above: with the fill status of a filled ring the backing matters, so the test can tell
    const { history, stereo } = await runChain(rig, DIRTY_FILLED);
    await stereo.close();
    expect(history.some((h) => differ(h.r, h.r_ref))).toBe(true);
  });
});
