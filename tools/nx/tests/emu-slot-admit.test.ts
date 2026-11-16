import { mkdtempSync, rmSync, utimesSync, writeFileSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, expect, it } from 'vitest';
import { ADMIT_MAX_MS, acquire, release } from '../../devtools/emu-slot.mjs';

const dirs: string[] = [];
const slotDir = () => {
  const dir = mkdtempSync(join(tmpdir(), 'emu-slot-'));
  dirs.push(dir);
  return dir;
};
afterEach(() => {
  for (const dir of dirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

// An admit lock that names a live process (our parent) but was written long ago.
const staleAdmit = (dir: string, ageMs: number) => {
  const admit = join(dir, 'admit');
  writeFileSync(admit, String(process.ppid));
  const then = (Date.now() - ageMs) / 1000;
  utimesSync(admit, then, then);
  return admit;
};

it('an admit lock older than ADMIT_MAX_MS is taken over even when its pid is alive', async () => {
  const dir = slotDir();
  staleAdmit(dir, ADMIT_MAX_MS + 5_000);
  const quiet = () => {};
  const got = await Promise.race([
    acquire(dir, 2, false, quiet),
    new Promise<null>((resolve) => setTimeout(() => resolve(null), 3_000)),
  ]);
  expect(got).not.toBeNull();
  expect(existsSync(got!.lock)).toBe(true);
  release(got!.lock);
  release(got!.admit);
});

it('a fresh admit lock of a live process still makes a waiter wait', async () => {
  const dir = slotDir();
  const admit = staleAdmit(dir, 0);
  let resolved = false;
  const pending = acquire(dir, 2, false, () => {}).then((got) => {
    resolved = true;
    return got;
  });
  await new Promise((resolve) => setTimeout(resolve, 1_500));
  expect(resolved).toBe(false);
  // The holder goes away: the waiter proceeds.
  rmSync(admit, { force: true });
  const got = await pending;
  expect(existsSync(got.lock)).toBe(true);
  release(got.lock);
  release(got.admit);
});
