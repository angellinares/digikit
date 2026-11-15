import { describe, expect, it } from 'vitest';
import { CONTEXT_REGS, FRIENDLY, contextBad, drawContext, f2w, resolveContext, w2f } from '../src/context.ts';
import { PyRandom } from '../src/prng.ts';

describe('drawContext', () => {
  it('draws the same context for the same seed, for every register of the list', () => {
    const a = drawContext(new PyRandom(0xd1617));
    expect(drawContext(new PyRandom(0xd1617))).toEqual(a);
    expect(Object.keys(a).sort()).toEqual([...CONTEXT_REGS].sort());
  });

  it('never moves M7 and M14, which the stock epilogue and every stack operation use', () => {
    const rng = new PyRandom(1);
    for (let i = 0; i < 200; i++) {
      const c = drawContext(rng);
      expect([c.M7, c.M14]).toEqual([-1, 1]);
    }
  });

  it('is hostile: over many draws the registers leave their friendly values', () => {
    const rng = new PyRandom(2);
    const seen = new Set<string>();
    for (let i = 0; i < 200; i++) {
      const c = drawContext(rng);
      for (const n of CONTEXT_REGS) if (c[n] !== FRIENDLY[n]) seen.add(n);
    }
    expect([...seen].sort()).toEqual(['L0', 'L1', 'L2', 'L3', 'L4', 'L5', 'M13', 'M15', 'M5', 'M6', 'MODE1']);
  });
});

describe('resolveContext', () => {
  it('takes "friendly" as the constants and an object as given, with the missing registers friendly', () => {
    const rng = new PyRandom(3);
    expect(resolveContext('friendly', rng)).toEqual(FRIENDLY);
    expect(resolveContext({ M5: 7 }, rng)).toEqual({ ...FRIENDLY, M5: 7 });
  });
});

describe('contextBad', () => {
  it('names the registers that came back different, comparing as 32-bit words', () => {
    const ctx = { ...FRIENDLY, M7: -1, M5: 4 };
    const back: Record<string, number | null> = { ...ctx, M7: 0xffffffff, M5: 5 };
    expect(contextBad(back, ctx)).toEqual(['M5']);
    expect(contextBad({ ...back, L2: 7 }, ctx)).toEqual(['M5', 'L2']);
  });

  it('refuses a register the rig did not report', () => {
    const ctx = { ...FRIENDLY };
    expect(() => contextBad({ ...ctx, L2: null }, ctx, 0x90000)).toThrow(/did not report register L2/);
  });
});

describe('float words', () => {
  it('converts between float32 and its bits', () => {
    expect(f2w(1)).toBe(0x3f800000);
    expect(w2f(0x3f800000)).toBe(1);
    expect(Number.isNaN(w2f(0xffffffff))).toBe(true);
  });
});
