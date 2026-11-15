/**
 * The runtime guard of core/dsp-voice (docs/findings/51-pluck-tune-guard.md) on the SHARC-only rig.
 * Port of test_dsp_guard.py (T1g phase 5).
 *
 * The wrapper and a fixture routine are assembled from the module sources and run the way the engine calls them. Fixtures
 * (tests/fixtures/modules/dyn-guard-nan/src/dsp): `clean` (a ramp, inside the bound), `nan_legacy` (NaN while the voice is active),
 * `loud` (100.0 while active). The guard clamps the machine knob words to the row's declared ranges for the call, puts the engine's
 * words back, checks the block (finite, at most 8.0 in magnitude), and on a fault zeroes the block, ends the voice, counts the fault in
 * the row and the diagnostic words, and after 8 faults in 1500 blocks disables the type.
 */
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import {
  DIAG,
  FRAME,
  GUARD,
  REC,
  RECORDS,
  RETURN_SW,
  TABLE,
  VOICE_ROW,
  VOICE_STATE,
  VOICE_TYPE,
  VoiceRig,
  nw,
  stopsAt,
  w2f,
  type VoiceCall,
} from '@digi/sharc-rig-kit';

const TYPE = VOICE_TYPE;
const FIX = path.resolve(import.meta.dirname, '../../../../../tests/fixtures/modules/dyn-guard-nan/src/dsp');
const BASE = 0xb93800;
const CL_SEEN = VOICE_STATE; // the clean fixture's copy of the knob words it saw
const RAMP = Array.from({ length: 32 }, (_, k) => k * 0.125);
// words of _dv_diag (dl_diag words 17, 18, 19 to 22, 23)
const [D_FAULTS, D_TYPEVOICE, D_PARAMS, D_DISABLED] = [15, 16, 17, 21];
const any = (xs: readonly number[]): boolean => xs.some((x) => x !== 0);

/** The wrapper around one of the fixture routines. */
class GuardRig extends VoiceRig {
  constructor(fixture: string, symbol: string) {
    super({
      routine: {
        text: readFileSync(path.join(FIX, `${fixture}.s`), 'utf8'),
        base: BASE,
        externs: (core) => ({ _dv_frame_hw: core._dv_frame_hw!, _dv_exp2: core._dv_exp2!, _cl_seen: CL_SEEN }),
        symbol,
      },
    });
  }

  /** Declare the range of knob j (`{ 1: [lo, hi] }`) in the registered type's row. */
  ranges(knobs: Record<number, [number, number]>): void {
    for (const [j, [lo, hi]] of Object.entries(knobs)) this.dm.set(nw(TABLE) + 2 + Number(j), (lo | (hi << 16)) >>> 0);
  }

  knobs(track: number, words: number[]): void {
    words.forEach((w, j) => this.hw(0x6d + 0x30 * track + j + 2 * Math.floor(j / 4), w));
  }

  arm(track = 0): void {
    this.recWord(2 * track, 0x1b8, 1);
  }

  static rec(v: number): number {
    return RECORDS + v * REC;
  }

  /** One call of the even voice of `track`; the state the next call needs is carried over. */
  async step(track = 0, blocksBefore = 0): Promise<[number[], VoiceCall]> {
    const v = 2 * track;
    if (blocksBefore) this.dm.set(nw(DIAG), this.word(nw(DIAG)) + blocksBefore);
    const out = await this.call(v, {
      reads: {
        diag: [nw(DIAG), 32],
        row: [nw(TABLE), VOICE_ROW],
        g: [nw(GUARD), 32],
        frame: [nw(FRAME), 256],
        flags: [nw(GuardRig.rec(v) + 0x1b8), 1],
        seen: [nw(CL_SEEN), 8],
      },
      maxSteps: 60000,
      ctx: 'friendly',
    });
    expect(stopsAt(out, RETURN_SW), out.halt).toBe(true);
    for (const [key, base] of [
      ['diag', DIAG],
      ['row', TABLE],
      ['g', GUARD],
      ['frame', FRAME],
    ] as const)
      out.reads[key]!.forEach((w, k) => this.dm.set(nw(base) + k, w ?? 0));
    this.dm.set(nw(GuardRig.rec(v) + 0x1b8), out.reads.flags![0]!);
    return [out.reads.out!.map((w) => Math.fround(w2f(w!))), out];
  }
}

const clean = () => new GuardRig('clean', '_cl_render');

describe('core/dsp-voice runtime guard on the rig', () => {
  it('a clean routine is untouched', async () => {
    const rig = clean();
    const words = [0x4000, 0x1000, 0x2000, 0x1234, 0x7f00, 0x0, 0x0abc, 0x7f00];
    rig.setType(0, TYPE);
    rig.knobs(0, words);
    rig.arm();
    const frame0 = Array.from({ length: 256 }, (_, k) => rig.word(nw(FRAME) + k));
    const [y, out] = await rig.step();
    expect(y).toEqual(RAMP);
    expect(out.reads.seen, 'in range: the routine sees the engine words').toEqual(words);
    expect(out.reads.frame, 'the frame copy is as the engine left it').toEqual(frame0);
    expect(out.reads.flags![0]! & 0xffff, 'ACTIVE and the mark are the routine own').toBe(0x5a01);
    expect(out.reads.diag![D_FAULTS]).toBe(0);
    expect(out.reads.diag![D_TYPEVOICE]).toBe(0);
    expect(out.reads.g![6], 'no word was changed').toBe(0);
  });

  it('the knob words are clamped to the declared ranges for the call only', async () => {
    const rig = clean();
    rig.setType(3, TYPE);
    // knob 1 declares 0x1000 .. 0x2000, knob 5 1 .. 5; the others are undeclared (0 .. 0x7f00: 0x2001 passes)
    rig.ranges({ 1: [0x1000, 0x2000], 5: [1, 5] });
    const words = [0xffff, 0x0800, 0x2001, 0x0000, 0x8000, 0x0000, 0x7f01, 0x7f00];
    rig.knobs(3, words);
    rig.arm(3);
    const frame0 = Array.from({ length: 256 }, (_, k) => rig.word(nw(FRAME) + k));
    const [y, out] = await rig.step(3);
    expect(out.reads.seen).toEqual([0x7f00, 0x1000, 0x2001, 0, 0x7f00, 1, 0x7f00, 0x7f00]);
    expect(out.reads.frame, 'the engine words are put back, neighbours untouched').toEqual(frame0);
    expect(out.reads.g![6], 'the knobs the clamp changed').toBe(0b01110011);
    expect(y).toEqual(RAMP);
  });

  it('a NaN block is zeroed, the voice ends and the fault is counted', async () => {
    const rig = new GuardRig('nan_legacy', '_gn_render');
    rig.setType(0, TYPE);
    const words = [0x4000, 0x1000, 0x2000, 0, 0x7000, 0x0100, 0, 0x7f00];
    rig.knobs(0, words);
    rig.arm();
    const [y, out] = await rig.step();
    expect(any(y)).toBe(false);
    expect(y.every(Number.isFinite)).toBe(true);
    expect(out.reads.flags![0]! & 0xffff, 'ACTIVE and the mark are clear: the voice ended').toBe(0);
    const d = out.reads.diag!;
    expect(d[D_FAULTS]! & 0xffff, 'one fault').toBe(1);
    expect((d[D_FAULTS]! >>> 16) & 0xff, 'kind 1 (Inf or NaN)').toBe(1);
    expect(d[D_TYPEVOICE]).toBe(TYPE);
    const got: number[] = [];
    for (let m = 0; m < 4; m++) got.push(d[D_PARAMS + m]! & 0xffff, d[D_PARAMS + m]! >>> 16);
    expect(got, 'the 8 words of the faulting call').toEqual(words);
    expect(out.reads.row![13], "the type's own count").toBe(1);
    // an idle voice renders silence: no fault
    const [y2, out2] = await rig.step();
    expect(any(y2)).toBe(false);
    expect(out2.reads.diag![D_FAULTS]).toBe(d[D_FAULTS]);
  });

  it('an overlevel block is a fault of kind 2', async () => {
    const rig = new GuardRig('loud', '_ld_render');
    rig.setType(0, TYPE);
    rig.arm();
    const [y, out] = await rig.step();
    expect(any(y)).toBe(false);
    expect((out.reads.diag![D_FAULTS]! >>> 16) & 0xff).toBe(2);
  });

  it('the 8th fault within a second disables the type and a slow trickle does not', async () => {
    const rig = new GuardRig('nan_legacy', '_gn_render');
    rig.setType(0, TYPE);
    // a fault every 1600 blocks (> 1500): the window restarts each time, the type stays on
    for (let i = 0; i < 10; i++) {
      rig.arm();
      await rig.step(0, 1600);
    }
    expect(rig.word(nw(TABLE) + 10)).toBe(0);
    expect(rig.word(nw(DIAG) + D_FAULTS) >>> 24).toBe(0);
    expect(rig.word(nw(TABLE) + 13)).toBe(10);
    // eight in a row, one block apart
    let out: VoiceCall | undefined;
    for (let i = 0; i < 8; i++) {
      rig.arm();
      let y: number[];
      [y, out] = await rig.step(0, 1);
      expect(any(y)).toBe(false);
    }
    const d = out!.reads.diag!;
    expect(out!.reads.row![10], 'the row names the disabled type').toBe(TYPE);
    expect(d[D_FAULTS]! >>> 24).toBe(1);
    expect(d[D_DISABLED]).toBe(TYPE);
    const faults = d[D_FAULTS]! & 0xffff;
    expect(faults, 'the last slow fault opened the window: the 8th in it is the 7th of the quick run').toBe(17);
    // the disabled type is not called any more: a silent block, ACTIVE as the engine set it, no new fault
    rig.arm();
    const [y, o2] = await rig.step(0, 1);
    expect(any(y)).toBe(false);
    expect(o2.reads.flags![0]! & 0xffff).toBe(1);
    expect(o2.reads.diag![D_FAULTS]! & 0xffff).toBe(faults);
    // the odd voice copies the (silent) block
    const odd = await rig.call(1, { ctx: 'friendly' });
    expect(any(odd.reads.out!.map((w) => w ?? 0))).toBe(false);
  });
});
