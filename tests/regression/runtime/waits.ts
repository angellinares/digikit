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
 * Wait until `frames` more frames of emulated time have passed (1500 a second). With a DSP the clock is its frame counter.
 * Without one (`zero`, `cold`) it is `status.elapsed`, the ColdFire instruction count, at 88,000 instructions a frame.
 * Wall time only guards against a stuck daemon.
 */
export async function waitEmulatedFrames(session: Session, frames: number, { hangMs = 30_000 } = {}): Promise<void> {
  const coupled = session.dsp === 'sharc';
  const unit = coupled ? 1 : INSNS_PER_FRAME;
  const clock = coupled ? 'DSP frames' : 'ColdFire instructions';
  let progress: string | undefined;
  let restarting = false;
  const read = async () => {
    const status = await session.status();
    const key = restartProgress(status);
    restarting = progress !== undefined && key !== progress;
    progress = key;
    return Number(coupled ? ((status.audio as { frames?: number } | null)?.frames ?? 0) : (status.elapsed ?? 0));
  };
  const start = await read();
  const target = start + frames * unit;
  let seen = start;
  let lastAdvance = Date.now();
  while (seen < target) {
    await delay(50);
    const now = await read();
    if (now > seen || restarting) lastAdvance = Date.now();
    else if (Date.now() - lastAdvance >= hangMs) {
      throw new Error(`${clock} stuck at ${now} (started at ${start}, wanted ${target}) for ${hangMs} ms`);
    }
    seen = now;
  }
}

/** Wall milliseconds the emulated clock may stand still before a wait fails (a paused daemon, a halted DSP); a slow but rising clock never trips it. DIGI_CLOCK_STUCK_MS overrides it. */
export const CLOCK_STUCK_MS = Number(process.env.DIGI_CLOCK_STUCK_MS ?? 60_000);

/** Emulated milliseconds of a status reply: the DSP frame counter with a DSP, else the ColdFire instruction count. */
export function emulatedClockMs(status: { audio?: unknown; elapsed?: unknown }): number {
  const frames = (status.audio as { frames?: number } | null | undefined)?.frames;
  return frames !== undefined
    ? frames / FRAMES_PER_MS
    : Number(status.elapsed ?? 0) / (INSNS_PER_FRAME * FRAMES_PER_MS);
}

/**
 * Poll `read` until `done(value)` (`retry` also retries a failing `read`); fail with `message` and the last value after `emulatedMs` of emulated time.
 * The clock only counts forward: a reboot or snapshot restore lowers it and the next reading is the new start.
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
    hangMs: hang,
  }: { emulatedMs: number; message: string; intervalMs?: number; retry?: boolean; hangMs?: number },
): Promise<T> {
  let waited = 0;
  let last: number | undefined;
  const hangMs = hang ?? CLOCK_STUCK_MS;
  let lastAdvance = Date.now();
  let lastProgress: string | undefined;
  for (;;) {
    let value: T | undefined;
    let failure: unknown;
    try {
      value = await read();
      if (done(value)) return value as T;
    } catch (error) {
      // `retry`: a read that fails while the device restarts is tried again (like `expect.poll`).
      if (!retry) throw error;
      failure = error;
    }
    if (waited >= emulatedMs) {
      throw new Error(
        `${message}: not seen within ${emulatedMs} ms of emulated time (last: ${failure ? String(failure) : JSON.stringify(value)})`,
      );
    }
    // The clock starts at the first miss: a wait that is satisfied at once asks the daemon nothing more.
    last ??= emulatedClockMs(await session.status());
    await delay(intervalMs);
    const reply = await session.status();
    const now = emulatedClockMs(reply);
    // A restart's cold boot moves the epoch and instruction counts while the clock stands still: that is movement too.
    const progress = restartProgress(reply);
    const restarting = lastProgress !== undefined && progress !== lastProgress;
    lastProgress = progress;
    if (restarting && now === last) lastAdvance = Date.now();
    if (now > last) {
      waited += now - last;
      lastAdvance = Date.now();
    } else if (now < last) {
      lastAdvance = Date.now(); // a restart lowers the clock: that is movement
    } else if (Date.now() - lastAdvance >= hangMs) {
      const mode = (reply as { mode?: string }).mode;
      throw new Error(
        `${message}: emulated clock stuck at ${now} ms (mode ${mode ?? 'unknown'})${describeRestart(reply)} for ${hangMs} ms`,
      );
    }
    last = now;
  }
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
