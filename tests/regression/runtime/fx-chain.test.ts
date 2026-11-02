import path from 'node:path';
import { LOG_KIND } from '@digi/dk-session/gen_proto';
import { describe, expect, test } from 'vitest';
import type { PatchedSession } from '../support.ts';
import { analysisJson, bounded, captureNote, compare, FREE_PC, frames, rows, rpc, slotsFor, word } from './fx-live.ts';
import { GAIN, RT, SINE, triggerFrame } from './fixtures.ts';
import { chainCheck, fxExpectations } from './dkm.ts';
import { inspectFxBoot, selectSine, withPreparedFxSession } from './fx-session.ts';
import { DEVICES, FIRMWARE, record } from './live.ts';
import { readSmallRing } from './runtime-menu.ts';
import { planSymbol } from './runtime-plan.ts';

for (const device of DEVICES) {
  // These nested suites are ordered: the plain owned process closes before the hooked image starts.
  describe.sequential(`${device.toUpperCase()} ${FIRMWARE[device]} BASIC FX chain`, () => {
    const slots = slotsFor(device);
    const effects = Array.from({ length: slots + 1 }, (_, index) => (index === 0 ? GAIN : `${GAIN}-${index + 1}`));
    const inventory = [SINE, ...effects];
    const symbols = ['DKRT_BOOT', 'DKRT_ASSIGN', 'dynreg_info'];
    let plainPcm: string;

    const capture = async (runtime: PatchedSession, context: ReturnType<typeof withPreparedFxSession>, tag: string) => {
      const { session } = runtime;
      await rpc(session, 'run.mode', { mode: 'paused' });
      await rpc(session, 'pcm.tap', { action: 'start' });
      // DT2 checks the assigned sine type and selected-track readback; DN2 retains its hint-only choice.
      await selectSine(runtime, context.profile);
      const current = await frames(session);
      const trigger = triggerFrame(device);
      record(context.artifacts, `${tag}-timing`, { currentFrame: current, triggerFrame: trigger });
      const file = path.join(context.artifacts, `${tag}.wav`);
      // Rig.note: same instruction offset after the DSP boundary, held for the whole 1500-frame capture.
      await captureNote(session, { file, trigger, tag: `BASIC FX ${tag}`, tapStarted: true });
      const pcm = await analysisJson<{ peak: number }>(
        'tools.patchkit.pk_digikit_fx',
        ['pcm-peak', file],
        context.artifacts,
        `${tag}-peak`,
      );
      if (tag !== 'full') expect(pcm.peak, `${tag}: audible sine`).toBeGreaterThan(0.01);
      return file;
    };

    describe('plain image', () => {
      const context = withPreparedFxSession(device, 'BASIC FX plain', {
        image: { name: 'fx-plain', modules: RT, inventory, symbols, policy: 'default' },
        initial: 'plain',
        presets: { plain: { selection: ['modules/m0.dkm'] } },
      });
      test('loads the sine safely and renders the unhooked audible reference', async () => {
        const runtime = context.fixture.runtime;
        const state = await inspectFxBoot(runtime, context.plan, [SINE], context.artifacts, 'plain-boot');
        context.rememberEpoch(state.epoch);
        plainPcm = await capture(runtime, context, 'plain');
      });
    });

    describe('hooked image', () => {
      const context = withPreparedFxSession(device, 'BASIC FX dry/full', {
        image: {
          name: 'fx-chain',
          modules: [...RT, 'core/dynfx', 'core/fxpages'],
          inventory,
          symbols: [...symbols, 'fx.rows', '_dl_area', 'DKRT_TABLE_BAD', 'DKRT_LOG', 'DKRT_LOG_COUNT'],
          policy: 'default',
        },
        initial: 'dry',
        // Dry audibility and full relative PCM below own the legacy level thresholds.
        requireAudible: false,
        presets: {
          dry: { selection: ['modules/m0.dkm'] },
          full: { selection: inventory.map((_, index) => `modules/m${index}.dkm`) },
        },
      });
      // A single case performs the explicit dry -> full reset; no redundant beforeEach load of full.
      test('keeps free rows transparent and refuses overflow without changing the full serial gain chain', async () => {
        const { fixture, plan, artifacts } = context;
        const dry = fixture.runtime;
        const dryState = await inspectFxBoot(dry, plan, [SINE], artifacts, 'dry-boot');
        context.rememberEpoch(dryState.epoch);
        expect(dryState.capacity.FX_SLOTS).toEqual({ limit: slots, used: 0, largest: 0 });
        expect((await rows(dry.session, plan, slots)).map((row) => row[0])).toEqual(Array(slots).fill(FREE_PC));
        const dryPcm = await capture(dry, context, 'dry');
        expect(plainPcm, 'plain image reference ran before the hooked image').toBeDefined();
        await compare(plainPcm, dryPcm, 1, artifacts, 'plain-dry', true);
        // The fixture collects card overlay, PCM, guard and fault evidence before discarding dry.
        const full = await fixture.reload(context.baseline('full'));
        const fullState = await inspectFxBoot(full, plan, [SINE, ...effects.slice(0, slots)], artifacts, 'full-boot', [
          ...Array(1 + slots).fill(0),
          -34,
        ]);
        context.rememberEpoch(fullState.epoch);
        expect(fullState.modules).toHaveLength(1 + slots);
        expect(fullState.capacity.FX_SLOTS).toEqual({ limit: slots, used: slots, largest: 0 });
        const chain = await rows(full.session, plan, slots);
        const card = full.session.cardPath;
        if (!card) throw new Error('BASIC FX requires the loaded card inventory');
        const effectFiles = effects.map((_, index) =>
          path.join(path.dirname(card), `seed/digikit/modules/m${index + 1}.dkm`),
        );
        for (const [index, effect] of fullState.modules.slice(1).entries()) {
          const base = planSymbol(plan, '_dl_area') + effect.dsp_off;
          const oracle = await fxExpectations(effectFiles[index]!, base);
          record(artifacts, `full-row-${index}-oracle`, oracle);
          expect(chain[index]!.slice(0, 12), `row ${index}: exact pc/descriptor/state/enabled/defaults`).toEqual(
            oracle.row,
          );
        }
        const snapshot = { modules: fullState.modules, capacity: fullState.capacity };
        record(artifacts, 'full-capacity', snapshot);
        const fit = await chainCheck(snapshot, effectFiles);
        record(artifacts, 'full-fit', fit);
        expect(fit.ranges_ok, 'overflow leaves pairwise disjoint DSP ranges').toBe(true);
        expect((await word(full.session, planSymbol(plan, 'DKRT_TABLE_BAD'))) >>> 24).toBe(0);
        const events = await bounded(() => readSmallRing(full.session, plan));
        record(artifacts, 'full-events', events);
        expect(events.map((event) => event.kind)).not.toContain(LOG_KIND.TABLE_BAD);
        expect(fit.live.each[0]).toMatchObject({
          resource: 'FX_SLOTS',
          need: 1,
          free: 0,
          describe: 'effect slots 1 needed, 0 free',
          text: 'FX 1 > 0 FREE',
        });
        expect(fit.empty.set).toMatchObject({ index: slots, resource: 'FX_SLOTS', need: 1, free: 0 });
        const fullPcm = await capture(full, context, 'full');
        // Retained tolerances: <=2 ULPs, <0.001 dB, and <=128 onset samples.
        await compare(dryPcm, fullPcm, 0.5 ** slots, artifacts, 'full');
      });
    });
  });
}
