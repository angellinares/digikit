import { encoder } from '@digi/test-support';
import fs from 'node:fs';
import path from 'node:path';
import { describe, expect, test } from 'vitest';
import { expectRuntimeBoot, withReusablePatchedSession } from '@digi/regression/support.ts';
import { liveArtifacts, record } from '@digi/regression/runtime/artifacts.ts';
import { bootInfoLoaded, buildCard } from '@digi/regression/runtime/card-kit.ts';
import { DEVICES, liveOptions } from '@digi/regression/runtime/devices.ts';
import { advance, pickSine } from '@digi/regression/runtime/fx-live.ts';
import { drainInputs, shot, tap } from '@digi/regression/runtime/fx-session.ts';
import { patchkit } from '@digi/regression/runtime/patchkit.ts';
import { expectRuntimeUi } from '@digi/regression/runtime/ready.ts';
import {
  planSymbol,
  prepareRuntimePlanOffline,
  validateRuntimePlan,
  type RuntimePlan,
} from '@digi/regression/runtime/runtime-plan.ts';

for (const device of DEVICES) {
  describe(`${device} Pluck UI preview without DSP`, () => {
    let plan: RuntimePlan;
    let artifacts: string;
    const fixture = withReusablePatchedSession(
      { ...liveOptions(device), dsp: 'zero' },
      {
        prepareBaseline: async (session) => {
          artifacts = liveArtifacts(session, 'pluck-ui-preview');
          const prepared = await prepareRuntimePlanOffline(session.options, session.runDir, artifacts, ['DKRT_BOOT']);
          plan = prepared.plan;
          const dir = fs.mkdtempSync(path.join(session.runDir, 'pluck-preview-'));
          const file = path.join(dir, 'pluck.dkm');
          const packed = await patchkit(['pack', 'digikit/pluck', '--device', device, '--ui-preview', '-o', file]);
          fs.writeFileSync(path.join(artifacts, 'pack.log'), `${packed.stdout}\n${packed.stderr}`);
          const storePath = 'modules/pluck.dkm';
          return {
            cardImage: await buildCard(device, {
              dir,
              modules: [{ name: storePath, file }],
              selection: [storePath],
            }),
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
        finishCase: async ({ session }) => session.expectNoFault('Pluck UI preview cleanup'),
      },
      'Pluck UI preview zero-DSP fixture',
    );

    test('loads the preview and edits its machine page with no SHARC audio session', async () => {
      const { session, dk } = fixture.runtime;
      await expectRuntimeBoot(fixture.runtime);
      validateRuntimePlan(plan, session);
      await expectRuntimeUi(session, plan, artifacts);
      const boot = await bootInfoLoaded(session, plan);
      expect(boot.loaded).toBe(1);
      expect(boot.results).toEqual([0]);
      const modules = await dk.modules();
      record(artifacts, 'modules', modules);
      expect(modules.map((module) => module.name)).toEqual(['digikit/pluck']);
      expect(modules[0]!.dsp_len).toBe(0);
      expect(modules[0]!.types).toHaveLength(1);
      expect(modules[0]!.faults).toBe(0);
      expect(modules[0]!.guard_off).toBe(false);
      expect((await session.status()).dsp).toBe('zero');
      expect((await session.status()).audio).toBeNull();

      await drainInputs(session);
      await session.client.call('run.mode', { mode: 'paused' });
      // This preview is the sole dynamic machine, after the stock machine rows.
      await pickSine(session, device === 'dt2' ? 7 : 5, 16);
      await tap(session, device === 'dt2' ? 'SRC' : 'SYN');
      const before = await shot(session, artifacts, 'pluck-page-before');
      // Match the stock encoder acceleration: isolated detents can round to zero.
      for (let i = 0; i < 30; i++) {
        await session.client.inputTurn(encoder('ENC B'), 1);
        await advance(session, 2);
      }
      await advance(session, 300);
      expect(await shot(session, artifacts, 'pluck-page-after')).not.toBe(before);
      expect((await session.status()).audio).toBeNull();
      await session.expectNoFault('Pluck machine page and encoder without DSP');
    });
  });
}
