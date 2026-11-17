import path from 'node:path';
import { LOG_KIND } from '@digi/dk-session/gen_proto';
import { describe, expect, test } from 'vitest';
import { withWaitApi, type TestRuntime } from '@digi/regression/support.ts';
import {
  analysisJson,
  bounded,
  captureNote,
  compare,
  dspWords,
  expectGainRow,
  FREE_PC,
  frames,
  inspect,
  rows,
  rpc,
  runFrames,
  slotsFor,
  word,
} from '@digi/regression/runtime/fx-live.ts';
import { GAIN, RT, SINE, triggerFrame } from '@digi/regression/runtime/fixtures.ts';
import { chainCheck, fxExpectations } from '@digi/regression/runtime/dkm.ts';
import { inspectFxBoot, selectSine, withPreparedFxSession } from '@digi/regression/runtime/fx-session.ts';
import { record } from '@digi/regression/runtime/artifacts.ts';
import { loadCode } from '@digi/regression/runtime/load-states.ts';
import { DEVICES, FIRMWARE } from '@digi/regression/runtime/devices.ts';
import { readSmallRing } from '@digi/regression/runtime/runtime-menu.ts';
import { planSymbol } from '@digi/regression/runtime/runtime-plan.ts';
import { bindBootClock, emulatorDk, waitForBootEmulated } from '@digi/regression/runtime/waits.ts';

// The FX contract suite of core/dynfx on test/dyn-fx-gain, one case per concern on one image per device (docs/plans/test-architecture.md T2):
//   1. plain image: the unhooked sine is the audible reference;
//   2. hooked image, dry card: free rows are transparent;
//   3. wet card: one effect loads, its row, PCM gain and call counter are exact; a protocol select and restart deselects it again;
//   4. full card: effects beyond the free rows are refused, the loaded chain multiplies its gains;
//   5. card without the fx service file: the stock FX pages stay and the effect is refused.
// It replaces fx-chain.device.test.ts and fx-reuse.device.test.ts (ledger: docs/plans/ledgers/t2-fx-contract.md).

for (const device of DEVICES) {
  // These nested suites are ordered: the plain owned process closes before the hooked image starts.
  describe(`${device.toUpperCase()} ${FIRMWARE[device]} FX contract, dynfx`, () => {
    const slots = slotsFor(device);
    const effects = Array.from({ length: slots + 1 }, (_, index) => (index === 0 ? GAIN : `${GAIN}-${index + 1}`));
    const inventory = [SINE, ...effects];
    const symbols = ['DKRT_BOOT', 'DKRT_ASSIGN', 'dynreg_info'];
    let plainPcm: string;

    const capture = async (runtime: TestRuntime, context: ReturnType<typeof withPreparedFxSession>, tag: string) => {
      const { session } = runtime;
      await rpc(session, 'run.mode', { mode: 'paused' });
      await rpc(session, 'pcm.tap', { action: 'start' });
      // DT2 checks the assigned sine type and selected-track readback; DN2 retains its hint-only choice.
      await selectSine(runtime, context.profile);
      const current = await frames(session);
      const trigger = triggerFrame(device);
      record(context.artifacts, `${tag}-timing`, { currentFrame: current, triggerFrame: trigger });
      const file = path.join(context.artifacts, `${tag}.wav`);
      // Same instruction offset after the DSP boundary, held for the whole 1500-frame capture.
      await captureNote(session, { file, trigger, tag: `FX contract ${tag}`, tapStarted: true });
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
      const context = withPreparedFxSession(device, 'FX contract plain', {
        image: { name: 'fx-plain', modules: RT, inventory, symbols, policy: 'default' },
        initial: 'plain',
        presets: { plain: { selection: ['modules/m0.dkm'] } },
        requireAudible: true,
      });
      test('loads the sine safely and renders the unhooked audible reference', async () => {
        const runtime = context.fixture.runtime;
        const state = await inspectFxBoot(runtime, context.plan, [SINE], context.artifacts, 'plain-boot');
        context.rememberEpoch(state.epoch);
        plainPcm = await capture(runtime, context, 'plain');
      });
    });

    describe('hooked image', () => {
      const context = withPreparedFxSession(device, 'FX contract dry/wet/full', {
        image: {
          name: 'fx-chain',
          modules: [...RT, 'core/dynfx', 'core/fxpages'],
          inventory,
          symbols: [...symbols, 'fx.rows', '_dl_area', 'DKRT_TABLE_BAD', 'DKRT_LOG', 'DKRT_LOG_COUNT'],
          policy: 'default',
        },
        initial: 'dry',
        // The relative PCM comparisons below own the level thresholds.
        requireAudible: false,
        presets: {
          dry: { selection: ['modules/m0.dkm'] },
          wet: { selection: ['modules/m0.dkm', 'modules/m1.dkm'] },
          // no fx service file on this card (machines.dks stays): the effect is refused
          nofx: { selection: ['modules/m0.dkm', 'modules/m1.dkm'], noServices: ['fx'] },
          full: { selection: inventory.map((_, index) => `modules/m${index}.dkm`) },
        },
      });

      // One case: the cards are ordered dry, wet, deselected, full. Each reload is a clean boot of the same daemon.
      test('keeps free rows transparent, loads one effect exactly, deselects it, and refuses overflow without changing the full serial gain chain', async () => {
        const { fixture, plan, artifacts, rememberEpoch } = context;
        const dry = fixture.runtime;
        // `inspect` (fx-live): module list, boot words, DKRT_ASSIGN, one type, no dk/fault, capacity, every free row FREE_PC, UI reached.
        const dryState = await inspect(dry, plan, slots, false, artifacts, 'dry-boot');
        rememberEpoch(dryState.epoch);
        const dryPcm = await capture(dry, context, 'dry');
        expect(plainPcm, 'plain image reference ran before the hooked image').toBeDefined();
        await compare(plainPcm, dryPcm, 1, artifacts, 'plain-dry', true);

        // Wet: one effect.
        const wet = await fixture.reload(context.baseline('wet'));
        expect(wet.session).toBe(dry.session);
        expect(wet.epoch).toBeGreaterThan(dry.epoch);
        await expect(dry.dk.info()).rejects.toThrow();
        // Wet: the relocated DSP image against the independent oracle, dsp_len = needs, rows = 0, needs > 0, the exact row, free rows after it.
        const wetState = await inspect(wet, plan, slots, true, artifacts, 'wet-boot');
        rememberEpoch(wetState.epoch);
        const wetOracle = wetState.oracle!;
        const card = wet.session.cardPath;
        if (!card) throw new Error('the FX contract requires the loaded card inventory');
        const seed = (index: number) => path.join(path.dirname(card), `seed/digikit/modules/m${index}.dkm`);
        const wetPcm = await capture(wet, context, 'wet');
        await compare(dryPcm, wetPcm, 0.5, artifacts, 'gain');
        const afterNote = await rows(wet.session, plan, slots);
        expectGainRow(afterNote[0]!, wetOracle);
        record(artifacts, 'gain-after-note', afterNote);
        const stateAddress = afterNote[0]![2]!;
        const before = (await dspWords(wet.session, stateAddress, 1))[0]!;
        await runFrames(wet.session, 100);
        const after = (await dspWords(wet.session, stateAddress, 1))[0]!;
        expect(before).toBeGreaterThan(1000);
        expect(after - before, 'one call per block').toBeGreaterThanOrEqual(90);
        expect(after - before).toBeLessThanOrEqual(110);
        await context.evidence(wet, 'before-deselect', false);

        // Deselect through the protocol and restart. This restart deliberately retains guest writes; it is not a clean load.
        await rpc(wet.session, 'run.mode', { mode: 'free' });
        await bounded(() => wet.dk.select(['modules/m0.dkm']));
        const epoch = (await bounded(() => wet.session.status())).epoch;
        expect(typeof epoch).toBe('number');
        await bounded(() => wet.dk.restart());
        wet.dk.cancelPending();
        const rebootDk = emulatorDk(wet.session);
        bindBootClock(rebootDk, wet.session);
        const reboot = withWaitApi({ session: wet.session, dk: rebootDk, close: wet.close });
        try {
          // The boot wait counts emulated time (waitForBootEmulated), not wall time.
          await waitForBootEmulated(rebootDk, { emulatedMs: 90_000 });
          await bounded(() => wet.session.client.guardSet(context.baseline('wet').guard));
          const deselected = await inspect(reboot, plan, slots, false, artifacts, 'deselected-boot', Number(epoch));
          rememberEpoch(deselected.epoch);
          const deselectedPcm = await capture(reboot, context, 'deselected');
          await compare(dryPcm, deselectedPcm, 1, artifacts, 'deselected', true);
        } finally {
          rebootDk.cancelPending();
        }

        // Full: every free row used, the overflow effect refused. The fixture collects card overlay, PCM, guard and fault evidence before discarding the previous case.
        const full = await fixture.reload(context.baseline('full'));
        const fullState = await inspectFxBoot(full, plan, [SINE, ...effects.slice(0, slots)], artifacts, 'full-boot', [
          ...Array(1 + slots).fill(0),
          -34,
        ]);
        rememberEpoch(fullState.epoch);
        expect(fullState.modules).toHaveLength(1 + slots);
        expect(fullState.capacity.FX_SLOTS).toEqual({ limit: slots, used: slots, largest: 0 });
        const chain = await rows(full.session, plan, slots);
        const effectFiles = effects.map((_, index) => seed(index + 1));
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

      test('without the fx service the stock FX pages stay and the effect is refused', async () => {
        const { fixture, plan, artifacts, rememberEpoch } = context;
        const nofx = await fixture.reload(context.baseline('nofx'));
        // The sine loads; the effect module is REFUSED: its record kind has no owner (the fx service file is not on the card), E_MISSING.
        const state = await inspectFxBoot(nofx, plan, [SINE], artifacts, 'nofx-boot', [0, loadCode('E_MISSING')], {
          noServices: ['fx'],
        });
        rememberEpoch(state.epoch);
        expect(state.capacity.FX_SLOTS?.used ?? 0, 'no effect row is in use').toBe(0);
        expect((await rows(nofx.session, plan, slots)).map((row) => row[0]), 'every free row stays free').toEqual(
          Array(slots).fill(FREE_PC),
        );
        // No hook is installed, so the sine is as audible as on the plain image.
        const pcm = await capture(nofx, context, 'nofx');
        await compare(plainPcm, pcm, 1, artifacts, 'plain-nofx', true);
      });
    });
  });
}
