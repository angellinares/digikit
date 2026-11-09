import { expect } from 'vitest';
import { liveArtifacts } from '@digi/regression/runtime/artifacts.ts';
import { word } from '@digi/regression/runtime/fx-live.ts';
import { readTextRows, tap } from '@digi/regression/runtime/fx-session.ts';
import { expectRuntimeUi, runtimeTests } from '@digi/regression/runtime/ready.ts';
import { openDigikitMenu, openRuntimeAbout } from '@digi/regression/runtime/runtime-menu.ts';
import { planSymbol, prepareRuntimePlan } from '@digi/regression/runtime/runtime-plan.ts';

runtimeTests(
  'empty-store DIGIKIT menu without DSP',
  async ({ session, dk }) => {
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
    const press = (key: string, settle?: number) => tap(session, key, settle);

    await openDigikitMenu(press);
    await press('DOWN');
    await press('DOWN');
    await press('DOWN');
    await press('YES', 800);
    expect(await word(session, planSymbol(plan, 'DK_LOAD_N'))).toBe(1);
    expect(await readTextRows(session, planSymbol(plan, 'DK_LOAD'), 1, 24)).toEqual(['NO STORE']);

    await press('NO');
    await press('UP');
    await press('UP');
    await press('UP');
    const about = await openRuntimeAbout(press, session, dk, plan, artifacts);
    expect(about).toContain('BOOT: 0 OF 0 OK');
    await press('DOWN');
    await press('UP');
    await session.expectNoFault('ABOUT text-page navigation without DSP');

    await press('NO');
    await press('DOWN');
    await press('DOWN');
    await press('YES', 600);
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
