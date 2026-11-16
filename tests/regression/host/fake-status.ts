import { vi } from 'vitest';

/** Instructions per emulated millisecond (the wait API reads `status().elapsed` in these). */
const INSNS_PER_MS = 132_000;

/**
 * The status side of a fake session for the wait API: a paused daemon whose emulated clock advances on `client.step` and by `msPerRead`
 * on every status read, and whose panel inputs are drained. `step` is the fake's own `client.step`, so a test may count it.
 */
export function fakeClock({ msPerRead = 0, mode = 'paused' }: { msPerRead?: number; mode?: string } = {}) {
  let elapsed = 0;
  const step = vi.fn(async (n: number) => {
    elapsed += n;
  });
  const status = vi.fn(async () => {
    elapsed += msPerRead * INSNS_PER_MS;
    return { mode, elapsed, cf: { input_pending: 0 }, scheduled_inputs: 0 };
  });
  return { status, step };
}
