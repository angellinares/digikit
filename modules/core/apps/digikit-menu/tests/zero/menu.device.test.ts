import { expect } from 'vitest';
import { liveArtifacts } from '@digi/regression/runtime/artifacts.ts';
import { word } from '@digi/regression/runtime/fx-live.ts';
import { readTextRows } from '@digi/regression/runtime/fx-session.ts';
import { expectRuntimeUi, runtimeTests } from '@digi/regression/runtime/ready.ts';
import { openDigikitMenu, openRuntimeAbout } from '@digi/regression/runtime/flows/menu.ts';
import { planSymbol, prepareRuntimePlan } from '@digi/regression/runtime/runtime-plan.ts';

runtimeTests(
  'empty-store DIGIKIT menu without DSP',
  async (rt) => {
    const { session } = rt;
    const status = await session.status();
    expect(status.dsp).toBe('zero');
    expect(status.audio).toBeNull();
    const artifacts = liveArtifacts(session, 'menu-zero');
    const plan = await prepareRuntimePlan(session, artifacts, [
      'DKRT_BOOT',
      'DK_LOAD',
      'DK_LOAD_N',
      'DK_ABOUT',
      'DK_ABOUT_N',
      'DK_MEM',
      'DK_MEM_N',
    ]);
    await expectRuntimeUi(session, plan, artifacts, { fresh: true });
    const { ui } = rt;
    const drained = { drained: true } as const;
    const press = (key: string) => ui.tap(key, { until: drained });
    const peek = (name: string) => () => word(session, planSymbol(plan, name));

    await openDigikitMenu(rt);
    await press('DOWN');
    await press('DOWN');
    await press('DOWN');
    await ui.tap('YES', { until: { state: peek('DK_LOAD_N'), done: (n) => n === 1, what: 'MODULES page rows' } });
    expect(await word(session, planSymbol(plan, 'DK_LOAD_N'))).toBe(1);
    expect(await readTextRows(session, planSymbol(plan, 'DK_LOAD'), 1, 24)).toEqual(['NO STORE']);

    await press('NO');
    await press('UP');
    await press('UP');
    await press('UP');
    const about = await openRuntimeAbout(rt, plan, artifacts);
    expect(about).toContain('BOOT: 0 OF 0 OK');
    await press('DOWN');
    await press('UP');
    await session.expectNoFault('ABOUT text-page navigation without DSP');

    await press('NO');
    await press('DOWN');
    await press('DOWN');
    await ui.tap('YES', { until: { state: peek('DK_MEM_N'), done: (n) => n > 0, what: 'MEMORY page rows' } });
    await ui.wait({ ms: 200, why: 'the count shows before the last MEMORY row is filled' });
    const count = await word(session, planSymbol(plan, 'DK_MEM_N'));
    expect(count).toBeGreaterThan(0);
    expect(count).toBeLessThanOrEqual(8);
    const memory = await readTextRows(session, planSymbol(plan, 'DK_MEM'), count, 24);
    expect(memory).toContain('MODS  .......... 0/8');
    await press('DOWN');
    await press('UP');
    expect(await readTextRows(session, planSymbol(plan, 'DK_MEM'), count, 24)).toEqual(memory);
    await press('NO');
    await press('NO');
    await press('NO');
    await session.expectNoFault('DIGIKIT menu navigation without DSP');
  },
  { dsp: 'zero' },
);
