import { button } from '@digi/test-support';
import { expect, test } from 'vitest';
import { bounded, word, REDRAW, SETTLE } from '@digi/regression/runtime/fx-live.ts';
import { FX, SINE } from '@digi/regression/runtime/fixtures.ts';
import { shot, selectSine } from '@digi/regression/runtime/fx-session.ts';
import { amountShown, kitMenu, kitSlotOne, selectTrack, setAmount } from '@digi/regression/runtime/fx-track-ui.ts';
import { record } from '@digi/regression/runtime/artifacts.ts';
import { sendKey, trackRouteSuite } from './fx-track-route-support.ts';

// Track FX send page, one function of the former composite fx-track-route test (setup: fx-track-route-support.ts).

trackRouteSuite('kit', { initial: 'send', presets: ['send'], base: false }, (t) => {
  const { context, sym, chain, amounts, settings, pageTwo } = t;
  const boot = t.booter('send');
  test('kit loading never zeros amounts and another pattern receives the same settings', async () => {
        // kit loading never zeros amounts and another pattern receives the same settings
        console.info('track FX leg:', 'kit loading never zeros amounts and another pattern receives the same settings');
        {
          const { runtime } = await boot('kit', [SINE, FX], 'send');
          const { session } = runtime;
          const { profile, artifacts } = context;
          await selectSine(runtime, profile);
          await pageTwo(runtime);
          await setAmount(runtime, profile, 'ENC A', 0, 40);
          await kitMenu(runtime, 1);
          await runtime.ui.tap('YES', { until: { ms: 600, why: REDRAW } });
          await kitSlotOne(runtime);
          await shot(session, artifacts, 'kit-save-slot');
          await runtime.ui.tap('YES', { until: { ms: 600, why: REDRAW } });
          await runtime.ui.tap('YES', { until: { ms: 1500, why: REDRAW } });
          await shot(session, artifacts, 'kit-saved');
          await runtime.ui.wait({ ms: 500, why: SETTLE });
          await selectTrack(runtime, 2);
          await pageTwo(runtime);
          await setAmount(runtime, profile, 'ENC A', 1, 100);
          await runtime.ui.wait({ ms: 2600, why: SETTLE });
          const nload = await word(session, sym('fxp_nload'));
          await kitMenu(runtime, 0);
          await runtime.ui.tap('YES', { until: { ms: 700, why: REDRAW } });
          await kitSlotOne(runtime);
          const seen: [number, number][] = [];
          try {
            await bounded(() => session.client.inputButton(button('YES'), true));
            for (let ms = 0; ms < 700; ms += 2) {
              await runtime.ui.wait({ ms: 2, why: SETTLE });
              const value = (await chain(runtime))[0]![19]!;
              if (!seen.length || seen.at(-1)![1] !== value) seen.push([ms, value]);
              if (ms === 60) await bounded(() => session.client.inputButton(button('YES'), false));
            }
          } finally {
            await bounded(() => session.client.inputButton(button('YES'), false));
          }
          await runtime.ui.wait({ ms: 500, why: SETTLE });
          expect(await word(session, sym('fxp_nload'))).not.toBe(nload);
          expect(await word(session, sym('DKRT_FXS') + 16)).toBeGreaterThan(0);
          expect(await amountShown(session, profile, 0)).toBe(40);
          expect(await amountShown(session, profile, 1)).toBe(100);
          expect(seen.length).toBeGreaterThan(0);
          expect(seen.every(([, value]) => value === seen[0]![1])).toBe(true);
          expect(seen[0]![1] & 0xffff).toBe((100 << 8) | 40);
          record(artifacts, 'kit-2ms-amount-observations', seen);
          const firstKit = await word(session, profile.fx.kit_ptr);
          await runtime.ui.tap('PTN', { until: { ms: 800, why: REDRAW } });
          await runtime.ui.tap('TRIG 2', { until: { ms: 300, why: REDRAW } });
          await runtime.ui.wait({ ms: 1500, why: SETTLE });
          expect(await word(session, profile.fx.kit_ptr), 'A02 owns another kit object').not.toBe(firstKit);
          expect(await amountShown(session, profile, 0)).toBe(40);
          expect(await amountShown(session, profile, 1)).toBe(100);
          expect((await amounts(runtime)).slice(0, 2)).toEqual([40, 100]);
          await runtime.ui.tap('PTN', { until: { ms: 800, why: REDRAW } });
          await runtime.ui.tap('TRIG 1', { until: { ms: 1500, why: REDRAW } });
          expect(await amountShown(session, profile, 0)).toBe(40);
          expect(await amountShown(session, profile, 1)).toBe(100);
          await runtime.ui.wait({ ms: 2600, why: SETTLE });
          expect((await settings(runtime, 'kit-after')).entries.get(sendKey)?.slice(0, 4)).toBe('2864');
        }

  });
});
