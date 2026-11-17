import path from 'node:path';
import type { DkSession } from '@digi/dk-session';
import { describe, expect, test } from 'vitest';
import { withWaitApi, type TestRuntime } from '@digi/regression/support.ts';
import { bounded, compare, pcmMetrics, frames, rows, rpc, word, REDRAW, SETTLE } from '@digi/regression/runtime/fx-live.ts';
import { GAIN, SINE } from '@digi/regression/runtime/fixtures.ts';
import { createFxKitUi, watchFxKitRows, type FxKitObservation } from '@digi/regression/runtime/fx-kit-ui.ts';
import { knobFloat, mainPage, pageKit } from '@digi/regression/runtime/fx-pages-ui.ts';
import {
  inspectFxBoot,
  selectSine,
  shot,
  withPausedFxProtocol,
  withPreparedFxSession,
} from '@digi/regression/runtime/fx-session.ts';
import { halfword, trackNote } from '@digi/regression/runtime/fx-track-ui.ts';
import { record } from '@digi/regression/runtime/artifacts.ts';
import { DEVICES, FIRMWARE } from '@digi/regression/runtime/devices.ts';
import { expectRuntimeUi } from '@digi/regression/runtime/ready.ts';
import { symbolsOf } from '@digi/regression/runtime/service-symbols.ts';
import { emulatorDk } from '@digi/regression/runtime/waits.ts';

const GAIN_B = 'test/dyn-fx-gain-b';
const MARK = 0x46a7;
const defaultsAB = [
  0x3200,
  ...Array<null>(6).fill(null),
  0x7f00,
  0x1900,
  ...Array<null>(6).fill(null),
  0x4000,
  ...Array<null>(8).fill(null),
];
const defaultsBA = [...defaultsAB.slice(8, 16), ...defaultsAB.slice(0, 8), ...defaultsAB.slice(16)];
const isDefault = (got: number[], want: (number | null)[]) =>
  want.every((value, i) => value === null || Math.abs(got[i]! - value) <= 1);
const expectedRecord = (ids: number[], values: number[], size: number) => {
  const result = ids.flatMap((id, row) => (id ? values.slice(row * 8, row * 8 + 8) : Array<number>(8).fill(0)));
  result.push(...ids);
  return [...result, ...Array<number>(size - result.length).fill(0)];
};
const factor = (values: number[]) =>
  [0, 1].reduce(
    (gain, row) =>
      gain *
      (1 + Math.fround(Math.fround(values[row * 8 + 7]!) / Math.fround(0x7f00)) * (knobFloat(values[row * 8]!) - 1)),
    1,
  );

for (const device of DEVICES) {
  describe(`${device.toUpperCase()} ${FIRMWARE[device]} FX kit/project persistence`, () => {
    const full = device === 'dt2';
    const context = withPreparedFxSession(device, 'FX kit/project persistence', {
      image: {
        name: 'fx-kit-persist',
        modules: ['core/runtime', 'core/dynfx', 'core/fxpages'],
        inventory: [SINE, GAIN, GAIN_B],
        symbols: [
          'DKRT_BOOT',
          'fx.rows',
        ],
        policy: 'fx-kit',
      },
      initial: full ? 'stock' : 'effects',
      presets: {
        stock: { selection: ['modules/m0.dkm'] },
        effects: { selection: ['modules/m0.dkm', 'modules/m1.dkm', 'modules/m2.dkm'] },
      },
      // The relative PCM comparisons below retain the legacy audibility oracle.
      requireAudible: false,
    });

    test('round-trips kit/project values and seeds defaults for absent or reordered effect IDs', async () => {
      const { fixture, plan, profile, artifacts } = context;
      let runtime: TestRuntime = fixture.runtime;
      const session = runtime.session;
      const fx = profile.fx;
      const count = fx.value_indices.length / 8;
      // the state of the hooks is in the fx service file: `symbolsOf` has its symbols after each boot (`inspectFxBoot`)
      const sym = (name: string) => symbolsOf(runtime, plan)(name);
      const cell = (name: string) => word(session, sym(name));
      const halfwords = async (base: number, n: number) =>
        Promise.all(Array.from({ length: n }, (_, i) => halfword(session, base + 2 * i)));
      const ids = () => halfwords(sym('fxp_ids'), count);
      const kit = pageKit(runtime, profile);
      const kitBase = async () => (await word(session, fx.kit_ptr)) + fx.array_off;
      const values = async () => {
        const base = await kitBase();
        return Promise.all(fx.value_indices.map((index) => halfword(session, base + 2 * index)));
      };
      const stock = async () => halfword(session, (await kitBase()) + 64);
      const mark = async () => halfword(session, (await kitBase()) + 2 * fx.mark_index);
      const spare = async (base: number) => halfwords(base + fx.store_off, fx.store_halfwords);
      const rowWords = async () => (await rows(session, plan, count)).slice(0, 2).map((row) => [row[4], row[13]]);
      const poke16 = async (at: number, value: number) => {
        for (let byte = 0; byte < 2; byte++) {
          const address = at + byte;
          const shift = 8 * (3 - (address % 4));
          const old = await word(session, address & ~3);
          await rpc(session, 'mem.cf.poke', {
            addr: address & ~3,
            value: ((old & ~(0xff << shift)) | (((value >>> (8 * (1 - byte))) & 0xff) << shift)) >>> 0,
          });
        }
      };
      const ui = () => createFxKitUi(runtime, plan, profile, artifacts);
      const noZero = (tag: string, seen: FxKitObservation[]) => {
        record(artifacts, `${tag}-row-transitions`, seen);
        expect(
          seen.filter(([, , p0, mix]) => p0 === 0 || mix === 0),
          `${tag}: no transient zero p0/mix`,
        ).toEqual([]);
      };
      const inspect = async (names: string[], tag: string, afterEpoch?: number) => {
        const boot = await inspectFxBoot(runtime, plan, names, artifacts, tag, undefined, { afterEpoch });
        context.rememberEpoch(boot.epoch);
      };
      const protocols: DkSession[] = [];
      const restart = async (selection: string[], tag: string, names: string[]) => {
        await context.evidence(runtime, `${tag}-before-restart`);
        await withPausedFxProtocol(runtime, (dk) => dk.select(selection));
        const epoch = (await bounded(() => session.status())).epoch;
        expect(typeof epoch).toBe('number');
        context.rememberEpoch(epoch);
        await rpc(session, 'run.mode', { mode: 'free' });
        await bounded(() => runtime.dk.restart());
        runtime.dk.cancelPending();
        const dk = emulatorDk(session);
        protocols.push(dk);
        runtime = withWaitApi({ session, dk, close: runtime.close });
        // Keep the original epoch for both barriers; neither silence nor a stale UI proves restart.
        await expectRuntimeUi(session, plan, artifacts, { afterEpoch: Number(epoch) });
        await bounded(() => session.client.guardSet(context.baseline('effects').guard));
        await inspect(names, `${tag}-boot`, Number(epoch));
      };
      const pcm = new Map<string, string>();
      const note = async (tag: string) => {
        await mainPage(runtime);
        const file = path.join(artifacts, `${tag}.wav`);
        await trackNote(runtime, file, (await frames(session)) + 300);
        pcm.set(tag, file);
      };
      const ratio = (base: string, sample: string, gain: number, identical = false) =>
        compare(pcm.get(base)!, pcm.get(sample)!, gain, artifacts, sample, identical);
      const page = async (row: number, tag: string) => {
        await ui().fxPage(3 + row);
        await runtime.ui.wait({ ms: 600, why: SETTLE });
        return shot(session, artifacts, tag);
      };
      try {
        let stockK1 = 0x4800;
        if (full) {
          await inspect([SINE], 'stock-boot');
          await selectSine(runtime, profile);
          const raw = await stock();
          await kit.poke(32, 0x4800);
          stockK1 = await stock();
          expect(stockK1).not.toBe(raw);
          // the stock boot has no fx service (no hook cells): only the stock kit values are checked
          await ui().saveKit(1, 'k1');
          await kit.poke(32, 0x2000);
          const moved = await stock();
          expect(moved).not.toBe(stockK1);
          await ui().loadKit(1, 'k1');
          expect(Math.abs((await stock()) - stockK1)).toBeLessThanOrEqual(0x40);
          expect(await stock()).not.toBe(moved);
          await restart(['modules/m0.dkm', 'modules/m1.dkm', 'modules/m2.dkm'], 'effects', [SINE, GAIN, GAIN_B]);
        } else await inspect([SINE, GAIN, GAIN_B], 'effects-boot');
        const effectIds = await ids();
        expect(effectIds[0]).toBe(0x4625);
        expect(effectIds[1]).not.toBe(0);
        expect(effectIds[1]).not.toBe(effectIds[0]);
        expect(effectIds[2]).toBe(0);
        console.info(
          `fx service counters: ${(await Promise.all(['fxp_ninit', 'fxp_nload', 'fxp_nsave', 'fxp_nsound'].map(async (n) => `${n}=${await cell(n)}`))).join(' ')}`,
        );
        // the init hook is not observable in this test (boot init runs before fx.dks binds; Load Kit does not call the initialiser, finding 61); the service seeds a live kit on activation and tick
        expect(await word(session, sym('DKRT_FXP') + 24)).toBe(2);
        expect(await mark()).toBe(MARK);
        expect(isDefault(await values(), defaultsAB)).toBe(true);
        await selectSine(runtime, profile);
        const gotDefault = await values();
        expect(isDefault(gotDefault, defaultsAB)).toBe(true);
        const defaultValues = gotDefault.map((value, i) => defaultsAB[i] ?? value);
        if (!full) {
          // DN2 quick setup emits the same zero-ID record as a sine-only boot.
          await kit.poke(32, stockK1);
          for (let row = 0; row < count; row++) await poke16(sym('fxp_ids') + row * 2, 0);
          try {
            await ui().saveKit(1, 'k1');
          } finally {
            for (let row = 0; row < count; row++) await poke16(sym('fxp_ids') + row * 2, effectIds[row]!);
          }
          expect(await spare(await cell('fxp_save_dst'))).toEqual(Array(fx.store_halfwords).fill(0));
          await mainPage(runtime);
        }
        await note('default');
        const turned: number[] = [];
        for (const [row, gain, mix] of [
          [0, 0x6400, 0x4000],
          [1, 0x2800, 0x2000],
        ] as const) {
          await ui().fxPage(3 + row);
          const rowKit = pageKit(runtime, profile, row);
          turned.push(await rowKit.setRaw('ENC A', 0, gain), await rowKit.setRaw('LEVEL', 7, mix));
        }
        await kit.poke(32, 0x3000);
        const stockSaved = await stock();
        await mainPage(runtime);
        const saved = await values();
        expect([saved[0], saved[7], saved[8], saved[15]]).toEqual(turned);
        await note('turned');
        await ratio('default', 'turned', factor(saved) / factor(defaultValues));
        const wordsSaved = await rowWords();
        const hashes = [await page(0, 'fx-page4-turned'), await page(1, 'fx-page5-turned')];
        await mainPage(runtime);
        const firstKit = await word(session, fx.kit_ptr);
        await runtime.ui.tap('PTN', { until: { ms: 800, why: REDRAW } });
        await runtime.ui.tap('TRIG 2', { until: { ms: 300, why: REDRAW } });
        let waited = 300;
        while ((await mark()) !== MARK && waited < 8000) {
          await runtime.ui.wait({ ms: 100, why: SETTLE });
          waited += 100;
        }
        expect(await word(session, fx.kit_ptr), 'A02 owns another kit object').not.toBe(firstKit);
        expect(isDefault(await values(), defaultsAB)).toBe(true);
        expect(await mark()).toBe(MARK);
        expect((await values()).slice(0, 16)).not.toEqual(saved.slice(0, 16));
        await runtime.ui.wait({ ms: 1200, why: SETTLE });
        await runtime.ui.tap('PTN', { until: { ms: 800, why: REDRAW } });
        await runtime.ui.tap('TRIG 1', { until: { ms: 1500, why: REDRAW } });
        expect(await word(session, fx.kit_ptr), 'A01 retains its kit object').toBe(firstKit);
        expect((await values()).slice(0, 16)).toEqual(saved.slice(0, 16));
        const saves = await cell('fxp_nsave');
        await ui().saveKit(2, 'k2');
        expect(await cell('fxp_nsave')).toBeGreaterThan(saves);
        const source = await cell('fxp_last_save');
        const sourceValues = await Promise.all(fx.value_indices.map((index) => halfword(session, source + 2 * index)));
        expect(await spare(await cell('fxp_save_dst'))).toEqual(
          expectedRecord(effectIds, sourceValues, fx.store_halfwords),
        );
        for (const [row, gain] of [
          [0, 0x1800],
          [1, 0x5400],
        ] as const) {
          await kit.poke(fx.value_indices[row * 8]!, gain);
          await kit.poke(fx.value_indices[row * 8 + 7]!, 0x7f00);
        }
        await kit.poke(32, 0x1000);
        await runtime.ui.wait({ ms: 300, why: SETTLE });
        await mainPage(runtime);
        const changed = await values();
        for (const index of [0, 7, 8, 15]) expect(changed[index]).not.toBe(saved[index]);
        await note('changed');
        const loads = await cell('fxp_nload');
        const loaded = await ui().loadKit(2, 'k2');
        expect(await cell('fxp_nload')).toBeGreaterThan(loads);
        expect((await values()).slice(0, 16)).toEqual(saved.slice(0, 16));
        expect(loaded.file).toEqual(expectedRecord(effectIds, saved, fx.store_halfwords));
        expect(Math.abs((await stock()) - stockSaved)).toBeLessThanOrEqual(0x40);
        expect(await rowWords()).toEqual(wordsSaved);
        noZero('kit', loaded.seen);
        await note('loaded');
        await ratio('turned', 'loaded', 1, true);
        const differs = await pcmMetrics(pcm.get('changed')!, pcm.get('loaded')!, 1, artifacts, 'changed-v-loaded');
        expect(differs.identical).toBe(false);
        expect([await page(0, 'fx-page4-loaded'), await page(1, 'fx-page5-loaded')]).toEqual(hashes);
        await mainPage(runtime);
        const stockLoaded = await ui().loadKit(1, 'k1b');
        const defaults = await values();
        expect(isDefault(defaults, defaultsAB)).toBe(true);
        expect(defaults.slice(0, 16)).not.toEqual(saved.slice(0, 16));
        noZero('stock-kit', stockLoaded.seen);
        expect(stockLoaded.file).toEqual(Array(fx.store_halfwords).fill(0));
        expect(Math.abs((await stock()) - stockK1)).toBeLessThanOrEqual(0x40);
        await note('again');
        await ratio('default', 'again', 1);
        const projectValues = new Map([
          [0, 0x5200],
          [7, 0x3000],
          [8, 0x1000],
          [15, 0x6000],
        ]);
        for (const [index, value] of projectValues) await kit.poke(fx.value_indices[index]!, value);
        await runtime.ui.wait({ ms: 300, why: SETTLE });
        const projectWords = await rowWords();
        const beforeSave = await cell('fxp_nsave');
        await runtime.ui.tap('SETTINGS', { until: { ms: 300, why: REDRAW }, held: 'FUNC' });
        await shot(session, artifacts, 'project-save-menu');
        await runtime.ui.tap('YES', { until: { ms: 500, why: REDRAW } });
        await runtime.ui.tap('YES', { until: { ms: 500, why: REDRAW } });
        await runtime.ui.wait({ ms: 3500, why: SETTLE });
        await shot(session, artifacts, 'project-saved');
        expect((await cell('fxp_nsave')) - beforeSave).toBeGreaterThanOrEqual(100);
        for (const [index, value] of projectValues)
          await kit.poke(fx.value_indices[index]!, 0x7f00 - Math.floor(value / 2));
        await runtime.ui.wait({ ms: 300, why: SETTLE });
        await runtime.ui.tap('SETTINGS', { until: { ms: 300, why: REDRAW } });
        for (let i = 0; i < 3; i++) await runtime.ui.tap('YES', { until: { ms: 500, why: REDRAW } });
        const projectSeen = await watchFxKitRows(runtime, plan, profile, 'YES', 7000, 10);
        await shot(session, artifacts, 'project-loaded');
        const projectGot = await values();
        expect([...projectValues.keys()].map((index) => projectGot[index])).toEqual([...projectValues.values()]);
        expect(await rowWords()).toEqual(projectWords);
        noZero('project', projectSeen);
        if (full) {
          await restart(['modules/m0.dkm', 'modules/m2.dkm', 'modules/m1.dkm'], 'swap', [SINE, GAIN_B, GAIN]);
          expect((await ids()).slice(0, 3)).toEqual([effectIds[1], effectIds[0], 0]);
          await selectSine(runtime, profile);
        } else {
          const defs = sym('fxp_defs');
          const a = await halfwords(defs, 8);
          const b = await halfwords(defs + 16, 8);
          await poke16(sym('fxp_ids'), effectIds[1]!);
          await poke16(sym('fxp_ids') + 2, effectIds[0]!);
          for (let i = 0; i < 8; i++) {
            await poke16(defs + 2 * i, b[i]!);
            await poke16(defs + 16 + 2 * i, a[i]!);
          }
          expect((await ids()).slice(0, 3)).toEqual([effectIds[1], effectIds[0], 0]);
        }
        const swapped = await ui().loadKit(2, 'swap');
        const swappedValues = await values();
        expect(isDefault(swappedValues, defaultsBA)).toBe(true);
        expect(swappedValues[0]).not.toBe(saved[0]);
        noZero('swap', swapped.seen);
        await context.evidence(runtime, 'persistence-complete');
      } finally {
        for (const dk of protocols) dk.cancelPending();
      }
    });
  });
}
