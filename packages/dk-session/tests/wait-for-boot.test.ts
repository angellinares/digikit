// waitForBoot after restart(): with the debug module the log shows the new boot (BOOT_START without the RESTART request); without it INFO has no LOG capability
// and the device answering again after a silent spell does.
import assert from 'node:assert/strict';
import { test } from 'vitest';
import { DkSession, encode8in7 } from '@digi/dk-session';
import { CAP, CMD as GCMD, LOG_KIND, SERVICE_CMD } from '@digi/dk-session/gen_proto';

const concat = (...p: Uint8Array[]) => Uint8Array.from(Buffer.concat(p.map((x) => Buffer.from(x))));
const PREFIX = [0xf0, 0x7d, 0x64, 0x6b, 1];
const reply = (cmd: number, seq: number, status: number, body: number[] = []) =>
  concat(
    Uint8Array.from(PREFIX),
    Uint8Array.of(cmd | 0x40, seq),
    encode8in7(Uint8Array.of(status & 255, ...body)),
    Uint8Array.of(0xf7),
  );
const be32 = (n: number) => [(n >>> 24) & 255, (n >>> 16) & 255, (n >>> 8) & 255, n & 255];

/** A device whose INFO has, while `debug` is set, the LOG capability and a log. */
class Device {
  debug = false;
  silent = false;
  rebooted = false;
  queue: Uint8Array[] = [];
  send(msg: Uint8Array) {
    if (this.silent) return;
    const cmd = msg[5]!;
    const seq = msg[6]!;
    if (cmd === GCMD.INFO) {
      const caps = (1 << CAP.SERVICE_CMDS) | (this.debug ? 1 << CAP.LOG : 0);
      const body = [0, 1, 0, 5, ...Buffer.from('dt2\0'), ...be32(1000), ...be32(32768), 0, ...be32(0), ...be32(0)];
      body.push(...be32(0x000500), ...be32(caps), 0, 0, 0, 0, 0, 0, 0, 0);
      return void this.queue.push(reply(cmd, seq, 0, body.slice(1)));
    }
    if (cmd === GCMD.RESTART) return void this.queue.push(reply(cmd, seq, 0));
    if (cmd === SERVICE_CMD.LOG) {
      if (!this.debug) return void this.queue.push(reply(cmd, seq, -60));
      // the log: a RESTART request of the old boot, or BOOT_START of the new one
      const kind = this.rebooted ? LOG_KIND.BOOT_START : LOG_KIND.UPLOAD;
      const a0 = this.rebooted ? 0 : GCMD.RESTART;
      const ev = [
        ...be32(1),
        ...be32(0),
        kind >> 8,
        kind & 255,
        0,
        0,
        ...be32(a0),
        ...be32(0),
        ...be32(0),
        ...be32(0),
        ...be32(0),
      ];
      return void this.queue.push(reply(cmd, seq, 0, [...be32(1), ...be32(1), 1, ...ev]));
    }
    this.queue.push(reply(cmd, seq, -60));
  }
  async recv() {
    return this.queue.shift() ?? null;
  }
}

test('without the debug module an answer after a silent spell is the new boot', async () => {
  const dev = new Device();
  const s = new DkSession(dev, { timeoutMs: 200, retries: 0 });
  await s.restart();
  let polls = 0;
  await s.waitForBoot(async () => {
    polls++;
    dev.silent = polls === 2; // the old boot answers, the device resets (silent), the new boot answers
  }, 5000);
  assert.equal(polls, 3);
});

test('with the debug module the log shows the new boot: BOOT_START and no RESTART request', async () => {
  const dev = new Device();
  dev.debug = true;
  const s = new DkSession(dev, { timeoutMs: 200, retries: 0 });
  await s.restart();
  let polls = 0;
  await s.waitForBoot(async () => {
    polls++;
    dev.rebooted = polls >= 2; // the old boot's log holds the RESTART request; after one more poll the log is the new boot's
  }, 5000);
  assert.equal(polls, 2);
});

test('a device that never restarts and has no log times out', async () => {
  const dev = new Device();
  const s = new DkSession(dev, { timeoutMs: 50, retries: 0 });
  await s.restart();
  await assert.rejects(
    s.waitForBoot(async () => {}, 30),
    /no new boot/,
  );
});
