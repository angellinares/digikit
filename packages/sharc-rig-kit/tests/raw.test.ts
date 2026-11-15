import { describe, expect, it } from 'vitest';
import { placeAligned } from '../src/raw.ts';
import type { SharcObject } from '../src/toolchain.ts';

const object = (over: Partial<SharcObject>): SharcObject => ({
  text: '',
  parcels: 0,
  symbols: {},
  data: {},
  dataSymbols: {},
  ...over,
});

describe('placeAligned', () => {
  it('puts each data run at its alignment and a symbol at its run plus offset', () => {
    const a = object({
      data: { '.A': { bytes: new Uint8Array(6), bss: false, align: 4 } },
      dataSymbols: { _a: { section: '.A', offset: 2 } },
    });
    const b = object({
      data: { '.B': { bytes: new Uint8Array(8), bss: true, align: 64 }, '.C': { bytes: new Uint8Array(1), bss: true } },
      dataSymbols: { _b: { section: '.B', offset: 4 }, _c: { section: '.C', offset: 0 } },
    });
    const { data, inits } = placeAligned([a, b], 0x1000);
    expect(data).toEqual({ _a: 0x1002, _b: 0x1040 + 4, _c: 0x1048 });
    expect(inits.map((i) => i.address)).toEqual([0x1000, 0x1040, 0x1048]);
  });
});
