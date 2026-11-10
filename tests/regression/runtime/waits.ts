import { setTimeout as delay } from 'node:timers/promises';
import {
  describeRestart,
  restartProgress,
  withinRuntimeDeadline as beforeReadinessDeadline,
  type Session,
} from '@digi/test-support';
import { timePhase } from '../timing.ts';

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
 * Wait until `frames` more frames of emulated time have passed (1500 a second); the clock is `emulatedFrames`.
 * Wall time only guards against a stuck daemon (paused or dead: neither the DSP nor the ColdFire advances).
 */
export async function waitEmulatedFrames(session: Session, frames: number, { hangMs = 30_000 } = {}): Promise<void> {
  let progress: string | undefined;
  let restarting = false;
  const read = async () => {
    const status = await session.status();
    const key = restartProgress(status);
    restarting = progress !== undefined && key !== progress;
    progress = key;
    return emulatedFrames(status);
  };
  const start = await read();
  const target = start + frames;
  let seen = start;
  let lastAdvance = Date.now();
  while (seen < target) {
    await delay(50);
    const now = await read();
    if (now > seen || restarting) lastAdvance = Date.now();
    else if (Date.now() - lastAdvance >= hangMs) {
      throw new Error(`emulated clock stuck at ${now} frames (started at ${start}, wanted ${target}) for ${hangMs} ms`);
    }
    seen = now;
  }
}

/** Wall milliseconds the emulated clock may stand still before a wait fails (a paused daemon, a halted DSP); a slow but rising clock never trips it. DIGI_CLOCK_STUCK_MS overrides it. */
export const CLOCK_STUCK_MS = Number(process.env.DIGI_CLOCK_STUCK_MS ?? 60_000);

/** Emulated milliseconds of a status reply: `emulatedFrames` at 1500 frames a second. */
export function emulatedClockMs(status: { audio?: unknown; elapsed?: unknown }): number {
  return emulatedFrames(status) / FRAMES_PER_MS;
}

/**
 * A watch on the emulated clock for a loop that waits on something else: each `tick()` reads the status and returns true once
 * `emulatedMs` of emulated time have passed since the first tick; it throws `message` when the clock stands still (and no load or restart moves) for `hangMs` of wall time.
 * The clock only counts forward: a reboot or snapshot restore lowers it and the next reading is the new start.
 */
export function emulatedWatch(
  session: Pick<Session, 'status'>,
  { emulatedMs, message, hangMs = CLOCK_STUCK_MS }: { emulatedMs: number; message: string; hangMs?: number },
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
        `${message}: emulated clock stuck at ${now} ms (mode ${mode ?? 'unknown'})${describeRestart(reply)} for ${hangMs} ms${detail()}`,
      );
    }
    last = now;
    return waited >= emulatedMs;
  };
}

/**
 * Poll `read` until `done(value)` (`retry` also retries a failing `read`); fail with `message` and the last value after `emulatedMs` of emulated time.
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
  }: { emulatedMs: number; message: string; intervalMs?: number; retry?: boolean; hangMs?: number },
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
        `${message}: not seen within ${emulatedMs} ms of emulated time (last: ${failure ? String(failure) : JSON.stringify(value)})`,
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

const bootClocks = new WeakMap<object, Pick<Session, 'status'>>();

/** Tie a DkSession to the emulator session it talks to, so `waitForBootEmulated` can count emulated time. */
export function bindBootClock(dk: object, session: Pick<Session, 'status'>): void {
  bootClocks.set(dk, session);
}

/**
 * `dk.waitForBoot()` with the emulated clock instead of its 90 s wall limit: a slow host only slows the boot, a stuck daemon trips the guard.
 * A dk that was never bound to a session keeps the wall-time wait.
 */
export function waitForBootEmulated(
  dk: { waitForBoot(sleep?: (ms: number) => Promise<void>, timeoutMs?: number): Promise<void> },
  { emulatedMs = 90_000 }: { emulatedMs?: number } = {},
): Promise<void> {
  const session = bootClocks.get(dk);
  if (!session) return dk.waitForBoot();
  const tick = emulatedWatch(session, { emulatedMs, message: 'no new boot' });
  return dk.waitForBoot(async (ms) => {
    await delay(ms);
    if (await tick()) throw new Error(`no new boot: not seen within ${emulatedMs} ms of emulated time`);
  }, Number.POSITIVE_INFINITY);
}

/** Wait for `ms` of emulated time (the daemon must run freely). */
export function waitEmulated(session: Session, ms: number, opts: { hangMs?: number } = {}): Promise<void> {
  return waitEmulatedFrames(session, Math.ceil(ms * FRAMES_PER_MS), opts);
}

/** Shared lifecycle deadline; preserve the existing readiness-helper export for its callers. */
export { beforeReadinessDeadline };

/** Press, hold, release (also when the press fails), then let the UI settle. */
export async function tap(
  session: Session,
  send: (down: boolean) => Promise<unknown>,
  holdMs: number,
  settleMs = 1200,
): Promise<void> {
  try {
    await send(true);
    await waitEmulated(session, holdMs);
  } finally {
    await send(false);
  }
  await timePhase('settle.input', () => waitEmulated(session, settleMs));
}
