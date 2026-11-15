/**
 * PCM planes and pool records for the machine rigs, written the way the runtime does: planes in free L2, then the record, with
 * `ready` last (port of tests/support/sample_pool.py). Used by machines that play a sample from the pool: their conformance setup
 * and their playback tests.
 */
import { nw } from './context.ts';
import { PyRandom } from './prng.ts';

export const PLANE_L = nw(0x200f0000); // normal-word addresses of the test PCM planes (free L2)
export const PLANE_R = nw(0x200f8000);
export const TOKEN = 7;

type Poke = (address: number, word: number) => void;

/** n int16 samples of noise from a fixed seed. */
export function noise(seed: number, n: number): number[] {
  const rng = new PyRandom(seed);
  return Array.from({ length: n }, () => rng.randRange(-32768, 32768));
}

/** Two int16 samples per word, the first in the low half; an odd tail is padded with zero. */
export function pack(plane: readonly number[]): number[] {
  const p = plane.map((v) => v & 0xffff);
  if (p.length % 2) p.push(0);
  const out: number[] = [];
  for (let i = 0; i < p.length; i += 2) out.push((p[i]! | (p[i + 1]! << 16)) >>> 0);
  return out;
}

/** Write the packed planes at the two plane addresses. */
export function writePlanes(
  poke: Poke,
  left: readonly number[],
  right: readonly number[],
  planeL = PLANE_L,
  planeR = PLANE_R,
): void {
  pack(left).forEach((w, k) => poke(planeL + k, w));
  pack(right).forEach((w, k) => poke(planeR + k, w));
}

/** Publish record `index` of the table at word address `base` (8 words per record). */
export function publish(
  poke: Poke,
  base: number,
  index: number,
  frames: number,
  srcRate: number,
  channels: number,
  token = TOKEN,
  planeL = PLANE_L,
  planeR = PLANE_R,
): void {
  const rec = base + 8 * index;
  const baseR = channels === 1 ? planeL : planeR;
  [0, token, frames, srcRate, channels, 1, planeL, baseR].forEach((w, k) => poke(rec + k, w));
  poke(rec, 1); // ready last
}
