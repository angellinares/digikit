/**
 * core/dsp-voice and a raw sine routine (tests/support/raw_sine.s) on the SHARC-only rig.
 * Port of the rig tests of test_dsp_voice.py (T1g phase 5); the test that builds the module with an excluded store stays in Python.
 *
 * The wrapper and the sine routine are assembled from the module sources, linked at test bases with the firmware symbols stood in by test
 * addresses, and run the way the engine calls them: R4 = voice record, R8 = output block, R14 = loop counter, a frame built by the
 * caller's CJUMP. The stock path must fall through with nothing but the displaced stack adjustment and R0-R2 / I4 changed; a registered
 * type must run the routine (even voice) or copy its block (odd voice) and leave the callee-saved registers as found.
 */
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import {
  CONTEXT_REGS,
  DIAG,
  FRAME,
  M13_HOSTILE,
  M6_HOSTILE,
  MASK,
  OUT,
  PyRandom,
  RECORDS,
  RETURN_SW,
  RING_A,
  RING_B,
  STACK,
  STEP_WORD,
  STOCK_BP,
  TABLE,
  UNPACK_BP,
  VOICE_CORE,
  VOICE_STATE,
  VOICE_TYPE,
  VoiceRig,
  applyConditionals,
  crumbRig,
  drawContext,
  nw,
  stopsAt,
  w2f,
  type VoiceCall,
} from '@digi/sharc-rig-kit';

const TYPE = VOICE_TYPE;
const u32 = (x: number): number => x >>> 0;
const SAVED = ['R3', 'R5', 'R15', 'I0', 'I5', 'M1', 'M3', 'I8', 'I9', 'M8'];
const any = (words: readonly (number | null | undefined)[] | undefined): boolean => (words ?? []).some((w) => w);

/** Every context register leaves as it came in (M7 / M14 are the stock epilogue's and hold). */
function contextIntact(out: VoiceCall): void {
  for (const name of CONTEXT_REGS) expect(u32(out.regs[name]!), `${name} in ${out.in[name]}`).toBe(u32(out.in[name]!));
}

function savedIntact(out: VoiceCall): void {
  for (const name of SAVED) expect(out.regs[name], name).toBe(out.in[name]);
}

const crumb = (stage: number, v: number, t: number): number => stage | (v << 8) | (t << 16);

/** One registered voice with a note on; the odd voice sees the even voice's block in place. */
function voiceCall(rig: VoiceRig, v: number, ctx: NonNullable<Parameters<VoiceRig['call']>[1]>['ctx']) {
  const t = Math.floor(v / 2);
  rig.setType(t, TYPE);
  rig.setNote(t, 60);
  rig.recWord(v, 0x1b8, 1);
  if (v % 2)
    for (let k = 0; k < 32; k++) {
      rig.dm.set(nw(OUT) + 32 * v + k, 0);
      rig.dm.set(nw(OUT) + 32 * (v - 1) + k, 0x3f000000 + k);
    }
  return rig.call(v, { ctx });
}

describe('core/dsp-voice on the rig', () => {
  it('every word redecodes', () => {
    const rig = VoiceRig.sine();
    expect(rig.coreChecked).toBeGreaterThan(120);
    expect(rig.routineChecked).toBeGreaterThan(90);
  });

  it('an unregistered type falls through to the stock renderer', async () => {
    const rig = VoiceRig.sine();
    rig.setType(2, 0);
    const out = await rig.call(5);
    expect(stopsAt(out, STOCK_BP), out.halt).toBe(true);
    // only the displaced instruction's effect (I7 - 104 bytes) and R0-R2 / I4 differ; the callee-saved set is intact
    expect(out.regs.I7).toBe(STACK - 104);
    expect(out.regs.I6).toBe(STACK);
    // the context registers are the stock code's own: L4 (zeroed for the scan) is put back, nothing else was touched
    contextIntact(out);
    savedIntact(out);
    expect(out.regs.R4).toBe(out.in.R4);
    expect(out.regs.R8).toBe(out.in.R8);
    // a slot whose type no row registers does not walk the table
    expect(out.instructions).toBeLessThan(60);
  });

  it('with no registered type a voice costs a check and voice 0 keeps the heartbeat', async () => {
    const rig = VoiceRig.sine();
    // two free rows, then the end
    for (let k = 0; k < 3 * 24; k++) rig.dm.set(nw(TABLE) + k, k % 24 === 0 && k < 48 ? 0xffffffff : 0);
    rig.setType(2, TYPE); // a type that would be registered if a row had it
    const reads: Record<string, [number, number]> = { diag: [nw(DIAG), 16], mask: [nw(MASK), 1] };
    let out = await rig.call(5, { reads });
    expect(stopsAt(out, STOCK_BP), out.halt).toBe(true);
    expect([out.regs.I7, out.regs.I6]).toEqual([STACK - 104, STACK]);
    contextIntact(out);
    savedIntact(out);
    expect(out.instructions).toBeLessThan(15);
    expect(any(out.reads.diag), 'no beat and no breadcrumb from a voice other than 0').toBe(false);
    // voice 0: the heartbeat, and the mask of what is registered (free rows are no type)
    out = await rig.call(0, { reads });
    expect(stopsAt(out, STOCK_BP), out.halt).toBe(true);
    expect(out.reads.diag![0]).toBe(1);
    expect(out.reads.mask![0]).toBe(0);
    rig.dm.set(nw(TABLE) + 2 * 24, 31);
    rig.dm.set(nw(TABLE) + 3 * 24, 40);
    rig.dm.set(nw(TABLE) + 4 * 24, 0);
    rig.dm.set(nw(TABLE) + 24, 9); // a registered row after a free one
    out = await rig.call(0, { reads, mask: 0 });
    expect(out.reads.mask![0]).toBe(u32((1 << 9) | (1 << 31)));
  });

  for (const track of [0, 1, 4, 7, 15])
    it(`a registered type selects by the track and the right half of the cache word: track ${track}`, async () => {
      const rig = VoiceRig.sine();
      // the neighbour in the same 32-bit cache word is another type: it must not be mistaken for ours
      rig.setType(track ^ 1, TYPE + 1);
      rig.setType(track, TYPE);
      rig.setNote(track, 60);
      const v = 2 * track;
      rig.recWord(v, 0x1b8, 1); // ACTIVE
      const out = await rig.call(v);
      expect(stopsAt(out, RETURN_SW), out.halt).toBe(true);
      expect(any(out.reads.out)).toBe(true);
      expect(out.regs.I6, "RFRAME restores the caller's frame pointer").toBe(0x20000ff0);
      expect(out.regs.I7, 'RFRAME restores I7').toBe(STACK);
      // the neighbour track's voice, with the other type, falls through
      const rig2 = VoiceRig.sine();
      rig2.setType(track, TYPE);
      rig2.setType(track ^ 1, TYPE + 1);
      const out2 = await rig2.call(2 * (track ^ 1));
      expect(stopsAt(out2, STOCK_BP)).toBe(true);
    });

  it('the callee-saved registers survive a rendered voice', async () => {
    const rig = VoiceRig.sine();
    rig.setType(0, TYPE);
    rig.setNote(0, 60);
    rig.recWord(0, 0x1b8, 1);
    const out = await rig.call(0);
    expect(stopsAt(out, RETURN_SW)).toBe(true);
    savedIntact(out);
    contextIntact(out);
  });

  it("the odd voice copies the even voice's block", async () => {
    const rig = VoiceRig.sine();
    rig.setType(0, TYPE);
    rig.setNote(0, 60);
    rig.recWord(0, 0x1b8, 1);
    rig.recWord(1, 0x1b8, 1);
    const even = await rig.call(0);
    expect(stopsAt(even, RETURN_SW)).toBe(true);
    // the block the even voice wrote is in the output page; the odd voice's call sees it in place
    even.reads.out!.forEach((w, k) => rig.dm.set(nw(OUT) + k, w ?? 0));
    const odd = await rig.call(1);
    expect(stopsAt(odd, RETURN_SW)).toBe(true);
    expect(odd.reads.out).toEqual(even.reads.out);
  });

  for (const v of [0, 1, 2, 3, 14, 15, 30, 31])
    for (const m6 of M6_HOSTILE)
      for (const m13 of M13_HOSTILE)
        it(`the machine path does not depend on M6 or M13: voice ${v} M6 ${m6} M13 ${m13}`, async () => {
          // The hardware report's two causes: the odd-voice copy and the registry scan used M6, the machine call jumped by M13. Any M6
          // in {0x1b8, 4, 0, -1} and M13 in {4, -2, 0x10}: the same block as with the old friendly constants, and every context
          // register intact afterwards.
          const ref = await voiceCall(VoiceRig.sine(), v, 'friendly');
          expect(stopsAt(ref, RETURN_SW), ref.halt).toBe(true);
          expect(any(ref.reads.out)).toBe(true);
          const out = await voiceCall(VoiceRig.sine(), v, { M6: m6, M13: m13 });
          expect(stopsAt(out, RETURN_SW), out.halt).toBe(true);
          expect(out.reads.out).toEqual(ref.reads.out);
          contextIntact(out);
          expect([out.regs.I6, out.regs.I7]).toEqual([0x20000ff0, STACK]);
        });

  for (let seed = 0; seed < 12; seed++)
    it(`a drawn context changes nothing: seed ${seed}`, async () => {
      // Random contexts (M5, M6, M13, M15, L0-L5, MODE1 drawn from hostile values): the same block, the registers intact, the
      // callee-saved set as found; the stock path too.
      const rng = new PyRandom(seed);
      for (const v of [2 * (seed % 16), 2 * (seed % 16) + 1]) {
        const ctx = drawContext(rng);
        const ref = await voiceCall(VoiceRig.sine(), v, 'friendly');
        const out = await voiceCall(VoiceRig.sine(), v, ctx);
        expect(stopsAt(out, RETURN_SW), `${JSON.stringify(ctx)} ${out.halt}`).toBe(true);
        expect(out.reads.out, JSON.stringify(ctx)).toEqual(ref.reads.out);
        contextIntact(out);
        savedIntact(out);
        // the unregistered neighbour falls through with the context untouched
        const stock = VoiceRig.sine();
        stock.setType(Math.floor(v / 2), 0);
        const so = await stock.call(v, { ctx });
        expect(stopsAt(so, STOCK_BP), `${JSON.stringify(ctx)} ${so.halt}`).toBe(true);
        contextIntact(so);
      }
    });
});

const PROBE = `
.SECTION/PM seg_pmco;
.GLOBAL _probe;
_probe:
    I0 = 0x2009B000;
    R0 = M5;
    DM(0, I0) = R0;
    R0 = M6;
    DM(1, I0) = R0;
    R0 = M7;
    DM(2, I0) = R0;
    R0 = M13;
    DM(3, I0) = R0;
    R0 = M14;
    DM(4, I0) = R0;
    R0 = M15;
    DM(5, I0) = R0;
    R0 = L0;
    DM(6, I0) = R0;
    R0 = L1;
    DM(7, I0) = R0;
    R0 = L2;
    DM(8, I0) = R0;
    R0 = L3;
    DM(9, I0) = R0;
    R0 = L4;
    DM(10, I0) = R0;
    R0 = L5;
    DM(11, I0) = R0;
    R0 = MODE1;
    DM(12, I0) = R0;
    JUMP (M13, I13);
`;

describe('core/dsp-voice context and breadcrumbs on the rig', () => {
  it('the context is pinned for the routine', async () => {
    // The routine runs with M5 = 0, M6 = 1, M7 = -1, M13 = 0, M14 = 1, M15 = -1, the L registers 0 and MODE1 with SIMD, TRUNCATE and
    // ALUSAT clear and RND32 set, whatever the context was (the ABI in the wrapper's header).
    const rng = new PyRandom(7);
    for (let i = 0; i < 8; i++) {
      const rig = new VoiceRig({
        routine: { text: PROBE, base: 0xb93800, externs: () => ({}), symbol: '_probe' },
      });
      rig.setType(0, TYPE);
      rig.setNote(0, 60);
      rig.recWord(0, 0x1b8, 1);
      const ctx = drawContext(rng);
      const out = await rig.call(0, { ctx, reads: { seen: [nw(VOICE_STATE), 13] } });
      const at = JSON.stringify(ctx);
      expect(stopsAt(out, RETURN_SW), `${at} ${out.halt}`).toBe(true);
      const seen = out.reads.seen!;
      expect(seen.slice(0, 6), at).toEqual([0, 1, 0xffffffff, 0, 1, 0xffffffff]);
      expect(seen.slice(6, 12), at).toEqual([0, 0, 0, 0, 0, 0]);
      expect(seen[12]! & 0x20a000, at).toBe(0);
      expect(seen[12]! & 0x10000, at).not.toBe(0);
      contextIntact(out);
    }
  });

  it('breadcrumbs name each step and reach both reply records', async () => {
    // The wrapper writes stage | voice << 8 | type << 16 before each step (finding 48): into its own diagnostic word 12 and, at once,
    // into both reply records with the header and the count of dispatches, so that a core that stops inside the routine leaves them.
    for (const v of [0, 4, 30]) {
      const rig = crumbRig();
      rig.setRings();
      const t = Math.floor(v / 2);
      rig.setType(t, TYPE);
      rig.setNote(t, 60);
      rig.recWord(v, 0x1b8, 1);
      const reads: Record<string, [number, number]> = {
        ra: [nw(RING_A), 0x400 / 4],
        rb: [nw(RING_B), 0x400 / 4],
        diag: [nw(DIAG), 16],
      };
      const out = await rig.call(v, { reads, ctx: 'friendly' });
      expect(stopsAt(out, RETURN_SW), out.halt).toBe(true);
      const want = crumb(7, v, TYPE); // the epilogue is the last step
      expect([out.reads.diag![12], out.reads.diag![13], out.reads.diag![14]], `voice ${v}`).toEqual([want, 1, 1]); // one dispatch, one return
      for (const ring of ['ra', 'rb'] as const) {
        const r = out.reads[ring]!;
        expect([r[192], r[193], r[213]], `${ring} ${v}`).toEqual([want, 1, 1]);
        expect(r[45], `${ring} ${v}`).toBe(0x20d1a6); // the header: 0xD1A6, 32 words
        // nothing else in the record was written
        expect(r.filter((x) => x).length, `${ring} ${v}`).toBe(4);
      }
    }
  });

  it('the module step word is cleared before the call and the records are left alone', async () => {
    // The module step is the static word `_dk_step` (its address is `_dv_rec[2]`, dsp-link.inc); the wrapper clears it right before it
    // calls the routine, so a step copied by the block tail belongs to a call of this block. The reply records' word 298 is the tail's,
    // the wrapper does not touch it.
    const rig = crumbRig();
    rig.setRings();
    rig.setType(1, TYPE);
    rig.setNote(1, 60);
    rig.recWord(2, 0x1b8, 1);
    rig.dm.set(nw(RING_A) + 298, 7);
    rig.dm.set(nw(RING_B) + 298, 9);
    rig.dm.set(nw(STEP_WORD), 5);
    const reads: Record<string, [number, number]> = {
      ra: [nw(RING_A) + 296, 4],
      rb: [nw(RING_B) + 296, 4],
      step: [nw(STEP_WORD), 1],
    };
    const out = await rig.call(2, { reads, ctx: 'friendly' });
    expect(stopsAt(out, RETURN_SW), out.halt).toBe(true);
    expect(out.reads.step).toEqual([0]);
    expect(out.reads.ra![2]).toBe(7);
    expect(out.reads.rb![2]).toBe(9);
  });

  it('the routine sees the called breadcrumb', async () => {
    const rig = crumbRig();
    rig.setRings();
    rig.setType(1, TYPE);
    rig.setNote(1, 60);
    rig.recWord(2, 0x1b8, 1);
    const out = await rig.call(2, { reads: { seen: [nw(VOICE_STATE), 4] }, ctx: 'friendly' });
    expect(stopsAt(out, RETURN_SW), out.halt).toBe(true);
    const want = crumb(4, 2, TYPE); // ROUTINE_CALLED: voice 2, the type in play
    expect(out.reads.seen).toEqual([want, want, want, 1]);
  });

  it('the odd voice counts no dispatch and ends at the epilogue', async () => {
    const rig = crumbRig();
    rig.setRings();
    rig.setType(1, TYPE);
    rig.setNote(1, 60);
    const out = await rig.call(3, { reads: { diag: [nw(DIAG), 16] }, ctx: 'friendly' });
    expect(stopsAt(out, RETURN_SW), out.halt).toBe(true);
    expect(out.reads.diag![12]).toBe(crumb(7, 3, TYPE));
    expect(out.reads.diag!.slice(13, 15)).toEqual([0, 0]);
  });

  it('the stock path leaves one breadcrumb and an unregistered added type two', async () => {
    const rig = crumbRig();
    rig.setRings();
    rig.setType(2, 0);
    const reads: Record<string, [number, number]> = { diag: [nw(DIAG), 16], ra: [nw(RING_A), 0x400 / 4] };
    let out = await rig.call(5, { reads, ctx: 'friendly' });
    expect(stopsAt(out, STOCK_BP), out.halt).toBe(true);
    expect(out.reads.diag![12]).toBe(crumb(1, 5, 0));
    expect(out.reads.diag![13]).toBe(0);
    expect(any(out.reads.ra), 'a stock type writes nothing into the reply records').toBe(false);
    // type 12: no module registered it (the table holds TYPE only); the stock path runs, with a breadcrumb that says so
    rig.setType(2, 12);
    out = await rig.call(5, { reads, ctx: 'friendly' });
    expect(stopsAt(out, STOCK_BP), out.halt).toBe(true);
    expect(out.reads.diag![12]).toBe(crumb(3, 5, 12));
    expect(out.reads.ra![192]).toBe(crumb(3, 5, 12));
    expect(out.regs.R3).toBe(out.in.R3);
    expect(out.regs.R5).toBe(out.in.R5);
    // a type below 7 that is not registered either: the engine's own, no stage 3
    rig.setType(2, 6);
    out = await rig.call(5, { reads, ctx: 'friendly' });
    expect(out.reads.diag![12]).toBe(crumb(1, 5, 0));
  });

  it('the bisect build takes the stock path for added types with no call', async () => {
    // DV_BISECT_C2 (finding 49, docs/PATCHING.md "Rung C2"): an added type, registered or not, runs the stock renderer with the stage 3
    // breadcrumb written inline, the same words as the stock build's for an unregistered type. A breakpoint on `_dv_crumb` proves that
    // the bisect build never enters it (so no CALL and no RTS run on this path), and a registered type is not dispatched (the dispatch
    // counter counts the stage 3 passes in this build, the returns stay 0).
    const reads: Record<string, [number, number]> = { diag: [nw(DIAG), 16], ra: [nw(RING_A), 0x400 / 4] };
    const bisect = ['DV_BISECT_C2'];
    for (const t of [12, TYPE]) {
      // 12: no module registered it; TYPE: the table holds it
      const rig = crumbRig(bisect);
      rig.setRings();
      rig.setType(2, t);
      const out = await rig.call(5, { reads, ctx: 'friendly', breaks: [rig.coreSymbols._dv_crumb!] });
      expect(stopsAt(out, STOCK_BP), `${t} ${out.halt}`).toBe(true);
      // the dispatch counter counts stage 3 passes in this build; the routine is never called, so no return
      expect(out.reads.diag![12]).toBe(crumb(3, 5, t));
      expect(out.reads.diag!.slice(13, 15)).toEqual([1, 0]);
      expect(out.reads.ra![192]).toBe(crumb(3, 5, t));
      expect(out.reads.ra![193]).toBe(1);
      expect(out.regs.R3).toBe(out.in.R3);
      expect(out.regs.R5).toBe(out.in.R5);
    }
    // the stock build writes the same words for the unregistered type
    let rig = crumbRig();
    rig.setRings();
    rig.setType(2, 12);
    const ref = await rig.call(5, { reads, ctx: 'friendly' });
    expect(ref.reads.diag![12]).toBe(crumb(3, 5, 12));
    expect(ref.reads.ra![192]).toBe(crumb(3, 5, 12));
    // a type below 7 is the engine's own in both builds: no breadcrumb beyond stage 1
    rig = crumbRig(bisect);
    rig.setRings();
    rig.setType(2, 6);
    const out = await rig.call(5, { reads, ctx: 'friendly' });
    expect(out.reads.diag![12]).toBe(crumb(1, 5, 0));
  });
});

const C3_COMMON = ['DV_C3_FINE'];
const C3_VARIANTS: Record<string, string[]> = {
  'a-delayed': [...C3_COMMON, 'DV_C3_DB'],
  'b-noirq': [...C3_COMMON, 'DV_C3_NOIRQ'],
  'c-nosys': [...C3_COMMON, 'DV_C3_NOSYS'],
  // rung C3d (finding 49 section 10): the deep breadcrumbs 11 to 22 (the DAG1 register stores through R2 are the default build now)
  'd1-deep': [...C3_COMMON, 'DV_C3_DEEP'],
};

describe('core/dsp-voice build variants on the rig', () => {
  for (const name of Object.keys(C3_VARIANTS).sort())
    it(`the c3 variants keep the registered path working: ${name}`, async () => {
      // Rung C3 (finding 49 section 9, docs/PATCHING.md): each variant removes one suspect from the registered path and must still do
      // what the rung C build does: dispatch the routine on the even voice, copy on the odd voice, end at the epilogue breadcrumb, leave
      // the context as found. The fine breadcrumbs 8, 9 and 10 are written inline in that order before the CALL of stage 2.
      const defines = C3_VARIANTS[name]!;
      const reads: Record<string, [number, number]> = {
        diag: [nw(DIAG), 16],
        ra: [nw(RING_A), 0x400 / 4],
        rb: [nw(RING_B), 0x400 / 4],
      };
      for (const v of [0, 2, 3, 30]) {
        const t = Math.floor(v / 2);
        const rig = crumbRig(defines);
        rig.setRings();
        rig.setType(t, TYPE);
        rig.setNote(t, 60);
        rig.recWord(v, 0x1b8, 1);
        const out = await rig.call(v, { reads, ctx: 'friendly' });
        expect(stopsAt(out, RETURN_SW), `${name} ${v} ${out.halt}`).toBe(true);
        const want = crumb(7, v, TYPE);
        const even = v % 2 === 0 ? 1 : 0;
        expect(out.reads.diag!.slice(12, 15), `${name} ${v}`).toEqual([want, even, even]);
        for (const ring of ['ra', 'rb'] as const) {
          expect(out.reads[ring]![192]).toBe(want);
          expect(out.reads[ring]![45]).toBe(0x20d1a6);
        }
        contextIntact(out);
        expect(out.regs.I7, 'RFRAME: I7 = I6, as the stock epilogue').toBe(out.in.I6);
      }
      // the fine breadcrumbs, in order, at the label after each one
      const rig = crumbRig(defines);
      rig.setRings();
      rig.setType(1, TYPE);
      rig.setNote(1, 60);
      rig.recWord(2, 0x1b8, 1);
      for (const [label, stage] of [
        ['_dv_f1b', 8],
        ['_dv_f2b', 9],
        ['_dv_f3b', 10],
      ] as const) {
        const out = await rig.call(2, { reads, ctx: 'friendly', breaks: [rig.coreSymbols[label]!] });
        expect(out.reads.diag![12], `${name} ${label}`).toBe(crumb(stage, 2, TYPE));
        expect(out.reads.ra![192]).toBe(crumb(stage, 2, TYPE));
        expect(out.reads.rb![192]).toBe(crumb(stage, 2, TYPE));
        // none of the fine breadcrumbs counts as a dispatch
        expect(out.reads.diag!.slice(13, 15)).toEqual([0, 0]);
      }
    });

  it('the deep breadcrumbs come in order inside the frame save: d1-deep', async () => {
    // Rung C3d: twelve more inline breadcrumbs (stages 11 to 22) between the MODIFY of I7 and the save of MODE1, each at the label after
    // it, none counted as a dispatch, none touching the registers the frame save has not stored yet (R0, R2, I4 only).
    const reads: Record<string, [number, number]> = { diag: [nw(DIAG), 16], ra: [nw(RING_A), 0x400 / 4] };
    const rig = crumbRig(C3_VARIANTS['d1-deep']);
    rig.setRings();
    rig.setType(1, TYPE);
    rig.setNote(1, 60);
    rig.recWord(2, 0x1b8, 1);
    for (let stage = 11; stage < 23; stage++) {
      const out = await rig.call(2, { reads, ctx: 'friendly', breaks: [rig.coreSymbols[`_dv_d${stage}b`]!] });
      expect(out.reads.diag![12], `stage ${stage}`).toBe(crumb(stage, 2, TYPE));
      expect(out.reads.ra![192]).toBe(crumb(stage, 2, TYPE));
      expect(out.reads.diag!.slice(13, 15)).toEqual([0, 0]);
    }
    const out = await rig.call(2, { reads, ctx: 'friendly' });
    expect(stopsAt(out, RETURN_SW), out.halt).toBe(true);
    contextIntact(out);
  });

  it('the c3 noirq variant masks interrupts inside and restores MODE1', async () => {
    const rig = crumbRig(C3_VARIANTS['b-noirq']);
    rig.setRings();
    rig.setType(1, TYPE);
    rig.setNote(1, 60);
    rig.recWord(2, 0x1b8, 1);
    const ctx = { MODE1: 0x1011800 | 0x8000 }; // IRPTEN set, TRUNCATE set: the pin clears it, the epilogue puts it back
    const inside = await rig.call(2, { ctx, breaks: [rig.coreSymbols._dv_f3b!] });
    expect(inside.regs.MODE1! & 0x1000, 'IRPTEN is clear once the registered path is past its mask').toBe(0);
    expect(inside.regs.MODE1! & 0x8000, 'the pins still apply').toBe(0);
    let out = await rig.call(2, { ctx });
    expect(stopsAt(out, RETURN_SW), out.halt).toBe(true);
    expect(u32(out.regs.MODE1!)).toBe(u32(out.in.MODE1!));
    expect(out.regs.MODE1! & 0x1000).not.toBe(0);
    // the stock path and the engine's own types never touch IRPTEN
    rig.setType(2, 6);
    out = await rig.call(5, { ctx });
    expect(stopsAt(out, STOCK_BP)).toBe(true);
    expect(u32(out.regs.MODE1!)).toBe(u32(out.in.MODE1!));
  });

  it('the c3 nosys variant writes no system register on the registered path', () => {
    const text = applyConditionals(readFileSync(VOICE_CORE, 'utf8'), C3_VARIANTS['c-nosys']);
    const path = text.split('_dv_stock:')[0]!;
    expect(path).not.toContain('MODE1 =');
    expect(path).not.toContain('BIT SET MODE1');
    expect(path).not.toContain('BIT CLR MODE1');
    expect(path, 'the read stays (harmless); only the writes are gone').toContain('R3 = MODE1;');
  });

  it('every build restores B3 before I3', () => {
    // A load of a B register also loads its I register (PRM p.6-24, "Circular Buffer Programming Model"); the stock epilogue restores B3
    // first, in every build (the emulator does not model the I load).
    const order = (defines: string[]): [number, number] => {
      const lines = applyConditionals(readFileSync(VOICE_CORE, 'utf8'), defines)
        .split('\n')
        .map((x) => x.trim());
      const at = (prefix: string) => lines.findIndex((x) => x.startsWith(prefix));
      return [at('B3 = DM(-24, I6);'), at('I3 = DM(-19, I6);')];
    };
    for (const defines of [C3_VARIANTS['a-delayed']!, []]) {
      const [b, i] = order(defines);
      expect(b).toBeGreaterThanOrEqual(0);
      expect(b).toBeLessThan(i);
    }
  });

  it('without reply records the breadcrumb stays local', async () => {
    const rig = crumbRig(); // core/dsp-link absent or before its first block: _dv_rec holds 0, nothing may be written through it
    rig.setType(0, TYPE);
    rig.setNote(0, 60);
    rig.recWord(0, 0x1b8, 1);
    const out = await rig.call(0, { reads: { diag: [nw(DIAG), 16], low: [0, 256] }, ctx: 'friendly' });
    expect(stopsAt(out, RETURN_SW), out.halt).toBe(true);
    expect(out.reads.diag![12]).toBe(crumb(7, 0, TYPE));
    expect(out.reads.diag!.slice(13, 15)).toEqual([1, 1]);
    expect(any(out.reads.low)).toBe(false);
  });
});

async function sineBlocks(rig: VoiceRig, note: number, tune = 64, blocks = 60): Promise<number[]> {
  rig.setType(0, TYPE);
  rig.setNote(0, note, tune);
  rig.recWord(0, 0x1b8, 1);
  const ys: number[] = [];
  for (let i = 0; i < blocks; i++) {
    const out = await rig.call(0, { reads: { state: [nw(VOICE_STATE), 32], rec: [nw(RECORDS) + 110, 1] } });
    expect(stopsAt(out, RETURN_SW), out.halt).toBe(true);
    ys.push(...out.reads.out!.map((w) => w2f(w!)));
    out.reads.state!.forEach((w, k) => rig.dm.set(nw(VOICE_STATE) + k, w ?? 0));
    rig.dm.set(nw(RECORDS) + 110, out.reads.rec![0]!);
  }
  return ys;
}

/** Frequency from the interpolated upward zero crossings (works for a few cycles). */
function peakHz(x: readonly number[], fs = 48000): number {
  const t: number[] = [];
  for (let i = 0; i + 1 < x.length; i++)
    if (x[i]! < 0 && x[i + 1]! >= 0) t.push(i + -x[i]! / (x[i + 1]! - x[i]!));
  expect(t.length, 'fewer than two periods').toBeGreaterThanOrEqual(3);
  return (fs * (t.length - 1)) / (t.at(-1)! - t[0]!);
}

describe('core/dsp-voice raw sine routine on the rig', () => {
  for (const note of [12, 36, 60, 69, 84, 108, 127])
    it(`the sine follows the note within a cent: note ${note}`, async () => {
      const rig = VoiceRig.sine();
      const want = 440 * 2 ** ((note - 69) / 12);
      const y = await sineBlocks(rig, note, 64, Math.max(100, Math.trunc((4.2 * 48000) / want / 32)));
      const got = peakHz(y);
      const cents = 1200 * Math.log2(got / want);
      expect(Math.abs(cents), `${note} ${got} ${want}`).toBeLessThan(1);
      const amp = Math.max(...y.map(Math.abs));
      expect(amp).toBeGreaterThan(0.35); // velocity 100 / 127 * 0.5 = 0.3937
      expect(amp).toBeLessThan(0.4);
    });

  it('tune shifts the pitch by semitones', async () => {
    const rig = VoiceRig.sine();
    const y = await sineBlocks(rig, 60, 65, 100);
    const want = 440 * 2 ** ((61 - 69) / 12);
    expect(Math.abs(1200 * Math.log2(peakHz(y) / want))).toBeLessThan(1);
  });

  it('a retrigger restarts the oscillator and a stopped voice is silent', async () => {
    const rig = VoiceRig.sine();
    const y = await sineBlocks(rig, 60, 64, 4);
    expect(Math.abs(y[0]!), 'the oscillator starts at phase 0').toBeLessThan(1e-6);
    // byte +0x1b9 now holds the mark; the arm routine's short store of 1 at +0x1b8 clears it: a new note
    rig.recWord(0, 0x1b8, 1);
    let out = await rig.call(0, { reads: { state: [nw(VOICE_STATE), 32] } });
    const y2 = out.reads.out!.map((w) => w2f(w!));
    expect(Math.abs(y2[0]!)).toBeLessThan(1e-6);
    // ACTIVE = 0: zeros
    rig.recWord(0, 0x1b8, 0);
    out = await rig.call(0);
    expect(out.reads.out).toEqual(Array(32).fill(0));
  });

  it('a machine with a parameter on knob 7 gets a fixed level before the stock unpack', async () => {
    // Row word 9 is knob 7's declared range: a machine whose parameters reach knob 7 (param_slots) has one, and its tracks' LEV word in
    // the frame copy becomes 0x6400 (level 100) before the stock unpack reads it; the word's other half stays.
    const rig = VoiceRig.sine();
    const lev = nw(FRAME + 0xec + 0x60 * 3);
    rig.setType(3, TYPE);
    rig.dm.set(lev, 0x12340100);
    rig.dm.set(nw(TABLE) + 9, 0x7f00 << 16);
    let out = await rig.unpack(3);
    expect(stopsAt(out, UNPACK_BP)).toBe(true);
    expect(out.reads.lev).toEqual([0x12346400]);
    rig.dm.set(nw(TABLE) + 9, 0); // knob 7 is the stock LEV
    out = await rig.unpack(3);
    expect(stopsAt(out, UNPACK_BP)).toBe(true);
    expect(out.reads.lev).toEqual([0x12340100]);
  });

  it('the unpack hook leaves other tracks and costs a check without rows', async () => {
    const rig = VoiceRig.sine();
    rig.dm.set(nw(TABLE) + 9, 0x7f00 << 16);
    rig.setType(3, TYPE);
    rig.setType(4, 2);
    const lev = nw(FRAME + 0xec + 0x60 * 4);
    rig.dm.set(lev, 0x0100);
    let out = await rig.unpack(4);
    expect(stopsAt(out, UNPACK_BP)).toBe(true);
    expect(out.reads.lev).toEqual([0x0100]);
    out = await rig.unpack(3, 0);
    expect(stopsAt(out, UNPACK_BP)).toBe(true);
    expect(out.instructions).toBeLessThan(6);
  });
});
