import { expect } from 'vitest';
import type { PatchedSession } from '@digi/regression/support.ts';
import { runtimeTests } from '@digi/regression/runtime/ready.ts';
import { CMD_RESET, CMD_STATUS, installProbe, PROBE, readRing, status } from '@digi/regression/runtime/probe-kit.ts';

/** A run is 1674 frames and takes about 33 s of emulated time; the test checks this many frames from the start of the capture, not a whole run. */
const FRAMES = 60;

async function expectProbeTransport(patched: PatchedSession): Promise<void> {
  const { session, dk } = patched;
  await installProbe(patched, PROBE);
  expect((await dk.request(CMD_RESET)).status).toBe(0);
  // the effect starts a few thousand blocks after the boot; the collector then fills its ring
  let last: Record<string, number> = {};
  await expect
    .poll(
      async () => {
        const reply = await dk.request(CMD_STATUS);
        expect(reply.status, 'STATUS answers').toBe(0);
        last = status(reply.body);
        return last.frames_ok;
      },
      { timeout: 300_000, interval: 2000, message: `${FRAMES} frames` },
    )
    .toBeGreaterThanOrEqual(FRAMES);
  expect(last.seq_errors, `sequence errors: ${JSON.stringify(last)}`).toBe(0);
  expect(last.cks_errors, `checksum errors: ${JSON.stringify(last)}`).toBe(0);
  expect(last.drops, `records the callback missed: ${JSON.stringify(last)}`).toBe(0);
  await session.expectNoFault('probe transport');
}

runtimeTests('the SHARC probe sends framed words that the collector receives without errors', expectProbeTransport, { dsp: 'sharc',
  timeout: 600_000,
});

/** The words the beacon stores for its call number n (the counter after the increment): the counter, the two initialised words and the descriptor magic, each held for two calls. */
const BEACON_K1 = 0x5a5a1234;
const BEACON_K2 = 0xc0de0001;
const BEACON_MAGIC = 0x444b4658;
function beaconWord(n: number): number {
  switch ((n >>> 1) & 3) {
    case 0:
      return (0xbe000000 | (n & 0xffffff)) >>> 0;
    case 1:
      return BEACON_K1;
    case 2:
      return BEACON_K2;
    default:
      return BEACON_MAGIC;
  }
}

/** The beacon stores one step word on every call. The block tail copies it into every reply record, so the collector gets a word from every block: no zeros, consecutive counter
 * values, and the initialised constants exact. */
async function expectBeaconWords(patched: PatchedSession): Promise<void> {
  const { session, dk } = patched;
  await installProbe(patched, 'test/step-beacon');
  expect((await dk.request(CMD_RESET)).status).toBe(0);
  const WORDS = 400;
  let last: Record<string, number> = {};
  await expect
    .poll(
      async () => {
        last = status((await dk.request(CMD_STATUS)).body);
        return last.words;
      },
      { timeout: 120_000, interval: 2000, message: 'beacon words' },
    )
    .toBeGreaterThanOrEqual(WORDS);
  expect(last.drops, `records the callback missed: ${JSON.stringify(last)}`).toBe(0);
  const words = await readRing(dk, WORDS);
  expect(words.length).toBe(WORDS);
  expect(words.filter((w) => w === 0).length, 'no zero words').toBe(0);
  // the first counter word names the call number; every word after it must follow the beacon's pattern for the next call
  const first = words.findIndex((w) => w >>> 24 === 0xbe);
  expect(first, 'a counter word in the ring').toBeGreaterThanOrEqual(0);
  const n0 = words[first]! & 0xffffff;
  const bad = words
    .slice(first)
    .map((w, i) => [i, w, beaconWord(n0 + i)] as const)
    .filter(([, w, want]) => w !== want);
  expect(bad.slice(0, 4), `words that break the pattern (call ${n0} at word ${first})`).toEqual([]);
  await session.expectNoFault('step beacon');
}

runtimeTests('the step beacon words reach the collector in every block', expectBeaconWords, { dsp: 'sharc', timeout: 300_000 });
