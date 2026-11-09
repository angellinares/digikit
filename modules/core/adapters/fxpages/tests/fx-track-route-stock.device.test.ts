import path from 'node:path';
import { expect, test } from 'vitest';
import { compare, word } from '@digi/regression/runtime/fx-live.ts';
import { FX, SINE } from '@digi/regression/runtime/fixtures.ts';
import { shot, tap, selectSine } from '@digi/regression/runtime/fx-session.ts';
import { trackNote } from '@digi/regression/runtime/fx-track-ui.ts';
import { trackRouteSuite } from './fx-track-route-support.ts';

// Track FX send page, one function of the former composite fx-track-route test (setup: fx-track-route-support.ts).

trackRouteSuite('stock', { initial: 'sine', presets: ['sine', 'master'], base: true }, (t) => {
  const { context, triggerFrame, base, sym, chain, capacity } = t;
  const boot = t.booter('sine');
  test('preserves stock pixels, stock PCM and master effects without a send amount', async () => {
        // stock FX pixels and PCM equal the image without the hook
        console.info('track FX leg:', 'stock FX pixels and PCM equal the image without the hook');
        {
          const { runtime, state } = await boot('stock', [SINE], 'sine');
          expect(state.results).toEqual([0]);
          expect(await word(runtime.session, sym('fxp_send_count'))).toBe(0);
          await selectSine(runtime, context.profile);
          await tap(runtime.session, 'FX', 400);
          expect(await shot(runtime.session, context.artifacts, 'stock-fx1')).toBe(base.hash);
          await tap(runtime.session, 'NO', 300);
          const file = path.join(context.artifacts, 'stock.wav');
          await trackNote(runtime, file, triggerFrame);
          await capacity(runtime, 0);
          await compare(base.pcm, file, 1, context.artifacts, 'stock-no-hook', true);
          await context.evidence(runtime, 'stock-audible', true);
        }


        // a master effect preserves the original FX pixels and has no send amount
        console.info('track FX leg:', 'a master effect preserves the original FX pixels and has no send amount');
        {
          const { runtime, state } = await boot('master', [SINE, FX], 'master');
          expect(state.results).toEqual([0, 0]);
          const row = (await chain(runtime))[0]!;
          expect(row[0]).not.toBe(0xffffffff);
          expect(row.slice(18, 23)).toEqual([0, 0, 0, 0, 0]);
          await selectSine(runtime, context.profile);
          await tap(runtime.session, 'FX', 400);
          expect(await shot(runtime.session, context.artifacts, 'master-fx1')).toBe(base.hash);
          await capacity(runtime, 0);
        }

  }, 600_000);
});
