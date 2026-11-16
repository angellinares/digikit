import assert from 'node:assert/strict';
import { test } from 'vitest';
import { decodeCommitEvent, readBlock } from '../src/commit-records.ts';

test('the commit event gives the counts and the block range', () => {
  const c = decodeCommitEvent({ args: [2, 1 | (4 << 8) | (1 << 16), 0x47e90000, 640, 0] });
  assert.deepEqual(c, { index: 2, machineRows: 1, paramRows: 4, pageRows: 1, addr: 0x47e90000, size: 640 });
});

test('a block is read as big-endian longwords and an unmapped word fails', async () => {
  const words = new Map([
    [0x1000, 0x01020304],
    [0x1004, 0xfffefdfc],
  ]);
  const peek = async (a: number) => words.get(a) ?? null;
  assert.deepEqual([...(await readBlock(peek, 0x1000, 8))], [1, 2, 3, 4, 0xff, 0xfe, 0xfd, 0xfc]);
  await assert.rejects(readBlock(peek, 0x1000, 12), /unmapped/);
  await assert.rejects(readBlock(peek, 0x1002, 4), /aligned/);
});
