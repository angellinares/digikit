/**
 * core/fx, the send route on the Digitone II (modules/core/adapters/fx/src/dsp/fxsend-dn2.s, finding 67): the slot mix on the SHARC-only rig.
 * Port of test_fx_send_dn2.py (T1g phase 5).
 *
 * The Digitone II has no track buses before its master function: the send bus is the sum over the 16 voice slots of the gain of the slot's
 * track times the slot's bus (the map slot -> track at `dsp.fx.value.send.slot_map`). The model is the routine's own float32 arithmetic,
 * as in fx-send.rig.test.ts (gain, ramp, return add), slots summed in order.
 */
import { describe, expect, it } from 'vitest';
import { FxRig, PyRandom, TRACKS } from '@digi/sharc-rig-kit';
import {
  AMOUNTS,
  EFFECTS,
  SIGNALS,
  STEP,
  addReturn,
  clean,
  f32,
  gain,
  half,
  ramp01,
  same,
  sendBus,
  sendRow,
  zeros,
} from './fx-send-model.ts';

// 0x3d042108: the master function's fade of a flagged slot, w_k = 1 - k / 31 (w_0 = 1)
const FADE_STEP = f32(1 / 31);

/** A flagged slot's bus as the routine scales it (finding 67): each channel times w_k, w_0 = 1.0, w_(k+1) = w_k - 1/31, in float32. */
function faded(x: readonly number[]): number[] {
  const out = [...x];
  for (const ch of [0, 32]) {
    let w = 1;
    for (let k = 0; k < 32; k++) {
      out[ch + k] = f32(x[ch + k]! * w);
      w = f32(w - FADE_STEP);
    }
  }
  return out;
}

type Slots = Record<number, number[]>;

/**
 * The bus of one block: for each slot in order, g_k * its bus with the gain of its track (g_k = old + (k + 1) * step, added per sample);
 * returns the bus and the new gains per track. SLOTS: slot number -> its 64 floats; slots not in it are silent. FADE: the slots whose flag
 * byte is set (their bus is scaled by `faded` first).
 */
function slotBus(
  amounts: readonly number[],
  old: readonly number[],
  slotTrack: readonly number[],
  slots: Slots,
  fade: ReadonlySet<number> = new Set(),
): [number[], number[]] {
  const neu = [...amounts, ...zeros(TRACKS)].slice(0, TRACKS).map(gain);
  const bus = zeros(64);
  for (let v = 0; v < TRACKS; v++) {
    const t = slotTrack[v]!;
    if (neu[t] === 0 && old[t] === 0) continue;
    const step = f32(f32(neu[t]! - old[t]!) * STEP);
    let g = old[t]!;
    let x = slots[v] ?? zeros(64);
    if (fade.has(v)) x = faded(x);
    for (let k = 0; k < 32; k++) {
      g = f32(g + step);
      for (const ch of [0, 32]) bus[ch + k] = f32(bus[ch + k]! + f32(x[ch + k]! * g));
    }
  }
  return [bus, neu];
}

const amountsBy = (byTrack: Record<number, number>): number[] =>
  Array.from({ length: TRACKS }, (_, t) => byTrack[t] ?? 0);

async function dn2Rig(
  slotTrack: readonly number[],
  slots: Slots,
  o: { fade?: Iterable<number>; amounts?: number[] } = {},
): Promise<FxRig> {
  const rig = await FxRig.open([sendRow(o.amounts ? { amounts: o.amounts } : {})], { sources: EFFECTS, device: 'dn2' });
  rig.setSlotmap(slotTrack);
  rig.setFade(o.fade ?? []);
  for (const [v, x] of Object.entries(slots)) rig.setSlot(Number(v), x);
  return rig;
}

const sigs = (vs: number[]): Slots => Object.fromEntries(vs.map((v) => [v, SIGNALS[v]!]));

describe('core/fx send route on the Digitone II', () => {
  it('a chord is the sum of its slots', async () => {
    // three slots sound track 3 (a chord and a release tail), one track 5, one has no amount: the bus and the return equal the float32
    // model bit for bit (ramp block, steady blocks, a change of amount)
    const slotTrack = zeros(TRACKS);
    for (const [v, t] of [[0, 3], [1, 3], [2, 5], [7, 3], [9, 9]] as const) slotTrack[v] = t; // prettier-ignore
    const slots = sigs([0, 1, 2, 7, 9]);
    let amounts = amountsBy({ 3: 127, 5: 64, 9: 0 });
    const rig = await dn2Rig(slotTrack, slots, { amounts });
    try {
      const ret = ramp01;
      let old = zeros(TRACKS);
      let bus: number[] = [];
      for (const [n, mode] of (['in', 'steady', 'steady'] as const).entries()) {
        rig.setReturn(ret);
        [bus, old] = slotBus(amounts, old, slotTrack, slots);
        const b = await rig.sendBlock();
        clean(b, `block ${n}`);
        same(await rig.bus(), bus, `block ${n}`);
        same(b.ret, addReturn(ret, half(bus), 1.0, mode), `block ${n}`);
        same(await rig.prevGains(0), old);
        same(b.master, zeros(64));
      }
      amounts = amountsBy({ 3: 32, 5: 127, 9: 100 });
      rig.setAmounts(0, amounts);
      for (let n = 0; n < 2; n++) {
        rig.setReturn(ret);
        [bus, old] = slotBus(amounts, old, slotTrack, slots);
        const b = await rig.sendBlock();
        same(await rig.bus(), bus, `block ${n}`);
        same(b.ret, addReturn(ret, half(bus), 1.0, 'steady'), `block ${n}`);
      }
      // the chord is one track: the same sum as one bus with the three slots added in slot order
      expect(bus.some((v) => v !== 0)).toBe(true);
    } finally {
      await rig.close();
    }
  });

  it('with one slot for each track the slot mix is the track mix', async () => {
    // the identity map and slot v = track v: the Digitone II object gives the Digitakt II object's bus
    const slotTrack = Array.from({ length: TRACKS }, (_, t) => t);
    const rig = await dn2Rig(slotTrack, sigs(slotTrack));
    try {
      let old = zeros(TRACKS);
      for (let i = 0; i < 3; i++) {
        let bus: number[];
        [bus, old] = sendBus(AMOUNTS, old);
        await rig.sendBlock();
        same(await rig.bus(), bus);
      }
    } finally {
      await rig.close();
    }
  });

  it('a free slot adds nothing whatever its map entry says', async () => {
    // every map entry names track 3 (amount 127), only slot 4 sounds: the bus is that slot's, and with no sounding slot the bus is silent
    // and the return is not touched
    const slotTrack = Array<number>(TRACKS).fill(3);
    const amounts = amountsBy({ 3: 127 });
    const rig = await dn2Rig(slotTrack, sigs([4]), { amounts });
    try {
      let old = zeros(TRACKS);
      for (let i = 0; i < 3; i++) {
        let bus: number[];
        [bus, old] = slotBus(amounts, old, slotTrack, sigs([4]));
        await rig.sendBlock();
        same(await rig.bus(), bus);
      }
      same(await rig.bus(), SIGNALS[4]!.map((x) => f32(x * gain(127))));
    } finally {
      await rig.close();
    }
    const rng = new PyRandom(3);
    const silent = await dn2Rig(slotTrack, {}, { amounts });
    try {
      for (let i = 0; i < 4; i++) {
        const ret = Array.from({ length: 64 }, () => f32(rng.uniform(-1, 1)));
        silent.setReturn(ret);
        const b = await silent.sendBlock();
        same(b.ret, ret);
        same(await silent.bus(), zeros(64));
      }
    } finally {
      await silent.close();
    }
  });

  it('a sound on a track without an amount adds nothing', async () => {
    const slotTrack = zeros(TRACKS);
    slotTrack[6] = 8;
    const rig = await dn2Rig(slotTrack, sigs([6]), { amounts: amountsBy({ 2: 127 }) });
    try {
      expect((await rig.sendBlock()).returned).toBe(true);
      for (let i = 0; i < 2; i++) {
        await rig.sendBlock();
        same(await rig.bus(), zeros(64));
      }
    } finally {
      await rig.close();
    }
  });

  it('a fading slot is scaled like the master function scales it', async () => {
    // the flag byte of a slot is set for one block (the DSP gives the slot to a voice on another track; the map follows a block later):
    // the slot enters the bus with the weights 1 - k / 31, bit for bit the float32 model, in the ramp block and in a steady block; the slot
    // next to it, the same track and not flagged, is added whole; a flag on a slot whose track has no amount adds nothing
    const slotTrack = zeros(TRACKS);
    for (const [v, t] of [[0, 3], [1, 3], [2, 5], [7, 3]] as const) slotTrack[v] = t; // prettier-ignore
    const slots = sigs([0, 1, 2, 7]);
    const amounts = amountsBy({ 3: 127, 5: 64 });
    const fades = [[1], [0, 7], [2], [4, 9, 11], Array.from({ length: TRACKS }, (_, v) => v)];
    for (const fade of fades) {
      const rig = await dn2Rig(slotTrack, slots, { fade, amounts });
      try {
        const ret = ramp01;
        let old = zeros(TRACKS);
        for (const [n, mode] of (['in', 'steady'] as const).entries()) {
          rig.setReturn(ret);
          let bus: number[];
          [bus, old] = slotBus(amounts, old, slotTrack, slots, new Set(fade));
          const b = await rig.sendBlock();
          const at = `fade ${fade} block ${n}`;
          clean(b, at);
          same(await rig.bus(), bus, at);
          same(b.ret, addReturn(ret, half(bus), 1.0, mode), at);
        }
      } finally {
        await rig.close();
      }
    }
    // the fade is not a gain: a flagged slot differs from the whole one, the whole one is what the other tests pin
    const [whole] = slotBus(amounts, zeros(TRACKS), slotTrack, slots);
    const [flagged] = slotBus(amounts, zeros(TRACKS), slotTrack, slots, new Set([1]));
    expect(whole).not.toEqual(flagged);
    // the next block, the flag back to 0: the bus is the whole slots again
    const rig = await dn2Rig(slotTrack, slots, { fade: [1], amounts });
    try {
      await rig.sendBlock();
      rig.setFade([]);
      const old = slotBus(amounts, zeros(TRACKS), slotTrack, slots, new Set([1]))[1];
      const [bus] = slotBus(amounts, old, slotTrack, slots);
      await rig.sendBlock();
      same(await rig.bus(), bus);
    } finally {
      await rig.close();
    }
  });

  it('the slot mix keeps the registers of the hook', async () => {
    // 24 hostile contexts give every register back (the send hook's contract on this device too)
    const slotTrack = Array.from({ length: TRACKS }, (_, v) => v % 5);
    const slots = sigs(Array.from({ length: TRACKS }, (_, v) => v));
    for (let seed = 0; seed < 24; seed++) {
      const rig = await FxRig.open([sendRow()], { sources: EFFECTS, device: 'dn2', seed });
      try {
        rig.setSlotmap(slotTrack);
        rig.setFade(seed % 2 ? [1, 4] : []);
        for (const [v, x] of Object.entries(slots)) rig.setSlot(Number(v), x);
        clean(await rig.sendBlock(), `seed ${seed}`);
      } finally {
        await rig.close();
      }
    }
  });

  it('the slot mix costs', async () => {
    // printed for the finding: instructions of the send hook with one row and 1 slot, 4 slots and 16 slots sounding
    const slotTrack = Array.from({ length: TRACKS }, (_, t) => t);
    for (const n of [1, 4, 16]) {
      const slots = sigs(Array.from({ length: n }, (_, v) => v));
      const amounts = Array.from({ length: TRACKS }, (_, t) => (t < n ? 127 : 0));
      const rig = await dn2Rig(slotTrack, slots, { amounts });
      try {
        await rig.sendBlock(undefined, 'friendly');
        const b = await rig.sendBlock(undefined, 'friendly');
        console.log(`DN2 send hook, one row, ${n} slots with an amount: ${b.instructions}`);
        expect(b.returned).toBe(true);
      } finally {
        await rig.close();
      }
    }
  });
});
