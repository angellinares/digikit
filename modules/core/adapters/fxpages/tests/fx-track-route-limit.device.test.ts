import { expect, test } from 'vitest';
import { advance } from '@digi/regression/runtime/fx-live.ts';
import { SINE } from '@digi/regression/runtime/fixtures.ts';
import { shot, selectSine } from '@digi/regression/runtime/fx-session.ts';
import { setAmount } from '@digi/regression/runtime/fx-track-ui.ts';
import { names, trackRouteSuite } from './fx-track-route-support.ts';

// Track FX send page, one function of the former composite fx-track-route test (setup: fx-track-route-support.ts).

trackRouteSuite('limit', { initial: 'limit', presets: ['limit'], base: false }, (t) => {
  const { device, context, chain, amounts, capacity, pageTwo } = t;
  const boot = t.booter('limit');
  test('the device send limit leaves excess effects on the master without send cells', async () => {
        // the device send limit leaves excess effects on the master without send cells
        console.info('track FX leg:', 'the device send limit leaves excess effects on the master without send cells');
        {
          const { runtime, state } = await boot('limit', [SINE, ...names], 'limit');
          expect(state.results).toEqual([0, 0, 0, 0]);
          expect((await chain(runtime)).slice(0, 3).map((row) => row[18])).toEqual([
            ...Array(context.sendSlots).fill(1),
            ...Array(3 - context.sendSlots).fill(0),
          ]);
          expect(context.sendSlots).toBe(device === 'dt2' ? 2 : 1);
          await capacity(runtime, context.sendSlots);
          await selectSine(runtime, context.profile);
          await pageTwo(runtime);
          await shot(runtime.session, context.artifacts, 'limit-page2');
          await setAmount(runtime.session, context.profile, 'ENC A', 0, 20);
          if (context.sendSlots >= 2) await setAmount(runtime.session, context.profile, 'ENC B', 0, 30, 1);
          await advance(runtime.session, 300);
          expect((await amounts(runtime, 0))[0]).toBe(20);
          expect((await amounts(runtime, 1))[0]).toBe(context.sendSlots >= 2 ? 30 : 0);
          expect((await amounts(runtime, 2))[0]).toBe(0);
        }

  });
});
