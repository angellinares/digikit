import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { Session } from '@digi/test-support';
import { afterAll, describe, expect, test } from 'vitest';
import { FIRMWARE, liveOptions } from '../runtime/devices.ts';

/** Stock firmware with the reply-record check on (SHARC_REPLY_INFLIGHT, docs/INSTRUMENTATION.md): the firmware builds the next record while the
 * previous one is sent, so the core must not change the record that the transfer is reading. The check runs in `fail` mode; the daemon prints a progress
 * line every 500 exchanges and a summary when it exits. Emulated time to run: 1.5 DSP blocks per ms, so about SKIP + 500 blocks. */
const WAIT_MS = 2000;
/** Exchanges left out: the stock firmware writes a few record words while it boots (DT2 1.17: PC 0xb88e47 in frames 1788 and 1789). */
const SKIP = 2500;

/** DT2 only: a stock DN2 daemon in snapshot mode made no SPI2 exchange in 12 s of emulated time (the DN2 case runs through the patched runtime, see finding 78). */
for (const device of ['dt2'] as const) {
  describe(`${device.toUpperCase()} ${FIRMWARE[device]} reply record in flight`, () => {
    const saved = process.env.SHARC_REPLY_INFLIGHT;
    process.env.SHARC_REPLY_INFLIGHT = saved ?? 'fail';
    process.env.SHARC_REPLY_INFLIGHT_SKIP = String(SKIP);
    afterAll(() => {
      delete process.env.SHARC_REPLY_INFLIGHT_SKIP;
      if (saved === undefined) delete process.env.SHARC_REPLY_INFLIGHT;
      else process.env.SHARC_REPLY_INFLIGHT = saved;
    });

    test('stock firmware changes no word of the record while it is sent', async () => {
      const runDir = fs.mkdtempSync(path.join(os.tmpdir(), 'digi-reply-inflight-'));
      const session = await Session.start({ ...liveOptions(device), dsp: 'sharc', runDir });
      let stderr = '';
      try {
        await session.wait(WAIT_MS);
        await session.expectNoFault('reply record in flight');
      } finally {
        await session.close();
        stderr = fs.readFileSync(path.join(runDir, 'daemon.stderr'), 'utf8');
        fs.rmSync(runDir, { recursive: true, force: true });
      }
      // progress lines and the exit summary; a daemon may create more than one engine
      const summaries = [
        ...stderr.matchAll(/reply-inflight: (\d+) frames armed;.*early (\d+), late (\d+) word changes/g),
      ];
      expect(summaries.length, `the daemon printed no summary:\n${stderr.slice(-600)}`).toBeGreaterThan(0);
      const frames = Math.max(...summaries.map((m) => Number(m[1])));
      const early = Math.max(...summaries.map((m) => Number(m[2])));
      expect(frames, 'frames checked').toBeGreaterThanOrEqual(SKIP + 100);
      expect(early, `early changes:\n${stderr.slice(-1200)}`).toBe(0);
    }, 600_000);
  });
}
