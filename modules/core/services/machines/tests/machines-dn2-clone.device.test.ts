import { expect, test } from 'vitest';
import { cardRig } from '@digi/regression/runtime/card-rig.ts';
import { FIRMWARE, describeOn } from '@digi/regression/runtime/devices.ts';
import { pickSine, word } from '@digi/regression/runtime/fx-live.ts';
import { rootsOf } from '@digi/regression/runtime/image-roots.ts';
import { decodeTables } from '@digi/regression/runtime/project-kit.ts';
import { carrierCard, expectServiceLoaded, notePeak, tableSymbols } from './machines-kit.ts';

/**
 * A machine row of the image on the Digitone II with `machines.dks` loaded (port of the patchkit smoke cell `dn2-1.12:core/machines+test/dn2-clone`, whose test is
 * tests/fixtures/modules/dn2-clone/tests/select.toml). `test/dn2-clone` adds "FM Clone" as type 5: an unmodified clone of FM TONE (no pages of its own) listed
 * before MIDI. YES commits the type: after a TRIG the audio mirror holds 5, the TX frame sends FM TONE's type 0 (clone_of 0) and the clone sounds.
 * One boot. `test/sine` is on the card only to make the harness put `machines.dks` there.
 */
const AUDIO_MIRROR_DN2_112 = 0x8000487c; // SRAM, first byte: the selected machine type (select.toml)
const TX_FRAME_DN2_112 = 0x80005ef4; // the frame word whose upper half is the type the DSP is sent (select.toml)

describeOn('dn2', `DN2 ${FIRMWARE.dn2} machines.dks: the added machine FM Clone`, () => {
  const run = cardRig('dn2', 'machines-dn2-clone', {
    dsp: 'sharc',
    roots: rootsOf('rt-dn2-clone'),
    symbols: tableSymbols('dn2'),
    initial: carrierCard,
  });

  test('FM Clone is the 5th list entry, YES commits type 5 and the DSP is sent FM TONE\'s type', async () => {
    const rt = await run.first();
    const { session } = rt;
    await expectServiceLoaded(rt, run);

    // the row: a clone of FM TONE (type 0), its own name, FM TONE's descriptor ids (all 0 here, level 10), no DSP type of its own
    const tables = await decodeTables(session, run.plan);
    expect(tables.machines['FM Clone'], 'the row of FM Clone').toMatchObject({
      short: 'FMC',
      clone_of: 0,
      own_dsp: 0,
      knobs: [0, 0, 0, 0, 0, 0, 0, 0],
      level: 10,
    });

    // FUNC+SRC, four DOWN, YES, YES: the entry is at list position 4 (before MIDI); then a note
    await pickSine(rt, 4);
    expect(await notePeak(rt, run.artifacts), 'the clone is audible').toBeGreaterThan(0.001);
    expect((await word(session, AUDIO_MIRROR_DN2_112)) >>> 24, 'the audio mirror holds the committed type').toBe(5);
    expect((await word(session, TX_FRAME_DN2_112)) >>> 16, 'the TX frame sends FM TONE\'s type').toBe(0);
    await session.expectNoFault('FM Clone selected and played');
  }, 900_000);
});
