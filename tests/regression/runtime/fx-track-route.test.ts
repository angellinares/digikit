import { execFile } from 'node:child_process';
import path from 'node:path';
import { promisify } from 'node:util';
import { button, encoder, repoRoot } from '@digi/test-support';
import { describe, expect, test } from 'vitest';
import type { PatchedSession } from '../support.ts';
import { advance, analysisJson, bounded, compare, frames, rows, rpc, word } from './fx-live.ts';
import { FX, SINE, triggerFrame as triggerFor } from './fixtures.ts';
import { withRouteFxSession } from './fx-route-live.ts';
import { inspectFxBoot, withPausedFxProtocol, selectSine, shot, tap } from './fx-session.ts';
import { amountShown, kitMenu, kitSlotOne, selectTrack, setAmount, soundImage, trackNote } from './fx-track-ui.ts';
import { DEVICES, FIRMWARE, record } from './live.ts';
import { planSymbol } from './runtime-plan.ts';
import { overlayWrites } from './write-safety.ts';

const exec = promisify(execFile);
const names = [FX, `${FX}-2`, `${FX}-3`];
const routeKey = (name: string) => `fx/route/${name}`;
const sendKey = `fx/send/${FX}`;
const sineCard = { selection: ['modules/m0.dkm'] };
const masterCard = { selection: ['modules/m0.dkm', 'modules/m1.dkm'] };
const sendCard = { ...masterCard, settings: [[routeKey(FX), '01']] as [string, string][] };
const limitCard = {
  selection: ['modules/m0.dkm', 'modules/m1.dkm', 'modules/m2.dkm', 'modules/m3.dkm'],
  settings: names.map((name): [string, string] => [routeKey(name), '01']),
};

interface StoreDump {
  settings: {
    active: string | null;
    copies: { copy: string; seq: number; entries: { key: string; value_hex: string }[] }[];
  };
}

// Every leg starts from an overlay-free card and ready snapshot. The image without
// core/fxpages closes before the regular image starts; only artifacts cross that boundary.
for (const device of DEVICES) {
  describe.sequential(`${device.toUpperCase()} ${FIRMWARE[device]} track FX send page`, () => {
    let baseHash: string;
    let basePcm: string;
    const triggerFrame = triggerFor(device);

    describe('image without the page hook', () => {
      const context = withRouteFxSession(device, 'track FX base', {
        image: 'track-base',
        initial: 'sine',
        presets: { sine: sineCard },
      });
      test('captures the original FX pixels and PCM', async () => {
        const { fixture, plan, profile, artifacts } = context;
        const runtime = fixture.runtime;
        const state = await inspectFxBoot(runtime, plan, [SINE], artifacts, 'base-boot');
        context.rememberEpoch(state.epoch);
        await selectSine(runtime, profile);
        await tap(runtime.session, 'FX', 400);
        baseHash = await shot(runtime.session, artifacts, 'base-fx1');
        await tap(runtime.session, 'NO', 300);
        basePcm = path.join(artifacts, 'base.wav');
        await trackNote(runtime, basePcm, triggerFrame);
      });
    });

    describe('image with the page hook', () => {
      const context = withRouteFxSession(device, 'track FX send pages', {
        image: 'track',
        initial: 'sine',
        presets: { sine: sineCard, master: masterCard, send: sendCard, limit: limitCard },
        // Page, kit and project legs are idle; PCM legs explicitly require an audible signal.
        requireAudible: false,
      });
      const pcm = new Map<string, string>();
      const locks = new Map<string, { overlay: string; cc: string; shown: number }>();
      const sym = (name: string) => planSymbol(context.plan, name);
      const chain = (runtime: PatchedSession) => rows(runtime.session, context.plan, context.slots);
      const amounts = async (runtime: PatchedSession, row = 0) => {
        const words = (await chain(runtime))[row]!.slice(19, 23);
        return Array.from({ length: 16 }, (_, track) => (words[track >> 2]! >>> (8 * (track % 4))) & 0xff);
      };
      const capacity = async (runtime: PatchedSession, used: number) => {
        expect((await withPausedFxProtocol(runtime, (dk) => dk.capacity())).SEND_SLOTS).toEqual({
          limit: context.sendSlots,
          used,
          largest: 0,
        });
      };
      const settings = async (runtime: PatchedSession, tag: string) => {
        const overlay = path.join(context.artifacts, `${tag}.overlay.bin`);
        await rpc(runtime.session, 'card.overlay', { path: overlay });
        const binary = path.join(repoRoot(), 'out/native/target/nogen/release/patchkit');
        const result = await exec(
          binary,
          ['store', 'dump', context.baseline('send').cardImage!, '--device', device, '--overlay', overlay, '--json'],
          { cwd: repoRoot(), timeout: 30_000 },
        );
        const dump = JSON.parse(result.stdout) as StoreDump;
        record(context.artifacts, `${tag}-store`, dump);
        const active = dump.settings.copies.find((copy) => copy.copy === dump.settings.active);
        expect(active, 'valid runtime settings copy').toBeDefined();
        return {
          overlay,
          seq: active!.seq,
          entries: new Map(active!.entries.map((entry) => [entry.key, entry.value_hex])),
        };
      };
      const pageTwo = async (runtime: PatchedSession) => {
        await tap(runtime.session, 'FX', 400);
        await tap(runtime.session, 'FX', 400);
      };

      // Ten formerly separate tests now share this bounded composite wall-clock budget.
      test('preserves stock pixels, routes, kit amounts and project controls across ten clean baselines', async () => {
        let firstBoot = true;
        const boot = async (tag: string, selectedNames: string[], preset: string) => {
          const runtime = firstBoot ? context.fixture.runtime : await context.fixture.reload(context.baseline(preset));
          firstBoot = false;
          const state = await inspectFxBoot(runtime, context.plan, selectedNames, context.artifacts, `${tag}-boot`);
          context.rememberEpoch(state.epoch);
          return { runtime, state };
        };
        // stock FX pixels and PCM equal the image without the hook
        console.info('track FX leg:', 'stock FX pixels and PCM equal the image without the hook');
        {
          const { runtime, state } = await boot('stock', [SINE], 'sine');
          expect(state.results).toEqual([0]);
          expect(await word(runtime.session, sym('fxp_send_count'))).toBe(0);
          await selectSine(runtime, context.profile);
          await tap(runtime.session, 'FX', 400);
          expect(await shot(runtime.session, context.artifacts, 'stock-fx1')).toBe(baseHash);
          await tap(runtime.session, 'NO', 300);
          const file = path.join(context.artifacts, 'stock.wav');
          await trackNote(runtime, file, triggerFrame);
          await capacity(runtime, 0);
          await compare(basePcm, file, 1, context.artifacts, 'stock-no-hook', true);
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
          expect(await shot(runtime.session, context.artifacts, 'master-fx1')).toBe(baseHash);
          await capacity(runtime, 0);
        }

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
          expect(page1).not.toBe(baseHash);
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

        // kit loading never zeros amounts and another pattern receives the same settings
        console.info('track FX leg:', 'kit loading never zeros amounts and another pattern receives the same settings');
        {
          const { runtime } = await boot('kit', [SINE, FX], 'send');
          const { session } = runtime;
          const { profile, artifacts } = context;
          await selectSine(runtime, profile);
          await pageTwo(runtime);
          await setAmount(session, profile, 'ENC A', 0, 40);
          await kitMenu(session, 1);
          await tap(session, 'YES', 600);
          await kitSlotOne(session);
          await shot(session, artifacts, 'kit-save-slot');
          await tap(session, 'YES', 600);
          await tap(session, 'YES', 1500);
          await shot(session, artifacts, 'kit-saved');
          await advance(session, 500);
          await selectTrack(session, 2);
          await pageTwo(runtime);
          await setAmount(session, profile, 'ENC A', 1, 100);
          await advance(session, 2600);
          const nload = await word(session, sym('fxp_nload'));
          await kitMenu(session, 0);
          await tap(session, 'YES', 700);
          await kitSlotOne(session);
          const seen: [number, number][] = [];
          try {
            await bounded(() => session.client.inputButton(button('YES'), true));
            for (let ms = 0; ms < 700; ms += 2) {
              await advance(session, 2);
              const value = (await chain(runtime))[0]![19]!;
              if (!seen.length || seen.at(-1)![1] !== value) seen.push([ms, value]);
              if (ms === 60) await bounded(() => session.client.inputButton(button('YES'), false));
            }
          } finally {
            await bounded(() => session.client.inputButton(button('YES'), false));
          }
          await advance(session, 500);
          expect(await word(session, sym('fxp_nload'))).not.toBe(nload);
          expect(await word(session, sym('DKRT_FXS') + 16)).toBeGreaterThan(0);
          expect(await amountShown(session, profile, 0)).toBe(40);
          expect(await amountShown(session, profile, 1)).toBe(100);
          expect(seen.length).toBeGreaterThan(0);
          expect(seen.every(([, value]) => value === seen[0]![1])).toBe(true);
          expect(seen[0]![1] & 0xffff).toBe((100 << 8) | 40);
          record(artifacts, 'kit-2ms-amount-observations', seen);
          const firstKit = await word(session, profile.fx.kit_ptr);
          await tap(session, 'PTN', 800);
          await tap(session, 'TRIG 2', 300);
          await advance(session, 1500);
          expect(await word(session, profile.fx.kit_ptr), 'A02 owns another kit object').not.toBe(firstKit);
          expect(await amountShown(session, profile, 0)).toBe(40);
          expect(await amountShown(session, profile, 1)).toBe(100);
          expect((await amounts(runtime)).slice(0, 2)).toEqual([40, 100]);
          await tap(session, 'PTN', 800);
          await tap(session, 'TRIG 1', 1500);
          expect(await amountShown(session, profile, 0)).toBe(40);
          expect(await amountShown(session, profile, 1)).toBe(100);
          await advance(session, 2600);
          expect((await settings(runtime, 'kit-after')).entries.get(sendKey)?.slice(0, 4)).toBe('2864');
        }

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

        for (const turn of ['base', 'none', 'amount', 'send'] as const) {
          // Project save controls stay ordered: base, none, amount, send.
          console.info('track FX leg:', `project save control: ${turn}`);
          {
            const { runtime } = await boot(`lock-${turn}`, [SINE, FX], 'send');
            const { session } = runtime;
            const { artifacts, profile } = context;
            await selectSine(runtime, profile);
            await tap(session, 'NO', 300);
            if (turn === 'send' || turn === 'amount') await pageTwo(runtime);
            await shot(session, artifacts, `lock-${turn}-page`);
            const held = turn === 'none' || turn === 'send';
            let cc: { hex: string };
            try {
              if (held) {
                await tap(session, 'RECORD', 200);
                await tap(session, 'TRIG 1', 200);
                await bounded(() => session.client.inputButton(button('TRIG 1'), true));
                await advance(session, 100);
              }
              await rpc(session, 'midi.out');
              if (turn === 'amount') await setAmount(session, profile, 'ENC A', 0, 40);
              else if (turn === 'send') {
                await bounded(() => session.client.inputTurn(encoder('ENC A'), 11));
                await advance(session, 10);
              }
              await advance(session, 300);
              await shot(session, artifacts, `lock-${turn}-turned`);
              cc = await rpc<{ hex: string }>(session, 'midi.out');
            } finally {
              if (held) await bounded(() => session.client.inputButton(button('TRIG 1'), false));
            }
            if (held) {
              await advance(session, 300);
              await tap(session, 'RECORD', 300);
            }
            const shown = await amountShown(session, profile, 0);
            await tap(session, 'SETTINGS', 400, 'FUNC');
            await tap(session, 'YES', 700);
            await tap(session, 'YES', 500);
            await advance(session, 3500);
            const overlay = path.join(artifacts, `lock-${turn}.overlay.bin`);
            await rpc(session, 'card.overlay', { path: overlay });
            locks.set(turn, { overlay, cc: cc!.hex, shown });
            record(artifacts, `lock-${turn}-result`, { overlay, cc: cc!.hex, shown });
            if (turn === 'send') {
              const result = await analysisJson<{
                trig_changed_bytes: number;
                send_changed_bytes: number;
                amount_diff: { differ_bytes: Record<string, number> };
              }>(
                'tools.patchkit.pk_digikit_fxroute',
                [
                  'lock-check',
                  '--device',
                  device,
                  '--card',
                  context.baseline('send').cardImage!,
                  ...['base', 'none', 'amount', 'send'].flatMap((tag) => [`--${tag}`, locks.get(tag)!.overlay]),
                ],
                artifacts,
                'lock-project-byte-oracles',
              );
              expect(result.trig_changed_bytes, 'panel script records a trig: method control').toBeGreaterThan(0);
              // The legacy script could not produce a stock-knob lock control. Preserve
              // its bounded claim: same pattern bytes, supported by the class flag above.
              expect(result.send_changed_bytes, 'held send turn saves no lock bytes').toBe(0);
              expect(locks.get('amount')!.shown).toBe(40);
              expect(
                (result.amount_diff.differ_bytes.kit ?? 0) + (result.amount_diff.differ_bytes.pool ?? 0),
                'amount saves no kit or Sound record bytes',
              ).toBe(0);
              expect(locks.get('send')!.cc, 'send knob emits no MIDI CC').toBe('');
            }
          }
        }
      }, 600_000);
    });
  });
}
