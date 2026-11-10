import { afterEach, beforeEach, expect, test, vi } from 'vitest';
import { startNoProgressWatchdog, type WatchedSession } from '../watchdog.ts';

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

type Status = Awaited<ReturnType<WatchedSession['status']>>;

function fake(next: (n: number) => Status) {
  let n = 0;
  const close = vi.fn(async () => {});
  const clientClose = vi.fn();
  const session: WatchedSession = {
    status: async () => next(n++),
    close,
    client: { close: clientClose },
  };
  return { session, close, clientClose };
}

const free = (frames: number, mainloop = 1): Status => ({
  mode: 'free',
  dsp_frames: frames,
  cf: { mainloop_hits: mainloop },
  audio: { frames },
});

const options = { intervalMs: 5000, limitMs: 60_000, log: () => {} };

test('a session that makes progress is not closed', async () => {
  const { session, close } = fake((n) => free(n * 100));
  const dog = startNoProgressWatchdog(session, options);
  await vi.advanceTimersByTimeAsync(300_000);
  dog.stop();
  expect(close).not.toHaveBeenCalled();
  expect(dog.tripped).toBeUndefined();
});

test('a paused session without a job is idle on purpose', async () => {
  const { session, close } = fake(() => ({ mode: 'paused', job: null, dsp_frames: 5, cf: { mainloop_hits: 5 } }));
  const dog = startNoProgressWatchdog(session, options);
  await vi.advanceTimersByTimeAsync(300_000);
  dog.stop();
  expect(close).not.toHaveBeenCalled();
});

test('a free-running session without progress is closed after the limit', async () => {
  const lines: string[] = [];
  const { session, close, clientClose } = fake(() => ({ ...free(7, 9), cf: { mainloop_hits: 9, icount: 5 } }));
  const dog = startNoProgressWatchdog(session, { ...options, log: (line) => lines.push(line) });
  await vi.advanceTimersByTimeAsync(50_000);
  expect(close).not.toHaveBeenCalled();
  await vi.advanceTimersByTimeAsync(20_000);
  expect(close).toHaveBeenCalledTimes(1);
  expect(clientClose).toHaveBeenCalledTimes(1);
  expect(lines.join('\n')).toMatch(/no emulated progress for \d+ s \(frames 7, mainloop 9, icount 5, boot undefined\)/);
  expect(dog.tripped).toMatch(/no emulated progress/);
});

test('a status call that never answers counts as no progress', async () => {
  const close = vi.fn(async () => {});
  const session: WatchedSession = { status: () => new Promise(() => {}), close };
  startNoProgressWatchdog(session, options);
  await vi.advanceTimersByTimeAsync(80_000);
  expect(close).toHaveBeenCalledTimes(1);
});

test('a cold load moves only the instruction counts: that is progress, also when the count drops', async () => {
  const { session, close } = fake((n) => ({
    mode: 'free',
    job: null,
    dsp_frames: 0,
    cf: { mainloop_hits: 0, icount: 100 },
    boot: { icount: n % 7 === 0 ? 1 : n * 1000 },
  }));
  const dog = startNoProgressWatchdog(session, options);
  await vi.advanceTimersByTimeAsync(300_000);
  dog.stop();
  expect(close).not.toHaveBeenCalled();
});

test('a new epoch is progress', async () => {
  const { session, close } = fake((n) => ({ ...free(7, 9), epoch: Math.floor(n / 6) }));
  const dog = startNoProgressWatchdog(session, options);
  await vi.advanceTimersByTimeAsync(300_000);
  dog.stop();
  expect(close).not.toHaveBeenCalled();
});

test('the second connection is used when the session has one', async () => {
  const main = vi.fn(async () => free(1));
  const second = vi.fn(async () => free(1));
  const session: WatchedSession = { status: main, watchStatus: second, close: async () => {} };
  const dog = startNoProgressWatchdog(session, options);
  await vi.advanceTimersByTimeAsync(20_000);
  dog.stop();
  expect(main).not.toHaveBeenCalled();
  expect(second).toHaveBeenCalled();
});

test('a paused session with a boot inside firmware.load is not idle', async () => {
  const { session, close } = fake(() => ({
    mode: 'paused',
    job: null,
    dsp_frames: 5,
    cf: { mainloop_hits: 5, icount: 1 },
    boot: { icount: 0 },
  }));
  startNoProgressWatchdog(session, options);
  await vi.advanceTimersByTimeAsync(80_000);
  expect(close).toHaveBeenCalledTimes(1);
});

test('a firmware that spins after its boot (instruction count rises, nothing else, no job) is closed', async () => {
  const { session, close } = fake((n) => ({
    mode: 'free',
    job: null,
    boot: null,
    dsp_frames: 7,
    cf: { mainloop_hits: 9, icount: n * 1_000_000 },
    audio: { frames: 7 },
  }));
  const dog = startNoProgressWatchdog(session, options);
  await vi.advanceTimersByTimeAsync(80_000);
  expect(close).toHaveBeenCalledTimes(1);
  expect(dog.tripped).toMatch(/no emulated progress/);
});

test('the same rising instruction count is progress while a job runs', async () => {
  const { session, close } = fake((n) => ({
    mode: 'free',
    job: { kind: 'load' },
    dsp_frames: 7,
    cf: { mainloop_hits: 9, icount: n * 1_000_000 },
  }));
  const dog = startNoProgressWatchdog(session, options);
  await vi.advanceTimersByTimeAsync(300_000);
  dog.stop();
  expect(close).not.toHaveBeenCalled();
});
