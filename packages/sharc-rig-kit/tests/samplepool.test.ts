import { describe, expect, it } from 'vitest';
import { PLANE_L, PLANE_R, TOKEN, noise, pack, publish, writePlanes } from '../src/samplepool.ts';

describe('pack', () => {
  it('puts two int16 samples in a word, the first in the low half, and pads an odd tail with zero', () => {
    expect(pack([1, -1, 3])).toEqual([0xffff0001, 3]);
  });
});

describe('writePlanes', () => {
  it('writes the packed planes at their two addresses', () => {
    const w = new Map<number, number>();
    writePlanes((a, v) => w.set(a, v), [1, 2], [3, 4, 5, 6]);
    expect([...w]).toEqual([
      [PLANE_L, 0x00020001],
      [PLANE_R, 0x00040003],
      [PLANE_R + 1, 0x00060005],
    ]);
  });
});

describe('publish', () => {
  it('writes the record of the pool and the ready word last; a mono sample reads its right plane from the left plane', () => {
    const writes: [number, number][] = [];
    publish((a, v) => writes.push([a, v]), 1000, 2, 64, 48000, 1);
    const rec = 1000 + 16;
    expect(writes.at(-1)).toEqual([rec, 1]);
    expect(writes.slice(0, 8).map(([, v]) => v)).toEqual([0, TOKEN, 64, 48000, 1, 1, PLANE_L, PLANE_L]);
    writes.length = 0;
    publish((a, v) => writes.push([a, v]), 1000, 0, 64, 48000, 2);
    expect(writes[7]).toEqual([1007, PLANE_R]);
  });
});

describe('noise', () => {
  it('draws int16 values from a fixed seed, the same every time', () => {
    const a = noise(1, 6);
    expect(a).toEqual([-15157, -24497, 664, -17313, 32169, 26147]); // random.Random(1).randrange(-32768, 32768)
    expect(noise(1, 6)).toEqual(a);
  });
});
