import { fmtVersion } from '@digi/dk-session';
import { pollUntil } from '@digi/test-support';
import { expect } from 'vitest';
import type { TestRuntime } from '../../support.ts';
import { record } from '../artifacts.ts';
import { planSymbol, type RuntimePlan } from '../runtime-plan.ts';
import { aboutIsComplete, readAboutRows } from '../runtime-menu.ts';

/** The panel flows of the DIGIKIT menu. Each takes `rt` and uses only `rt.ui` and state reads (docs/plans/test-architecture.md, T2 step 3). */

type Rt = Pick<TestRuntime, 'session' | 'dk' | 'ui' | 'clock'>;

/**
 * `holdMs`: how long a key is held (default 20 ms). `gapMs`: an extra fixed wait after every key, for an input path where `drained` says nothing
 * (remote and probe keys do not pass the daemon's input queue).
 */
export interface MenuOpts {
  holdMs?: number;
  gapMs?: number;
}

const key = async (rt: Rt, k: string, o: MenuOpts): Promise<void> => {
  await rt.ui.tap(k, { until: { drained: true }, holdMs: o.holdMs });
  if (o.gapMs) await rt.ui.wait({ ms: o.gapMs, why: 'this input path has no drained state' });
};

/** A key whose effect has no observable state: the firmware took the input, then the page change settles. */
const settle = async (rt: Rt, k: string, ms: number, why: string, o: MenuOpts): Promise<void> => {
  await key(rt, k, o);
  await rt.ui.wait({ ms, why });
};

/**
 * SETTINGS > DIGIKIT, from any state. `checkpoint` runs after the eighth DOWN (the ninth row) and after the menu opened.
 * The page changes of NO, SETTINGS and YES have no state row, so they keep a fixed wait; the DOWN moves end when the firmware took the input.
 */
export async function openDigikitMenu(
  rt: Rt,
  o: MenuOpts & { checkpoint?: (name: string) => Promise<void> } = {},
): Promise<void> {
  const { checkpoint } = o;
  await settle(rt, 'NO', 2000, 'the menu page change has no state row', o);
  await settle(rt, 'SETTINGS', 3000, 'the SETTINGS page has no state row', o);
  for (let i = 0; i < 8; i++) await key(rt, 'DOWN', o);
  if (checkpoint) {
    await rt.ui.wait({ ms: 300, why: 'the cursor redraw before a frame checkpoint' });
    await checkpoint('settings-ninth-row');
  }
  await settle(rt, 'YES', 3000, 'the DIGIKIT page has no state row', o);
  await checkpoint?.('digikit-main');
}

/** The ABOUT rows once the page is complete (`fill_about` ends on RAM KEPT); [] while the buffer is empty or half filled. */
async function aboutNow(rt: Rt, plan: RuntimePlan): Promise<string[]> {
  const count = await rt.session.client.memPeek(planSymbol(plan, 'DK_ABOUT_N'));
  if (count === null || count < 1 || count > 24) return [];
  return readAboutRows(rt.session, plan);
}

/** YES on the first row (ABOUT), ending on the complete ABOUT rows. */
async function pressAbout(rt: Rt, plan: RuntimePlan, o: MenuOpts): Promise<string[]> {
  await key(rt, 'YES', o);
  return pollUntil(rt.clock, () => aboutNow(rt, plan), aboutIsComplete, {
    withinMs: 10_000,
    message: 'ABOUT page is complete',
    intervalMs: 50,
  });
}

/** The ABOUT page of the DIGIKIT menu, opened through the panel (a session that runs freely or one that steps). */
export async function openAbout(rt: Rt, plan: RuntimePlan, o: MenuOpts = {}): Promise<string[]> {
  await openDigikitMenu(rt, o);
  return pressAbout(rt, plan, o);
}

/** ABOUT must describe the live runtime: the first open starts from an empty buffer and ends on the runtime row, BOOT and RAM KEPT. */
export async function openRuntimeAbout(
  rt: Rt,
  plan: RuntimePlan,
  artifacts: string,
  o: MenuOpts = {},
): Promise<string[]> {
  let initialCount: number | null | undefined;
  let rows: string[] = [];
  let runtimeRow: string | undefined;
  let failed = true;
  try {
    initialCount = await rt.session.client.memPeek(planSymbol(plan, 'DK_ABOUT_N'));
    expect(initialCount, 'ABOUT buffer is empty before its first open').toBe(0);
    runtimeRow = `RUNTIME ${fmtVersion((await rt.dk.info()).runtime)}`;
    rows = await pressAbout(rt, plan, o);
    expect(rows, 'ABOUT describes the live runtime').toContain(runtimeRow);
    failed = false;
    return rows;
  } finally {
    record(artifacts, 'about-before-open', {
      initialCount: initialCount ?? null,
      runtimeRow: runtimeRow ?? null,
      rows,
      expired: failed,
    });
  }
}

/**
 * ABOUT in safe mode: opening DIGIKIT shows a dialog that names the reason (NO closes it), and the first row is the SAFE notice, so ABOUT is the second.
 * The cursor may rest below the first row after the dialog: go to the top, then to ABOUT.
 */
export async function openSafeAbout(rt: Rt, plan: RuntimePlan, o: MenuOpts = {}): Promise<string[]> {
  await openDigikitMenu(rt, o);
  await settle(rt, 'NO', 1000, 'closing the safe-mode dialog has no state row', o);
  for (let i = 0; i < 3; i++) await key(rt, 'UP', o);
  await key(rt, 'DOWN', o);
  return pressAbout(rt, plan, o);
}
