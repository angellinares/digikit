import { expect, test } from 'vitest';
import { cardRig } from '@digi/regression/runtime/card-rig.ts';
import { FIRMWARE, describeOn } from '@digi/regression/runtime/devices.ts';
import { dspBytes, pickSine, REDRAW } from '@digi/regression/runtime/fx-live.ts';
import { rootsOf } from '@digi/regression/runtime/image-roots.ts';
import { decodeTables, trackType } from '@digi/regression/runtime/project-kit.ts';
import {
  carrierCard,
  ENCODERS,
  expectServiceLoaded,
  notePeak,
  tableSymbols,
  turnChangesOneWord,
} from './machines-kit.ts';

/**
 * A machine row of the image on the Digitakt II with `machines.dks` loaded (port of the patchkit smoke cell `dt2-1.17:core/machines+test/machine-clone`, whose
 * test is tests/fixtures/modules/machine-clone/tests/select.toml). `test/machine-clone` adds the machine XClone to the image: a clone of SAMPLE (stock type 0)
 * with the descriptor ID fields of MANUAL SLICE, permanent type 8 (types.toml). The machines hooks live in `machines.dks`; `test/sine` is on the card only
 * because its machine record makes the harness put the service file there and the runtime load it.
 * One boot. SHARC audio: the clone sounds and the DSP's selector for type 8 is set.
 */
const SELECTOR_DT2_117 = 0x255970; // DSP address read by the cell: the byte for the selected type (finding 04, 06)

describeOn('dt2', `DT2 ${FIRMWARE.dt2} machines.dks: the added machine XClone`, () => {
  const run = cardRig('dt2', 'machines-dt2-clone', {
    dsp: 'sharc',
    roots: rootsOf('rt-machine-clone'),
    symbols: tableSymbols('dt2'),
    initial: carrierCard,
  });

  test('XClone is in the list, selecting it commits type 8, its SRC page has the descriptor knobs and the clone sounds', async () => {
    const rt = await run.first();
    const { session } = rt;
    await expectServiceLoaded(rt, run);

    // the row the table builder wrote: a clone of SAMPLE with MANUAL SLICE's descriptor ids (module.toml f0 .. f8)
    const tables = await decodeTables(session, run.plan);
    expect(tables.machines['XClone'], 'the row of XClone').toMatchObject({
      short: 'XCL',
      clone_of: 0,
      own_dsp: 0,
      knobs: [0xf8, 0xf9, 0, 0xfb, 0xfc, 0xfd, 0, 0xfe],
      level: 0x0a,
    });

    // FUNC+SRC, seven DOWN from the top of the list, YES, YES: the list has the entry at position 7 and the choice reaches the track (type byte of track 1)
    await pickSine(rt, 7, 12);
    expect(await trackType(session, 1), 'track 1 holds the permanent type of XClone').toBe(8);

    // the SRC page of the clone: knobs A and B (descriptor ids 0xf8, 0xf9) each change one parameter word, not the same one
    await rt.ui.tap('SRC', { until: { ms: 300, why: REDRAW } });
    const a = await turnChangesOneWord(rt, ENCODERS[0]!);
    const b = await turnChangesOneWord(rt, ENCODERS[1]!);
    expect(b.index, 'knob B edits another word than knob A').not.toBe(a.index);
    expect(a.after, 'knob A moved up').toBeGreaterThan(a.before);

    // a note: the clone sounds, and the DSP's selector for the track holds type 8
    expect(await notePeak(rt, run.artifacts), 'the clone is audible').toBeGreaterThan(0.001);
    expect([...(await dspBytes(session, SELECTOR_DT2_117, 1))], 'DSP selector for type 8').toEqual([0x08]);
    await session.expectNoFault('XClone selected and played');
  }, 900_000);
});
