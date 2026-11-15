import { describe, expect, it } from 'vitest';
import { runPool } from '../src/pool.ts';

describe('runPool', () => {
  it('returns the results in the order of the items and never runs more than `jobs` at once', async () => {
    let running = 0;
    let peak = 0;
    const out = await runPool([5, 1, 4, 2, 3, 0], 2, async (n) => {
      peak = Math.max(peak, ++running);
      await new Promise((r) => setTimeout(r, n));
      running--;
      return n * 10;
    });
    expect(out).toEqual([50, 10, 40, 20, 30, 0]);
    expect(peak).toBe(2);
  });

  it('rejects when one job fails', async () => {
    await expect(
      runPool([1, 2, 3], 2, async (n) => {
        if (n === 2) throw new Error('two');
        return n;
      }),
    ).rejects.toThrow('two');
  });

  it('runs nothing for no items', async () => {
    expect(await runPool([], 4, async () => 1)).toEqual([]);
  });
});
