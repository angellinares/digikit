import path from 'node:path';
import { describe, expect, test } from 'vitest';
import { createFxKitUi } from '@digi/regression/runtime/fx-kit-ui.ts';
import { analysisJson, captureNote, frames, rows } from '@digi/regression/runtime/fx-live.ts';
import { SINE, triggerFrame } from '@digi/regression/runtime/fixtures.ts';
import { bits, mainPage, pageKit } from '@digi/regression/runtime/fx-pages-ui.ts';
import { inspectFxBoot, selectSine, withPreparedFxSession } from '@digi/regression/runtime/fx-session.ts';
import { DEVICES, FIRMWARE } from '@digi/regression/runtime/devices.ts';

// The device smoke of the real module: one clean card, the module loads, its SHIFT knob reaches the row, a note sounds. The adapter legs
// (pages, signed values, kit, send amounts) are the FX contract suite on test/dyn-fx-gain-bipolar (core/fxpages fx-bipolar), and the shifter's
// sound (tones, image, level, state independence) is on the SHARC rig (tests/rig/freqshift.toml). This file goes with R12-5 (fx.dks).
const FX = 'digikit/freqshift';
const shiftFloat = (raw: number) => Math.fround(-64 + Math.fround(127 * Math.fround(raw / 32512)));

for (const device of DEVICES) {
  describe(`${device.toUpperCase()} ${FIRMWARE[device]} frequency shifter`, () => {
    const context = withPreparedFxSession(device, 'frequency shifter', {
      image: {
        name: 'fx-freqshift',
        modules: ['core/runtime', 'core/dynreg', 'core/dyndsp', 'core/dynfx', 'core/fxpages'],
        inventory: [SINE, FX],
        symbols: ['DKRT_BOOT', 'fx.rows', 'fxp_count', 'fxp_list_buf', 'fxp_rows', 'params.fp0.index', 'pf_table'],
        // The kit library and the runtime settings are written, as in the route and kit tests.
        policy: 'fx-routes',
      },
      initial: 'master',
      // The module starts as a send effect: the master route is stored.
      presets: { master: { selection: ['modules/m0.dkm', 'modules/m1.dkm'], settings: [[`fx/route/${FX}`, '00']] } },
      // The level of the note is checked below.
      requireAudible: false,
    });

    test('loads on a clean card, takes the SHIFT knob into its row and plays a note without a fault', async () => {
      const { fixture, plan, profile, artifacts, slots } = context;
      const runtime = fixture.runtime;
      const state = await inspectFxBoot(runtime, plan, [SINE, FX], artifacts, 'master-boot');
      context.rememberEpoch(state.epoch);
      await selectSine(runtime, profile);
      const kit = pageKit(runtime.session, profile);
      const ui = createFxKitUi(runtime, plan, profile, artifacts);
      await mainPage(runtime.session);
      await ui.fxPage(3);
      const raw = await kit.setRaw('ENC A', 0, 0x6800);
      await mainPage(runtime.session);
      const row = (await rows(runtime.session, plan, slots))[0]!;
      expect(row[4], 'the SHIFT word of the row is the float of the knob').toBe(bits(shiftFloat(raw)));
      expect(row[17], 'no effect fault').toBe(0);
      const file = path.join(artifacts, 'shift-up.wav');
      await captureNote(runtime.session, {
        file,
        trigger: Math.max(triggerFrame(device), (await frames(runtime.session)) + 300),
        tag: 'frequency shifter note',
        length: 1600,
      });
      const pcm = await analysisJson<{ peak: number }>(
        'tools.patchkit.pk_digikit_fx',
        ['pcm-peak', file],
        artifacts,
        'shift-up-peak',
      );
      expect(pcm.peak, 'the shifted sine is audible').toBeGreaterThan(0.01);
      expect((await rows(runtime.session, plan, slots))[0]![17], 'no effect fault after the note').toBe(0);
    });
  });
}
