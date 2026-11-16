import { describeRestart, restartProgress, type ProgressStatus } from './progress.ts';
import { MS, PROGRESS_HANG_S, withinRuntimeDeadline, type Session } from './session.ts';

/** Same shape as `Clock` in @digi/dk-session (structural; no import). Time in ms. */
export interface Clock {
  now(): Promise<number>;
  pause(ms: number): Promise<void>;
  /** What the emulator reports (mode, CPU state, phase, error, PC): added to the message of a stuck clock. */
  describe?(): Promise<string>;
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
      // Round up: a positive wait steps at least one instruction (a float remainder like 1e-12 ms must not step none).
      for (let left = Math.ceil(ms * MS); left > 0;) {
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
    /** Wall time the clock may stand still before the wait fails (default `DIGI_CLOCK_STUCK_MS`, 60 s). */
    hangMs?: number;
  },
): Promise<T> {
  const { withinMs, message, intervalMs = 200, retry = false, onWait, hangMs = stuckMs() } = opts;
  const stuck = stuckGuard(typeof message === 'string' ? message : 'poll', hangMs, clock);
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
    await stuck(clockMs);
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

/** Wall time a clock may stand still while a wait pauses it (a halted guest, a daemon that does not step) before the wait fails. */
const stuckMs = () => Number(process.env.DIGI_CLOCK_STUCK_MS ?? 60_000);

/** Throws when the clock `now` did not move for `hangMs` of wall time: a wait on a clock that never moves would end only at the test timeout. */
function stuckGuard(what: string, hangMs: number, clock: Clock) {
  let last: number | undefined;
  let lastAdvance = Date.now();
  return async (now: number) => {
    if (last === undefined || now !== last) lastAdvance = Date.now();
    last = now;
    if (Date.now() - lastAdvance >= hangMs) {
      const detail = await clock.describe?.().catch((e: unknown) => `status failed: ${String(e)}`);
      throw new Error(
        `${what}: emulated clock stuck at ${now} ms for ${hangMs} ms (a halted or stopped guest?)${detail ? ` ${detail}` : ''}`,
      );
    }
  };
}

/** Let `ms` of clock time pass (an explicit fixed wait): pause in steps of at most 50 ms until now - start >= ms. Fails when the clock stands still for `hangMs` of wall time. */
export async function waitClock(
  clock: Clock,
  ms: number,
  { hangMs = stuckMs() }: { hangMs?: number } = {},
): Promise<void> {
  const start = await clock.now();
  const stuck = stuckGuard(`wait of ${ms} ms`, hangMs, clock);
  for (let now = start; now - start < ms;) {
    await clock.pause(Math.min(50, ms - (now - start)));
    now = await clock.now();
    await stuck(now);
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
    describe: async () => {
      const st = (await session.status()) as { mode?: string; epoch?: number; cf?: Record<string, unknown> };
      const cf = st.cf ?? {};
      const hex = (v: unknown) => (typeof v === 'number' ? `0x${v.toString(16)}` : String(v));
      return `[mode ${st.mode}, epoch ${st.epoch}, cpu_state ${cf.cpu_state}, phase ${cf.phase}, error ${JSON.stringify(cf.error)}, pc ${hex(cf.pc)}, icount ${cf.icount}, guard_hits ${cf.guard_hits}]`;
    },
  };
}
