import { encoder } from '@digi/test-support';
import fs from 'node:fs';
import path from 'node:path';
import { buildCard } from '@digi/regression/runtime/card-kit.ts';
import { describe, expect, test } from 'vitest';
import { withReusablePatchedSession, expectRuntimeBoot } from '@digi/regression/support.ts';
import { liveArtifacts } from '@digi/regression/runtime/artifacts.ts';
import { DEVICES, liveOptions } from '@digi/regression/runtime/devices.ts';
import { advance, word } from '@digi/regression/runtime/fx-live.ts';
import { shot, tap } from '@digi/regression/runtime/fx-session.ts';
import { expectRuntimeUi } from '@digi/regression/runtime/ready.ts';
import { openDigikitMenu } from '@digi/regression/runtime/runtime-menu.ts';
import {
  planSymbol,
  prepareRuntimePlanOffline,
  validateRuntimePlan,
  type RuntimePlan,
} from '@digi/regression/runtime/runtime-plan.ts';

for (const device of DEVICES) {
  describe(`${device} SDK UI without DSP`, () => {
    let plan: RuntimePlan;
    let artifacts: string;
    const fixture = withReusablePatchedSession(
      { ...liveOptions(device), dsp: 'zero' },
      {
        prepareBaseline: async (session) => {
          artifacts = liveArtifacts(session, 'sdk-ui-zero');
          const prepared = await prepareRuntimePlanOffline(
            session.options,
            session.runDir,
            artifacts,
            ['DKRT_BOOT', 'dk_demo_value', 'dk_demo_checks', 'dk_demo_events_n', 'dk_demo_answer_v'],
            { modules: ['test/ui-demo'] },
          );
          plan = prepared.plan;
          const dir = fs.mkdtempSync(path.join(session.runDir, 'ui-card-'));
          return {
            preparedImage: prepared.preparedImage,
            cardImage: await buildCard(device, { dir }),
            guard: {
              protect: [
                {
                  lo: planSymbol(plan, 'DKRT_BOOT'),
                  hi: planSymbol(plan, 'DKRT_BOOT') + 8,
                  label: 'runtime boot identity',
                },
              ],
              allow_pc: [],
            },
          };
        },
        finishCase: async ({ session }) => session.expectNoFault('SDK UI case cleanup'),
      },
      'SDK UI zero-DSP fixture',
    );

    test('rows, list focus, dialogs and toast retain visible and callback state', async () => {
      const { session } = fixture.runtime;
      await expectRuntimeBoot(fixture.runtime);
      validateRuntimePlan(plan, session);
      await expectRuntimeUi(session, plan, artifacts, { fresh: true });
      expect((await session.status()).dsp).toBe('zero');
      expect((await session.status()).audio).toBeNull();
      const read = (name: string, offset = 0) => word(session, planSymbol(plan, name) + offset);
      const press = (key: string, settle?: number) => tap(session, key, settle);
      const frame = (tag: string) => shot(session, artifacts, tag);
      let row = 0;
      const go = async (target: number) => {
        while (row !== target) {
          await press(row < target ? 'DOWN' : 'UP');
          row += row < target ? 1 : -1;
        }
      };
      await openDigikitMenu(press);
      const initial = await frame('menu');
      const events = await read('dk_demo_events_n');
      await press('YES');
      expect(await read('dk_demo_events_n')).toBe(events + 1);
      expect(await frame('toggle-off')).not.toBe(initial);
      await press('YES');
      expect(await frame('toggle-on')).toBe(initial);
      await go(1);
      const value = await read('dk_demo_value');
      const valueFrame = await frame('value-before');
      // The firmware accelerates encoder events; an isolated event can round to zero.
      for (let i = 0; i < 30; i++) {
        await session.client.inputTurn(encoder('ENC A'), 1);
        await advance(session, 2);
      }
      await advance(session, 300);
      expect(await read('dk_demo_value')).toBeGreaterThan(value);
      expect(await frame('value-after')).not.toBe(valueFrame);

      await go(5);
      await press('YES');
      const page = await frame('text-page');
      // Text pages hide the cursor; move past the visible rows to exercise scrolling.
      for (let i = 0; i < 7; i++) await press('DOWN');
      expect(await frame('text-page-scrolled')).not.toBe(page);
      await press('NO');
      await go(6);
      await press('YES');
      await press('DOWN');
      const unchecked = await frame('list-unchecked');
      await press('YES');
      expect(await read('dk_demo_checks', 4)).toBe(1);
      expect(await frame('list-checked')).not.toBe(unchecked);
      await press('RIGHT');
      const panel = await frame('list-panel');
      await press('DOWN');
      expect(await frame('panel-second')).not.toBe(panel);
      const beforePanel = await read('dk_demo_events_n');
      await press('YES');
      expect(await read('dk_demo_events_n')).toBe(beforePanel + 1);
      await advance(session, 2500);
      await press('DOWN');
      await press('YES');
      expect(await read('dk_demo_checks', 8)).toBe(1);
      const refused = await frame('list-third-checked');
      const beforeBack = await read('dk_demo_events_n');
      await press('NO');
      expect(await read('dk_demo_events_n')).toBe(beforeBack + 1);
      expect(await frame('list-refused-back')).toBe(refused);
      await press('YES');
      await press('NO');
      await go(7);
      await press('YES');
      const empty = await frame('empty-list');
      const beforeEmpty = await read('dk_demo_events_n');
      await press('YES');
      await press('RIGHT');
      expect(await read('dk_demo_events_n')).toBe(beforeEmpty);
      expect(await frame('empty-list-no-actions')).toBe(empty);
      await press('NO');

      await go(3);
      await press('YES');
      const confirm = await frame('confirm');
      await press('NO');
      expect(await read('dk_demo_answer_v')).toBe(0);
      expect(await frame('confirm-dismissed')).not.toBe(confirm);
      await press('YES');
      await press('YES');
      expect(await read('dk_demo_answer_v')).toBe(1);
      await go(4);
      await press('YES');
      await press('DOWN');
      await press('DOWN');
      await press('YES');
      expect(await read('dk_demo_answer_v')).toBe(2);
      await press('YES');
      const choice = await frame('choice');
      const beforeCancel = await read('dk_demo_events_n');
      await press('NO');
      expect(await read('dk_demo_events_n')).toBe(beforeCancel);
      await press('YES');
      expect(await frame('choice-reopened')).toBe(choice);
      await press('NO');

      await go(2);
      const beforeToast = await read('dk_demo_events_n');
      await press('YES', 50);
      expect(await read('dk_demo_events_n')).toBe(beforeToast + 1);
      await press('NO', 50);
      await press('NO', 50);
      const toast = await frame('toast');
      await advance(session, 2500);
      expect(await frame('toast-expired')).not.toBe(toast);
      await session.expectNoFault('SDK row/list/dialog/toast interactions without DSP');
      expect((await session.status()).audio).toBeNull();
    });
  });
}
