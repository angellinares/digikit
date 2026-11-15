import { describe, expect, it } from 'vitest';
import { OpQueue, hex, pokeHalf } from '../src/ops.ts';

describe('OpQueue', () => {
  it('keeps the order of pokes, masked pokes and calls, and joins neighbours of one kind', () => {
    const q = new OpQueue();
    q.poke(1, 10);
    q.poke(2, -1);
    q.pokeMasked(3, 0xff, 0x12);
    q.poke(4, 5);
    expect(q.take()).toEqual([
      {
        poke: [
          [1, 10],
          [2, 0xffffffff],
        ],
      },
      { pokem: [[3, 0xff, 0x12]] },
      { poke: [[4, 5]] },
    ]);
    expect(q.take()).toEqual([]);
  });
});

describe('pokeHalf', () => {
  it('changes one halfword of a word of an array of halfwords', () => {
    const q = new OpQueue();
    pokeHalf(q, 100, 4, 0x1234); // halfword 4: the low half of word 102
    pokeHalf(q, 100, 5, 0x1_5678); // halfword 5: the high half of word 102, value cut to 16 bits
    expect(q.take()).toEqual([
      {
        pokem: [
          [102, 0xffff, 0x1234],
          [102, 0xffff0000, 0x56780000],
        ],
      },
    ]);
  });
});

describe('hex', () => {
  it('writes a number as Python does with {n:#x}', () => {
    expect(hex(255)).toBe('0xff');
    expect(hex(-1)).toBe('-0x1');
  });
});
