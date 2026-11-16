// MidiRpc waits in emulated time (an emulatedProbe) and in wall time (none), over a fake transport and a fake emulated clock.
import assert from 'node:assert/strict';
import { afterEach, beforeEach, test, vi } from 'vitest';
import { DkError, DkSession, rpcRequest, type EmulatedProbe } from '../src/index.ts';

const CREATE_DIR = 0x11;

// every timer and Date.now below is fake: time passes only when a test advances it, so host speed does not matter
beforeEach(() => {
  vi.useFakeTimers();
});
afterEach(() => {
  vi.useRealTimers();
});

/** Advances fake time in small steps until `p` settles (bounded), and returns `p`. */
async function settle<T>(p: Promise<T>): Promise<Promise<T>> {
  let done = false;
  p.then(
    () => (done = true),
    () => (done = true),
  );
  for (let i = 0; i < 5000 && !done; i++) await vi.advanceTimersByTimeAsync(20);
  assert.ok(done, 'the call settled in fake time');
  return p;
}

/** An emulated clock that runs 100 ms per 5 ms of wall time (a slow host), and the probe that ends in emulated time. */
function fakeEmulator() {
  const clock = { now: 0 };
  const timer = setInterval(() => (clock.now += 100), 5);
  const probe: EmulatedProbe = async (run, emulatedMs) => {
    const ctl = new AbortController();
    const start = clock.now;
    const watch = setInterval(() => clock.now - start >= emulatedMs && ctl.abort(), 1);
    try {
      return await run({ signal: ctl.signal, ms: 20_000 });
    } finally {
      clearInterval(watch);
    }
  };
  return { clock, probe, stop: () => clearInterval(timer) };
}

/** Delivers the reply once `ready()` is true; `recv` ends at its wall timeout or when the signal aborts. */
function transport(ready: () => boolean) {
  const reply = rpcRequest(CREATE_DIR, 0, Uint8Array.of(0));
  let sent = false;
  return {
    async send() {
      sent = true;
    },
    recv(timeoutMs: number, signal?: AbortSignal): Promise<Uint8Array | null> {
      return new Promise((resolve) => {
        const end = Date.now() + timeoutMs;
        const poll = setInterval(() => {
          if (sent && ready()) done(reply);
          else if (Date.now() >= end || signal?.aborted) done(null);
        }, 2);
        const done = (m: Uint8Array | null) => {
          clearInterval(poll);
          resolve(m);
        };
      });
    },
  };
}

test('an answer at emulated 5 s arrives although the wall timeout is far shorter', async () => {
  const emu = fakeEmulator();
  try {
    const dk = new DkSession(
      transport(() => emu.clock.now >= 5000),
      { timeoutMs: 6000, emulatedProbe: emu.probe },
    );
    const t0 = Date.now();
    const got = await settle(dk.rpc(CREATE_DIR, Uint8Array.of(0), 6000));
    assert.deepEqual([...got], [0]);
    assert.ok(Date.now() - t0 < 5000);
  } finally {
    emu.stop();
  }
});

test('no answer within the emulated timeout fails with the usual timeout error', async () => {
  const emu = fakeEmulator();
  try {
    const dk = new DkSession(
      transport(() => false),
      { emulatedProbe: emu.probe },
    );
    await assert.rejects(settle(dk.rpc(CREATE_DIR, Uint8Array.of(0), 1000)), (e: unknown) => {
      assert.ok(e instanceof DkError);
      assert.equal(e.kind, 'timeout');
      assert.match(e.message, /no reply to MidiRpc 11 from the device/);
      return true;
    });
    assert.ok(emu.clock.now >= 1000);
  } finally {
    emu.stop();
  }
});

test('without an emulated clock the wait is wall time', async () => {
  const emu = fakeEmulator();
  try {
    // the clock passes 5 s of emulated time in about 250 ms of wall time, but the session does not use it
    const dk = new DkSession(transport(() => emu.clock.now >= 5000));
    const t0 = Date.now();
    await assert.rejects(settle(dk.rpc(CREATE_DIR, Uint8Array.of(0), 60)), /no reply to MidiRpc 11 from the device/);
    assert.ok(Date.now() - t0 < 200);
    const dk2 = new DkSession(transport(() => true));
    assert.deepEqual([...(await settle(dk2.rpc(CREATE_DIR, Uint8Array.of(0), 500)))], [0]);
  } finally {
    emu.stop();
  }
});
