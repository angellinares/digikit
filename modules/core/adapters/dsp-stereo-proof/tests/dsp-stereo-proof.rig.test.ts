/**
 * Private callsite observer against the original DN2 1.12 helpers, not hardware: port of test_dsp_stereo_proof.py (T1g phase 5b), on the
 * SHARC-only rig with the stock DSP image.
 *
 * Public PRM Rev1.5 Table4-22 (printed4-54, PDF159) and Table4-32 (printed4-61, PDF166) AND branch conditions across PEs. MODE1 is not a
 * complementary register (Type18a, printed17-2..3); BIT SET/CLR change their specified register, while TST/XOR set BTF. SIMD Mode (PDF102)
 * specifies one cycle PEYEN latency; the rejection path waits two NOPs after each change.
 *
 * Not ported: `test_packaging_retargets_only_the_five_engine_calls` (patches the stock firmware image with `patchkit apply`; it stays in Python).
 */
import { describe, expect, it } from 'vitest';
import { DN2_112, assemble, link, stockAvailable, stockSkipReason, type StereoSession } from '@digi/sharc-rig-kit';
import {
  BACK,
  CALLER,
  FRAME,
  INPUT,
  OUTPUT,
  PRIVATE,
  STATE,
  bits,
  buildProof,
  call,
  canonical,
  fresh,
  peekWord,
  regs,
  relationWords,
  seed,
  u32,
  values,
  type ProofBuild,
} from './proof-rig.ts';

const SAVED = ['R3', 'R5', 'R6', 'R7', 'R9', 'R10', 'R11', 'R13', 'R14', 'R15', 'I0', 'I1', 'I3', 'I4', 'I5', 'I6', 'I7', 'ASTATX', 'ASTATY', 'MODE1'];
const sine = (block: number): number[] => Array.from({ length: 32 }, (_, i) => bits(0.3 * Math.sin(i * 0.31 + block)));
const poke32 = (s: StereoSession, value: number): void => {
  for (let i = 0; i < 32; i++) s.poke(INPUT + 4 * i, value);
};

const IDENTITY_CASES: [number, number, boolean][] = [0, 1].flatMap((mode): [number, number, boolean][] => [
  [mode, 0, false],
  [mode, 7, true],
  [mode, 15, false],
]);

const enabled = stockAvailable(DN2_112);
const suite = enabled ? describe : describe.skip;
const title = 'the private first-order stereo proof observer (DN2 1.12)';

suite(enabled ? title : `${title}: ${stockSkipReason(DN2_112)}`, () => {
  const b: ProofBuild = enabled ? buildProof(false) : (undefined as never);
  const syms = enabled ? b.syms : {};
  const pc = (name: string): number => b.pc[name]!;

  it('the observer fits its private L2 page, and the original call sites are the ones the proof retargets', async () => {
    expect(b.end - PRIVATE).toBeLessThanOrEqual(4096);
    // the stock sites: a direct call of the stock function with the frame-save and the explicit continuation push in its delay slots
    const s = await fresh(b);
    try {
      const sites: [number, number][] = [
        [0x1c94fd, 0xb80c39],
        [0x1c96cb, 0xb80b93],
      ];
      const stock = await s.stock(sites.map(([site]) => [0x28000000 + 2 * site, 18]));
      sites.forEach(([site, target], k) => {
        expect(stock[k]!.startsWith(siteCode(site, target)), `site ${site.toString(16)}`).toBe(true);
      });
    } finally {
      await s.close();
    }
  });

  it.each(IDENTITY_CASES)(
    'identity: private histories, coefficients and left context: mode %i, slot %i, in place %s',
    async (mode, slot, inPlace) => {
      const left = await fresh(b);
      const proof = await fresh(b);
      try {
        const state = seed(left, slot);
        seed(proof, slot);
        proof.poke(syms._sp_mode!, mode);
        const output = inPlace ? INPUT : OUTPUT;
        for (let block = 0; block < 3; block++) {
          for (const s of [left, proof]) {
            s.poke(state + 21 * 4, bits(0.7 - block * 0.1));
            s.pokeWords(INPUT, sine(block));
          }
          const reference = await call(left, pc('stock_render'), regs(state, { R12: output }));
          const observed = await call(proof, pc('_sp_render'), regs(state, { R12: output }));
          expect(await values(proof, state, 28)).toEqual(await values(left, state, 28));
          expect(await values(proof, output, 32)).toEqual(await values(left, output, 32));
          expect(relationWords(await values(proof, syms._sp_audio! + 128 * slot, 32))).toEqual(
            relationWords((await values(left, output, 32)).map((w) => u32(w ^ (mode ? 0x80000000 : 0)))),
          );
          for (const name of SAVED) expect(observed.regs[name], name).toBe(reference.regs[name]);
          expect(await peekWord(proof, syms._sp_mismatches!)).toBe(0);
          expect(observed.instructions).toBeLessThan(6000);

          const stores = (r: typeof observed): Map<number, number> => {
            const counts = new Map<number, number>();
            for (const [, address] of r.stores) counts.set(address, (counts.get(address) ?? 0) + 1);
            return counts;
          };
          const originalWords = new Set<number>();
          for (const [lo, count] of [[state, 28], [output, 32]] as const)
            for (let a = lo; a < lo + count * 4; a += 4) originalWords.add(canonical(a));
          const onlyOriginal = (r: typeof observed): Map<number, number> =>
            new Map([...stores(r)].filter(([a]) => originalWords.has(a)));
          expect(onlyOriginal(observed), 'the same stores to the original words as stock').toEqual(onlyOriginal(reference));
          const expectedState = await values(left, state, 28);
          for (const i of [24, 25]) expectedState[i] = u32(expectedState[i]! ^ (mode ? 0x80000000 : 0));
          expect(relationWords(await values(proof, syms._sp_states! + slot * 112, 28))).toEqual(relationWords(expectedState));
          const allowed = new Set<number>();
          for (const [lo, hi] of [[state, state + 112], [output, output + 128], [PRIVATE, b.end], [FRAME - 512, FRAME]] as const)
            for (let a = lo; a < hi; a += 4) allowed.add(canonical(a));
          for (const [, address, width] of observed.stores) {
            expect(width, `store at ${address.toString(16)}`).toBe(4);
            expect(allowed.has(address), `store at ${address.toString(16)} is outside the original words, the private page and the frame`).toBe(true);
          }
        }
        expect(await peekWord(proof, syms._sp_active_mode!)).toBe(mode);
        expect(await peekWord(proof, syms._sp_mode_errors!)).toBe(0);
        expect(await peekWord(proof, syms._sp_nonfinite!)).toBe(0);
        for (const counter of ['_sp_opposite_audio', '_sp_opposite_history'])
          expect((await peekWord(proof, syms[counter]!)) > 0, counter).toBe(Boolean(mode));
        expect(await peekWord(proof, syms._sp_shadow_calls!)).toBe(3);
        expect(await peekWord(proof, syms._sp_nonzero_input!)).toBeGreaterThan(0);
        expect(await peekWord(proof, syms._sp_nonidentity_coeff!)).toBeGreaterThan(0);
        expect(await peekWord(proof, syms._sp_low_entry_frame!)).toBe(FRAME);
        expect(await peekWord(proof, syms._sp_low_entry_sp!)).toBe(FRAME - 8);
      } finally {
        await Promise.all([left.close(), proof.close()]);
      }
    },
  );

  it.each([0, 1])('reset: deferred clone and four-word mirror: mode %i', async (mode) => {
    const s = await fresh(b);
    try {
      const state = seed(s, 7);
      s.poke(syms._sp_mode!, mode);
      const result = await call(s, pc('_sp_reset'), regs(state));
      expect(result.regs.R2).toBe(CALLER);
      expect(await values(s, state + 24 * 4, 4)).toEqual([0, 0, 0, 0]);
      expect(await peekWord(s, syms._sp_initialized! + 7 * 4)).toBe(0);
      poke32(s, bits(0.2));
      await call(s, pc('_sp_render'), regs(state));
      for (let i = 24; i < 28; i++) s.poke(syms._sp_states! + 112 * 7 + 4 * i, bits(0.5));
      await call(s, pc('_sp_reset'), regs(state));
      expect(await values(s, syms._sp_states! + 112 * 7 + 24 * 4, 4)).toEqual([0, 0, 0, 0]);
      expect(await peekWord(s, syms._sp_reset_mirrors!)).toBe(1);
    } finally {
      await s.close();
    }
  });

  it('private history corruption is observed without left mutation', async () => {
    const s = await fresh(b);
    try {
      const state = seed(s);
      poke32(s, bits(0.2));
      await call(s, pc('_sp_render'), regs(state));
      s.poke(syms._sp_states! + 25 * 4, bits(9.0));
      await call(s, pc('_sp_render'), regs(state));
      expect(await peekWord(s, syms._sp_mismatches!)).toBeGreaterThan(0);
      expect(await peekWord(s, syms._sp_first_state!)).toBe(state);
      expect(await peekWord(s, syms._sp_first_input!)).toBe(INPUT);
      expect(await peekWord(s, syms._sp_first_output!)).toBe(OUTPUT);
    } finally {
      await s.close();
    }
  });

  it.each(['render', 'reset'].flatMap((entry) => [false, true].map((tfY) => [entry, tfY] as const)))(
    'SIMD rejection preserves stock behaviour and the original frame: %s, TF_Y %s',
    async (entry, tfY) => {
      const proof = await fresh(b);
      try {
        const state = seed(proof);
        const context = regs(state, {
          MODE1: 0x210000,
          ASTATX: 0x01234567,
          ASTATY: 0x76503210 | (tfY ? 0x40000 : 0),
          STKYX: 0x00112233,
          STKYY: 0x00445566,
          PX1: 0x12345678,
          PX2: 0xfedcba98,
        });
        const MSF: [string, string] = ['f'.repeat(20), '123456789abcdef01234'];
        const MSB: [string, string] = ['f'.repeat(20), 'abcd12345678ef907654'];
        const stockEntry = pc(`stock_${entry}`);
        const frameBefore = await values(proof, FRAME - 512, 133);
        const observed = await proof.raw(pc(`_sp_${entry}`), context, {
          maxSteps: 50,
          breaks: [BACK, stockEntry],
          readRegs: Object.keys(context),
          specials: { MSF, MSB },
          readSpecials: ['MSF', 'MSB'],
        });
        expect(observed.halt).toBe('breakpoint');
        expect(observed.final_pc).toBe(stockEntry);
        for (const [name, value] of Object.entries(context))
          if (name !== 'I12') expect(observed.regs[name], name).toBe(u32(value)); // I12 is audited dead incoming scratch, overwritten by stock
        expect(await values(proof, FRAME - 512, 133)).toEqual(frameBefore);
        expect(observed.stores).toEqual([]);
        expect(observed.specials).toEqual({ MSF, MSB });
        expect(await peekWord(proof, syms._sp_render_calls!)).toBe(0);
        expect(await peekWord(proof, syms._sp_reset_calls!)).toBe(0);
      } finally {
        await proof.close();
      }
    },
  );

  it('the stock zero-count return still overwrites I12', async () => {
    const s = await fresh(b);
    try {
      const state = seed(s);
      s.poke(FRAME + 4, 0);
      const result = await call(s, pc('stock_render'), regs(state));
      expect(result.regs.I12).toBe(BACK - 1);
    } finally {
      await s.close();
    }
  });

  it.each([STATE - 4, STATE + 4, STATE + 16 * 112])('a non-matching record uses stock only: state %i', async (state) => {
    const left = await fresh(b);
    const proof = await fresh(b);
    try {
      for (const s of [left, proof]) {
        for (let i = 0; i < 28; i++) s.poke(state + i * 4, 0);
        s.poke(state + 21 * 4, bits(1.0));
        poke32(s, bits(0.2));
      }
      const reference = await call(left, pc('stock_render'), regs(state));
      const observed = await call(proof, pc('_sp_render'), regs(state));
      expect(await values(proof, OUTPUT, 32)).toEqual(await values(left, OUTPUT, 32));
      expect(observed.regs.I6).toBe(reference.regs.I6);
      expect(await peekWord(proof, syms._sp_shadow_calls!)).toBe(0);
      expect(await peekWord(proof, syms._sp_fallbacks!)).toBe(1);
    } finally {
      await Promise.all([left.close(), proof.close()]);
    }
  });

  it.each([0, 31, 33])('trailing count passthrough: %i', async (count) => {
    const reference = await fresh(b);
    const proof = await fresh(b);
    try {
      const state = seed(reference);
      seed(proof);
      for (const s of [reference, proof]) {
        s.poke(FRAME + 4, count);
        for (let i = 0; i < 34; i++) {
          s.poke(INPUT + i * 4, bits(0.2));
          s.poke(OUTPUT + i * 4, 0xa55a3cc3);
        }
      }
      await call(reference, pc('stock_render'), regs(state));
      await call(proof, pc('_sp_render'), regs(state));
      expect(await values(proof, OUTPUT, 34)).toEqual(await values(reference, OUTPUT, 34));
      expect(await values(proof, state, 28)).toEqual(await values(reference, state, 28));
      expect(await peekWord(proof, syms._sp_shadow_calls!)).toBe(0);
      expect(await peekWord(proof, syms._sp_fallbacks!)).toBe(1);
    } finally {
      await Promise.all([reference.close(), proof.close()]);
    }
  });

  it.each(['L3', 'L4', 'L5'])('a non-linear stock DAG falls back without private access: %s', async (register) => {
    const proof = await fresh(b);
    try {
      const state = seed(proof);
      const context = regs(state, { [register]: 128 });
      const observed = await proof.raw(pc('_sp_render'), context, {
        maxSteps: 200,
        breaks: [BACK, pc('stock_render')],
        readRegs: Object.keys(context),
      });
      expect(observed.halt).toBe('breakpoint');
      expect(observed.final_pc).toBe(pc('stock_render'));
      for (const [name, value] of Object.entries(context)) if (name !== 'I12') expect(observed.regs[name], name).toBe(u32(value));
      expect(await values(proof, syms._sp_initialized!, 16)).toEqual(Array.from({ length: 16 }, () => 0));
      expect(await peekWord(proof, syms._sp_shadow_calls!)).toBe(0);
    } finally {
      await proof.close();
    }
  });

  it('scratch DAGs are private and caller bounds restored', async () => {
    const reference = await fresh(b);
    const proof = await fresh(b);
    try {
      const state = seed(reference);
      seed(proof);
      for (const s of [reference, proof]) poke32(s, bits(0.2));
      const context = regs(state, { L0: 64, L1: 128, B0: 0x200c0000, B1: 0x200c0100 });
      const expected = await call(reference, pc('stock_render'), context);
      const observed = await call(proof, pc('_sp_render'), context);
      expect(await values(proof, OUTPUT, 32)).toEqual(await values(reference, OUTPUT, 32));
      for (const name of ['I0', 'I1', 'L0', 'L1', 'B0', 'B1', 'MODE1', 'ASTATX', 'ASTATY']) expect(observed.regs[name], name).toBe(expected.regs[name]);
      expect(await peekWord(proof, syms._sp_mismatches!)).toBe(0);
    } finally {
      await Promise.all([reference.close(), proof.close()]);
    }
  });

  it.each([
    [0x7f7fffff, 7],
    [0x7fc00001, 9],
  ])('a divergent right does not leak sticky status: fault %i, sticky bit %i', async (fault, stickyBit) => {
    const reference = await fresh(b);
    const proof = await fresh(b);
    try {
      const state = seed(reference);
      seed(proof);
      const context = regs(state, { STKYX: 0, STKYY: 0x1234 });
      for (const s of [reference, proof]) {
        poke32(s, bits(0.2));
        await call(s, pc(s === reference ? 'stock_render' : '_sp_render'), context);
        s.poke(state + 22 * 4, bits(2.0));
      }
      proof.poke(syms._sp_states! + 24 * 4, fault);
      const expected = await call(reference, pc('stock_render'), context);
      const finish = pc('_sp_render_finish');
      const beforeRestore = await proof.raw(pc('_sp_render'), context, {
        maxSteps: 12000,
        breaks: [BACK, finish],
        readRegs: Object.keys(context),
      });
      expect(beforeRestore.halt).toBe('breakpoint');
      expect(beforeRestore.final_pc).toBe(finish);
      // the sticky bit of the divergent right is set before the observer restores the caller's status
      expect(((beforeRestore.regs.STKYX ?? 0) >>> stickyBit) & 1).toBe(1);
      const done = await proof.raw(finish, {}, { maxSteps: 100, breaks: [BACK], resume: true, readRegs: Object.keys(context) });
      expect(done.halt).toBe('breakpoint');
      expect(done.final_pc).toBe(BACK);
      expect(await values(proof, OUTPUT, 32)).toEqual(await values(reference, OUTPUT, 32));
      expect(await values(proof, state, 28)).toEqual(await values(reference, state, 28));
      expect(done.regs.STKYX).toBe(expected.regs.STKYX);
      expect(done.regs.STKYY).toBe(expected.regs.STKYY);
      expect(await peekWord(proof, syms._sp_mismatches!)).toBeGreaterThan(0);
    } finally {
      await Promise.all([reference.close(), proof.close()]);
    }
  });

  it.each(['copied_input', 'copied_history'])('polarity detects left alias controls without left mutation: %s', async (fault) => {
    const reference = await fresh(b);
    const proof = await fresh(b);
    try {
      const state = seed(reference);
      seed(proof);
      proof.poke(syms._sp_mode!, 1);
      const context = regs(state, { STKYX: 0, STKYY: 0x1234 });
      for (let block = 0; block < (fault === 'copied_history' ? 2 : 1); block++) {
        for (const s of [reference, proof]) s.pokeWords(INPUT, sine(block));
        const expected = await call(reference, pc('stock_render'), context);
        if (fault === 'copied_history' && block === 0) {
          await call(proof, pc('_sp_render'), context);
          expect(await peekWord(proof, syms._sp_mismatches!)).toBe(0);
          continue;
        }
        const boundary = pc('_sp_right_start');
        const paused = await proof.raw(pc('_sp_render'), context, { maxSteps: 12000, breaks: [BACK, boundary] });
        expect(paused.final_pc).toBe(boundary);
        if (fault === 'copied_input') {
          (await values(proof, INPUT, 32)).forEach((word, i) => proof.poke(syms._sp_audio! + 4 * i, word));
        } else {
          // deliberately replace independent histories AFTER a nonzero trajectory
          expect(await peekWord(proof, syms._sp_opposite_history!)).toBeGreaterThan(0);
          for (const i of [24, 25]) proof.poke(syms._sp_states! + 4 * i, await peekWord(proof, state + 4 * i));
        }
        const names = ['ASTATX', 'ASTATY', 'STKYX', 'STKYY', 'MODE1', 'I6', 'I7', 'R3', 'R11', 'R15'];
        const done = await proof.raw(boundary, {}, { maxSteps: 12000, breaks: [BACK], resume: true, readRegs: names });
        expect(done.halt === 'breakpoint' && done.final_pc === BACK).toBe(true);
        expect(await peekWord(proof, syms._sp_mismatches!)).toBeGreaterThan(0);
        expect(await peekWord(proof, syms._sp_nonfinite!)).toBe(0);
        expect(await values(proof, state, 28)).toEqual(await values(reference, state, 28));
        expect(await values(proof, OUTPUT, 32)).toEqual(await values(reference, OUTPUT, 32));
        for (const name of names) expect(done.regs[name], name).toBe(expected.regs[name]);
      }
    } finally {
      await Promise.all([reference.close(), proof.close()]);
    }
  });

  it.each([2, 0xffffffff])('an invalid mode uses stock only: %i', async (mode) => {
    const proof = await fresh(b);
    try {
      const state = seed(proof);
      proof.poke(syms._sp_mode!, mode);
      await call(proof, pc('_sp_render'), regs(state));
      expect(await peekWord(proof, syms._sp_mode_errors!)).toBe(1);
      expect(await peekWord(proof, syms._sp_shadow_calls!)).toBe(0);
    } finally {
      await proof.close();
    }
  });

  it.each(['render', 'reset'])('the mode is latched before first use and cannot switch: %s', async (entry) => {
    const proof = await fresh(b);
    try {
      const state = seed(proof);
      proof.poke(syms._sp_mode!, 1);
      poke32(proof, bits(0.2));
      await call(proof, pc(`_sp_${entry}`), regs(state));
      expect(await peekWord(proof, syms._sp_active_mode!)).toBe(1);
      const shadows = await peekWord(proof, syms._sp_shadow_calls!);
      proof.poke(syms._sp_mode!, 0);
      await call(proof, pc('_sp_render'), regs(state));
      expect(await peekWord(proof, syms._sp_mode_errors!)).toBe(1);
      expect(await peekWord(proof, syms._sp_shadow_calls!)).toBe(shadows);
      proof.poke(syms._sp_mode!, 1);
      await call(proof, pc('_sp_render'), regs(state));
      expect(await peekWord(proof, syms._sp_shadow_calls!)).toBe(shadows);
    } finally {
      await proof.close();
    }
  });

  it.each(['input', 'coefficient', 'left_history', 'right_history', 'output'])(
    'a non-finite polarity trajectory refuses further shadows: %s',
    async (source) => {
      const reference = await fresh(b);
      const proof = await fresh(b);
      try {
        const state = seed(reference);
        seed(proof);
        proof.poke(syms._sp_mode!, 1);
        for (const s of [reference, proof]) poke32(s, bits(0.2));
        await call(reference, pc('stock_render'), regs(state, { STKYX: 0 }));
        await call(proof, pc('_sp_render'), regs(state, { STKYX: 0 }));
        if (source === 'right_history') {
          proof.poke(syms._sp_states! + 24 * 4, 0x7fc00001);
        } else if (source === 'output') {
          // finite inputs/state overflow only the deliberately corrupted right
          proof.poke(syms._sp_states! + 24 * 4, 0x7f7fffff);
          for (const s of [reference, proof]) s.poke(state + 22 * 4, bits(2.0));
        } else {
          const address = { input: INPUT, coefficient: state + 21 * 4, left_history: state + 24 * 4 }[source]!;
          for (const s of [reference, proof]) s.poke(address, 0x7fc00001);
        }
        const expected = await call(reference, pc('stock_render'), regs(state, { STKYX: 0 }));
        const observed = await call(proof, pc('_sp_render'), regs(state, { STKYX: 0 }));
        expect(await peekWord(proof, syms._sp_nonfinite!)).toBeGreaterThan(0);
        expect(await values(proof, state, 28)).toEqual(await values(reference, state, 28));
        expect(await values(proof, OUTPUT, 32)).toEqual(await values(reference, OUTPUT, 32));
        for (const name of ['MODE1', 'ASTATX', 'STKYX', 'I6', 'I7']) expect(observed.regs[name], name).toBe(expected.regs[name]);
        const shadows = await peekWord(proof, syms._sp_shadow_calls!);
        await call(proof, pc('_sp_render'), regs(state, { STKYX: 0 }));
        expect(await peekWord(proof, syms._sp_shadow_calls!)).toBe(shadows);
      } finally {
        await Promise.all([reference.close(), proof.close()]);
      }
    },
  );

  it('the polarity signed-zero relation holds', async () => {
    const proof = await fresh(b);
    try {
      const state = seed(proof);
      proof.poke(syms._sp_mode!, 1);
      for (const i of [24, 25]) proof.poke(state + 4 * i, 0);
      for (let i = 0; i < 32; i++) proof.poke(INPUT + 4 * i, (i % 2) * 0x80000000);
      await call(proof, pc('_sp_render'), regs(state));
      expect(await peekWord(proof, syms._sp_mismatches!)).toBe(0);
      expect(await peekWord(proof, syms._sp_nonfinite!)).toBe(0);
      expect(await peekWord(proof, syms._sp_opposite_audio!)).toBe(0);
    } finally {
      await proof.close();
    }
  });
});

/** The stock call site as it is assembled: `CJUMP target (DB)` with the frame save and the explicit continuation push (the address after the site's 6 parcels) in its delay slots, as hex. */
function siteCode(site: number, target: number): string {
  const text = `.SECTION/PM seg_pmco;\n.GLOBAL _site;\n_site:\n    CJUMP _t (DB);\n    DM(I7, M7) = R2;\n    DM(I7, M7) = 0x${(site + 6).toString(16)};\n`;
  return link(assemble(text), site, { _t: target }).codeBoot;
}
