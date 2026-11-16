import { expect, test } from 'vitest';
import { cardRig } from '@digi/regression/runtime/card-rig.ts';
import { FIRMWARE, describeOn } from '@digi/regression/runtime/devices.ts';
import { pickSine, word, REDRAW } from '@digi/regression/runtime/fx-live.ts';
import { rootsOf } from '@digi/regression/runtime/image-roots.ts';
import { decodeTables } from '@digi/regression/runtime/project-kit.ts';
import { carrierCard, ENCODERS, expectServiceLoaded, tableSymbols, turnChangesOneWord } from './machines-kit.ts';

/**
 * A machine with its own parameter pages on the Digitone II with `machines.dks` loaded (port of the patchkit smoke cell
 * `dn2-1.12:core/machines+core/params+test/dn2-pages`, whose test is tests/fixtures/modules/dn2-pages/tests/select.toml). `test/dn2-pages` adds the machine
 * PAGES (type 6, listed before MIDI, `own_dsp`) and two SYN pages of its own: PGA with ALPH BETA GAMA DELT, PGB with EPSL ZETA. The group hook places the
 * machine's row in the machine menu; the page's draw, code, key and turn hooks (in `machines.dks`) make the SYN key cycle the two pages and the knobs edit
 * their parameters. One boot. `test/sine` is on the card only to make the harness put `machines.dks` there.
 */
const AUDIO_MIRROR_DN2_112 = 0x8000487c; // SRAM, first byte: the selected machine type (select.toml of dn2-clone)

describeOn('dn2', `DN2 ${FIRMWARE.dn2} machines.dks: own pages of the machine PAGES`, () => {
  const run = cardRig('dn2', 'machines-dn2-pages', {
    dsp: 'sharc',
    roots: rootsOf('rt-dn2-pages'),
    symbols: tableSymbols('dn2'),
    initial: carrierCard,
  });

  test('PAGES is the 5th list entry; SYN cycles its two pages; a knob changes the parameter of its page', async () => {
    const rt = await run.first();
    const { session } = rt;
    await expectServiceLoaded(rt, run);

    // the page rows the table builder wrote: names, titles, knob layout and the sound slot of each knob (module.toml of the fixture)
    const tables = await decodeTables(session, run.plan);
    expect(tables.machines['Pages'], 'the row of PAGES').toMatchObject({ short: 'PGS', clone_of: 0, own_dsp: 1 });
    const pages = tables.pages.filter((p) => p.machine === 'Pages').sort((x, y) => (x.name < y.name ? -1 : 1));
    expect(pages.map((p) => [p.name, p.title])).toEqual([
      ['PGA', 'Pages Test'],
      ['PGB', 'Pages Test'],
    ]);
    const layout = (page: (typeof pages)[number]) =>
      page.knobs.flatMap((k, knob) => (typeof k === 'string' ? [{ knob, name: k, slot: page.slots[knob]! }] : []));
    const pageA = layout(pages[0]!);
    const pageB = layout(pages[1]!);
    expect(pageA.map((k) => [k.knob, k.name, k.slot])).toEqual([
      [0, 'Alpha Level', 25],
      [1, 'Beta Half', 26],
      [2, 'Gamma High', 27],
      [3, 'Delta Skip', 30],
    ]);
    expect(pageB.map((k) => [k.knob, k.name, k.slot])).toEqual([
      [0, 'Epsilon Low', 40],
      [1, 'Zeta Full', 41],
    ]);
    // the parameter rows: ranges and defaults of the fixture
    expect(tables.params['Gamma High']).toMatchObject({ min: 0x100, max: 0x7f00, default: 0x7000 });

    // FUNC+SRC, four DOWN, YES, YES: the entry is at list position 4 and the choice reaches the audio side after a TRIG
    await pickSine(rt, 4);
    await rt.ui.tap('TRIG 1', { until: { ms: 500, why: REDRAW } });
    expect((await word(session, AUDIO_MIRROR_DN2_112)) >>> 24, 'the audio mirror holds type 6').toBe(6);

    // SYN: page A, page B, page A again. The pages draw differently, and the cycle returns to the first.
    const syn = async () => {
      await rt.ui.tap('SYN', { until: { ms: 400, why: REDRAW } });
      return session.frameHash();
    };
    const a = await syn();
    const b = await syn();
    const a2 = await syn();
    expect(b, 'page B draws differently from page A').not.toBe(a);
    expect(a2, 'the third SYN is page A again').toBe(a);

    // a knob edits the Sound word of its parameter. The slot is an index into the Sound array: two knobs are as far apart as their slots.
    const alpha = await turnChangesOneWord(rt, ENCODERS[pageA[0]!.knob]!);
    expect(alpha.after, 'ALPH moved up').toBeGreaterThan(alpha.before);
    expect(alpha.after, 'ALPH stays inside its range').toBeLessThanOrEqual(0x7f00);
    const delta = await turnChangesOneWord(rt, ENCODERS[pageA[3]!.knob]!);
    expect(delta.index - alpha.index, 'DELT and ALPH are as far apart as their slots').toBe(
      pageA[3]!.slot - pageA[0]!.slot,
    );
    // page B: its knob A is EPSL, in the slot 15 above ALPH's
    await rt.ui.tap('SYN', { until: { ms: 400, why: REDRAW } });
    const epsilon = await turnChangesOneWord(rt, ENCODERS[pageB[0]!.knob]!);
    expect(epsilon.index - alpha.index, 'EPSL and ALPH are as far apart as their slots').toBe(
      pageB[0]!.slot - pageA[0]!.slot,
    );
    await session.expectNoFault('PAGES pages cycled and edited');
  }, 900_000);
});
