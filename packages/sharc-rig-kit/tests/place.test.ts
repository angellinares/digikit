import { describe, expect, it } from 'vitest';
import { CODE_BASE, DATA_BASE, placeCode, placeData } from '../src/place.ts';
import type { SharcObject } from '../src/toolchain.ts';

const object = (over: Partial<SharcObject>): SharcObject => ({
  text: '',
  parcels: 0,
  symbols: {},
  data: {},
  dataSymbols: {},
  ...over,
});

describe('placeData', () => {
  it('puts each data section in a 4-byte run after the last and gives a symbol its section address plus its offset', () => {
    const a = object({
      data: { '.PKDATA': { bytes: new Uint8Array(6), bss: false }, '.PKBSS': { bytes: new Uint8Array(8), bss: true } },
      dataSymbols: { _a: { section: '.PKDATA', offset: 2 }, _b: { section: '.PKBSS', offset: 4 } },
    });
    const b = object({
      data: { '.VAR': { bytes: new Uint8Array(4), bss: true } },
      dataSymbols: { _c: { section: '.VAR', offset: 0 } },
    });
    const layout = placeData([a, b], DATA_BASE);
    // the 6-byte run takes 8 bytes of address space
    expect(layout.data).toEqual({ _a: DATA_BASE + 2, _b: DATA_BASE + 8 + 4, _c: DATA_BASE + 16 });
    expect(layout.inits.map((i) => [i.address, i.bytes.length, i.bss])).toEqual([
      [DATA_BASE, 6, false],
      [DATA_BASE + 8, 8, true],
      [DATA_BASE + 16, 4, true],
    ]);
    expect(layout.own[0]).toEqual([
      { address: DATA_BASE, bytes: 6 },
      { address: DATA_BASE + 8, bytes: 8 },
    ]);
    expect(layout.end).toBe(DATA_BASE + 20);
  });
});

describe('placeCode', () => {
  it('starts the objects one after the other, each rounded up to 0x100 short words', () => {
    const { bases, end } = placeCode([object({ parcels: 0x101 }), object({ parcels: 0x100 }), object({ parcels: 1 })]);
    expect(bases).toEqual([CODE_BASE, CODE_BASE + 0x200, CODE_BASE + 0x300]);
    expect(end).toBe(CODE_BASE + 0x400);
  });
});
