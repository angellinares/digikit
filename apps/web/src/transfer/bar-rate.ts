// The speed of the progress bar as reactive code (tests: apps/desktop/ui/tests/bar-rate.test.ts, which runs Solid). Call it in a component.
import { createEffect, createSignal, on, onCleanup, type Accessor } from 'solid-js';
import { createRate } from './rate-core';

/**
 * The bar's speed. Every change of `getSent` is a sample; a ticker lets the number fall when the reports stop. The samples start over when
 * `getRunning` turns from false to true. `on` re-runs its callback whenever a signal read in its getter notifies, and the page hands the bar
 * a new object on every progress event, so `running` notifies with the same value: only the previous value tells a real change.
 * `clock` and `tickMs` are for tests.
 */
export function createBarRate(
  getSent: () => number,
  getRunning: () => boolean,
  onSpeed?: (speed: number | undefined) => void,
  clock: () => number = Date.now,
  tickMs = 250,
): Accessor<number | undefined> {
  const rate = createRate();
  const [now, setNow] = createSignal(clock());
  const [speed, setSpeed] = createSignal<number>();
  // one effect for both: separate effects run in no fixed order, and a reset after the first sample of a new run would drop it
  createEffect(
    on([getRunning, getSent] as const, ([running, sent], prev) => {
      if (running && !prev?.[0]) rate.reset();
      const t = clock();
      rate.add(t, sent);
      setNow(t);
    }),
  );
  const timer = setInterval(() => setNow(clock()), tickMs);
  onCleanup(() => clearInterval(timer));
  createEffect(() => {
    const r = getRunning() ? rate.rate(now()) : undefined;
    setSpeed(r);
    onSpeed?.(r);
  });
  return speed;
}
