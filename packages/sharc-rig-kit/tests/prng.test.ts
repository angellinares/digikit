import { describe, expect, it } from 'vitest';
import { PyRandom } from '../src/prng.ts';

// Values of CPython's `random.Random` for the same seeds. The conformance cases are drawn from these sequences, so a change here
// changes every random walk and every hostile context, and with them the worst-case instruction counts the modules record.
describe('PyRandom', () => {
  it('draws the floats of random.Random', () => {
    const r = new PyRandom(0xd1617);
    expect([r.random(), r.random(), r.random()]).toEqual([0.08750455457670958, 0.176626531221877, 0.7953411735118725]);
    const u = new PyRandom(0xc0ffee + 24);
    expect([u.uniform(-0.6, 0.6), u.uniform(-0.6, 0.6), u.uniform(-0.6, 0.6)]).toEqual([
      0.2536143540140876, 0.015862949057914077, 0.06489004793482334,
    ]);
  });

  it('draws the integers of choice and randrange', () => {
    const c = new PyRandom(0xd1617);
    const picks = Array.from({ length: 8 }, () => c.choice([0, 4, 0x1b8, -2, 0x10, 7]));
    expect(picks).toEqual([0, 16, 4, 7, 440, -2, -2, -2]);
    const f = new PyRandom(0xfeed);
    expect(Array.from({ length: 5 }, () => f.randRange(0, 0x7f01))).toEqual([27672, 4188, 6867, 12705, 18879]);
    // 65536 needs 17 bits: half of the draws are rejected
    const s = new PyRandom(1);
    expect(Array.from({ length: 6 }, () => s.randRange(-32768, 32768))).toEqual([
      -15157, -24497, 664, -17313, 32169, 26147,
    ]);
  });

  it('seeds from more than 32 bits', () => {
    expect(new PyRandom(2 ** 40 + 5).random()).toEqual(0.5043802970418443);
  });
});
