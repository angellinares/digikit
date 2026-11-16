import path from 'node:path';
import { describe, expect, test } from 'vitest';
import { captureNote, compare, frames, slotsFor } from '@digi/regression/runtime/fx-live.ts';
import { GAIN, RT, SINE, triggerFrame } from '@digi/regression/runtime/fixtures.ts';
import { inspectFxBoot, selectSine, withPreparedFxSession } from '@digi/regression/runtime/fx-session.ts';
import { record } from '@digi/regression/runtime/artifacts.ts';
import { DEVICES, FIRMWARE } from '@digi/regression/runtime/devices.ts';

// Experiment for T2 step 2 (docs/plans/test-architecture.md): can two notes share one boot? The FX contract suites would then capture
// several PCM cases per card. The answer is yes when the second note, played in the same boot after the first voice is over, equals the same
// note played after a fresh boot. Off by default: run it with DIGIKIT_EXPERIMENT=two-notes in the device window
//   DIGIKIT_EXPERIMENT=two-notes pnpm nx:pilot run core/dynfx:test-device--fx-two-notes--dn2   (and --dt2)
// The sine voice ends after 96,000 samples (3,000 blocks of 32 samples) whether or not the key is up: the second note starts GAP blocks after
// the first, which is 3,000 blocks of voice life plus a margin. The fixed trigger frame of the fresh boot is the reference for both.
const ON = process.env.DIGIKIT_EXPERIMENT === 'two-notes';
const GAP = 3600;

for (const device of DEVICES) {
  describe.skipIf(!ON)(`${device.toUpperCase()} ${FIRMWARE[device]} FX two notes in one boot (experiment)`, () => {
    const slots = slotsFor(device);
    const context = withPreparedFxSession(device, 'FX two notes', {
      image: {
        name: 'fx-chain',
        modules: [...RT, 'core/dynfx', 'core/fxpages'],
        inventory: [SINE, GAIN],
        symbols: ['DKRT_BOOT', 'DKRT_ASSIGN', 'dynreg_info', 'fx.rows', '_dl_area', 'DKRT_TABLE_BAD', 'DKRT_LOG', 'DKRT_LOG_COUNT'],
        policy: 'default',
      },
      initial: 'wet',
      // The relative comparisons below own the levels.
      requireAudible: false,
      presets: { wet: { selection: ['modules/m0.dkm', 'modules/m1.dkm'] } },
    });

    test('the second note of a boot equals the same note after a fresh boot', async () => {
      const { fixture, plan, profile, artifacts, rememberEpoch } = context;
      const note = async (runtime: typeof fixture.runtime, tag: string, trigger: number) => {
        const file = path.join(artifacts, `${tag}.wav`);
        const current = await frames(runtime.session);
        record(artifacts, `${tag}-timing`, { currentFrame: current, triggerFrame: trigger });
        await captureNote(runtime.session, { file, trigger, tag: `two notes ${tag}` });
        return file;
      };
      const boot = async (runtime: typeof fixture.runtime, tag: string) => {
        const state = await inspectFxBoot(runtime, plan, [SINE, GAIN], artifacts, `${tag}-boot`);
        rememberEpoch(state.epoch);
        expect(state.capacity.FX_SLOTS).toEqual({ limit: slots, used: 1, largest: 0 });
        await selectSine(runtime, profile);
      };

      const first = fixture.runtime;
      await boot(first, 'first');
      const trigger = triggerFrame(device);
      const firstNote = await note(first, 'note-1', trigger);
      const secondNote = await note(first, 'note-2', trigger + GAP);

      const fresh = await fixture.reload(context.baseline('wet'));
      await boot(fresh, 'fresh');
      const freshNote = await note(fresh, 'fresh-note', trigger);

      // Diagnostic first: the first note equals the fresh note (the suites already rely on it).
      await compare(freshNote, firstNote, 1, artifacts, 'first-vs-fresh', true);
      // The experiment: the second note of a boot equals the fresh note, from the onset on, bit for bit.
      await compare(freshNote, secondNote, 1, artifacts, 'second-vs-fresh', true);
    });
  });
}
