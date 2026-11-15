/** The float32 model of the send route (the routine's own arithmetic) and the helpers the send rig tests share. */
import path from 'node:path';
import { expect } from 'vitest';
import { FxRig, PyRandom, ROUTE_SEND, TRACKS, w2f, type FxBlock, type Row } from '@digi/sharc-rig-kit';

export const f32 = Math.fround;
export const ONE = 0x3f800000;
export const POISON = 0x7fc00001;
const rng0 = new PyRandom(11);
export const RAMP = Array.from({ length: 64 }, (_, i) => f32(i / 64 - 0.5));
export const SIGNALS = Array.from({ length: TRACKS }, () => Array.from({ length: 64 }, () => f32(rng0.uniform(-1, 1))));
const ONE_127 = w2f(0x3c010204); // 1 / 127
export const STEP = 1 / 32;
export const zeros = (n: number): number[] => Array<number>(n).fill(0);

/** The gain of an amount byte: (byte / 127) squared in the routine's float32 operations. */
export function gain(byte: number): number {
  const a = f32(Math.min(byte, 127) * ONE_127);
  return f32(a * a);
}

const pad = (amounts: readonly number[]): number[] => [...amounts, ...zeros(TRACKS)].slice(0, TRACKS);

/** The bus of one block: the sum over the tracks of g_k * the track, g_k = old + (k + 1) * step added per sample; returns the bus and the new gains. */
export function sendBus(amounts: readonly number[], old: readonly number[]): [number[], number[]] {
  const neu = pad(amounts).map(gain);
  const bus = zeros(64);
  for (let t = 0; t < TRACKS; t++) {
    if (neu[t] === 0 && old[t] === 0) continue;
    const step = f32(f32(neu[t]! - old[t]!) * STEP);
    let g = old[t]!;
    for (let k = 0; k < 32; k++) {
      g = f32(g + step);
      for (const ch of [0, 32]) bus[ch + k] = f32(bus[ch + k]! + f32(SIGNALS[t]![ch + k]! * g));
    }
  }
  return [bus, neu];
}

/** The weights of the return add per sample (both channels): 1.0 steady, (k + 1) / 32 in, 1 - (k + 1) / 32 out, as float32 sums. */
function weights(mode: 'steady' | 'in' | 'out'): number[] {
  const out: number[] = [];
  for (let ch = 0; ch < 2; ch++) {
    let w = { steady: 1, in: 0, out: 1 }[mode];
    const dw = { steady: 0, in: STEP, out: -STEP }[mode];
    for (let k = 0; k < 32; k++) {
      w = f32(w + dw);
      out.push(w);
    }
  }
  return out;
}

export function addReturn(ret: readonly number[], wet: readonly number[], mix: number, mode: 'steady' | 'in' | 'out'): number[] {
  const w = weights(mode);
  const m = f32(mix);
  return ret.map((r, i) => f32(r + f32(wet[i]! * f32(w[i]! * m))));
}

export const half = (bus: readonly number[]): number[] => bus.map((x) => f32(x * 0.5));

/** Equal as Python compares float tuples: -0 equals 0. */
export function same(got: readonly number[], want: readonly number[], label = ''): void {
  expect(got.length, label).toBe(want.length);
  const bad = got.findIndex((x, i) => !(x === want[i]));
  if (bad >= 0) expect.fail(`${label} sample ${bad}: got ${got[bad]}, want ${want[bad]}`);
}


const REPO = path.resolve(import.meta.dirname, '../../../../..');
export const EFFECTS = [path.join(REPO, 'tests/support/fx_effects.s')];

export const clean = (b: FxBlock, what = ''): void => {
  expect(b.returned, `${what} ${b.halt}`).toBe(true);
  expect(b.ctxBad, what).toEqual([]);
  expect(b.regsBad, what).toEqual([]);
};

export const load = (rig: FxRig): void => SIGNALS.forEach((s, t) => rig.setTrack(t, s));

export const AMOUNTS = [0, 0, 0, 127, 0, 64, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
export const sendRow = (o: Partial<Row> = {}): Row => ({
  pc: '_te_half',
  state: '_te_state_a',
  route: ROUTE_SEND,
  amounts: AMOUNTS,
  ...o,
});
export const free: Row = { pc: '', state: '' };
export const amountsOf = (track: number, value: number): number[] => Array.from({ length: TRACKS }, (_, t) => (t === track ? value : 0));

export async function open(rows: Row[], o: { seed?: number; recordStores?: boolean | 'all' } = {}): Promise<FxRig> {
  return FxRig.open(rows, { sources: EFFECTS, ...o });
}

/** One test body with its own rig, closed afterwards. */
export const withRig =
  (rows: Row[], fn: (rig: FxRig) => Promise<void>, o: { seed?: number; recordStores?: boolean | 'all' } = {}) =>
  async () => {
    const rig = await open(rows, o);
    try {
      await fn(rig);
    } finally {
      await rig.close();
    }
  };

export const ramp01 = Array.from({ length: 64 }, (_, k) => f32(0.01 * k));
