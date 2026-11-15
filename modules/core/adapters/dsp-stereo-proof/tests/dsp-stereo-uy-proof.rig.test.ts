/**
 * Finite U/Y observer against the original helpers; placements share one entry/state. Port of test_dsp_stereo_uy_proof.py (T1g phase 5b), on the
 * SHARC-only rig with the stock DSP image.
 *
 * The repeated-placement fixture models early then late occurrences without forcing stock G-table gates or claiming that a natural native
 * scenario reaches both.
 */
import { describe, expect, it } from 'vitest';
import { DN2_112, assemble, link, stockAvailable, stockSkipReason, type CallResult, type StereoSession } from '@digi/sharc-rig-kit';
import {
  BACK,
  FRAME,
  INPUT,
  PRIVATE,
  bits,
  buildProof,
  call,
  canonical,
  fresh,
  peekWord,
  regs,
  relationWords,
  seed as seedFirst,
  u32,
  values,
  type ProofBuild,
} from './proof-rig.ts';

const STATE = 0x2533e0;
const STRIDE = 32;
const SAVED_LEFT = ['R3', 'R5', 'R6', 'R7', 'R9', 'R10', 'R11', 'R13', 'R14', 'R15', 'I0', 'I1', 'I3', 'I4', 'I5', 'I6', 'I7', 'MODE1', 'ASTATX', 'ASTATY', 'STKYX', 'STKYY'];

const seed = (s: StereoSession, slot = 0): number => {
  const state = STATE + STRIDE * slot;
  for (let i = 0; i < 8; i++) s.poke(state + 4 * i, 0);
  for (const [i, v] of Object.entries({ 2: 0.6, 3: 0.3, 4: 0.2, 5: 0.4, 6: 0.1, 7: -0.05 })) s.poke(state + 4 * Number(i), bits(v));
  return state;
};
const audio = (s: StereoSession, phase: number): void => {
  for (let i = 0; i < 32; i++) s.poke(INPUT + 4 * i, bits(0.3 * Math.sin(i * 0.31 + phase)));
};
const context = (state: number, extra: Record<string, number> = {}): Record<string, number> =>
  regs(state, { R12: INPUT, STKYX: 0, STKYY: 0x1234, ...extra });

/** The stock call site as it is assembled: `CJUMP target (DB)` and the two delay slots (`delay`: the text of the second instruction, with the push of the address `cont`). */
function siteCode(site: number, target: number, kind: 'frame' | 'compute'): string {
  const second = kind === 'frame' ? 'DM(I7, M7) = R2;' : 'DM(I7, M7) = R2, R4 = R5 + R10;';
  const cont = site + (kind === 'frame' ? 6 : 8);
  const text = `.SECTION/PM seg_pmco;\n.GLOBAL _site;\n_site:\n    CJUMP _t (DB);\n    ${second}\n    DM(I7, M7) = 0x${cont.toString(16)};\n`;
  return link(assemble(text), site, { _t: target }).codeBoot;
}

const enabled = stockAvailable(DN2_112);
const suite = enabled ? describe : describe.skip;
const title = 'the private U/Y stereo proof observer (DN2 1.12)';

suite(enabled ? title : `${title}: ${stockSkipReason(DN2_112)}`, () => {
  const b: ProofBuild = enabled ? buildProof(true) : (undefined as never);
  const syms = enabled ? b.syms : {};
  const pc = (name: string): number => b.pc[name]!;
  const freshUy = (mode = 0): Promise<StereoSession> => fresh(b, mode);

  /** Everything the left channel must keep: its state, its in-place output and the caller's registers. */
  async function assertLeft(reference: StereoSession, proof: StereoSession, state: number, expected: CallResult, observed: Pick<CallResult, 'regs'>) {
    expect(await values(proof, state, 8)).toEqual(await values(reference, state, 8));
    expect(await values(proof, INPUT, 32)).toEqual(await values(reference, INPUT, 32));
    for (const name of SAVED_LEFT) expect(observed.regs[name], name).toBe(expected.regs[name]);
  }

  it('the observer fits its private L2 pages, keeps the first stage as a prefix, and the original call sites are the ones the proof retargets', async () => {
    expect(b.end - PRIVATE).toBeLessThan(8192);
    expect(b.prefixHolds, 'appending U/Y must retain every original first-stage instruction/relocation').toBe(true);
    const s = await freshUy();
    try {
      const sites: [number, number, 'frame' | 'compute'][] = [
        [0x1c959f, 0xb81368, 'frame'],
        [0x1c9b7a, 0xb81368, 'frame'],
        [0x1c96aa, 0xb8135a, 'compute'],
      ];
      const stock = await s.stock(sites.map(([site]) => [0x28000000 + 2 * site, 18]));
      sites.forEach(([site, target, kind], k) => {
        expect(stock[k]!.startsWith(siteCode(site, target, kind)), `site ${site.toString(16)}`).toBe(true);
      });
    } finally {
      await s.close();
    }
  });

  const placements = [0, 1].flatMap((mode) => [0, 7, 15].map((slot) => [mode, slot] as const));
  it.each(placements)('repeated placements share histories and capture each occurrence: mode %i, slot %i', async (mode, slot) => {
    const reference = await freshUy();
    const proof = await freshUy(mode);
    try {
      const state = seed(reference, slot);
      seed(proof, slot);
      let last: { expected: CallResult; observed: CallResult } | undefined;
      for (let block = 0; block < 3; block++) {
        for (const placement of ['early', 'late']) {
          const phase = block + (placement === 'late' ? 0.5 : 0);
          for (const s of [reference, proof]) {
            s.poke(state + 2 * 4, bits(0.6 + phase * 0.01));
            audio(s, phase);
          }
          const expected = await call(reference, pc('uy_stock_render'), context(state));
          const observed = await call(proof, pc('_uy_render'), context(state));
          last = { expected, observed };
          await assertLeft(reference, proof, state, expected, observed);
          const priv = syms._uy_states! + slot * STRIDE;
          const right = await values(proof, priv, 8);
          const desired = await values(reference, state, 8);
          for (const word of [6, 7]) desired[word] = u32(desired[word]! ^ (mode ? 0x80000000 : 0));
          expect(relationWords(right)).toEqual(relationWords(desired));
          expect(relationWords(await values(proof, syms._uy_audio! + 128 * slot, 32))).toEqual(
            relationWords((await values(reference, INPUT, 32)).map((x) => u32(x ^ (mode ? 0x80000000 : 0)))),
          );
          expect(await peekWord(proof, syms._uy_mismatches!)).toBe(0);
          expect(await peekWord(proof, syms._uy_nonfinite!)).toBe(0);
          const counts = (r: CallResult, only: Set<number>): Map<number, number> => {
            const m = new Map<number, number>();
            for (const [, a] of r.stores) if (only.has(a)) m.set(a, (m.get(a) ?? 0) + 1);
            return m;
          };
          const leftWords = new Set<number>();
          for (const [lo, size] of [[state, 32], [INPUT, 128]] as const) for (let a = lo; a < lo + size; a += 4) leftWords.add(canonical(a));
          expect(counts(observed, leftWords), 'the same stores to the left words as stock').toEqual(counts(expected, leftWords));
          const allowed = new Set<number>();
          for (const [lo, hi] of [[state, state + 32], [INPUT, INPUT + 128], [PRIVATE, b.end], [FRAME - 512, FRAME]] as const)
            for (let a = lo; a < hi; a += 4) allowed.add(canonical(a));
          for (const [, address, width] of observed.stores) {
            expect(width).toBe(4);
            expect(allowed.has(address), `store at ${address.toString(16)}`).toBe(true);
          }
        }
      }
      expect(last).toBeDefined();
      expect(await peekWord(proof, syms._uy_shadow_calls!)).toBe(6);
      expect(await peekWord(proof, syms._uy_coeff_blocks!)).toBe(6);
      for (const name of ['mode_errors', 'fallbacks', 'nonfinite', 'mismatches']) expect(await peekWord(proof, syms[`_uy_${name}`]!), name).toBe(0);
      for (const name of ['opposite_audio', 'opposite_history'])
        expect((await peekWord(proof, syms[`_uy_${name}`]!)) > 0, name).toBe(Boolean(mode));
      // actual reset, then a new nonzero input, retaining the initialized private slot
      const priv = syms._uy_states! + slot * STRIDE;
      let expected = await call(reference, pc('uy_stock_reset'), context(state));
      let observed = await call(proof, pc('_uy_reset'), context(state));
      await assertLeft(reference, proof, state, expected, observed);
      expect(await values(proof, priv + 24, 2)).toEqual([0, 0]);
      expect(await peekWord(proof, syms._uy_reset_mirrors!)).toBe(1);
      for (const s of [reference, proof]) audio(s, 4);
      expected = await call(reference, pc('uy_stock_render'), context(state));
      observed = await call(proof, pc('_uy_render'), context(state));
      await assertLeft(reference, proof, state, expected, observed);
      expect(await peekWord(proof, syms._uy_mismatches!)).toBe(0);
    } finally {
      await Promise.all([reference.close(), proof.close()]);
    }
  });

  it.each(['input', 'history'])('polarity fault controls preserve the stock left: %s', async (fault) => {
    const reference = await freshUy();
    const proof = await freshUy(1);
    try {
      const state = seed(reference);
      seed(proof);
      for (const s of [reference, proof]) audio(s, 0);
      await call(reference, pc('uy_stock_render'), context(state));
      await call(proof, pc('_uy_render'), context(state));
      for (const s of [reference, proof]) audio(s, 1);
      const expected = await call(reference, pc('uy_stock_render'), context(state));
      const boundary = pc('_uy_right_start');
      const paused = await proof.raw(pc('_uy_render'), context(state), { maxSteps: 12000, breaks: [BACK, boundary] });
      expect(paused.final_pc).toBe(boundary);
      if (fault === 'input') {
        // actual U/Y is in place: the original left input was overwritten by left; reinstall its deliberately captured source values without inversion
        for (let i = 0; i < 32; i++) proof.poke(syms._uy_audio! + 4 * i, bits(0.3 * Math.sin(i * 0.31 + 1)));
      } else {
        expect(await peekWord(proof, syms._uy_opposite_history!)).toBeGreaterThan(0);
        for (const i of [6, 7]) proof.poke(syms._uy_states! + 4 * i, await peekWord(proof, state + 4 * i));
      }
      const done = await proof.raw(boundary, {}, { maxSteps: 12000, breaks: [BACK], resume: true, readRegs: Object.keys(context(state)) });
      expect(done.halt === 'breakpoint' && done.final_pc === BACK).toBe(true);
      await assertLeft(reference, proof, state, expected, done);
      expect(await peekWord(proof, syms._uy_mismatches!)).toBeGreaterThan(0);
      expect(await peekWord(proof, syms._uy_nonfinite!)).toBe(0);
    } finally {
      await Promise.all([reference.close(), proof.close()]);
    }
  });

  it.each(['_uy_render', '_sp_render'])('a cross-stage mode switch refuses both until reload: first %s', async (firstEntry) => {
    const proof = await freshUy(1);
    try {
      const uyState = seed(proof);
      const spState = seedFirst(proof);
      audio(proof, 0);
      const uyFirst = firstEntry.startsWith('_uy');
      await call(proof, pc(firstEntry), context(uyFirst ? uyState : spState));
      proof.poke(syms._sp_mode!, 0);
      const other = uyFirst ? '_sp_render' : '_uy_render';
      await call(proof, pc(other), context(other.startsWith('_sp') ? spState : uyState));
      expect(await peekWord(proof, syms._sp_mode_errors!)).toBeGreaterThan(0);
      proof.poke(syms._sp_mode!, 1);
      const shadows = async (): Promise<number[]> => [await peekWord(proof, syms._sp_shadow_calls!), await peekWord(proof, syms._uy_shadow_calls!)];
      const before = await shadows();
      await call(proof, pc('_sp_render'), context(spState));
      await call(proof, pc('_uy_render'), context(uyState));
      expect(await shadows()).toEqual(before);
    } finally {
      await proof.close();
    }
  });

  it.each([0, 31, 33])('count passthrough: %i', async (count) => {
    const reference = await freshUy();
    const proof = await freshUy();
    try {
      const state = seed(reference);
      seed(proof);
      for (const s of [reference, proof]) {
        s.poke(FRAME + 4, count);
        audio(s, 0);
      }
      const expected = await call(reference, pc('uy_stock_render'), context(state));
      const observed = await call(proof, pc('_uy_render'), context(state));
      await assertLeft(reference, proof, state, expected, observed);
      expect(await peekWord(proof, syms._uy_fallbacks!)).toBe(1);
      expect(await peekWord(proof, syms._uy_shadow_calls!)).toBe(0);
    } finally {
      await Promise.all([reference.close(), proof.close()]);
    }
  });

  it.each(['render', 'reset'].flatMap((entry) => [false, true].map((tfY) => [entry, tfY] as const)))(
    'the SIMD handoff preserves the frame and the context: %s, TF_Y %s',
    async (entry, tfY) => {
      const proof = await freshUy();
      try {
        const state = seed(proof);
        const registers = context(state, { MODE1: 0x210000, ASTATY: 0x76503210 | (tfY ? 0x40000 : 0) });
        const target = pc(`uy_stock_${entry}`);
        const before = await values(proof, FRAME - 512, 133);
        const observed = await proof.raw(pc(`_uy_${entry}`), registers, { maxSteps: 50, breaks: [BACK, target], readRegs: Object.keys(registers) });
        expect(observed.final_pc).toBe(target);
        for (const [k, v] of Object.entries(registers)) if (k !== 'I12') expect(observed.regs[k], k).toBe(u32(v));
        expect(await values(proof, FRAME - 512, 133)).toEqual(before);
        expect(observed.stores).toEqual([]);
      } finally {
        await proof.close();
      }
    },
  );

  it.each([STATE - 4, STATE + 4, STATE + STRIDE * 16])('a slot outside the table is passed through: state %i', async (bad) => {
    const reference = await freshUy();
    const proof = await freshUy();
    try {
      for (const s of [reference, proof]) {
        for (let i = 0; i < 8; i++) s.poke(bad + 4 * i, 0);
        audio(s, 0);
      }
      const expected = await call(reference, pc('uy_stock_render'), context(bad));
      const observed = await call(proof, pc('_uy_render'), context(bad));
      await assertLeft(reference, proof, bad, expected, observed);
      expect(await peekWord(proof, syms._uy_fallbacks!)).toBe(1);
    } finally {
      await Promise.all([reference.close(), proof.close()]);
    }
  });

  it.each(['L3', 'L4', 'L5'])('a linear DAG is required: %s', async (register) => {
    const proof = await freshUy();
    try {
      const state = seed(proof);
      const registers = context(state, { [register]: 128 });
      const target = pc('uy_stock_render');
      const observed = await proof.raw(pc('_uy_render'), registers, { maxSteps: 200, breaks: [BACK, target], readRegs: Object.keys(registers) });
      expect(observed.final_pc).toBe(target);
      for (const [k, v] of Object.entries(registers)) if (k !== 'I12') expect(observed.regs[k], k).toBe(u32(v));
      expect(await peekWord(proof, syms._uy_shadow_calls!)).toBe(0);
    } finally {
      await proof.close();
    }
  });

  const sources = ['input', 'coefficient', 'left_history', 'right_history', 'output'];
  it.each([0, 1].flatMap((mode) => sources.map((source) => [mode, source] as const)))(
    'a non-finite refusal is sticky and the left stays stock: mode %i, %s',
    async (mode, source) => {
      const reference = await freshUy();
      const proof = await freshUy(mode);
      try {
        const state = seed(reference);
        seed(proof);
        for (const s of [reference, proof]) audio(s, 0);
        await call(reference, pc('uy_stock_render'), context(state));
        await call(proof, pc('_uy_render'), context(state));
        if (source === 'right_history') {
          proof.poke(syms._uy_states! + 6 * 4, 0x7fc00001);
        } else if (source === 'output') {
          proof.poke(syms._uy_states! + 6 * 4, 0x7f7fffff);
          for (const s of [reference, proof]) s.poke(state + 2 * 4, bits(2.0));
        } else {
          const address = { input: INPUT, coefficient: state + 2 * 4, left_history: state + 6 * 4 }[source]!;
          for (const s of [reference, proof]) s.poke(address, 0x7fc00001);
        }
        const expected = await call(reference, pc('uy_stock_render'), context(state));
        const observed = await call(proof, pc('_uy_render'), context(state));
        await assertLeft(reference, proof, state, expected, observed);
        expect(await peekWord(proof, syms._uy_nonfinite!)).toBeGreaterThan(0);
        const shadows = await peekWord(proof, syms._uy_shadow_calls!);
        await call(proof, pc('_uy_render'), context(state));
        expect(await peekWord(proof, syms._uy_shadow_calls!)).toBe(shadows);
      } finally {
        await Promise.all([reference.close(), proof.close()]);
      }
    },
  );

  it.each([0, 1])('signed zero and the deferred reset: mode %i', async (mode) => {
    const proof = await freshUy(mode);
    try {
      const state = seed(proof, 7);
      await call(proof, pc('_uy_reset'), context(state));
      expect(await peekWord(proof, syms._uy_initialized! + 7 * 4)).toBe(0);
      expect(await peekWord(proof, syms._uy_reset_mirrors!)).toBe(0);
      expect(await values(proof, state + 6 * 4, 2)).toEqual([0, 0]);
      for (let i = 0; i < 32; i++) proof.poke(INPUT + i * 4, (i % 2) * 0x80000000);
      await call(proof, pc('_uy_render'), context(state));
      expect(await peekWord(proof, syms._uy_mismatches!)).toBe(0);
      expect(await peekWord(proof, syms._uy_nonfinite!)).toBe(0);
      expect(await peekWord(proof, syms._uy_opposite_audio!)).toBe(0);
    } finally {
      await proof.close();
    }
  });

  it('the scratch DAGs are restored', async () => {
    const reference = await freshUy();
    const proof = await freshUy(1);
    try {
      const state = seed(reference);
      seed(proof);
      for (const s of [reference, proof]) audio(s, 0);
      const registers = context(state, { L0: 64, L1: 128 });
      const expected = await call(reference, pc('uy_stock_render'), registers);
      const observed = await call(proof, pc('_uy_render'), registers);
      await assertLeft(reference, proof, state, expected, observed);
      for (const name of ['L0', 'L1', 'B0', 'B1']) expect(observed.regs[name], name).toBe(expected.regs[name]);
    } finally {
      await Promise.all([reference.close(), proof.close()]);
    }
  });
});
