/**
 * The pan hook of core/dsp-voice on DN2 1.12 (docs/design/dn2-stereo.md, section 1), on the SHARC-only rig with the stock DSP image.
 * Port of test_dsp_stereo_gain_slot.py (T1g phase 5b).
 *
 * The stock engine bytes run the block from the block ramp to the end of the pan loop (0x1c99de to 0x1c9b33: ramp, conditional fade,
 * metadata clear, 16 slots of pan) with `_gs_slot` patched in at 0x1c9af6, and the same block unpatched. A slot with no stamp is
 * bit-identical to the stock block; a stamped slot has plane A = stock(L).A and plane B = stock(R).B, where stock(X) is the unpatched
 * block run on X: the pan gains, the block ramp and the fade of R are the engine's own. Original firmware is supplied at run time.
 */
import { describe, expect, it } from 'vitest';
import {
  DN2_112,
  HEARTBEAT,
  PAN_BODY,
  PAN_END,
  PLANES,
  PLANE_TABLE,
  PyRandom,
  StereoRig,
  STEREO_TABLE,
  bits,
  buffers,
  buildEngine,
  engineRegs,
  planes,
  runBlock,
  stockAvailable,
  stockSkipReason,
  value,
  type EngineOptions,
  type StereoSession,
} from '@digi/sharc-rig-kit';

const ALL = Array.from({ length: 16 }, (_, v) => v);
type Block = Record<number, number[]>;

const blockOf = (rng: PyRandom, scale = 0.5): number[] => Array.from({ length: 32 }, () => bits(rng.uniform(-scale, scale)));
const blocks = (rng: PyRandom, scale?: number): Block => Object.fromEntries(ALL.map((v) => [v, blockOf(rng, scale)]));

interface Run {
  planes: Awaited<ReturnType<typeof planes>>;
  buffers: number[][];
  instructions: number;
}

/** The R buffers and the stamps of a hooked session. */
function stampSlots(rig: StereoRig, s: StereoSession, right: Block, stamp: (v: number) => number): void {
  for (const v of ALL) {
    s.pokeWords(rig.data._gs_rbuf! + 128 * v, right[v]!);
    s.poke(rig.data._gs_stamp! + 4 * v, stamp(v));
  }
}

async function runOne(rig: StereoRig, hooked: boolean, data: Block, engine: EngineOptions, right?: Block, stamp?: (v: number) => number): Promise<Run> {
  const s = await rig.session({ hooked });
  try {
    buildEngine(s, data, engine);
    if (hooked && right && stamp) stampSlots(rig, s, right, stamp);
    const result = await runBlock(s);
    return { planes: await planes(s), buffers: await buffers(s), instructions: result.instructions };
  } finally {
    await s.close();
  }
}

/** Stock block on L, stock block on R, patched block with R in the R buffers of the `stereo` slots. */
async function runTriple(rig: StereoRig, stereo: Set<number>, seed: number, engine: EngineOptions = {}) {
  const rng = new PyRandom(seed);
  const left = blocks(rng);
  const right = blocks(rng, 0.3);
  return {
    L: await runOne(rig, false, left, engine),
    R: await runOne(rig, false, right, engine),
    H: await runOne(rig, true, left, engine, right, (v) => (stereo.has(v) ? HEARTBEAT : 0)),
    left,
    right,
  };
}

function check(out: Awaited<ReturnType<typeof runTriple>>, stereo: Set<number>): void {
  const { L, R, H } = out;
  for (const v of ALL) {
    expect(H.planes[v]![0], `plane A of slot ${v}`).toEqual(L.planes[v]![0]);
    expect(H.planes[v]![1], `plane B of slot ${v}`).toEqual(stereo.has(v) ? R.planes[v]![1] : L.planes[v]![1]);
  }
  expect(H.buffers, 'L stays the stock path').toEqual(L.buffers);
}

const enabled = stockAvailable(DN2_112);
const suite = enabled ? describe : describe.skip;
const title = 'the pan hook of core/dsp-voice on the DN2 1.12 image';

suite(enabled ? title : `${title}: ${stockSkipReason(DN2_112)}`, () => {
  const rig = enabled ? StereoRig.build() : (undefined as never);

  it('no stamp is bit-identical to the stock block', async () => {
    const out = await runTriple(rig, new Set(), 1);
    check(out, new Set());
    // the price of the hooks for 16 mono slots: the stamp check of each slot and of the pre-pan loop, about 26 instructions per slot
    expect(out.H.instructions - out.L.instructions).toBeLessThan(16 * 30);
  });

  const sets: [string, Set<number>][] = [
    ['{0}', new Set([0])],
    ['{5}', new Set([5])],
    ['{15}', new Set([15])],
    ['{1,2,3}', new Set([1, 2, 3])],
    ['even slots', new Set(Array.from({ length: 8 }, (_, k) => 2 * k))],
    ['all slots', new Set(ALL)],
  ];
  it.each(sets)('a stamped slot pans R into plane B and L into plane A: %s', async (_name, stereo) => {
    const out = await runTriple(rig, stereo, 7);
    check(out, stereo);
    // the planes differ: L != R is audible in B
    for (const v of stereo) {
      expect(out.H.planes[v]![1]).not.toEqual(out.L.planes[v]![1]);
      expect(out.H.planes[v]![0]).toEqual(out.L.planes[v]![0]);
    }
  });

  it('a stamped slot with R equal to L reproduces the stock pair', async () => {
    const left = blocks(new PyRandom(3));
    const ref = await runOne(rig, false, left, {});
    const hooked = await runOne(rig, true, left, {}, left, () => HEARTBEAT);
    expect(hooked.planes).toEqual(ref.planes);
  });

  it.each([
    ['unstamped', 0],
    ['last-block', HEARTBEAT - 1],
    ['future-block', HEARTBEAT + 1],
  ])('a stamp of another block is a mono slot: %s', async (_name, stamp) => {
    const out = await runTriple(rig, new Set(), 11);
    const hooked = await runOne(rig, true, out.left, {}, out.right, () => stamp);
    expect(hooked.planes).toEqual(out.L.planes);
  });

  it.each(['ramp', 'fade', 'ramp+fade'])('R takes the block ramp and the fade of the engine: %s', async (shape) => {
    const rng = new PyRandom(21);
    const ramp = shape.includes('ramp')
      ? Object.fromEntries(ALL.map((v) => [v, [rng.uniform(0.2, 1.2), rng.uniform(-0.01, 0.01)] as [number, number]]))
      : undefined;
    const fade = shape.includes('fade') ? new Set([1, 4, 5, 9]) : undefined;
    const gainWords = Object.fromEntries([...(fade ?? [])].map((v) => [v, [0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0]]));
    const stereo = new Set([1, 2, 4, 9, 12]);
    const out = await runTriple(rig, stereo, 5, { ramp, fade, gainWords });
    check(out, stereo);
    if (fade) {
      // the fade is a real one: the last sample of a faded slot is (nearly) zero, the stock block says so for R as for L
      expect(Math.abs(value(out.R.planes[1]![1][31]!))).toBeLessThan(1e-3 * 0.1);
      expect(Math.abs(value(out.L.planes[1]![1][31]!))).toBeLessThan(1e-3 * 0.1);
    }
  });

  it("a fade flag of another slot does not fade this slot (negative control: the flags are per slot)", async () => {
    const rng = new PyRandom(8);
    const left = blocks(rng);
    const right = blocks(rng);
    const results: number[][] = [];
    for (const fade of [new Set([2]), new Set([3])]) {
      const gainWords = Object.fromEntries([...fade].map((v) => [v, [0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0]]));
      const run = await runOne(rig, true, left, { fade, gainWords }, right, (v) => (v === 2 ? HEARTBEAT : 0));
      results.push(run.planes[2]![1]);
    }
    expect(results[0], "slot 2's R is faded only when slot 2 has the fade flags").not.toEqual(results[1]);
  });

  // ---- the one-slot body: registers and the loop's own flag use ----

  const LIVE_OUT = ['R9', 'I3', 'I4', 'I5', 'I12', 'I6', 'I7'];

  async function runSlot(hooked: boolean, slot: number, stamped: boolean, block = 32) {
    const rng = new PyRandom(slot + 40);
    const left = blocks(rng);
    const s = await rig.session({ hooked });
    buildEngine(s, left);
    for (let i = 0; i < 32; i++) s.poke(rig.data._gs_rbuf! + 128 * slot + 4 * i, bits(0.125 * (i % 5)));
    s.poke(rig.data._gs_stamp! + 4 * slot, stamped ? HEARTBEAT : 0);
    const r = engineRegs({ I3: STEREO_TABLE + 4 * slot, I5: PLANE_TABLE + 8 * slot, I4: STEREO_TABLE + 0x354 + 28 * slot, R9: block, I12: 0x1234 });
    const result = await s.raw(PAN_BODY, r, {
      maxSteps: 4000,
      breaks: [0xb9f000, PAN_END],
      readRegs: ['I2', 'I3', 'I4', 'I5', 'I12', 'I6', 'I7', 'M4', 'ASTATX', 'R9'],
    });
    expect(result.halt === 'breakpoint' && result.final_pc === PAN_END, JSON.stringify(result)).toBe(true);
    return { s, result };
  }

  it.each([0, 7, 15].flatMap((slot) => [32, 1].map((block) => [slot, block] as const)))(
    'a mono slot runs the stock instructions, flags included: slot %i, block %i',
    async (slot, block) => {
      // block 1: the stock loop leaves after the first sample on the AZ of `R0 = dec(R9)`; the hook has to hand that flag back
      const stock = await runSlot(false, slot, false, block);
      const hooked = await runSlot(true, slot, false, block);
      try {
        for (const name of LIVE_OUT) expect(hooked.result.regs[name], name).toBe(stock.result.regs[name]);
        const base = PLANES + 0x100 * slot;
        expect(await hooked.s.words(base, 64)).toEqual(await stock.s.words(base, 64));
        expect(await hooked.s.peek(base + 4)).toBe(await stock.s.peek(base + 4));
      } finally {
        await Promise.all([stock.s.close(), hooked.s.close()]);
      }
    },
  );

  it.each([0, 9])('a stereo slot leaves the loop state as the stock slot does: slot %i', async (slot) => {
    const stock = await runSlot(false, slot, false);
    const hooked = await runSlot(true, slot, true);
    try {
      for (const name of LIVE_OUT) expect(hooked.result.regs[name], name).toBe(stock.result.regs[name]);
    } finally {
      await Promise.all([stock.s.close(), hooked.s.close()]);
    }
  });

  it('a stamped slot of a one-sample block is a mono slot', async () => {
    const stock = await runSlot(false, 3, false, 1);
    const hooked = await runSlot(true, 3, true, 1);
    try {
      const base = PLANES + 0x100 * 3;
      expect(await hooked.s.words(base, 64)).toEqual(await stock.s.words(base, 64));
    } finally {
      await Promise.all([stock.s.close(), hooked.s.close()]);
    }
  });
});
