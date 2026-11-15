/**
 * The reset mirrors and the guard (the unsupported-context fallback) of core/dsp-voice on DN2 1.12, on the SHARC-only rig with the stock
 * DSP image. Port of test_dsp_stereo_lifecycle.py (T1g phase 5b).
 *
 * The reset wrappers run the real stock resets of the image. The guard of the twins jumps to a logging stand-in (`stub: true`): they are
 * about what the wrapper does around the stock call, not about the stock stage. Original firmware is supplied at run time.
 */
import { describe, expect, it } from 'vitest';
import {
  BUFFERS,
  DN2_112,
  ENGINE,
  HEARTBEAT,
  STOCK,
  STUB_LOG,
  StereoRig,
  call,
  filled,
  newSession,
  stockAvailable,
  stockSkipReason,
  type StereoSession,
} from '@digi/sharc-rig-kit';

const OTHER = 9;
const SLOT = 5;
const LBUF = BUFFERS + 0x80 * SLOT;

const stamps = (rig: StereoRig, s: StereoSession, slots: Set<number>): void => {
  for (let v = 0; v < 16; v++) s.poke(rig.data._gs_stamp! + 4 * v, slots.has(v) ? HEARTBEAT : 0);
};
const seen = (rig: StereoRig, s: StereoSession, slots: Set<number>): void => {
  for (let v = 0; v < 16; v++) s.poke(rig.data._gs_seen! + 4 * v, slots.has(v) ? HEARTBEAT : 0);
};
const stubLog = (s: StereoSession): Promise<number[]> => s.words(STUB_LOG, 4);
const counters = (rig: StereoRig, s: StereoSession): Promise<number[]> => s.words(rig.data._gs_cnt!, 32);
const oneHot = (index: number): number[] => Array.from({ length: 32 }, (_, i) => Number(i === index));

interface Mirror {
  entry: string;
  stock: string;
  /** L record of slot 0 */
  record: number;
  /** bytes per slot */
  stride: number;
  rrec: string;
  size: number;
  /** the R words the reset sets: word -> value */
  sets: Record<number, number>;
}
const zeros = (...words: number[]): Record<number, number> => Object.fromEntries(words.map((w) => [w, 0]));
const MIRRORS: Mirror[] = [
  { entry: '_gs_fo_reset', stock: '_gs_fo_reset_stock', record: ENGINE + 0xe088, stride: 112, rrec: '_gs_fo_rec', size: 28, sets: zeros(24, 25, 26, 27) },
  { entry: '_gs_uy_reset', stock: '_gs_uy_reset_stock', record: ENGINE + 0x12148, stride: 32, rrec: '_gs_uy_rec', size: 8, sets: zeros(6, 7) },
  { entry: '_gs_held_reset', stock: '_gs_held_reset_stock', record: ENGINE + 0xeec8, stride: 20, rrec: '_gs_hd_rec', size: 5, sets: zeros(0, 1, 2, 3, 4) },
  { entry: '_gs_case1_reset', stock: '_gs_case1_reset_stock', record: ENGINE + 0xf308, stride: 204, rrec: '_gs_c1_rec', size: 51, sets: zeros(3, 4) },
  { entry: '_gs_case2_reset', stock: '_gs_case2_reset_stock', record: ENGINE + 0xffc8, stride: 300, rrec: '_gs_c2_rec', size: 75, sets: zeros(10, 11) },
  { entry: '_gs_case3_reset', stock: '_gs_case3_reset_stock', record: ENGINE + 0x11288, stride: 236, rrec: '_gs_c3_rec', size: 59, sets: zeros(5, 9, 10, 11, 12) },
  { entry: '_gs_case5_reset', stock: '_gs_case5_reset_stock', record: ENGINE + 0x127c8, stride: 212, rrec: '_gs_c5_rec', size: 53, sets: zeros(9, 10) },
  // history and cursor 0, fill status M14 (1); the read coordinate (13) and the pointer (16) stay
  { entry: '_gs_ring_reset', stock: '_gs_ring_reset_stock', record: ENGINE + 0x12348, stride: 72, rrec: '_gs_rg_rec', size: 18, sets: { 0: 0, 14: 0, 17: 1 } },
];
const mirrorId = (m: Mirror): string => m.entry.replace(/^_gs_/, '').replace(/_reset$/, '');

// (twin, arguments, counter word in `_gs_cnt`)
const TWINS: [string, number[], number][] = [
  ['_gs_fo', [32], 9],
  ['_gs_uy', [32], 10],
  ['_gs_c5', [32], 12],
  ['_gs_gain', [0, 0, 32], 11],
  ['_gs_held_early', [32], 0],
  ['_gs_held_late', [32], 1],
  ['_gs_shaper_early', [32], 2],
  ['_gs_shaper_late', [32], 3],
  ['_gs_case1', [32], 4],
  ['_gs_case2', [32], 5],
  ['_gs_case3', [32], 6],
  ['_gs_ring4', [32], 7],
  ['_gs_ring6', [32], 8],
];

const enabled = stockAvailable(DN2_112);
const suite = enabled ? describe : describe.skip;
const title = 'the reset mirrors and the guard of core/dsp-voice (DN2 1.12)';

suite(enabled ? title : `${title}: ${stockSkipReason(DN2_112)}`, () => {
  const rig = enabled ? StereoRig.build() : (undefined as never);
  const stubRig = enabled ? StereoRig.build({ stub: true }) : (undefined as never);

  // ---- reset mirrors ----

  it.each(MIRRORS.map((m) => [mirrorId(m), m] as const))(
    'the reset sets the private words of R as it does for L and only for its slot: %s',
    async (_id, m) => {
      const mirrored = await newSession(rig);
      const plain = await newSession(rig);
      try {
        for (const s of [mirrored, plain])
          for (let v = 0; v < 16; v++) s.pokeWords(m.record + m.stride * v, filled(m.size, 0x3f000000 + v));
        for (let v = 0; v < 16; v++) mirrored.pokeWords(rig.data[m.rrec]! + m.stride * v, filled(m.size, 0x3f800000 + v));
        await call(mirrored, rig.sym[m.entry]!, m.record + m.stride * SLOT, 0, 0, []);
        await call(plain, STOCK[m.stock]!, m.record + m.stride * SLOT, 0, 0, []);
        for (let v = 0; v < 16; v++)
          expect(await mirrored.words(m.record + m.stride * v, m.size), `L record of slot ${v} is the stock reset`).toEqual(
            await plain.words(m.record + m.stride * v, m.size),
          );
        for (let v = 0; v < 16; v++) {
          const expected = filled(m.size, 0x3f800000 + v);
          if (v === SLOT) for (const [i, value] of Object.entries(m.sets)) expected[Number(i)] = value;
          expect(await mirrored.words(rig.data[m.rrec]! + m.stride * v, m.size), `R record of slot ${v}`).toEqual(expected);
        }
        // the words the stock reset sets in L for the private ones are the same values (the ring's fill status is the stock's byte, 1)
        const stock = await plain.words(m.record + m.stride * SLOT, m.size);
        for (const [i, value] of Object.entries(m.sets)) expect(stock[Number(i)]! & 0xff).toBe(value & 0xff);
      } finally {
        await Promise.all([mirrored.close(), plain.close()]);
      }
    },
  );

  const where = ['below', 'past the last slot', 'unaligned'] as const;
  it.each(MIRRORS.flatMap((m) => where.map((w) => [mirrorId(m), w, m] as const)))(
    'a reset of a record that is no slot only runs the stock reset: %s, %s',
    async (_id, w, m) => {
      const s = await newSession(stubRig);
      try {
        const records = MIRRORS.map((x) => [x.rrec, 4 * x.size * 16] as const);
        for (const [name, size] of records) s.pokeWords(stubRig.data[name]!, filled(size / 4, 0x55555555));
        const address = { below: m.record - 64, 'past the last slot': m.record + 16 * m.stride, unaligned: m.record + 3 * m.stride + 4 }[w];
        await call(s, stubRig.sym[m.entry]!, address, 0, 0, []);
        const log = await stubLog(s);
        expect(log[3]).toBe(1);
        expect(log[0]).toBe(address);
        for (const [name, size] of records)
          expect(await s.words(stubRig.data[name]!, size / 4), name).toEqual(filled(size / 4, 0x55555555));
      } finally {
        await s.close();
      }
    },
  );

  // ---- the guard: a twin in a context it does not support ----

  const contexts: [string, Record<string, number>][] = [
    ['circular-l4', { L4: 64 }],
    ['circular-l3', { L3: 64 }],
    ['circular-l5', { L5: 8 }],
    ['block-16', {}],
  ];
  it.each(TWINS.flatMap(([entry, args, index]) => contexts.map(([name, regs]) => [entry, name, args, index, regs] as const)))(
    'an unsupported context clears the stamp and runs the stock stage once: %s, %s',
    async (entry, context, args, index, regs) => {
      const s = await newSession(stubRig);
      try {
        stamps(stubRig, s, new Set([SLOT]));
        const callArgs = args.map((a, i) => (context === 'block-16' && i === args.length - 1 ? 16 : a));
        await call(s, stubRig.sym[entry]!, 0x1234, LBUF, LBUF, callArgs, { regs });
        const log = await stubLog(s);
        expect(log[3]).toBe(1);
        expect(log.slice(0, 3)).toEqual([0x1234, LBUF, LBUF]);
        expect(await s.peek(stubRig.data._gs_stamp! + 4 * SLOT)).toBe(0);
        expect(await counters(stubRig, s)).toEqual(oneHot(index));
        expect(await s.peek(stubRig.data._gs_nbad!)).toBe(1);
        expect(await s.peek(stubRig.data._gs_hacc!)).toBe(3);
      } finally {
        await s.close();
      }
    },
  );

  it.each(TWINS)(
    'the guard counts the refusal of a stereo slot once per stage and touches no other slot: %s',
    async (entry, args, index) => {
      const s = await newSession(stubRig);
      try {
        stamps(stubRig, s, new Set([SLOT, OTHER]));
        seen(stubRig, s, new Set([SLOT, OTHER]));
        const callArgs = [...args.slice(0, -1), 16];
        await call(s, stubRig.sym[entry]!, 0x1234, LBUF, LBUF, callArgs);
        expect(await s.peek(stubRig.data._gs_stamp! + 4 * SLOT)).toBe(0);
        expect(await s.peek(stubRig.data._gs_stamp! + 4 * OTHER)).toBe(HEARTBEAT);
        expect(await counters(stubRig, s)).toEqual(oneHot(index));
        expect(await s.peek(stubRig.data._gs_hacc!)).toBe(3);
        // a second twin of the same slot sees an unstamped slot: the refusal is not counted twice
        await call(s, stubRig.sym[entry]!, 0x1234, LBUF, LBUF, callArgs);
        expect(await counters(stubRig, s)).toEqual(oneHot(index));
        expect((await stubLog(s))[3], 'the stock stage ran each time').toBe(2);
      } finally {
        await s.close();
      }
    },
  );

  it.each(TWINS)('a mono slot goes straight to the stock stage: %s', async (entry, args) => {
    const s = await newSession(stubRig);
    try {
      stamps(stubRig, s, new Set([OTHER]));
      s.poke(stubRig.data._gs_stamp! + 4 * SLOT, HEARTBEAT - 1);
      await call(s, stubRig.sym[entry]!, 0x1234, LBUF, LBUF, [...args.slice(0, -1), 16]);
      expect(await stubLog(s)).toEqual([0x1234, LBUF, LBUF, 1]);
      expect(await counters(stubRig, s)).toEqual(filled(32, 0));
      expect(await s.peek(stubRig.data._gs_hacc!)).toBe(0);
      expect(await s.peek(stubRig.data._gs_nbad!)).toBe(0);
    } finally {
      await s.close();
    }
  });

  it('every slot is found by its voice buffer', async () => {
    for (let slot = 0; slot < 16; slot++) {
      const s = await newSession(stubRig);
      try {
        stamps(stubRig, s, new Set(Array.from({ length: 16 }, (_, v) => v)));
        await call(s, stubRig.sym._gs_ring4!, 0, BUFFERS + 0x80 * slot, 0, [16]);
        const got = await s.peekMany(Array.from({ length: 16 }, (_, v) => [stubRig.data._gs_stamp! + 4 * v, 1] as [number, number]));
        expect(got.map((r) => r[0])).toEqual(Array.from({ length: 16 }, (_, v) => (v === slot ? 0 : HEARTBEAT)));
      } finally {
        await s.close();
      }
    }
  });
});
