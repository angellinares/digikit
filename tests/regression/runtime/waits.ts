import { setTimeout as delay } from 'node:timers/promises';
import { withinRuntimeDeadline as beforeReadinessDeadline, type Session } from '@digi/test-support';
import { expect } from 'vitest';
import { timePhase, timingContext } from '../timing.ts';

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
  const read = async () => {
    const status = await session.status();
    return Number(coupled ? ((status.audio as { frames?: number } | null)?.frames ?? 0) : (status.elapsed ?? 0));
  };
  const start = await read();
  const target = start + frames * unit;
  let seen = start;
  let lastAdvance = Date.now();
  while (seen < target) {
    await delay(50);
    const now = await read();
    if (now > seen) lastAdvance = Date.now();
    else if (Date.now() - lastAdvance >= hangMs) {
      throw new Error(`${clock} stuck at ${now} (started at ${start}, wanted ${target}) for ${hangMs} ms`);
    }
    seen = now;
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

/** The DN2 firmware needs a longer pre-install settle than the DT2. Emulated time, like `waitEmulated`. */
export async function settleUi(session: Session): Promise<void> {
  const ms = session.options.device === 'dn2' ? 45_000 : 12_000;
  await timePhase('settle.live', () => waitEmulated(session, ms), {
    ...timingContext(session),
    detail: `settle ${ms} ms`,
  });
  expect((await session.status()).mode, 'the daemon runs freely').toBe('free');
}
