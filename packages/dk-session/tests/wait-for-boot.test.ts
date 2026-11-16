// waitForBoot after restart(): with the debug module the log shows the new boot (BOOT_START without the RESTART request); without it INFO has no LOG capability
// and the device answering again after a silent spell does.
import assert from 'node:assert/strict';
import { test, vi } from 'vitest';
import { DkError, DkSession, encode8in7 } from '@digi/dk-session';
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

test('after the first round the log is read from the cursor, not from the start', async () => {
  const dev = new Device();
  dev.debug = true;
  const s = new DkSession(dev, { timeoutMs: 200, retries: 0 });
  await s.restart();
  const froms: number[] = [];
  const page = s.logPage.bind(s);
  s.logPage = (from, quick) => {
    froms.push(from);
    return page(from, quick);
  };
  let polls = 0;
  await s.waitForBoot(async () => {
    polls++;
    dev.rebooted = polls >= 4;
  }, 5000);
  assert.equal(froms[0], 0);
  assert.ok(
    froms.slice(1, -1).every((f) => f === 1),
    `rounds after the first ask from the last event: ${froms}`,
  );
  assert.equal(froms[froms.length - 1], 0, 'a new boot is read from its start');
});

test('a refused restart in the log still ends the wait with refused', async () => {
  class RefusedDevice extends Device {
    send(msg: Uint8Array) {
      if (msg[5] !== SERVICE_CMD.LOG) return super.send(msg);
      const ev = [
        ...be32(1),
        ...be32(0),
        LOG_KIND.UPLOAD >> 8,
        LOG_KIND.UPLOAD & 255,
        0,
        1, // flags: the reply status is in argument 1
        ...be32(GCMD.RESTART),
        ...be32(5),
        ...be32(0),
        ...be32(0),
        ...be32(0),
      ];
      this.queue.push(reply(msg[5]!, msg[6]!, 0, [...be32(1), ...be32(1), 1, ...ev]));
    }
  }
  const dev = new RefusedDevice();
  dev.debug = true;
  const s = new DkSession(dev, { timeoutMs: 200, retries: 0 });
  await s.restart();
  await assert.rejects(
    s.waitForBoot(async () => {}, 5000),
    (e: unknown) => e instanceof DkError && e.kind === 'refused',
  );
});

/** A device whose replies take `delayMs` of wall time; a receive that waits less gets nothing, as on a real transport. */
class SlowDevice extends Device {
  delayMs = 0;
  async recv(waitMs?: number) {
    if (this.queue.length === 0) return null;
    const want = this.delayMs;
    if (waitMs !== undefined && want > waitMs) {
      await new Promise((r) => setTimeout(r, waitMs));
      return null;
    }
    await new Promise((r) => setTimeout(r, want));
    return this.queue.shift() ?? null;
  }
}

test('a probe timeout longer than the default lets a slow host show the boot', async () => {
  const dev = new SlowDevice();
  dev.debug = true;
  dev.rebooted = true;
  const s = new DkSession(dev, { timeoutMs: 200, retries: 0 });
  await s.restart();
  dev.delayMs = 1300; // longer than the 1 s quick probe
  await s.waitForBoot(async () => {}, 20000, { probeTimeoutMs: 5000 });
});

/** A device that loses the first request (sent while it restarts); a receive ends at its wait or when `signal` fires. */
class LostDevice extends Device {
  lose = true;
  send(msg: Uint8Array) {
    if (this.lose) return void (this.lose = false);
    return super.send(msg);
  }
  async recv(waitMs?: number, signal?: AbortSignal) {
    if (this.queue.length > 0) return this.queue.shift()!;
    await new Promise<void>((r) => {
      const t = setTimeout(r, waitMs ?? 1000);
      signal?.addEventListener('abort', () => (clearTimeout(t), r()), { once: true });
    });
    return this.queue.shift() ?? null;
  }
}

test('a probe lost during the restart ends with the caller signal, and the next probe sees the new boot', async () => {
  const dev = new LostDevice();
  dev.debug = true;
  dev.rebooted = true;
  const s = new DkSession(dev, { timeoutMs: 60_000, retries: 0 });
  const started = Date.now();
  let probes = 0;
  await s.waitForBoot(async () => {}, 20_000, {
    // the harness's emulated bound: the first probe ends after 40 ms of the fake clock, far below the wall guard
    probe: async (run) => {
      probes++;
      const ctl = new AbortController();
      const timer = setTimeout(() => ctl.abort(), 40);
      try {
        return await run({ signal: ctl.signal, ms: 60_000 });
      } finally {
        clearTimeout(timer);
      }
    },
  });
  assert.equal(probes, 3, 'the lost info probe, then info and the log read of the second try');
  assert.ok(Date.now() - started < 5000, 'a lost probe did not wait for the wall guard');
});

test('a slow reply still succeeds while the signal has not fired', async () => {
  const dev = new SlowDevice();
  dev.debug = true;
  dev.rebooted = true;
  const s = new DkSession(dev, { timeoutMs: 200, retries: 0 });
  await s.restart();
  dev.delayMs = 1300; // wall-slow, as on a loaded host; the harness clock (emulated) has not reached its bound
  const ctl = new AbortController();
  await s.waitForBoot(async () => {}, 20_000, { probe: (run) => run({ signal: ctl.signal, ms: 60_000 }) });
  assert.equal(ctl.signal.aborted, false);
});

test('a cancelled read ends the wait', async () => {
  const dev = new Device();
  const s = new DkSession(dev, { timeoutMs: 50, retries: 0 });
  s.info = async () => {
    throw new DkError({ error: 'cancelled', kind: 'cancelled' });
  };
  await assert.rejects(
    s.waitForBoot(async () => {}, 5000),
    (e: unknown) => e instanceof DkError && e.kind === 'cancelled',
  );
});

test('the wait does not time out while the boot is reported', async () => {
  vi.useFakeTimers({ toFake: ['Date'] });
  try {
    const s = new DkSession(new Device(), { timeoutMs: 50, retries: 0 });
    let reads = 0;
    s.info = async () => {
      if (++reads <= 5) throw new Error('no answer'); // the port is silent while the controller boots
      return { caps: null } as Awaited<ReturnType<DkSession['info']>>;
    };
    let slept = 0;
    // a fake sleep: the clock moves, no real time passes
    const sleep = async (ms: number) => {
      slept += ms;
      vi.setSystemTime(Date.now() + ms);
    };
    // 5 passes of 500 ms are 2500 ms, more than the 1000 ms timeout; the reported boot keeps the wait alive
    await s.waitForBoot(sleep, 1000, { stillBooting: () => reads < 5 });
    assert.equal(reads, 6);
    assert.ok(slept > 1000);
  } finally {
    vi.useRealTimers();
  }
});

test('an aborted signal ends the wait', async () => {
  const dev = new Device();
  const s = new DkSession(dev, { timeoutMs: 200, retries: 0 });
  await s.restart();
  const stop = new AbortController();
  await assert.rejects(
    s.waitForBoot(
      async () => {
        stop.abort();
      },
      5000,
      { signal: stop.signal },
    ),
    (e: unknown) => e instanceof DkError && e.kind === 'cancelled',
  );
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
