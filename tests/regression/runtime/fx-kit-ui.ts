import { button } from '@digi/test-support';
import { REDRAW, SETTLE, bounded, rows, word, type Rt } from './fx-live.ts';
import { openPages } from './fx-pages-ui.ts';
import { shot, type FxProfile } from './fx-session.ts';
import { halfword, kitMenu, kitSlotOne } from './fx-track-ui.ts';
import { type RuntimePlan } from './runtime-plan.ts';
import { hasServiceSymbols, symbolsOf } from './service-symbols.ts';

export type FxKitObservation = [ms: number, row: number, p0: number, mix: number];

/** Legacy held-key load observation: sample every step, record each DSP row change. */
export async function watchFxKitRows(
  runtime: Rt,
  plan: RuntimePlan,
  profile: FxProfile,
  key: string,
  totalMs: number,
  step: number,
  each?: () => Promise<void>,
): Promise<FxKitObservation[]> {
  const { session } = runtime;
  const count = profile.fx.value_indices.length / 8;
  const read = async () => (await rows(session, plan, count)).map((row) => [row[4]!, row[13]!] as const);
  let last = await read();
  const seen: FxKitObservation[] = [];
  try {
    await bounded(() => session.client.inputButton(button(key), true));
    for (let ms = 0; ms < totalMs; ms += step) {
      await runtime.ui.wait({ ms: step, why: SETTLE });
      await each?.();
      const now = await read();
      for (let row = 0; row < count; row++) {
        if (now[row]![0] !== last[row]![0] || now[row]![1] !== last[row]![1])
          seen.push([ms, row, now[row]![0], now[row]![1]]);
      }
      last = now;
      if (ms === 60) await bounded(() => session.client.inputButton(button(key), false));
    }
  } finally {
    await bounded(() => session.client.inputButton(button(key), false));
  }
  return seen;
}

/** Shared firmware KIT menus, preserving the persistence runner's cursor resets and settling. */
export function createFxKitUi(runtime: Rt, plan: RuntimePlan, profile: FxProfile, artifacts: string) {
  const { session } = runtime;
  // `fxp_nload` and the other hook cells are the fx service file's (`symbolsOf`, bound after each boot by `inspectFxBoot`)
  const cell = (name: string) => word(session, symbolsOf(runtime, plan)(name));
  const slot = async (n: number) => {
    await kitSlotOne(runtime);
    for (let i = 1; i < n; i++) await runtime.ui.tap('DOWN', { until: { ms: 80, why: REDRAW } });
  };
  return {
    async fxPage(page: number) {
      await openPages(runtime);
      for (let i = 0; i < 5; i++) await runtime.ui.tap('UP', { until: { ms: 80, why: REDRAW } });
      for (let i = 0; i < page; i++) await runtime.ui.tap('DOWN', { until: { ms: 300, why: REDRAW } });
    },
    async saveKit(n: number, tag: string) {
      await kitMenu(runtime, 1);
      await runtime.ui.tap('YES', { until: { ms: 600, why: REDRAW } });
      await slot(n);
      await shot(session, artifacts, `${tag}-save-slot`);
      await runtime.ui.tap('YES', { until: { ms: 600, why: REDRAW } });
      await runtime.ui.tap('YES', { until: { ms: 1500, why: REDRAW } });
      await shot(session, artifacts, `${tag}-saved`);
      await runtime.ui.wait({ ms: 500, why: SETTLE });
    },
    async loadKit(n: number, tag: string) {
      await kitMenu(runtime, 0);
      await runtime.ui.tap('YES', { until: { ms: 700, why: REDRAW } });
      await slot(n);
      await shot(session, artifacts, `${tag}-load-slot`);
      const file: number[] = [];
      // the load cells are the fx service file's: a boot without it has none, so the file stays empty
      const hooked = hasServiceSymbols(runtime);
      const before = hooked ? await cell('fxp_nload') : 0;
      const seen = await watchFxKitRows(runtime, plan, profile, 'YES', 600, 2, async () => {
        if (hooked && !file.length && (await cell('fxp_nload')) !== before) {
          const base = (await cell('fxp_load_src')) + profile.fx.store_off;
          for (let i = 0; i < profile.fx.store_halfwords; i++) file.push(await halfword(session, base + 2 * i));
        }
      });
      await shot(session, artifacts, `${tag}-loaded`);
      return { seen, file };
    },
  };
}
