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
  const { session, close, clientClose } = fake(() => free(7, 9));
  const dog = startNoProgressWatchdog(session, { ...options, log: (line) => lines.push(line) });
  await vi.advanceTimersByTimeAsync(50_000);
  expect(close).not.toHaveBeenCalled();
  await vi.advanceTimersByTimeAsync(20_000);
  expect(close).toHaveBeenCalledTimes(1);
  expect(clientClose).toHaveBeenCalledTimes(1);
  expect(lines.join('\n')).toMatch(/no emulated progress for \d+ s \(frames 7, mainloop 9\)/);
  expect(dog.tripped).toMatch(/no emulated progress/);
});

test('a status call that never answers counts as no progress', async () => {
  const close = vi.fn(async () => {});
  const session: WatchedSession = { status: () => new Promise(() => {}), close };
  startNoProgressWatchdog(session, options);
  await vi.advanceTimersByTimeAsync(80_000);
  expect(close).toHaveBeenCalledTimes(1);
});
