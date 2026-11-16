import { setTimeout as delay } from 'node:timers/promises';
import { DkSession, EmuUsbTransport, type DkSessionOptions } from '@digi/dk-session';
import {
  deadlineExpired,
  describeRestart,
  restartProgress,
  withinRuntimeDeadline as beforeReadinessDeadline,
  type Session,
} from '@digi/test-support';

/** The DSP frame counter advances 1500 frames per emulated second. */
const FRAMES_PER_MS = 1.5;

/** ColdFire instructions per DSP frame: 132 M instructions a second over 1500 frames a second. */
const INSNS_PER_FRAME = 88_000;

/**
 * Emulated DSP frames of a status reply: the ColdFire instruction count (`status.elapsed`, `Runtime::elapsed_instructions` in the daemon,
 * counted from the last reset or restore in every mode) at 88,000 instructions a frame. The ColdFire runs whenever the daemon runs, so
 * the clock still rises after a deliberate DSP stop, when the DSP frame counter stands still, and a wait ends by emulated time instead
 * of hanging. The coupled DSP is paced by the ColdFire instruction count, so while it runs the two clocks agree. A reply without
 * `elapsed` falls back to the DSP frame counter.
 */
export function emulatedFrames(status: { audio?: unknown; elapsed?: unknown }): number {
  if (status.elapsed !== undefined && status.elapsed !== null) return Number(status.elapsed) / INSNS_PER_FRAME;
  return Number((status.audio as { frames?: number } | null | undefined)?.frames ?? 0);
}

/**
 * Emulated milliseconds one probe of a device wait may take. A probe lost while the device restarts ends after this much emulated time, and a slow
 * host slows the emulated clock too, so a probe does not fail under load (a wall bound is too short on a loaded host and too long after a lost request).
 */
export const PROBE_EMULATED_MS = 2000;

/** Wall milliseconds after which a probe fails whatever the clock says: a guard for a dead link, never the limit of a healthy wait. */
const PROBE_WALL_GUARD_MS = 180_000;

/** How often the emulated clock is read while a probe waits. */
const PROBE_READ_MS = 100;

/**
 * Run one probe (`dk.info`, `dk.readLog`, ...) bounded in emulated time: `run` gets a `Quick` whose signal fires `emulatedMs` of emulated time after the
 * probe started (the clock only counts forward; a restart that lowers it starts the count again). The probe then fails like a lost request.
 */
export async function withEmulatedProbe<T>(
  session: Pick<Session, 'status'>,
  run: (quick: { signal: AbortSignal; ms: number }) => Promise<T>,
  emulatedMs = PROBE_EMULATED_MS,
): Promise<T> {
  const ctl = new AbortController();
  let live = true;
  const watch = (async () => {
    let last: number | undefined;
    let waited = 0;
    while (live) {
      await delay(PROBE_READ_MS);
      if (!live) break;
      const now = emulatedClockMs(await session.status());
      if (last !== undefined && now > last) waited += now - last;
      last = now;
      if (waited >= emulatedMs) return ctl.abort();
    }
  })().catch(() => {});
  try {
    return await run({ signal: ctl.signal, ms: PROBE_WALL_GUARD_MS });
  } finally {
    live = false;
    await watch;
  }
}

/** A DkSession over the emulator's USB whose probes are bounded in emulated time (`withEmulatedProbe`). */
export function emulatorDk(
  session: Pick<Session, 'client' | 'status'>,
  options: Omit<DkSessionOptions, 'emulatedProbe'> = {},
): DkSession {
  return new DkSession(new EmuUsbTransport((method, params) => session.client.call(method, params)), {
    ...options,
    emulatedProbe: (run, emulatedMs) => withEmulatedProbe(session, run, emulatedMs),
  });
}

/** Wall milliseconds the emulated clock may stand still before a wait fails (a paused daemon, a halted DSP); a slow but rising clock never trips it. DIGI_CLOCK_STUCK_MS overrides it. */
export const CLOCK_STUCK_MS = Number(process.env.DIGI_CLOCK_STUCK_MS ?? 60_000);

/** Emulated milliseconds of a status reply: `emulatedFrames` at 1500 frames a second. */
export function emulatedClockMs(status: { audio?: unknown; elapsed?: unknown }): number {
  return emulatedFrames(status) / FRAMES_PER_MS;
}

/**
 * A watch on the emulated clock for a loop that waits on something else: each `tick()` reads the status and returns true once
 * `emulatedMs` of emulated time have passed since the first tick; it throws `message` (a function is called on failure) when the clock stands still (and no load or restart moves) for `hangMs` of wall time.
 * The clock only counts forward: a reboot or snapshot restore lowers it and the next reading is the new start.
 */
export function emulatedWatch(
  session: Pick<Session, 'status'>,
  {
    emulatedMs,
    message,
    hangMs = CLOCK_STUCK_MS,
  }: { emulatedMs: number; message: string | (() => string); hangMs?: number },
  detail: () => string = () => '',
): () => Promise<boolean> {
  let waited = 0;
  let last: number | undefined;
  let lastAdvance = Date.now();
  let lastProgress: string | undefined;
  return async () => {
    const reply = await session.status();
    const now = emulatedClockMs(reply);
    // A restart's cold boot moves the epoch and instruction counts while the clock stands still: that is movement too.
    const progress = restartProgress(reply);
    const restarting = lastProgress !== undefined && progress !== lastProgress;
    lastProgress = progress;
    if (last === undefined) {
      last = now;
      lastAdvance = Date.now();
      return false;
    }
    if (restarting && now === last) lastAdvance = Date.now();
    if (now > last) {
      waited += now - last;
      lastAdvance = Date.now();
    } else if (now < last) {
      lastAdvance = Date.now(); // a restart lowers the clock: that is movement
    } else if (Date.now() - lastAdvance >= hangMs) {
      const mode = (reply as { mode?: string }).mode;
      throw new Error(
        `${typeof message === 'function' ? message() : message}: emulated clock stuck at ${now} ms (mode ${mode ?? 'unknown'})${describeRestart(reply)} for ${hangMs} ms${detail()}`,
      );
    }
    last = now;
    return waited >= emulatedMs;
  };
}

/**
 * Poll `read` until `done(value)` (`retry` also retries a failing `read`); fail with `message` (a function is called on failure) and the last value after `emulatedMs` of emulated time.
 * A stuck or dead daemon is not this helper's job: the no-progress watchdog closes the session and `read` rejects.
 */
export async function pollEmulated<T>(
  session: Pick<Session, 'status'>,
  read: () => Promise<T>,
  done: (value: T) => boolean,
  {
    emulatedMs,
    message,
    intervalMs = 200,
    retry = false,
    hangMs,
  }: { emulatedMs: number; message: string | (() => string); intervalMs?: number; retry?: boolean; hangMs?: number },
): Promise<T> {
  let value: T | undefined;
  let failure: unknown;
  const tick = emulatedWatch(
    session,
    { emulatedMs, message, hangMs },
    () => ` (last: ${failure ? String(failure) : JSON.stringify(value)})`,
  );
  let expired = false;
  let started = false;
  for (;;) {
    failure = undefined;
    try {
      value = await read();
      if (done(value)) return value as T;
    } catch (error) {
      // `retry`: a read that fails while the device restarts is tried again (like `expect.poll`).
      if (!retry) throw error;
      failure = error;
    }
    if (expired) {
      throw new Error(
        `${typeof message === 'function' ? message() : message}: not seen within ${emulatedMs} ms of emulated time (last: ${failure ? String(failure) : JSON.stringify(value)})`,
      );
    }
    // The clock starts at the first miss: a wait that is satisfied at once asks the daemon nothing more.
    if (!started) {
      await tick();
      started = true;
    }
    await delay(intervalMs);
    expired = await tick();
  }
}

/**
 * `expect.poll` in emulated time: retry `read` (a failing read counts as a miss) until `assertion(value)` stops throwing, and fail with `message`
 * and the last assertion error after `emulatedMs` of emulated time. Replaces `expect.poll(read, { timeout })`, whose wall limit a loaded host cuts.
 */
export async function expectPollEmulated<T>(
  session: Pick<Session, 'status'>,
  read: () => Promise<T>,
  assertion: (value: T) => void,
  {
    emulatedMs = 30_000,
    message = 'poll',
    intervalMs = 200,
  }: { emulatedMs?: number; message?: string | (() => string); intervalMs?: number } = {},
): Promise<T> {
  return pollEmulated(
    session,
    async () => {
      const value = await read();
      assertion(value);
      return value;
    },
    () => true,
    { emulatedMs, message, intervalMs, retry: true },
  );
}

const bootClocks = new WeakMap<object, Pick<Session, 'status'>>();

/** Tie a DkSession to the emulator session it talks to, so `waitForBootEmulated` can count emulated time. */
export function bindBootClock(dk: object, session: Pick<Session, 'status'>): void {
  bootClocks.set(dk, session);
}

/**
 * `dk.waitForBoot()` with the emulated clock instead of its 90 s wall limit: a slow host only slows the boot, a stuck daemon trips the guard.
 * A dk that was never bound to a session keeps the wall-time wait.
 * Each probe keeps the 1 s quick wait, whatever the host load: the loop retries a missed probe, and the emulated clock is the limit. A long probe
 * blocks on a request that the restart lost, and every such probe costs 30 s of wall time and as much emulated time of the budget, while the
 * runtime's boot-time module loads wait for the next probe (R2c's DSP loads then ran out of the 30 s that `installModules` allows).
 */
export function waitForBootEmulated(
  dk: {
    waitForBoot(
      sleep?: (ms: number) => Promise<void>,
      timeoutMs?: number,
      opts?: { probe?: <T>(run: (quick: { signal: AbortSignal; ms: number }) => Promise<T>) => Promise<T> },
    ): Promise<void>;
  },
  { emulatedMs = 90_000 }: { emulatedMs?: number } = {},
): Promise<void> {
  const session = bootClocks.get(dk);
  if (!session) return dk.waitForBoot();
  const tick = emulatedWatch(session, { emulatedMs, message: 'no new boot' });
  return dk.waitForBoot(
    async (ms) => {
      await delay(ms);
      if (await tick()) throw new Error(`no new boot: not seen within ${emulatedMs} ms of emulated time`);
    },
    Number.POSITIVE_INFINITY,
    { probe: (run) => withEmulatedProbe(session, run) },
  );
}

/** The emulator session `bindBootClock` tied to `dk`, if any. */
export function bootClockOf(dk: object): Pick<Session, 'status'> | undefined {
  return bootClocks.get(dk);
}

/** Shared lifecycle deadline; preserve the existing readiness-helper export for its callers. */
export { beforeReadinessDeadline, deadlineExpired };
