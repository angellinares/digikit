import fs from 'node:fs';
import path from 'node:path';
import { encoder } from '../../../../packages/test-support/src/index.ts';
import { describe, expect, test } from 'vitest';
import { createFxKitUi } from '../../../../tests/regression/runtime/fx-kit-ui.ts';
import {
  advance,
  analysisJson,
  bounded,
  frames,
  rows,
  word,
} from '../../../../tests/regression/runtime/fx-live.ts';
import { SINE, triggerFrame } from '../../../../tests/regression/runtime/fixtures.ts';
import { bits, cstring, mainPage, openPages, pageKit } from '../../../../tests/regression/runtime/fx-pages-ui.ts';
import {
  inspectFxBoot,
  selectSine,
  shot,
  tap,
  withPreparedFxSession,
} from '../../../../tests/regression/runtime/fx-session.ts';
import { selectTrack, setAmount, trackNote } from '../../../../tests/regression/runtime/fx-track-ui.ts';
import { DEVICES, FIRMWARE, record } from '../../../../tests/regression/runtime/live.ts';
import { planSymbol } from '../../../../tests/regression/runtime/runtime-plan.ts';

const FX = 'digikit/freqshift';
const effectCard = { selection: ['modules/m0.dkm', 'modules/m1.dkm'] };
const shiftFloat = (raw: number) => Math.fround(-64 + Math.fround(127 * Math.fround(raw / 32512)));
const floatWord = (raw: number) => {
  const bytes = Buffer.alloc(4);
  bytes.writeUInt32LE(raw);
  return bytes.readFloatLE();
};

// The legacy runner exercises all four pristine-card legs on BOTH devices.
// In particular, the old DN2 TOML's "no send cells" header is stale.
for (const device of DEVICES) {
  describe.sequential(`${device.toUpperCase()} ${FIRMWARE[device]} frequency shifter`, () => {
    const context = withPreparedFxSession(device, 'frequency shifter', {
      image: {
        name: 'fx-freqshift',
        modules: ['core/runtime', 'core/dynreg', 'core/dyndsp', 'core/dynfx', 'core/fxpages'],
        inventory: [SINE, FX],
        symbols: [
          'DKRT_BOOT',
          'fx.rows',
          'fxp_count',
          'fxp_list_buf',
          'fxp_rows',
          'params.fp0.index',
          'pf_table',
          'fxp_send_count',
          'fxp_nload',
          'fxp_load_src',
        ],
        // The original shifter permits kit-library and runtime settings writes.
        policy: 'fx-routes',
      },
      initial: 'dry',
      presets: {
        dry: { selection: ['modules/m0.dkm'] },
        master: { ...effectCard, settings: [[`fx/route/${FX}`, '00']] },
        send: effectCard,
        other: effectCard,
      },
      // Its own dry amplitude > .01 and relative PCM oracles own audibility.
      requireAudible: false,
    });

    test('preserves page metadata, bipolar shift, kit roundtrip and master/send spectra across four clean cards', async () => {
      const { fixture, plan, profile, artifacts, slots, sendSlots } = context;
      const sym = (name: string) => planSymbol(plan, name);
      let runtime = fixture.runtime;
      const pcm: Record<string, string> = {};
      const shift: Record<string, number> = {};
      const mix: Record<string, number> = {};
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
        await trackNote(runtime, file, trigger);
        pcm[tag] = file;
      };
      const noFault = async () => expect((await row())[17]).toBe(0);

      await boot('dry', [SINE], false);
      await selectSine(runtime, profile);
      await mainPage(runtime.session);
      await sound('dry', true);

      const master = await boot('master', [SINE, FX]);
      await selectSine(runtime, profile);
      await openPages(runtime.session);
      for (let i = 0; i < 3; i++) await tap(runtime.session, 'DOWN', 300);
      await advance(runtime.session, 600);
      await shot(runtime.session, artifacts, 'fsh-page-default');
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
      expect(await cstring(runtime.session, await word(runtime.session, page))).toBe('FSH');
      expect(await cstring(runtime.session, await word(runtime.session, page + 4))).toBe('Freq Shifter');
      const param = (code: number) =>
        Promise.all(Array.from({ length: 15 }, (_, i) => word(runtime.session, sym('pf_table') + 60 * code + 4 * i)));
      const g = await param(first);
      const m = await param(first + 7);
      expect(g.slice(1, 5)).toEqual([profile.fx.value_indices[0], 0, 0x7f00, 0x4000]);
      expect(m.slice(1, 5)).toEqual([profile.fx.value_indices[7], 0, 0x7f00, 0x7f00]);
      expect(await Promise.all([g[10]!, g[12]!, m[10]!, m[12]!].map((at) => cstring(runtime.session, at)))).toEqual([
        'Freq Shift',
        'SHIFT',
        'Mix',
        'MIX',
      ]);
      let kit = pageKit(runtime.session, profile);
      let ui = createFxKitUi(runtime, plan, profile, artifacts);
      expect([await kit.raw(0), await kit.raw(7)]).toEqual([0x4000, 0x7f00]);
      expect(await words()).toEqual([0, bits(1)]);
      expect((await row()).slice(18, 23)).toEqual([0, 0, 0, 0, 0]);
      expect(master.capacity.SEND_SLOTS).toEqual({ limit: sendSlots, used: 0, largest: 0 });
      await mainPage(runtime.session);
      await sound('a');

      // Preserve the shifter's endpoint timings: approach, sixty fast events, settle.
      const endpoint = async (near: number, direction: number, target: number, tag: string) => {
        await ui.fxPage(3);
        await kit.setRaw('LEVEL', 7, near);
        for (let i = 0; i < 60; i++) {
          await bounded(() => runtime.session.client.inputTurn(encoder('LEVEL'), direction * 8));
          await advance(runtime.session, 2);
        }
        await advance(runtime.session, 100);
        await advance(runtime.session, 60);
        expect(await kit.raw(7)).toBe(target);
        expect((await words())[1]).toBe(target ? bits(1) : 0);
        await shot(runtime.session, artifacts, tag);
      };
      await endpoint(0x1000, -1, 0, 'fsh-page-mix-zero');
      await mainPage(runtime.session);
      await sound('b');
      await endpoint(0x7000, 1, 0x7f00, 'fsh-page-mix-top');
      const turn = async (raw: number, tag: string) => {
        await ui.fxPage(3);
        const got = await kit.setRaw('ENC A', 0, raw);
        await advance(runtime.session, 100);
        await shot(runtime.session, artifacts, `fsh-page-shift-${tag}`);
        const value = shiftFloat(got);
        expect(await words()).toEqual([bits(value), bits(1)]);
        shift[tag] = value;
      };
      await turn(0x6800, 'up');
      await mainPage(runtime.session);
      await sound('c');
      await turn(0x1800, 'down');
      await mainPage(runtime.session);
      await sound('d');
      await noFault();
      const saved = [await kit.raw(0), await kit.raw(7)];
      await mainPage(runtime.session);
      await ui.saveKit(2, 'fsh');
      await ui.fxPage(3);
      await kit.setRaw('ENC A', 0, 0x6800);
      await mainPage(runtime.session);
      const upRaw = await kit.raw(0);
      const loaded = await ui.loadKit(2, 'fsh');
      record(artifacts, 'fsh-load-observations', loaded);
      expect([await kit.raw(0), await kit.raw(7)]).toEqual(saved);
      expect(upRaw).not.toBe(saved[0]);
      expect(await words()).toEqual([bits(shift.down!), bits(1)]);
      shift.e = shift.down!;
      await ui.fxPage(3);
      await advance(runtime.session, 300);
      await shot(runtime.session, artifacts, 'fsh-page-loaded');
      await mainPage(runtime.session);
      await sound('e');
      await noFault();

      const send = await boot('send', [SINE, FX]);
      expect((await row()).slice(18, 23)).toEqual([1, 0, 0, 0, 0]);
      expect(await words()).toEqual([0, bits(1)]);
      expect(send.capacity.SEND_SLOTS).toEqual({ limit: sendSlots, used: 1, largest: 0 });
      expect(await word(runtime.session, sym('fxp_send_count'))).toBe(2);
      await selectSine(runtime, profile);
      kit = pageKit(runtime.session, profile);
      ui = createFxKitUi(runtime, plan, profile, artifacts);
      await mainPage(runtime.session);
      await tap(runtime.session, 'FX', 400);
      await tap(runtime.session, 'FX', 400);
      await shot(runtime.session, artifacts, 'send-page2');
      await setAmount(runtime.session, profile, 'ENC A', 0, 40);
      await shot(runtime.session, artifacts, 'send-turning');
      await advance(runtime.session, 300);
      expect(await amounts()).toEqual([40, ...Array(15).fill(0)]);
      await ui.fxPage(3);
      await advance(runtime.session, 300);
      await shot(runtime.session, artifacts, 'send-fsh-page');
      const sendRaw = await kit.setRaw('ENC A', 0, 0x6800);
      shift.send = shiftFloat(sendRaw);
      await advance(runtime.session, 100);
      expect(await words()).toEqual([bits(shift.send), bits(1)]);
      expect((await row())[18]).toBe(1);
      expect((await amounts())[0]).toBe(40);
      mix.send = floatWord((await row())[13]!);
      await mainPage(runtime.session);
      await sound('send');
      await ui.fxPage(3);
      const half = await kit.setRaw('LEVEL', 7, 0x4000);
      await advance(runtime.session, 100);
      mix.half = floatWord((await row())[13]!);
      expect(Math.abs(mix.half - half / 0x7f00)).toBeLessThan(1e-4);
      await mainPage(runtime.session);
      await sound('half');
      await noFault();

      await boot('other', [SINE, FX]);
      await selectSine(runtime, profile);
      ui = createFxKitUi(runtime, plan, profile, artifacts);
      kit = pageKit(runtime.session, profile);
      await mainPage(runtime.session);
      await selectTrack(runtime.session, 2);
      await tap(runtime.session, 'FX', 400);
      await tap(runtime.session, 'FX', 400);
      await setAmount(runtime.session, profile, 'ENC A', 1, 100);
      await advance(runtime.session, 300);
      expect(await amounts()).toEqual([0, 100, ...Array(14).fill(0)]);
      await selectTrack(runtime.session, 1);
      await ui.fxPage(3);
      const otherRaw = await kit.setRaw('ENC A', 0, 0x6800);
      await advance(runtime.session, 100);
      expect((await row())[4]).toBe(bits(shiftFloat(otherRaw)));
      await mainPage(runtime.session);
      await sound('other');

      const request = path.join(artifacts, 'freqshift-analysis-request.json');
      fs.writeFileSync(request, JSON.stringify({ module: FX, pcm, shift, mix }));
      const report = await analysisJson<{ pass: boolean; checks: { label: string; pass: boolean; detail: string }[] }>(
        'tools.patchkit.pk_digikit_fxshift',
        [request],
        artifacts,
        'freqshift-pcm-oracles',
      );
      expect(report.checks.length).toBeGreaterThan(0);
      for (const check of report.checks) expect(check.pass, `${check.label}: ${check.detail}`).toBe(true);
      expect(report.pass).toBe(true);
    }, 600_000);
  });
}
