import { encoder } from '@digi/test-support';
import fs from 'node:fs';
import path from 'node:path';
import { describe, expect, test } from 'vitest';
import { expectRuntimeBoot, withReusablePatchedSession } from '@digi/regression/support.ts';
import { liveArtifacts, record } from '@digi/regression/runtime/artifacts.ts';
import { bootInfoLoaded, buildCard } from '@digi/regression/runtime/card-kit.ts';
import { DEVICES, liveOptions } from '@digi/regression/runtime/devices.ts';
import { advance, pickSine, word } from '@digi/regression/runtime/fx-live.ts';
import { drainInputs, shot, tap } from '@digi/regression/runtime/fx-session.ts';
import { patchkit } from '@digi/regression/runtime/patchkit.ts';
import { trackType } from '@digi/regression/runtime/project-kit.ts';
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
      // DN2 1.12's track-1 sound mirror keeps the real type in its first byte
      // (pk_dn2_common.wire_type; finding 02). Check the transition as this mirror
      // can retain an old value after a second selection; this case selects once.
      const selectedType = () =>
        device === 'dt2' ? trackType(session, 1) : word(session, 0x8000487c).then((value) => value >>> 24);
      const pluckType = modules[0]!.types[0]!;
      expect(await selectedType(), 'fresh track 1 starts on a stock machine').not.toBe(pluckType);
      await tap(session, 'TRIG 1', 300, 'TRK');
      // This preview is the sole dynamic machine, after the stock machine rows.
      await pickSine(session, device === 'dt2' ? 7 : 5, 16);
      await tap(session, device === 'dt2' ? 'SRC' : 'SYN');
      // A DN2 trig copies the selected sound into the ColdFire audio mirror.
      if (device === 'dn2') await tap(session, 'TRIG 1');
      expect(await selectedType(), 'track 1 uses the registered Pluck machine').toBe(pluckType);
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
