import { describeRestart, restartProgress, type ProgressStatus } from './progress.ts';
import { MS, PROGRESS_HANG_S, withinRuntimeDeadline, type Session } from './session.ts';

/** Same shape as `Clock` in @digi/dk-session (structural; no import). Time in ms. */
export interface Clock {
  now(): Promise<number>;
  pause(ms: number): Promise<void>;
}

/** Real-time wait through the global timer (vitest fake timers replace it; `node:timers/promises` they do not). */
const delay = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

/** DSP frames per emulated millisecond (1500 a second): the fallback clock of a status reply without `elapsed`. */
const FRAMES_PER_MS = 1.5;

/** One slice of a step: 0.5 emulated seconds, each bounded by PROGRESS_HANG_S of wall time. */
const SLICE_INSNS = MS * 500;

type ClockStatus = ProgressStatus & { audio?: unknown; elapsed?: unknown; mode?: string };

function statusMs(status: ClockStatus): number {
  if (status.elapsed !== undefined && status.elapsed !== null) return Number(status.elapsed) / MS;
  return Number((status.audio as { frames?: number } | null | undefined)?.frames ?? 0) / FRAMES_PER_MS;
}

async function readClock(session: Pick<Session, 'status'>): Promise<{ ms: number; status: ClockStatus }> {
  const status = (await session.status()) as unknown as ClockStatus;
  return { ms: statusMs(status), status };
}

/**
 * Free run: `now` is the emulated ms of `status()`. `pause(ms)` waits `ms` of real time, then reads the status; it throws when neither
 * the clock nor `restartProgress` moved for `hangMs` of wall time. A clock that drops (reboot, restore) counts as movement.
 */
export function freeClock(
  session: Pick<Session, 'status'>,
  { hangMs = Number(process.env.DIGI_CLOCK_STUCK_MS ?? 60_000) }: { hangMs?: number } = {},
): Clock {
  let last: number | undefined;
  let lastProgress: string | undefined;
  let lastAdvance = Date.now();
  return {
    async now() {
      return (await readClock(session)).ms;
    },
    async pause(ms) {
      await delay(ms);
      const { ms: now, status } = await readClock(session);
      const progress = restartProgress(status);
      const moved = last === undefined || now !== last || (lastProgress !== undefined && progress !== lastProgress);
      last = now;
      lastProgress = progress;
      if (moved) {
        lastAdvance = Date.now();
        return;
      }
      if (Date.now() - lastAdvance >= hangMs) {
        throw new Error(
          `emulated clock stuck at ${now} ms (mode ${status.mode ?? 'unknown'})${describeRestart(status)} for ${hangMs} ms`,
        );
      }
    },
  };
}

/** Stepped (paused daemon): `pause(ms)` steps floor(ms * MS) instructions in slices, each bounded by PROGRESS_HANG_S of wall time. */
export function steppedClock(session: Pick<Session, 'status' | 'client'>): Clock {
  return {
    async now() {
      return (await readClock(session)).ms;
    },
    async pause(ms) {
      for (let left = Math.floor(ms * MS); left > 0;) {
        const n = Math.min(left, SLICE_INSNS);
        await withinRuntimeDeadline(Date.now() + PROGRESS_HANG_S * 1000, () =>
          session.client.step(n, { timeout_wall_s: PROGRESS_HANG_S }),
        );
        left -= n;
      }
    },
  };
}

/**
 * Poll `read` until `done(value)`; fail after `withinMs` of clock time. `retry`: a throwing read counts as a miss.
 * `message` may be a function, resolved only on failure. The first read happens at once; the clock is read only after a miss.
 */
export async function pollUntil<T>(
  clock: Clock,
  read: () => Promise<T>,
  done: (v: T) => boolean,
  opts: {
    withinMs: number;
    message: string | (() => string);
    intervalMs?: number;
    retry?: boolean;
    onWait?: (r: { kind: 'state'; what: string; clockMs: number; wallMs: number; ok: boolean }) => void;
  },
): Promise<T> {
  const { withinMs, message, intervalMs = 200, retry = false, onWait } = opts;
  const wallStart = Date.now();
  let start: number | undefined;
  let clockMs = 0;
  let value: T | undefined;
  let failure: unknown;
  const what = typeof message === 'string' ? message : 'poll';
  for (;;) {
    failure = undefined;
    try {
      value = await read();
      if (done(value)) {
        if (start !== undefined)
          onWait?.({ kind: 'state', what, clockMs: clockMs - start, wallMs: Date.now() - wallStart, ok: true });
        return value;
      }
    } catch (error) {
      if (!retry) throw error;
      failure = error;
    }
    clockMs = await clock.now();
    // A clock that drops (reboot, restore) starts the count again.
    if (start === undefined || clockMs < start) start = clockMs;
    onWait?.({ kind: 'state', what, clockMs: clockMs - start, wallMs: Date.now() - wallStart, ok: false });
    if (clockMs - start >= withinMs) {
      throw new Error(
        `${typeof message === 'function' ? message() : message}: not seen within ${withinMs} ms of emulated time (last: ${failure ? String(failure) : JSON.stringify(value)})`,
      );
    }
    await clock.pause(intervalMs);
  }
}

/** Let `ms` of clock time pass (an explicit fixed wait): pause in steps of at most 50 ms until now - start >= ms. */
export async function waitClock(clock: Clock, ms: number): Promise<void> {
  const start = await clock.now();
  for (let now = start; now - start < ms; now = await clock.now()) {
    await clock.pause(Math.min(50, ms - (now - start)));
  }
}

/**
 * The emulator clock for both run modes: each `pause` reads `status.mode` and steps a paused daemon (`steppedClock`) or waits on a
 * running one (`freeClock`). A fixture may switch modes inside a case (free while the device boots, paused for protocol reads).
 */
export function emulatorClock(session: Pick<Session, 'status' | 'client'>, opts: { hangMs?: number } = {}): Clock {
  const free = freeClock(session, opts);
  const stepped = steppedClock(session);
  return {
    now: () => free.now(),
    pause: async (ms) => {
      const status = (await session.status()) as { mode?: string };
      return status.mode === 'paused' ? stepped.pause(ms) : free.pause(ms);
    },
  };
}
