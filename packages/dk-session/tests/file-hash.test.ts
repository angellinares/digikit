// The store's hash slot of a file (`fileHash`): the vectors of `dksample::file_hash` and the runtime's `hash32.rs` (made with the reference lookup3
// variant of packages/dksample), over the lengths that cross its 12-byte blocks.
import assert from 'node:assert/strict';
import { test } from 'vitest';
import { fileHashHex } from '@digi/dk-session';

const data = (n: number) => Uint8Array.from({ length: n }, (_, i) => (i * 7 + 3) & 255);
const slot = (h: string) => h + '0'.repeat(56);

test('the slot is the odd content hash, four big-endian bytes, then 28 zeros', () => {
  const want: Record<number, string> = {
    0: '43fa243b',
    1: '82ea421b',
    4: '4779e253',
    5: 'dc43f42f',
    8: '45d2434f',
    9: '5c55f61f',
    12: '9498fe83',
    13: 'ef8c52e1',
    24: 'f3b06325',
    25: '36603f21',
    1000: 'ed9b9ced',
    32775: 'fc7a664d',
  };
  for (const [n, h] of Object.entries(want)) assert.equal(fileHashHex(data(Number(n))), slot(h), `${n} bytes`);
  assert.equal(fileHashHex(new TextEncoder().encode('abc')), slot('49bec845'));
});
