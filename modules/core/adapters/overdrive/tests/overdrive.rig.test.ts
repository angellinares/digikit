/**
 * core/overdrive on the concrete SHARC+ core (SHARC-only rig): `_od_sat` called the way the stock chain driver calls the overdrive
 * (C frame, record, buffers, R14 = 32 - voice), against a float reference of the curves; `_od_split2` on a frame copy.
 * Port of test_overdrive.py (T1g phase 5).
 */
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import { RawRig, f2w, w2f } from '@digi/sharc-rig-kit';

const SRC = path.join(import.meta.dirname, '../src/dsp/overdrive.s');
const BASE = 0xb90000;
// stand-ins: the stock routine, the hook's tail, the caller
const [STOCK, TAIL, RET] = [0xb9f000, 0xb9f010, 0xb9f100];
const FRAME = 0x200c0000; // the frame copy's stand-in: byte addresses, as the DSP addresses its frame
// byte addresses (L2), as index registers address them
const [REC, IN, OUT, FP] = [0x200b0000, 0x200b0100, 0x200b0200, 0x200b1000];
const CALLER_I6 = 0x200b2000;

const u32 = (n: number): number => n >>> 0;

async function openRig() {
  const rig = await RawRig.open({
    objects: [{ text: readFileSync(SRC, 'utf8'), includes: [path.dirname(SRC)], base: BASE }],
    externs: { _fw_frame: FRAME, _fw_sat: STOCK, _od_tail: TAIL, _od_tail2: TAIL },
    returnSw: RET,
    breaks: [STOCK, TAIL],
  });
  const ctx: Record<string, number> = { M5: 0, M6: 1, M7: 0xffffffff, M14: 1 };
  for (const r of ['L3', 'L4', 'L5', 'L6', 'L7']) ctx[r] = 0;
  const w = (a: number, k: number) => a / 4 + k;

  return {
    rig,
    ctx,
    w,
    flavour: (track: number, f: number) => rig.poke(rig.data._od_flav! / 4 + track, f),
    async sat(voice: number, x: number[], rec: Record<number, number>, n = 32, stock = false) {
      for (let k = 0; k < 24; k++) rig.poke(w(REC, k), f2w(rec[k] ?? 0));
      x.forEach((v, k) => rig.poke(w(IN, k), f2w(v)));
      rig.poke(w(FP, 1), n);
      rig.poke(w(FP, 0), CALLER_I6);
      rig.poke(w(FP, -1), RET - 1);
      const regs: Record<string, number> = { ...ctx, R4: REC, R8: IN, R12: OUT, R14: 32 - voice, I6: FP, I7: FP - 8 };
      for (const r of ['R3', 'R5', 'R7', 'R9', 'R10', 'R11', 'R13', 'R15', 'I3', 'I5'])
        regs[r] = 0x1000 + Object.keys(regs).length;
      const keep: Record<string, number> = {};
      for (const [r, v] of Object.entries(regs))
        if ('RI'.includes(r[0]!) && !['R0', 'R1', 'R2', 'R4', 'R8', 'R12', 'I4'].includes(r)) keep[r] = v;
      // the return's RFRAME: the caller's frame
      if (!stock) [keep.I6, keep.I7] = [CALLER_I6, FP];
      const res = await rig.call('_od_sat', regs, {
        readRegs: Object.keys(keep),
        reads: [
          [w(OUT, 0), 32],
          [w(REC, 0), 14],
        ],
      });
      return {
        halt: res.halt,
        finalPc: res.final_pc,
        out: (res.reads[0] ?? []).map((v) => Math.fround(w2f(v ?? 0))),
        rec: (res.reads[1] ?? []).map((v) => w2f(v ?? 0)),
        changed: Object.entries(keep)
          .filter(([r, v]) => res.regs[r] == null || u32(res.regs[r]!) !== u32(v))
          .map(([r]) => r),
      };
    },
  };
}

function shape(f: number, z: number): number {
  const clamp = (v: number, lo: number, hi: number) => Math.min(Math.max(v, lo), hi);
  switch (f) {
    case 0: {
      let y = clamp(z / 8, -1, 1);
      for (let i = 0; i < 3; i++) y *= 2 - Math.abs(y);
      return y;
    }
    case 1:
      return Math.tanh(4 * z);
    case 2:
      return clamp(z, -1, 1);
    case 3:
      return z / (1 + Math.abs(z));
    case 4:
      return Math.tanh(z + 0.4) - Math.tanh(0.4);
    case 5: {
      const t = (z + 1) * 0.25 + 64;
      return 1 - 4 * Math.abs(t - Math.floor(t) - 0.5);
    }
    case 6:
      return clamp(4 * z + 0.3, -0.6, 1) - 0.3;
    default:
      return clamp(64 * z, -1, 1);
  }
}

function reference(f: number, x: number[], rec: Record<number, number>): { out: number[]; rec: Record<number, number> } {
  let dt = Math.min(Math.max(rec[0] ?? 0, rec[1] ?? 0), 1);
  dt *= 1 + (dt * dt) / 2;
  const mt = Math.min(Math.max(rec[2] ?? 0, 0), 1);
  let d = rec[5] ?? 0;
  let m = rec[12] ?? 0;
  const ds = (dt - d) / 32;
  const ms = (mt - m) / 32;
  const out: number[] = [];
  for (const v of x) {
    const y = shape(f, 8 * d * v);
    out.push(v + m * (y - v));
    d += ds;
    m += ms;
  }
  return { out, rec: { 4: rec[5] ?? 0, 5: dt, 6: ds, 11: rec[12] ?? 0, 12: mt, 13: ms } };
}

const X = Array.from({ length: 32 }, (_, i) => Math.sin(((i * 2 * Math.PI) / 32) * 3) * 0.6);

describe('core/overdrive on the rig', () => {
  const names = ['stk', 'tnh4', 'hard', 'soft', 'tube', 'fold', 'fuzz', 'sqr'];
  names.forEach((name, f) =>
    it(`each flavour is its curve with the stock ramps: ${name}`, async () => {
      const o = await openRig();
      try {
        o.flavour(5, f);
        const rec = { 0: 0.3, 1: 0.1, 2: 0.8, 5: 0.15, 12: 0.4 };
        const res = await o.sat(11, X, rec); // voice 11: track 5
        expect(res.finalPc).toBe(RET);
        expect(res.halt).toBe('breakpoint');
        expect(res.changed).toEqual([]);
        const want = reference(f, X, rec);
        // numpy assert_allclose: |a - b| <= atol + rtol * |b|, atol 2e-4, rtol 1e-7
        res.out.forEach((v, k) => expect(Math.abs(v - want.out[k]!)).toBeLessThanOrEqual(2e-4 + 1e-7 * Math.abs(want.out[k]!)));
        for (const [k, v] of Object.entries(want.rec)) expect(Math.abs(res.rec[Number(k)]! - v), `word ${k}`).toBeLessThan(1e-6);
      } finally {
        await o.rig.close();
      }
    }),
  );

  it('odd blocks continue in the stock routine', async () => {
    const o = await openRig();
    try {
      o.flavour(5, 3);
      const res = await o.sat(10, X, { 0: 0.5, 2: 1.0 }, 16, true); // track 5, but not a 32-sample block
      expect(res.finalPc).toBe(STOCK);
      expect(res.changed).toEqual([]);
    } finally {
      await o.rig.close();
    }
  });

  it('full drive is one and a half times the stock drive', async () => {
    const o = await openRig();
    try {
      let res = await o.sat(0, X, { 0: 1.0, 2: 1.0, 5: 1.5, 12: 1.0 });
      expect(res.rec[5]).toBe(1.5);
      res = await o.sat(0, X, { 0: 0.5, 2: 1.0 });
      expect(Math.abs(res.rec[5]! - 0.5625)).toBeLessThan(1e-7);
    } finally {
      await o.rig.close();
    }
  });

  it('the split keeps the routing bit and the flavours', async () => {
    const o = await openRig();
    try {
      const vals = Array.from({ length: 16 }, (_, t) => (t * 0x100) & 0xf00);
      const word = (t: number) => (FRAME + 0x138 + 0x60 * t) / 4;
      vals.forEach((v, t) => o.rig.poke(word(t), 0x4000_0000 | v));
      const res = await o.rig.call('_od_split2', { ...o.ctx, R0: 0x1234, R1: 0x5678 }, {
        readRegs: ['R0', 'R1'],
        reads: [
          ...vals.map((_, t) => [word(t), 1] as [number, number]),
          [o.rig.data._od_flav! / 4, 16],
        ],
      });
      expect(res.final_pc).toBe(TAIL);
      vals.forEach((v, t) => {
        expect(res.reads[t]![0], `track ${t}`).toBe((0x4000_0000 | (v & 0x100)) >>> 0);
        expect(res.reads[16]![t]).toBe(v >> 9);
      });
      expect(res.regs.R0).toBe(0x1234);
      expect(res.regs.R1).toBe(0x5678);
    } finally {
      await o.rig.close();
    }
  });
});
