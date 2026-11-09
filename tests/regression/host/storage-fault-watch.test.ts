import { expect, test } from 'vitest';
import { validateStorageWatch, type StorageWatchEvent } from '../runtime/storage-fault-watch.ts';

const latch = 0x47e8b0a4;
const mask = 0x00310000;
const allowed: [number, number][] = [
  [0x403117c4, 0x403117fa],
  [0x47e8b000, 0x47e8b0a2],
];
const cpu = (addr: number, pc = allowed[0]![0]): StorageWatchEvent => ({
  t: 'w',
  ic: 10,
  addr: `0x${addr.toString(16)}`,
  pc: `0x${pc.toString(16)}`,
  size: 4,
  value: '0x0',
});

test('cached and raw uncached alias writes retain the same latch and mounted address', () => {
  const rows = [cpu(latch), cpu(latch + 0x08000000), cpu(0x44f2bd68 + 0x08000000, 0x4015a722)];
  expect(validateStorageWatch(rows, { events: 3, logged: 3 }, latch, mask, allowed).map((row) => row.address)).toEqual([
    latch,
    latch,
    0x44f2bd68,
  ]);
});

test('aliased rogue CPU writer is rejected instead of disappearing from latch overlap', () => {
  const row = cpu(latch + 0x08000000, 0x40123456);
  expect(() => validateStorageWatch([row], { events: 1, logged: 1 }, latch, mask, allowed)).toThrow(
    'unexpected latch writer',
  );
});

test('aliased DMA latch writer is rejected without a CPU-PC exemption', () => {
  const row: StorageWatchEvent = { t: 'dma', ic: 10, addr: `0x${(latch + 0x08000000).toString(16)}`, size: 4 };
  expect(() => validateStorageWatch([row], { events: 1, logged: 1 }, latch, mask, allowed)).toThrow(
    'non-CPU latch writer',
  );
});

test('valid parsed prefix cannot pass merely because reported counters agree', () => {
  const prefix = [cpu(latch)];
  expect(() => validateStorageWatch(prefix, { events: 2, logged: 2 }, latch, mask, allowed)).toThrow(
    'parsed write count differs',
  );
});

test('configured log truncation is rejected before interpreting the prefix', () => {
  expect(() => validateStorageWatch([cpu(latch)], { events: 2, logged: 1 }, latch, mask, allowed)).toThrow(
    'report is truncated',
  );
});
