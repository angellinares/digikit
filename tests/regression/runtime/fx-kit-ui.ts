import { button } from '@digi/test-support';
import type { PatchedSession } from '../support.ts';
import { advance, bounded, rows, word } from './fx-live.ts';
import { openPages } from './fx-pages-ui.ts';
import { shot, tap, type FxProfile } from './fx-session.ts';
import { halfword, kitMenu, kitSlotOne } from './fx-track-ui.ts';
import { planSymbol, type RuntimePlan } from './runtime-plan.ts';

export type FxKitObservation = [ms: number, row: number, p0: number, mix: number];

/** Legacy held-key load observation: sample every step, record each DSP row change. */
export async function watchFxKitRows(
  runtime: PatchedSession,
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
      await advance(session, step);
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
export function createFxKitUi(runtime: PatchedSession, plan: RuntimePlan, profile: FxProfile, artifacts: string) {
  const { session } = runtime;
  const cell = (name: string) => word(session, planSymbol(plan, name));
  const slot = async (n: number) => {
    await kitSlotOne(session);
    for (let i = 1; i < n; i++) await tap(session, 'DOWN', 80);
  };
  return {
    async fxPage(page: number) {
      await openPages(session);
      for (let i = 0; i < 5; i++) await tap(session, 'UP', 80);
      for (let i = 0; i < page; i++) await tap(session, 'DOWN', 300);
    },
    async saveKit(n: number, tag: string) {
      await kitMenu(session, 1);
      await tap(session, 'YES', 600);
      await slot(n);
      await shot(session, artifacts, `${tag}-save-slot`);
      await tap(session, 'YES', 600);
      await tap(session, 'YES', 1500);
      await shot(session, artifacts, `${tag}-saved`);
      await advance(session, 500);
    },
    async loadKit(n: number, tag: string) {
      await kitMenu(session, 0);
      await tap(session, 'YES', 700);
      await slot(n);
      await shot(session, artifacts, `${tag}-load-slot`);
      const file: number[] = [];
      const before = await cell('fxp_nload');
      const seen = await watchFxKitRows(runtime, plan, profile, 'YES', 600, 2, async () => {
        if (!file.length && (await cell('fxp_nload')) !== before) {
          const base = (await cell('fxp_load_src')) + profile.fx.store_off;
          for (let i = 0; i < profile.fx.store_halfwords; i++) file.push(await halfword(session, base + 2 * i));
        }
      });
      await shot(session, artifacts, `${tag}-loaded`);
      return { seen, file };
    },
  };
}
