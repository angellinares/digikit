import assert from 'node:assert/strict';
import { afterEach, beforeEach, test, vi } from 'vitest';
import { freeClock, pollUntil, steppedClock, waitClock, type Clock } from '../src/emulated-clock.ts';
import { MS } from '../src/session.ts';

beforeEach(() => {
  vi.useFakeTimers();
});
afterEach(() => {
  vi.useRealTimers();
});

/** A clock that moves only when paused. */
function manualClock(): Clock & { at: number; pauses: number[] } {
  const clock = {
    at: 0,
    pauses: [] as number[],
    async now() {
      return clock.at;
    },
    async pause(ms: number) {
      clock.pauses.push(ms);
      clock.at += ms;
    },
  };
  return clock;
}

test('freeClock reads emulated ms of the status and falls back to DSP frames', async () => {
  let status: Record<string, unknown> = { elapsed: 5 * MS };
  const clock = freeClock({ status: async () => status } as never);
  assert.equal(await clock.now(), 5);
  status = { audio: { frames: 300 } };
  assert.equal(await clock.now(), 200);
});

test('freeClock pause waits and throws when the clock stands still', async () => {
  let elapsed = 0;
  const clock = freeClock({ status: async () => ({ elapsed }) } as never, { hangMs: 5000 });
  elapsed = MS;
  const first = clock.pause(1000);
  await vi.advanceTimersByTimeAsync(1000);
  await first;
  const second = clock.pause(3000).then(
    () => 'ok',
    (error: Error) => error,
  );
  await vi.advanceTimersByTimeAsync(3000);
  assert.equal(await second, 'ok');
  const third = clock.pause(3000).then(
    () => 'ok',
    (error: Error) => error,
  );
  await vi.advanceTimersByTimeAsync(3000);
  const error = await third;
  assert.ok(error instanceof Error && /emulated clock stuck at 1 ms/.test(error.message), String(error));
});

test('freeClock counts a dropping clock as movement', async () => {
  let elapsed = 100 * MS;
  const clock = freeClock({ status: async () => ({ elapsed }) } as never, { hangMs: 5000 });
  for (let i = 0; i < 5; i++) {
    elapsed -= MS;
    const paused = clock.pause(3000);
    await vi.advanceTimersByTimeAsync(3000);
    await paused;
  }
});

test('steppedClock steps floor(ms * MS) instructions in slices', async () => {
  const steps: number[] = [];
  const clock = steppedClock({
    status: async () => ({ elapsed: 0 }),
    client: { step: async (n: number) => void steps.push(n) },
  } as never);
  await clock.pause(1.5);
  assert.deepEqual(steps, [Math.floor(1.5 * MS)]);
  steps.length = 0;
  await clock.pause(1200);
  assert.deepEqual(steps, [500 * MS, 500 * MS, 200 * MS]);
});

test('pollUntil returns at once without reading the clock', async () => {
  const clock = manualClock();
  clock.now = async () => {
    throw new Error('clock read');
  };
  assert.equal(
    await pollUntil(
      clock,
      async () => 7,
      (v) => v === 7,
      { withinMs: 100, message: 'm' },
    ),
    7,
  );
});

test('pollUntil retries a failing read and calls onWait', async () => {
  const clock = manualClock();
  let calls = 0;
  const waits: boolean[] = [];
  const value = await pollUntil(
    clock,
    async () => {
      if (++calls < 3) throw new Error('lost');
      return calls;
    },
    (v) => v === 3,
    { withinMs: 1000, message: 'm', retry: true, onWait: (r) => waits.push(r.ok) },
  );
  assert.equal(value, 3);
  assert.deepEqual(waits, [false, false, true]);
  assert.deepEqual(clock.pauses, [200, 200]);
});

test('pollUntil fails after withinMs with the lazy message and the last value', async () => {
  const clock = manualClock();
  let resolved = 0;
  await assert.rejects(
    pollUntil(
      clock,
      async () => 'x',
      () => false,
      {
        withinMs: 500,
        intervalMs: 100,
        message: () => `lazy ${++resolved}`,
      },
    ),
    /lazy 1: not seen within 500 ms of emulated time \(last: "x"\)$/,
  );
  assert.equal(resolved, 1);
});

test('pollUntil without retry rethrows a failing read', async () => {
  await assert.rejects(
    pollUntil(
      manualClock(),
      async () => Promise.reject(new Error('boom')),
      () => true,
      { withinMs: 10, message: 'm' },
    ),
    /boom/,
  );
});

test('waitClock lets the time pass in steps of at most 50 ms', async () => {
  const clock = manualClock();
  await waitClock(clock, 120);
  assert.deepEqual(clock.pauses, [50, 50, 20]);
  await waitClock(clock, 0);
  assert.equal(clock.pauses.length, 3);
});

test('waitClock and pollUntil fail with a clear message when the clock never moves', async () => {
  const frozen: Clock = { now: async () => 7, pause: async () => {} };
  const advance = async (p: Promise<unknown>) => {
    const out = p.then(
      () => 'ok',
      (e: Error) => e,
    );
    for (let i = 0; i < 10; i++) await vi.advanceTimersByTimeAsync(500);
    return out;
  };
  const slowFrozen: Clock = { now: frozen.now, pause: (ms) => new Promise((r) => setTimeout(r, ms)) };
  const waited = await advance(waitClock(slowFrozen, 1000, { hangMs: 2000 }));
  assert.ok(
    waited instanceof Error && /wait of 1000 ms: emulated clock stuck at 7 ms/.test(waited.message),
    String(waited),
  );
  const polled = await advance(
    pollUntil(slowFrozen, async () => 0, Boolean, {
      withinMs: 10_000,
      message: 'ready',
      hangMs: 2000,
      intervalMs: 100,
    }),
  );
  assert.ok(polled instanceof Error && /ready: emulated clock stuck at 7 ms/.test(polled.message), String(polled));
});
