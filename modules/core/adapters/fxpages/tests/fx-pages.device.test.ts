import path from 'node:path';
import { DkSession, EmuUsbTransport } from '@digi/dk-session';
import { describe, expect, test } from 'vitest';
import type { PatchedSession } from '@digi/regression/support.ts';
import { advance, analysisJson, bounded, frames, rows, rpc, word } from '@digi/regression/runtime/fx-live.ts';
import { GAIN, SINE, triggerFrame } from '@digi/regression/runtime/fixtures.ts';
import {
  inspectFxBoot,
  selectSine,
  shot,
  tap,
  withPausedFxProtocol,
  withPreparedFxSession,
} from '@digi/regression/runtime/fx-session.ts';
import {
  bits,
  changed,
  cstring,
  effectPage,
  expectNoPages,
  knobFloat,
  mainPage,
  openPages,
  pageKit,
  pageState,
  stockPages,
} from '@digi/regression/runtime/fx-pages-ui.ts';
import { trackNote } from '@digi/regression/runtime/fx-track-ui.ts';
import { record } from '@digi/regression/runtime/artifacts.ts';
import { DEVICES, FIRMWARE } from '@digi/regression/runtime/devices.ts';
import { expectRuntimeUi } from '@digi/regression/runtime/ready.ts';
import { planSymbol } from '@digi/regression/runtime/runtime-plan.ts';

const RT = ['core/runtime', 'core/dynreg', 'core/dyndsp', 'core/dynfx'];
const inventory = [SINE, GAIN];
const presets = {
  stock: { selection: ['modules/m0.dkm'] },
  effect: { selection: ['modules/m0.dkm', 'modules/m1.dkm'] },
};

interface PcmMetrics {
  identical: boolean;
  rms_db: number | null;
  max_err_ulps: number;
  shift: number | null;
}

async function metrics(base: string, sample: string, gain: number, artifacts: string, tag: string) {
  return analysisJson<PcmMetrics>(
    'tools.patchkit.pk_digikit_fxpage',
    ['compare', base, sample, '--gain', String(gain)],
    artifacts,
    `pcm-${tag}`,
  );
}

async function exact(base: string, sample: string, artifacts: string, tag: string) {
  const result = await metrics(base, sample, 1, artifacts, tag);
  expect(result.identical, `${tag}: bit-identical PCM from onset`).toBe(true);
  expect(result.shift).not.toBeNull();
  expect(Math.abs(result.shift!)).toBeLessThanOrEqual(128);
}

async function scaled(base: string, sample: string, gain: number, ulps: number, artifacts: string, tag: string) {
  const result = await metrics(base, sample, gain, artifacts, tag);
  expect(result.rms_db, `${tag}: audible PCM`).not.toBeNull();
  expect(result.max_err_ulps).toBeLessThanOrEqual(ulps);
  expect(Math.abs(result.rms_db! - 20 * Math.log10(gain))).toBeLessThan(0.001);
  expect(result.shift).not.toBeNull();
  expect(Math.abs(result.shift!)).toBeLessThanOrEqual(128);
}

for (const device of DEVICES) {
  describe.sequential(`${device.toUpperCase()} ${FIRMWARE[device]} FUNC + FX effect pages`, () => {
    let referenceHashes: string[];
    let referencePcm: string;
    const trigger = triggerFrame(device);

    describe('image without the page hook', () => {
      const context = withPreparedFxSession(device, 'FX page reference', {
        image: { name: 'fxpage-step1', modules: RT, inventory, symbols: ['DKRT_BOOT', 'fx.rows'], policy: 'default' },
        initial: 'stock',
        presets: { stock: presets.stock },
        // Legacy relative PCM oracles own audibility; finite and safety evidence stays mandatory.
        requireAudible: false,
      });
      test('captures the three original send pages and dry sine', async () => {
        const { fixture, plan, profile, artifacts } = context;
        const runtime = fixture.runtime;
        const state = await inspectFxBoot(runtime, plan, [SINE], artifacts, 'reference-boot');
        context.rememberEpoch(state.epoch);
        await selectSine(runtime, profile);
        referenceHashes = await stockPages(runtime.session, artifacts, 'reference');
        await mainPage(runtime.session);
        referencePcm = path.join(artifacts, 'reference.wav');
        await trackNote(runtime, referencePcm, trigger);
      });
    });

    describe('image with the page hook', () => {
      const context = withPreparedFxSession(device, 'FX pages stock/effect/deselect', {
        image: {
          name: 'fxpage',
          modules: [...RT, 'core/fxpages'],
          inventory,
          symbols: [
            'DKRT_BOOT',
            'fx.rows',
            'DKRT_FXP',
            'fxp_count',
            'fxp_list_buf',
            'fxp_rows',
            'params.fp0.index',
            'pf_table',
          ],
          policy: 'default',
        },
        initial: 'stock',
        presets,
        // Legacy relative PCM oracles own audibility; finite and safety evidence stays mandatory.
        requireAudible: false,
      });

      test('preserves stock pixels/audio, maps page knobs, and removes the effect through SELECT/restart', async () => {
        const { fixture, plan, profile, artifacts, slots } = context;
        const sym = (name: string) => planSymbol(plan, name);
        const inspect = async (runtime: PatchedSession, tag: string, effect: boolean, afterEpoch?: number) => {
          const state = await inspectFxBoot(runtime, plan, effect ? [SINE, GAIN] : [SINE], artifacts, tag, undefined, {
            afterEpoch,
          });
          context.rememberEpoch(state.epoch);
          return state;
        };
        const note = async (runtime: PatchedSession, tag: string, at?: number) => {
          const file = path.join(artifacts, `${tag}.wav`);
          const current = await frames(runtime.session);
          const chosen = at ?? current + 300;
          record(artifacts, `${tag}-timing`, { currentFrame: current, triggerFrame: chosen });
          await trackNote(runtime, file, chosen);
          return file;
        };
        const rowWords = async (runtime: PatchedSession) => {
          const row = (await rows(runtime.session, plan, slots))[0]!;
          return [row[4], row[13]];
        };

        const stock = fixture.runtime;
        await inspect(stock, 'stock-boot', false);
        await expectNoPages(stock, plan);
        await selectSine(stock, profile);
        expect(await stockPages(stock.session, artifacts, 'stock'), 'stock DEL/REV/CHO pixels').toEqual(
          referenceHashes,
        );
        await mainPage(stock.session);
        const dry = await note(stock, 'stock', trigger);
        await exact(referencePcm, dry, artifacts, 'stock');
        const control = await note(stock, 'stock-control');
        const stockKit = pageKit(stock.session, profile);
        const indices = [...profile.fx.value_indices, profile.fx.mark_index];
        const quiet = await stockKit.snapshot();
        await advance(stock.session, 300);
        const drift = changed(quiet, await stockKit.snapshot());
        const before = await stockKit.snapshot();
        for (const [n, index] of indices.entries()) await stockKit.poke(index, 0x1000 + 0x111 * n);
        await advance(stock.session, 500);
        const after = await stockKit.snapshot();
        expect(
          indices.map((index) => after[index]),
          'free value indices retain distinct writes',
        ).toEqual(indices.map((_, n) => 0x1000 + 0x111 * n));
        expect(changed(before, after).filter((index) => !indices.includes(index) && !drift.includes(index))).toEqual(
          [],
        );
        const poked = await note(stock, 'stock-poked');
        const unchanged = await metrics(control, poked, 1, artifacts, 'free-indices');
        expect(unchanged.rms_db).not.toBeNull();
        expect(Math.abs(unchanged.rms_db!)).toBeLessThan(0.2);
        expect(unchanged.shift).not.toBeNull();
        expect(Math.abs(unchanged.shift!)).toBeLessThanOrEqual(512);

        // A clean effect card discards stock pokes; the fixture exports safety evidence first.
        const effect = await fixture.reload(context.baseline('effect'));
        await inspect(effect, 'effect-boot', true);
        await selectSine(effect, profile);
        await openPages(effect.session);
        const hashes: string[] = [];
        for (let page = 0; page < 4; page++) {
          if (page) await tap(effect.session, 'DOWN', 300);
          hashes.push(await shot(effect.session, artifacts, `effect-page${page + 1}`));
        }
        expect(new Set(hashes).size, 'four distinct send FX pages').toBe(4);
        const state = await pageState(effect, plan);
        record(artifacts, 'effect-page-state', state);
        expect(state.count).toBe(4);
        const listed = await Promise.all(
          Array.from({ length: state.count }, (_, i) => word(effect.session, sym('fxp_list_buf') + 4 * i)),
        );
        expect(listed).toEqual([...(device === 'dt2' ? [13, 14, 15] : [17, 18, 19]), profile.fx.page_first]);
        const row = sym('fxp_rows');
        const first = sym('params.fp0.index');
        const codes = await Promise.all(Array.from({ length: 9 }, (_, i) => word(effect.session, row + 8 + 4 * i)));
        expect(codes, 'GAIN at knob A, empty B-H, MIX at LEVEL').toEqual([first, 0, 0, 0, 0, 0, 0, 0, first + 7]);
        expect(await cstring(effect.session, await word(effect.session, row))).toBe('GAN');
        expect(await cstring(effect.session, await word(effect.session, row + 4))).toBe('Test Gain');
        const param = (code: number) =>
          Promise.all(Array.from({ length: 15 }, (_, i) => word(effect.session, sym('pf_table') + 60 * code + 4 * i)));
        const gain = await param(first);
        const mix = await param(first + 7);
        expect(gain.slice(1, 5), 'GAIN mirror/range/default').toEqual([profile.fx.value_indices[0], 0, 0x7f00, 0x3200]);
        expect(mix.slice(1, 5), 'MIX mirror/range/default').toEqual([profile.fx.value_indices[7], 0, 0x7f00, 0x7f00]);
        expect(
          await Promise.all([gain[10]!, gain[12]!, mix[10]!, mix[12]!].map((at) => cstring(effect.session, at))),
        ).toEqual(['Gain', 'GAIN', 'Mix', 'MIX']);
        const kit = pageKit(effect.session, profile);
        expect([await kit.raw(0), await kit.raw(7)], 'live kit defaults').toEqual([0x3200, 0x7f00]);
        expect(await rowWords(effect), 'DSP row defaults').toEqual([bits(0.5), bits(1)]);
        await mainPage(effect.session);
        const a = await note(effect, 'effect-a', trigger);

        await effectPage(effect.session);
        const quietEffect = await kit.snapshot();
        await advance(effect.session, 300);
        const effectDrift = changed(quietEffect, await kit.snapshot());
        const beforeTurn = await kit.snapshot();
        const rawGain = await kit.setRaw('ENC A', 0, 0x6400);
        const moved = changed(beforeTurn, await kit.snapshot());
        const own = profile.fx.value_indices[0]!;
        expect(moved, 'GAIN changes its own kit index').toContain(own);
        expect(
          moved.filter((index) => index !== own && !effectDrift.includes(index)),
          'no stock parameter changed',
        ).toEqual([]);
        await shot(effect.session, artifacts, 'effect-page4-gain-turned');
        const gainValue = knobFloat(rawGain);
        expect(await rowWords(effect), 'float32 GAIN conversion leaves MIX at one').toEqual([bits(gainValue), bits(1)]);
        await mainPage(effect.session);
        const b = await note(effect, 'effect-b');

        await effectPage(effect.session);
        const rawMix = await kit.setRaw('LEVEL', 7, 0x4000);
        await shot(effect.session, artifacts, 'effect-page4-mix-turned');
        const mixValue = Math.fround(Math.fround(rawMix) / Math.fround(32512));
        expect(await rowWords(effect), 'float32 MIX normalization preserves GAIN').toEqual([
          bits(gainValue),
          bits(mixValue),
        ]);
        await mainPage(effect.session);
        const c = await note(effect, 'effect-c');

        await effectPage(effect.session);
        await kit.minimum('LEVEL');
        expect(await kit.raw(7)).toBe(0);
        expect((await rowWords(effect))[1]).toBe(0);
        await shot(effect.session, artifacts, 'effect-page4-mix-zero');
        await mainPage(effect.session);
        const d = await note(effect, 'effect-d');

        await effectPage(effect.session);
        await kit.maximum('LEVEL', 7);
        expect(await kit.raw(7)).toBe(0x7f00);
        expect((await rowWords(effect))[1]).toBe(bits(1));
        await mainPage(effect.session);
        const e = await note(effect, 'effect-e');
        const transferred = await pageState(effect, plan);
        record(artifacts, 'effect-transferred', transferred);
        expect(transferred.errors).toBe(0);
        expect(transferred.sent).toBeGreaterThanOrEqual(4);
        expect([transferred.current[0], transferred.current[7]]).toEqual([transferred.dsp[0], transferred.dsp[7]]);
        await scaled(dry, a, 0.5, 2, artifacts, 'effect-a');
        await scaled(dry, b, gainValue, 3, artifacts, 'effect-b');
        await scaled(dry, c, 1 + mixValue * (gainValue - 1), 4, artifacts, 'effect-c');
        await exact(dry, d, artifacts, 'effect-d');
        await scaled(dry, e, gainValue, 3, artifacts, 'effect-e');

        // Real guest SELECT/restart retains writes from the effect boot.
        await context.evidence(effect, 'effect-before-deselect');
        await withPausedFxProtocol(effect, (dk) => dk.select(['modules/m0.dkm']));
        const epoch = (await bounded(() => effect.session.status())).epoch;
        expect(typeof epoch).toBe('number');
        context.rememberEpoch(epoch);
        await rpc(effect.session, 'run.mode', { mode: 'free' });
        await bounded(() => effect.dk.restart());
        effect.dk.cancelPending();
        const dk = new DkSession(new EmuUsbTransport((method, params) => effect.session.client.call(method, params)));
        const reboot = { session: effect.session, dk, close: effect.close };
        try {
          await expectRuntimeUi(effect.session, plan, artifacts, { afterEpoch: Number(epoch) });
          await bounded(() => effect.session.client.guardSet(context.baseline('effect').guard));
          await inspect(reboot, 'gone-boot', false, Number(epoch));
          await expectNoPages(reboot, plan);
          await selectSine(reboot, profile);
          expect(await stockPages(reboot.session, artifacts, 'gone'), 'stock pixels after effect removal').toEqual(
            referenceHashes,
          );
          await mainPage(reboot.session);
          await exact(referencePcm, await note(reboot, 'gone'), artifacts, 'gone');
          await context.evidence(reboot, 'gone');
        } finally {
          dk.cancelPending();
        }
      });
    });
  });
}
