import { describe, expect, it } from 'vitest';
import { profileBySymbol, profileRuns, totalOf } from '../src/profile.ts';

const symbols = { _a: 0x100, _loop: 0x110, _b: 0x120, _alias: 0x120 };
// _a: 4 instructions once; _loop: 3 instructions 8 times; the label at 0x120: one instruction twice
const counts: [number, number][] = [
  [0x100, 1],
  [0x101, 1],
  [0x103, 1],
  [0x104, 1],
  [0x110, 8],
  [0x112, 8],
  [0x113, 8],
  [0x120, 2],
];

describe('rig profile tables', () => {
  it('adds the counts per symbol, largest first, with shares of the total', () => {
    expect(totalOf(counts)).toBe(30);
    const rows = profileBySymbol(counts, symbols);
    expect(rows.map((r) => [r.symbol, r.count, r.pcs])).toEqual([
      ['_loop', 24, 3],
      ['_a', 4, 4],
      ['_alias', 2, 1], // two names at one address: the first in sort order
    ]);
    expect(rows[0]!.percent).toBeCloseTo(80);
  });

  it('finds equal-count runs as loop bodies and straight code', () => {
    const rows = profileRuns(counts, symbols);
    expect(rows[0]).toMatchObject({ from: '_loop', each: 8, length: 3, count: 24 });
    expect(rows.map((r) => r.count)).toEqual([24, 4, 2]);
  });

  it('starts a new run at a label and at a gap even when the count is the same', () => {
    const rows = profileRuns(
      [
        [0x10e, 1],
        [0x110, 1],
        [0x118, 1],
      ],
      symbols,
    );
    expect(rows.map((r) => r.from).sort()).toEqual(['_a+0xe', '_loop', '_loop+0x8']);
  });
});
