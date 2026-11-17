import path from 'node:path';
import { encoder } from '@digi/test-support';
import { describe, expect, test } from 'vitest';
import { createFxKitUi } from '@digi/regression/runtime/fx-kit-ui.ts';
import { bounded, captureNote, compare, frames, rows, word } from '@digi/regression/runtime/fx-live.ts';
import { SINE, triggerFrame } from '@digi/regression/runtime/fixtures.ts';
import { bits, cstring, mainPage, openPages, pageKit } from '@digi/regression/runtime/fx-pages-ui.ts';
import { inspectFxBoot, selectSine, shot, withPreparedFxSession } from '@digi/regression/runtime/fx-session.ts';
import { selectTrack, setAmount } from '@digi/regression/runtime/fx-track-ui.ts';
import { record } from '@digi/regression/runtime/artifacts.ts';
import { DEVICES, FIRMWARE } from '@digi/regression/runtime/devices.ts';
import { symbolsOf } from '@digi/regression/runtime/service-symbols.ts';

// The signed value template of core/fxpages on test/dyn-fx-gain-bipolar: a BIAS knob from -64 to +63 (default 0) whose float p0 gives the
// gain 1 + p0 / 64. This replaces the adapter legs of the frequency shifter test (docs/plans/ledgers/t2-fx-contract.md); the frequency shift
// itself is proven on the SHARC rig.
const FX = 'test/dyn-fx-gain-bipolar';
const ROUTE = `fx/route/${FX}`;
const effectCard = { selection: ['modules/m0.dkm', 'modules/m1.dkm'] };
const biasFloat = (raw: number) => Math.fround(-64 + Math.fround(127 * Math.fround(raw / 32512)));
const gainOf = (bias: number) => Math.fround(1 + bias / 64);
const floatWord = (raw: number) => {
  const bytes = Buffer.alloc(4);
  bytes.writeUInt32LE(raw);
  return bytes.readFloatLE();
};

for (const device of DEVICES) {
  describe(`${device.toUpperCase()} ${FIRMWARE[device]} FX contract, signed knob`, () => {
    const context = withPreparedFxSession(device, 'FX contract bipolar', {
      image: {
        name: 'fx-bipolar',
        modules: ['core/runtime', 'core/dynreg', 'core/dyndsp', 'core/dynfx', 'core/fxpages'],
        inventory: [SINE, FX],
        symbols: [
          'DKRT_BOOT',
          'fx.rows',
          'params.fp0.index',
          'pf_table',
        ],
        // The kit library and the runtime settings are written, as in the route and kit tests.
        policy: 'fx-routes',
      },
      initial: 'dry',
      presets: {
        dry: { selection: ['modules/m0.dkm'] },
        // The fixture starts on the send route; the master is stored.
        master: { ...effectCard, settings: [[ROUTE, '00']] },
        send: effectCard,
      },
      // The dry amplitude and the relative PCM comparisons below own audibility.
      requireAudible: false,
    });

    test('keeps page metadata, signed value roundtrip, kit save and load, master gain law and send amounts across three clean cards', async () => {
      const { fixture, plan, profile, artifacts, slots, sendSlots } = context;
      // the page state (`fxp_*`) is in the fx service file: `symbolsOf` has its symbols after each boot (`inspectFxBoot`); the other names are the image's
      const sym = (name: string) => symbolsOf(runtime, plan)(name);
      let runtime = fixture.runtime;
      const pcm: Record<string, string> = {};
      let mixHalf = 0;
      const chain = () => rows(runtime.session, plan, slots);
      const row = async () => (await chain())[0]!;
      const words = async () => {
        const r = await row();
        return [r[4], r[13]];
      };
      const amounts = async () => {
        const r = await row();
        return Array.from({ length: 16 }, (_, i) => (r[19 + (i >> 2)]! >>> (8 * (i % 4))) & 0xff);
      };
      const boot = async (tag: string, names: string[], reload = true) => {
        if (reload) {
          const previous = runtime;
          runtime = await fixture.reload(context.baseline(tag));
          expect(runtime.session).toBe(previous.session);
          expect(runtime.epoch).toBeGreaterThan(previous.epoch);
          await expect(previous.dk.info()).rejects.toThrow();
        }
        const state = await inspectFxBoot(runtime, plan, names, artifacts, `${tag}-boot`);
        context.rememberEpoch(state.epoch);
        return state;
      };
      const sound = async (tag: string, initial = false) => {
        const current = await frames(runtime.session);
        const trigger = initial ? triggerFrame(device) : current + 300;
        record(artifacts, `${tag}-timing`, { currentFrame: current, triggerFrame: trigger });
        const file = path.join(artifacts, `${tag}.wav`);
        await captureNote(runtime.session, { file, trigger, tag: 'bipolar note', length: 1600 });
        pcm[tag] = file;
      };
      const noFault = async () => expect((await row())[17]).toBe(0);
      // The row words and the kit are firmware state: a knob change is waited for on them.
      const rowIs = (what: string, done: (r: number[]) => boolean) =>
        runtime.ui.wait({ state: row, done, withinMs: 5000, what });
      const NO_PAGE_STATE = 'no firmware word shows the FX page change (only the page list and its rows)';

      await boot('dry', [SINE], false);
      await selectSine(runtime, profile);
      await mainPage(runtime);
      await sound('dry', true);

      // Master route: the page, the signed values and the gain law.
      const master = await boot('master', [SINE, FX]);
      await selectSine(runtime, profile);
      await openPages(runtime);
      for (let i = 0; i < 3; i++) await runtime.ui.tap('DOWN', { until: { drained: true } });
      await runtime.ui.wait({ ms: 600, why: NO_PAGE_STATE });
      await shot(runtime.session, artifacts, 'gbp-page-default');
      expect(await word(runtime.session, sym('fxp_count'))).toBe(4);
      expect(
        await Promise.all(Array.from({ length: 4 }, (_, i) => word(runtime.session, sym('fxp_list_buf') + 4 * i))),
      ).toEqual([...(device === 'dt2' ? [13, 14, 15] : [17, 18, 19]), profile.fx.page_first]);
      const first = sym('params.fp0.index');
      const page = sym('fxp_rows');
      expect(await Promise.all(Array.from({ length: 9 }, (_, i) => word(runtime.session, page + 8 + 4 * i)))).toEqual([
        first,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        first + 7,
      ]);
      expect(await cstring(runtime.session, await word(runtime.session, page))).toBe('GBP');
      expect(await cstring(runtime.session, await word(runtime.session, page + 4))).toBe('Test Gain Bipolar');
      const param = (code: number) =>
        Promise.all(Array.from({ length: 15 }, (_, i) => word(runtime.session, sym('pf_table') + 60 * code + 4 * i)));
      const g = await param(first);
      const m = await param(first + 7);
      // Signed template: the default 0 of -64 .. +63 is raw 0x4000; the MIX knob starts wet.
      expect(g.slice(1, 5)).toEqual([profile.fx.value_indices[0], 0, 0x7f00, 0x4000]);
      expect(m.slice(1, 5)).toEqual([profile.fx.value_indices[7], 0, 0x7f00, 0x7f00]);
      expect(await Promise.all([g[10]!, g[12]!, m[10]!, m[12]!].map((at) => cstring(runtime.session, at)))).toEqual([
        'Bias',
        'BIAS',
        'Mix',
        'MIX',
      ]);
      let kit = pageKit(runtime, profile);
      let ui = createFxKitUi(runtime, plan, profile, artifacts);
      expect([await kit.raw(0), await kit.raw(7)]).toEqual([0x4000, 0x7f00]);
      expect(await words()).toEqual([0, bits(1)]);
      expect((await row()).slice(18, 23)).toEqual([0, 0, 0, 0, 0]);
      expect(master.capacity.SEND_SLOTS).toEqual({ limit: sendSlots, used: 0, largest: 0 });
      await mainPage(runtime);
      await sound('a');

      // The MIX endpoints: approach, sixty fast events, settle.
      const endpoint = async (near: number, direction: number, target: number, tag: string) => {
        await ui.fxPage(3);
        await kit.setRaw('LEVEL', 7, near);
        for (let i = 0; i < 60; i++) {
          await bounded(() => runtime.session.client.inputTurn(encoder('LEVEL'), direction * 8));
          await runtime.ui.wait({ ms: 2, why: 'fast encoder events: the legacy burst spacing' });
        }
        await runtime.ui.wait({
          state: () => kit.raw(7),
          done: (v: number) => v === target,
          withinMs: 5000,
          what: 'the MIX knob is at its end',
        });
        expect(await kit.raw(7)).toBe(target);
        await rowIs('the MIX word at target', (r) => r[13] === (target ? bits(1) : 0));
        expect((await words())[1]).toBe(target ? bits(1) : 0);
        await shot(runtime.session, artifacts, tag);
      };
      const bias: Record<string, number> = {};
      const turn = async (raw: number, tag: string) => {
        await ui.fxPage(3);
        const got = await kit.setRaw('ENC A', 0, raw);
        const value = biasFloat(got);
        await rowIs('the SHIFT word follows the knob', (r) => r[4] === bits(value));
        await shot(runtime.session, artifacts, `gbp-page-bias-${tag}`);
        expect(await words()).toEqual([bits(value), bits(1)]);
        bias[tag] = value;
      };
      await turn(0x6800, 'up');
      await mainPage(runtime);
      await sound('c');
      // MIX bypass after a nonzero bias: a MIX that fails to bypass shows as gain.
      await endpoint(0x1000, -1, 0, 'gbp-page-mix-zero');
      await mainPage(runtime);
      await sound('b');
      await endpoint(0x7000, 1, 0x7f00, 'gbp-page-mix-top');
      await mainPage(runtime);
      await ui.fxPage(3);
      const halfRaw = await kit.setRaw('LEVEL', 7, 0x4000);
      await rowIs('the MIX word follows the knob', (r) => Math.abs(floatWord(r[13]!) - halfRaw / 0x7f00) < 1e-4);
      mixHalf = floatWord((await row())[13]!);
      expect(Math.abs(mixHalf - halfRaw / 0x7f00)).toBeLessThan(1e-4);
      await mainPage(runtime);
      await sound('h');
      await endpoint(0x7000, 1, 0x7f00, 'gbp-page-mix-top-again');
      await mainPage(runtime);
      await turn(0x1800, 'down');
      await mainPage(runtime);
      await sound('d');
      await noFault();
      const saved = [await kit.raw(0), await kit.raw(7)];
      await mainPage(runtime);
      await ui.saveKit(2, 'gbp');
      await ui.fxPage(3);
      await kit.setRaw('ENC A', 0, 0x6800);
      await mainPage(runtime);
      const upRaw = await kit.raw(0);
      const loaded = await ui.loadKit(2, 'gbp');
      record(artifacts, 'gbp-load-observations', loaded);
      expect([await kit.raw(0), await kit.raw(7)]).toEqual(saved);
      expect(upRaw).not.toBe(saved[0]);
      expect(await words()).toEqual([bits(bias.down!), bits(1)]);
      await ui.fxPage(3);
      await runtime.ui.wait({ ms: 300, why: NO_PAGE_STATE });
      await shot(runtime.session, artifacts, 'gbp-page-loaded');
      await mainPage(runtime);
      await sound('e');
      await noFault();

      // The gain law on the master: the dry PCM of the first card, scaled. The value a case plays is the float word that the row holds.
      await compare(pcm.dry!, pcm.a!, 1, artifacts, 'bias-zero', true);
      await compare(pcm.dry!, pcm.c!, gainOf(bias.up!), artifacts, 'bias-up');
      await compare(pcm.dry!, pcm.b!, 1, artifacts, 'mix-zero-after-bias', true);
      // MIX blends the dry and the wet block: dry + mix * (wet - dry) = 1 + mix * (gain - 1).
      await compare(pcm.dry!, pcm.h!, Math.fround(1 + mixHalf * (gainOf(bias.up!) - 1)), artifacts, 'mix-half');
      await compare(pcm.dry!, pcm.d!, gainOf(bias.down!), artifacts, 'bias-down');
      await compare(pcm.dry!, pcm.e!, gainOf(bias.down!), artifacts, 'bias-loaded');

      // Send route, the fixture's default route (no stored choice): the amounts of track 1, the value word, the MIX word.
      const send = await boot('send', [SINE, FX]);
      expect((await row()).slice(18, 23)).toEqual([1, 0, 0, 0, 0]);
      expect(await words()).toEqual([0, bits(1)]);
      expect(send.capacity.SEND_SLOTS).toEqual({ limit: sendSlots, used: 1, largest: 0 });
      expect(await word(runtime.session, sym('fxp_send_count'))).toBe(2);
      await selectSine(runtime, profile);
      kit = pageKit(runtime, profile);
      ui = createFxKitUi(runtime, plan, profile, artifacts);
      await mainPage(runtime);
      await runtime.ui.tap('FX', { until: { drained: true } });
      await runtime.ui.tap('FX', { until: { drained: true } });
      await runtime.ui.wait({ ms: 400, why: NO_PAGE_STATE });
      await shot(runtime.session, artifacts, 'send-page2');
      await setAmount(runtime, profile, 'ENC A', 0, 40);
      await shot(runtime.session, artifacts, 'send-turning');
      await rowIs('the send amount of track 1 reaches the row', (r) => (r[19]! & 0xff) === 40);
      expect(await amounts()).toEqual([40, ...Array(15).fill(0)]);
      await ui.fxPage(3);
      await runtime.ui.wait({ ms: 300, why: NO_PAGE_STATE });
      await shot(runtime.session, artifacts, 'send-gbp-page');
      const sendRaw = await kit.setRaw('ENC A', 0, 0x6800);
      await rowIs('the SHIFT word follows the knob', (r) => r[4] === bits(biasFloat(sendRaw)));
      expect(await words()).toEqual([bits(biasFloat(sendRaw)), bits(1)]);
      expect((await row())[18]).toBe(1);
      expect((await amounts())[0]).toBe(40);
      const half = await kit.setRaw('LEVEL', 7, 0x4000);
      await rowIs('the MIX word follows the knob', (r) => Math.abs(floatWord(r[13]!) - half / 0x7f00) < 1e-4);
      await noFault();

      // Another track's amount does not reach the first track's row words, and the value of track 1 is not touched.
      await mainPage(runtime);
      await selectTrack(runtime, 2);
      await runtime.ui.tap('FX', { until: { drained: true } });
      await runtime.ui.tap('FX', { until: { drained: true } });
      await runtime.ui.wait({ ms: 400, why: NO_PAGE_STATE });
      await setAmount(runtime, profile, 'ENC A', 1, 100);
      await rowIs('the send amount of track 2 reaches the row', (r) => ((r[19]! >>> 8) & 0xff) === 100);
      expect(await amounts()).toEqual([40, 100, ...Array(14).fill(0)]);
      await selectTrack(runtime, 1);
      await ui.fxPage(3);
      const otherRaw = await kit.setRaw('ENC A', 0, 0x2800);
      await rowIs('the SHIFT word follows the knob', (r) => r[4] === bits(biasFloat(otherRaw)));
      expect((await row())[4]).toBe(bits(biasFloat(otherRaw)));
      expect(await amounts()).toEqual([40, 100, ...Array(14).fill(0)]);
      await noFault();
    });
  });
}
