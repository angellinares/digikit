import path from 'node:path';
import { DkSession, EmuUsbTransport } from '@digi/dk-session';
import { describe, expect, test } from 'vitest';
import { DEVICES, FIRMWARE, expectRuntimeUi, record } from '@digi/regression/runtime/live.ts';
import {
  bounded,
  rpc,
  dspWords,
  rows,
  frames,
  capture,
  compare,
  inspect,
  expectGainRow,
  withGainFxSession,
} from '@digi/regression/runtime/fx-live.ts';

for (const device of DEVICES) {
  describe(`${device.toUpperCase()} ${FIRMWARE[device]} reusable FX fixture`, () => {
    const context = withGainFxSession(device, 'FX dry/gain/deselect/reset');
    test('reuses an owned daemon while clean reset and protocol restart preserve their distinct FX contracts', async () => {
      const { fixture, plan, dry, wet, artifacts, slots, rememberEpoch } = context;
      const initial = fixture.runtime;
      const initialState = await inspect(initial, plan, slots, false, artifacts, 'initial');
      rememberEpoch(initialState.epoch);
      const dryPcm = path.join(artifacts, 'dry.wav');
      await capture(initial, dryPcm);

      // Fresh-process equivalence passed on both devices in the earlier one-time fixture proof.
      // Permanent acceptance retains clean reload isolation without that extra process/boot.
      const dirty = await fixture.reload(wet);
      expect(dirty.session).toBe(initial.session);
      expect(dirty.epoch).toBeGreaterThan(initial.epoch);
      await expect(initial.dk.info()).rejects.toThrow();
      const wetState = await inspect(dirty, plan, slots, true, artifacts, 'wet');
      rememberEpoch(wetState.epoch);
      const wetPcm = path.join(artifacts, 'gain.wav');
      await capture(dirty, wetPcm);
      const afterNote = await rows(dirty.session, plan, slots);
      expectGainRow(afterNote[0]!, wetState.oracle!);
      record(artifacts, 'gain-after-note', afterNote);
      await compare(dryPcm, wetPcm, 0.5, artifacts, 'gain');
      const stateAddress = wetState.chain[0]![2]!;
      const before = (await dspWords(dirty.session, stateAddress, 1))[0]!;
      await bounded(async () =>
        dirty.session.client.runUntil({ frames: (await frames(dirty.session)) + 100, timeout_wall_s: 30 }),
      );
      const after = (await dspWords(dirty.session, stateAddress, 1))[0]!;
      expect(before).toBeGreaterThan(1000);
      expect(after - before).toBeGreaterThanOrEqual(90);
      expect(after - before).toBeLessThanOrEqual(110);
      await context.evidence(dirty, 'before-deselect');

      // This restart deliberately retains guest writes; it is not an independent-case clean load.
      await rpc(dirty.session, 'run.mode', { mode: 'free' });
      await bounded(() => dirty.dk.select(['modules/sine.dkm']));
      const epoch = (await bounded(() => dirty.session.status())).epoch;
      expect(typeof epoch).toBe('number');
      await bounded(() => dirty.dk.restart());
      dirty.dk.cancelPending();
      const rebootDk = new DkSession(
        new EmuUsbTransport((method, params) => dirty.session.client.call(method, params)),
      );
      const reboot = { session: dirty.session, dk: rebootDk, close: dirty.close };
      try {
        await bounded(() => rebootDk.waitForBoot());
        await expectRuntimeUi(dirty.session, plan, artifacts, { afterEpoch: Number(epoch) });
        await bounded(() => dirty.session.client.guardSet(wet.guard));
        const deselected = await inspect(reboot, plan, slots, false, artifacts, 'deselected', Number(epoch));
        rememberEpoch(deselected.epoch);
        const file = path.join(artifacts, 'deselected.wav');
        await capture(reboot, file);
        await compare(dryPcm, file, 1, artifacts, 'deselected', true);
      } finally {
        rebootDk.cancelPending();
      }

      const restored = await fixture.reload(dry);
      const resetState = await inspect(restored, plan, slots, false, artifacts, 'restored');
      rememberEpoch(resetState.epoch);
      expect(resetState.semantic).toEqual(initialState.semantic);
      expect(restored.epoch).toBeGreaterThan(dirty.epoch);
      const restoredPcm = path.join(artifacts, 'restored.wav');
      await capture(restored, restoredPcm);
      await compare(dryPcm, restoredPcm, 1, artifacts, 'restored', true);
    });
  });
}
