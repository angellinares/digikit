import { expect, test } from 'vitest';
import { DdrWriter } from '../runtime/sample-ddr-writer.ts';

test('whole loads serialize and a sealed skipped bank retires before the next INIT', async () => {
  const events: [number, number, number][] = [];
  let release!: () => void;
  const paused = new Promise<void>((resolve) => {
    release = resolve;
  });
  const writer = new DdrWriter(async (token, seq, op) => {
    events.push([token, seq, op]);
    if (token === 10 && seq === 1) await paused;
  });
  const a = writer.load(Buffer.alloc(960000), 10);
  const b = writer.load(Buffer.alloc(960000), 11, false);
  const c = writer.load(Buffer.alloc(960000), 12);
  await Promise.resolve();
  await Promise.resolve();
  expect(events).toEqual([
    [10, 0, 1],
    [10, 1, 2],
  ]);
  release();
  await Promise.all([a, b, c]);
  const controls = events.filter(([, , op]) => op !== 2);
  expect(controls).toEqual([
    [10, 0, 1],
    [10, 376, 3],
    [10, 378, 5],
    [10, 377, 4],
    [11, 0, 1],
    [11, 376, 3],
    [11, 377, 4],
    [12, 0, 1],
    [12, 376, 3],
    [12, 378, 5],
  ]);
});

test('capturing a failed load never releases queued or later backing writers', async () => {
  const tokens: number[] = [];
  const failure = new Error('completion uncertain');
  const writer = new DdrWriter(async (token, seq) => {
    tokens.push(token);
    if (seq === 2) throw failure;
  });
  const a = writer.load(Buffer.alloc(960000), 10).catch((error: unknown) => error);
  const b = writer.load(Buffer.alloc(960000), 11).catch((error: unknown) => error);
  expect(await a).toBe(failure);
  expect(await b).toBe(failure);
  await expect(writer.load(Buffer.alloc(960000), 12)).rejects.toBe(failure);
  expect(tokens).toEqual([10, 10, 10]);
});
