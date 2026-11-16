import type { Ui } from '../ui.ts';

/**
 * Panel keys for the page-navigation flows of the stock UI, over `rt.ui`: the input waits until the firmware took it (`drained`), then a
 * fixed wait stays, since these pages have no state row or event for "the page is drawn". A flow that can observe the page waits on that
 * state with `rt.ui.tap(key, { until: { state, done } })` instead.
 */
export function panelKeys({ ui }: { ui: Ui }) {
  const settle = (ms: number, why = 'the page settles after the input (nothing observable)') => ui.wait({ ms, why });
  async function tap(name: string | number, settleMs = 80): Promise<void> {
    await ui.tap(name, { until: { drained: true } });
    await settle(settleMs);
  }
  async function chord(held: string, key: string | number): Promise<void> {
    await ui.tap(key, { until: { drained: true }, held });
    await settle(300);
  }
  async function turn(name: string, detents: number): Promise<void> {
    await ui.turn(name, detents, { until: { drained: true } });
    await settle(200);
  }
  return { tap, chord, turn, settle };
}
