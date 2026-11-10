import path from 'node:path';
import { expect, test } from 'vitest';
import { advance, analysisJson, frames, word } from '@digi/regression/runtime/fx-live.ts';
import { FX, SINE } from '@digi/regression/runtime/fixtures.ts';
import { shot, tap, selectSine } from '@digi/regression/runtime/fx-session.ts';
import { amountShown, selectTrack, setAmount, soundImage, trackNote } from '@digi/regression/runtime/fx-track-ui.ts';
import { record } from '@digi/regression/runtime/artifacts.ts';
import { overlayWrites } from '@digi/regression/runtime/write-safety.ts';
import { routeKey, sendKey, trackRouteSuite } from './fx-track-route-support.ts';

// Track FX send page, one function of the former composite fx-track-route test (setup: fx-track-route-support.ts).

trackRouteSuite('send', { initial: 'sine', presets: ['sine', 'send'], base: true }, (t) => {
  const { device, context, triggerFrame, base, pcm, sym, chain, amounts, capacity, settings, pageTwo } = t;
  const boot = t.booter('sine');
  test('send knobs affect only their Sound word, track DSP amounts and settings, with squared PCM gain', async () => {
        // dry input remains available as a separate send PCM control
        console.info('track FX leg:', 'dry input remains available as a separate send PCM control');
        {
          const { runtime } = await boot('dry', [SINE], 'sine');
          await selectSine(runtime, context.profile);
          const file = path.join(context.artifacts, 'dry.wav');
          await trackNote(runtime, file, triggerFrame);
          pcm.set('dry', file);
          await context.evidence(runtime, 'dry-audible', true);
        }


        // send knobs affect only their Sound word, track DSP amounts and settings, with squared PCM gain
        console.info(
          'track FX leg:',
          'send knobs affect only their Sound word, track DSP amounts and settings, with squared PCM gain',
        );
        {
          const { runtime, state } = await boot('send', [SINE, FX], 'send');
          const { session } = runtime;
          const { profile, artifacts } = context;
          expect(state.results).toEqual([0, 0]);
          const row = (await chain(runtime))[0]!;
          expect(row[0]).not.toBe(0xffffffff);
          expect(row.slice(18, 23)).toEqual([1, 0, 0, 0, 0]);
          const info = sym('fxpages_info');
          const registry = sym('dynreg_info');
          const [regRows, regSize, regFirst, templateOffset] = await Promise.all(
            [21, 22, 23, 28].map((index) => word(session, registry + index * 4)),
          );
          const code = await word(session, info + 42 * 4);
          const template = await word(session, regRows! + (code - regFirst!) * regSize! + templateOffset!);
          const classes = profile.value_classes;
          expect(template, 'send knob uses the no-lock percent template').toBe(profile.fx.template_percent);
          expect((await word(session, classes + 0x44 * template)) & 9).not.toBe(0);
          expect((await word(session, classes + 0x44 * profile.fx.send_template)) & 9, 'stock send lock control').toBe(
            0,
          );
          expect(await word(session, sym('fxp_send_row') + 8)).toBe(code);
          expect(await word(session, sym('fxp_send_count'))).toBe(2);
          await capacity(runtime, 1);
          await selectSine(runtime, profile);
          await tap(session, 'FX', 400);
          const page1 = await shot(session, artifacts, 'send-page1');
          await tap(session, 'FX', 400);
          const page2 = await shot(session, artifacts, 'send-page2-track1');
          expect(page1).not.toBe(base.hash);
          expect(page2).not.toBe(page1);
          await tap(session, 'FX', 400);
          expect(await shot(session, artifacts, 'send-page1-again')).toBe(page1);
          await tap(session, 'FX', 400);
          const before = await soundImage(session, profile, 0);
          for (const index of profile.fx.send_indices) expect(before[0x14 / 2 + index]).toBe(0);
          await setAmount(session, profile, 'ENC A', 0, 40);
          await shot(session, artifacts, 'send-turning');
          const after = await soundImage(session, profile, 0);
          expect(
            after.flatMap((value, index) => (value === before[index] ? [] : [index])),
            'no other Sound word changes, record slot included',
          ).toEqual([0x14 / 2 + profile.fx.send_indices[0]!]);
          await advance(session, 300);
          expect(await amounts(runtime)).toEqual([40, ...Array(15).fill(0)]);
          await pageTwo(runtime);
          const at40 = await shot(session, artifacts, 'send-page2-track1-40');
          await selectTrack(session, 2);
          await shot(session, artifacts, 'send-page2-track2-after-select');
          expect(await amountShown(session, profile, 1)).toBe(0);
          await pageTwo(runtime);
          await shot(session, artifacts, 'send-page2-track2-0');
          await setAmount(session, profile, 'ENC A', 1, 100);
          await shot(session, artifacts, 'send-turning-track2');
          await advance(session, 300);
          expect((await amounts(runtime)).slice(0, 2)).toEqual([40, 100]);
          await selectTrack(session, 1);
          await pageTwo(runtime);
          const back = await shot(session, artifacts, 'send-page2-track1-back');
          expect(back === at40 || (await amountShown(session, profile, 0)) === 40).toBe(true);
          await advance(session, 2600);
          const stored = await settings(runtime, 'send-after');
          expect(stored.seq).toBeGreaterThan(1);
          expect(stored.entries.get(sendKey)).toBe('2864' + '00'.repeat(14));
          expect(stored.entries.get(routeKey(FX))).toBe('01');
          const writes = overlayWrites(stored.overlay, device);
          record(artifacts, 'send-store-writes', writes);
          const allowed = [
            profile.settings.copy_a,
            profile.settings.copy_b,
            profile.selection.copy_a,
            profile.selection.copy_b,
          ];
          expect(writes.store_sectors.every((sector) => allowed.includes(sector))).toBe(true);
          expect(
            writes.store_sectors.some(
              (sector) => sector === profile.settings.copy_a || sector === profile.settings.copy_b,
            ),
          ).toBe(true);
          await tap(session, 'NO', 300);
          const file = path.join(artifacts, 'send.wav');
          await trackNote(runtime, file, (await frames(session)) + 300);
          pcm.set('send', file);
          const result = await analysisJson<{ added_factor: number }>(
            'tools.patchkit.pk_digikit_fxsend',
            ['added-factor', pcm.get('dry')!, file],
            artifacts,
            'send-added-factor',
          );
          const want = 0.5 * (40 / 127) ** 2;
          expect(Math.abs(result.added_factor / want - 1)).toBeLessThan(0.02);
          await context.evidence(runtime, 'send-audible', true);
        }

  }, 600_000);
});
