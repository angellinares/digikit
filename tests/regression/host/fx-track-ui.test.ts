import { expect, test, vi } from 'vitest';
import type { Session } from '@digi/test-support';
import { halfword } from '../runtime/fx-track-ui.ts';

test('Sound halfwords preserve both bytes at odd and cross-word addresses', async () => {
  const memory = new Map([
    [0x100, 0x81234567],
    [0x104, 0x89abcdef],
  ]);
  const memPeek = vi.fn(async (address: number) => memory.get(address) ?? null);
  const session = { client: { memPeek } } as unknown as Session;
  expect(await halfword(session, 0x100)).toBe(0x8123);
  expect(await halfword(session, 0x101)).toBe(0x2345);
  expect(await halfword(session, 0x102)).toBe(0x4567);
  expect(await halfword(session, 0x103)).toBe(0x6789);
  expect(await halfword(session, 0x105)).toBe(0xabcd);
});

test('Sound halfwords at addresses from 0x80000000 up read unsigned word addresses', async () => {
  const memory = new Map([
    [0x80005e60, 0x81234567],
    [0x80005e64, 0x89abcdef],
  ]);
  const memPeek = vi.fn(async (address: number) => (address >= 0 ? (memory.get(address) ?? null) : null));
  const session = { client: { memPeek } } as unknown as Session;
  expect(await halfword(session, 0x80005e62)).toBe(0x4567);
  expect(await halfword(session, 0x80005e63)).toBe(0x6789);
  expect(memPeek).toHaveBeenCalledWith(0x80005e64);
});
