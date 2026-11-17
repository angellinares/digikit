import { encoder } from '@digi/test-support';
import { expect } from 'vitest';
import { bounded, rows, slotsFor, word, SETTLE } from '@digi/regression/runtime/fx-live.ts';
import { effectPage, mainPage } from '@digi/regression/runtime/fx-pages-ui.ts';
import { prepareRuntimePlan } from '@digi/regression/runtime/runtime-plan.ts';
import { serviceSymbol } from '@digi/regression/runtime/service-symbols.ts';
import { dn2Tests, type Dn2 } from './shot-dn2-support.ts';

// R6a, the DSP link queue (docs/plans/runtime-and-services.md section 6): the effect page's knob writes (live values on the link) and the sample track
// writes (the track table's owner requests) go through one queue. This test turns the GAIN knob of an effect page while a pool sample loads into a track,
// so the table publish and the knob words share the link. Both must land in DSP memory, and the link must report no failed write.
//
// DN2 OS 1.12, patch set `sample-tracks` (it has core/fxpages, core/dynfx, core/sample-pool and core/dsp-link). Written for the batched gate; not run yet.
//
// Read back: the effect row's p0 word and the track record through `dsp.peek` (the plan symbols `fx.rows` and `RECORDS`), and the page writer's counters in
// `DKRT_FXP` (word 4 `sent`: values the link confirmed; word 5 `errors`: live writes that failed).

const FXP_SENT = 16;
const FXP_ERRORS = 20;
const GAIN_DEFAULT = 0x3f000000; // 0.5f, the default of the gain effect's GAIN knob (test/dyn-fx-gain)
const floatOf = (bits: number) => {
  const b = Buffer.alloc(4);
  b.writeUInt32LE(bits >>> 0);
  return b.readFloatLE();
};

dn2Tests(
  'DN2 DSP link: effect knob writes and sample track writes together',
  'shot-dn2-link',
  ['a'],
  async (c: Dn2) => {
    const { runtime, session, api, settle, phase, rec, fixtures, file, artifacts, expectNoNewFault } = c;
    const plan = await prepareRuntimePlan(session, artifacts, ['fx.rows']);
    const slots = slotsFor('dn2');
    const fxp = await serviceSymbol(runtime, plan, 'fx', 'DKRT_FXP');
    const gain = async () => (await rows(session, plan, slots))[0]![4]!;
    const burst = async (detents: number, events: number) => {
      for (let i = 0; i < events; i++) {
        await bounded(() => session.client.inputTurn(encoder('ENC A'), detents));
        await runtime.ui.wait({ ms: 2, why: SETTLE });
      }
    };

    await phase('knob-turns-during-a-track-load', async () => {
      const before = await gain();
      expect(before, 'the gain effect row holds the default GAIN').toBe(GAIN_DEFAULT);
      const sentBefore = await word(session, fxp + FXP_SENT);
      await effectPage(runtime);
      // the load starts the track table writes; the turns go to the effect page meanwhile
      expect(await api.select(file('a'), 0), 'select queues').toBe(0);
      await burst(8, 20);
      expect((await settle(0)).state, 'the track reaches ready').toBe(3);
      await runtime.ui.wait({ ms: 300, why: SETTLE });
      await mainPage(runtime);
      // the knob: the DSP row has the last value, a float above the default (clockwise turns)
      const after = await gain();
      expect(floatOf(after), 'p0 follows the turns').toBeGreaterThan(floatOf(GAIN_DEFAULT));
      expect(Number.isFinite(floatOf(after))).toBe(true);
      // the track: the published record reads back
      const r = await rec(0);
      expect(r.unknown).toBe(0);
      expect(r.ready, 'the record is published (ready written last)').toBe(1);
      expect(r.frames).toBe(fixtures.a!.frames);
      // the link: values were confirmed and no live write failed
      expect(await word(session, fxp + FXP_SENT), 'the link confirmed knob values').toBeGreaterThan(sentBefore);
      expect(await word(session, fxp + FXP_ERRORS), 'no live write failed').toBe(0);
    });

    await phase('no-fault', async () => {
      await expectNoNewFault('the end of the concurrent writes');
    });
  },
  ['test/dyn-fx-gain'],
);
