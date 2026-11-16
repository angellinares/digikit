import { pollUntil } from '@digi/test-support';
import { expect, test } from 'vitest';
import { cardRig, RT } from '@digi/regression/runtime/card-rig.ts';
import { FIRMWARE, describeOn } from '@digi/regression/runtime/devices.ts';
import { pickSine, REDRAW, word, type Rt } from '@digi/regression/runtime/fx-live.ts';
import { decodeTables, dynType } from '@digi/regression/runtime/project-kit.ts';
import {
  DN2_AUDIO_MIRROR_112,
  DN2_TX_TYPE_TRACK1_112,
  ENCODERS,
  expectLoaded,
  holdTrig,
  namedKnobs,
  soundWord,
  soundWords,
  tableSymbols,
  turnChangesNothing,
  turnChangesOneWord,
  turnPastLimit,
  voiceWord,
} from './machines-kit.ts';

/**
 * What `machines.dks` does for a run-time machine with its own page on the Digitone II (`test/dyn-dn2`: machine DynSyn, one SYN page DYN with four parameters).
 * One emulator start, two boots of it: the plain runtime (FM TONE's SYN, FLTR and AMP pages are captured), then the card with the machine.
 * With the machine loaded: the rows are in the tables (names, titles, ranges, knob layout, slots), the stock pages show the same frames, the machine is in the list at
 * the first place after MIDI, YES commits its type (audio mirror, TX frame word), the Sound words hold the defaults of its parameters, the SYN key draws its page,
 * a knob edits the Sound word of its parameter (exactly the word of its slot), ranges clamp and the empty knobs move nothing.
 * Replaces the patch-time smoke cells of `test/dn2-clone` and `test/dn2-pages` and the parts a to c and e of `pk_proof_pages_dn2.py`,
 * see docs/plans/ledgers/smoke-machines-cells.md. SHARC for the module's DSP part.
 */
const ID = 'test/dyn-dn2';

/** The voice slot of the second note since the boot (the TRIG tap of the type check was the first). */
const HELD_VOICE = 1;

interface ParamRow {
  mirror: number;
  min: number;
  max: number;
  default: number;
}

/** The frames of FM TONE's SYN pages (four presses: the page cycle), FLTR and AMP of track 1 after a boot. */
async function stockFrames(rt: Rt): Promise<string[]> {
  const frames: string[] = [];
  for (const key of ['SYN', 'SYN', 'SYN', 'SYN', 'FLTR', 'AMP']) {
    await rt.ui.tap(key, { until: { ms: 300, why: REDRAW } });
    frames.push(await rt.session.frameHash());
  }
  return frames;
}

describeOn('dn2', `DN2 ${FIRMWARE.dn2} machines.dks: the own page of a run-time machine`, () => {
  const run = cardRig('dn2', 'machine-rows-dn2', {
    dsp: 'sharc',
    roots: RT,
    symbols: tableSymbols('dn2'),
    initial: () => ({}),
  });

  test('stock pages, rows, list entry, committed type, defaults, SYN page, knobs, clamping and empty knobs of DynSyn', async () => {
    const plain = await run.first();
    const stock = await stockFrames(plain);
    expect(new Set(stock).size, 'the stock pages show different frames').toBeGreaterThan(1);

    const rt = await run.load('with-machines', { modules: [ID], selection: ['modules/dyn-dn2.dkm'] });
    const { session } = rt;
    await expectLoaded(rt, run);
    expect(await stockFrames(rt), 'FM TONE SYN, FLTR and AMP show the same frames with machines.dks loaded').toEqual(stock);

    const tables = await decodeTables(session, run.plan);
    expect(tables.machines['DynSyn'], 'the machine row').toMatchObject({ short: 'DSY', clone_of: 0, own_dsp: 1 });
    const pages = tables.pages.filter((p) => p.machine === 'DynSyn');
    expect(pages.map((p) => [p.name, p.title])).toEqual([['DYN', 'Dyn Synth']]);
    const page = pages[0]!;
    const layout = namedKnobs(page.knobs).map((k) => ({ ...k, slot: page.slots[k.knob]! }));
    expect(layout.map((k) => k.name)).toEqual(['Dyn Alpha', 'Dyn Beta', 'Dyn Gamma', 'Dyn Delta']);
    // the fixture (module.c, build/gen/gallery.json) declares four parameters with these ranges and defaults; the Sound word of a parameter is its mirror index (finding 02, 119)
    const rows = Object.fromEntries(layout.map((k) => [k.name, tables.params[k.name] as ParamRow]));
    expect(rows['Dyn Alpha']).toMatchObject({ min: 0, max: 0x7f00, default: 0x2000 });
    expect(rows['Dyn Beta']).toMatchObject({ min: 0, max: 0x3f00, default: 0x1000 });
    expect(rows['Dyn Gamma']).toMatchObject({ min: 0x100, max: 0x7f00, default: 0x7000 });
    expect(rows['Dyn Delta']).toMatchObject({ min: 0, max: 0x7f00, default: 0x4000 });
    expect(
      layout.map((k) => [k.slot, rows[k.name]!.mirror]),
      'every knob has its own slot, the slot is the Sound word of the parameter',
    ).toEqual([
      [26, 26],
      [27, 27],
      [29, 29],
      [30, 30],
    ]);
    const empty = page.knobs.flatMap((k, knob) => (k === 0 ? [knob] : []));
    expect(empty.length, 'the page leaves knobs empty').toBeGreaterThan(0);

    // FUNC+SRC, five DOWN from FM TONE (the list is FM TONE, FM DRUM, WAVETONE, SWARMER, MIDI, then the first run-time machine; the hint of the round-trip test), YES, YES;
    // a TRIG sends the type to the audio side. The TRIG press is the first note since the boot: it uses voice slot 0 (round robin from 0, finding 06)
    const type = await dynType(session, run.plan, ID, 'dyn_pluck');
    expect(type, 'a run-time type (the range of core/dynreg)').toBeGreaterThanOrEqual(16);
    await pickSine(rt, 5);
    await rt.ui.tap('TRIG 1', { until: { drained: true } });
    const mirror = await pollUntil(rt.clock, () => word(session, DN2_AUDIO_MIRROR_112), (w) => w >>> 24 === type, {
      withinMs: 3000,
      message: 'the audio mirror holds the committed type',
      intervalMs: 50,
    });
    expect(mirror >>> 24, 'the audio mirror holds the committed type').toBe(type);
    // an own_dsp machine sends its own type to the DSP: the track's halfword of the TX frame (`mf_frametype`; the removed clone cell saw FM TONE's type 0 there)
    const tx = await pollUntil(rt.clock, () => word(session, DN2_TX_TYPE_TRACK1_112), (w) => w >>> 16 === type, {
      withinMs: 3000,
      message: 'the TX frame carries the machine type of track 1',
      intervalMs: 50,
    });
    expect(tx >>> 16, 'the TX frame carries the own type of the run-time machine').toBe(type);

    // the selected machine's parameters start at their declared defaults (the engine slots read zero until the first trig)
    const defaults = layout.map((k) => rows[k.name]!.default);
    const words = await pollUntil(rt.clock, () => soundWords(session), (w) => layout.every((k) => w[k.slot]! === rows[k.name]!.default), {
      withinMs: 3000,
      message: 'the Sound words hold the defaults of the machine',
      intervalMs: 50,
    });
    expect(
      layout.map((k) => words[k.slot]),
      'the Sound words of the four parameters are their defaults',
    ).toEqual(defaults);

    // SYN draws the own page: the screen is not the one before
    const before = await session.frameHash();
    await rt.ui.tap('SYN', { until: { ms: 400, why: REDRAW } });
    expect(await session.frameHash(), 'the SYN key shows the page').not.toBe(before);

    // each knob edits exactly the Sound word of its slot, upward
    for (const k of layout) {
      const edit = await turnChangesOneWord(rt, ENCODERS[k.knob]!);
      expect(edit.index, `${k.name} edits the Sound word of its slot`).toBe(k.slot);
      expect(edit.after, `${k.name} moved up`).toBeGreaterThan(edit.before);
    }

    // a held note: turn the knob first (TRIG not held: with TRIG held the firmware may record a p-lock instead of editing the Sound word), then hold TRIG 1; the engine word of the voice (TX frame block of voice slot 1, the second note since the boot) equals the Sound word
    const alpha = layout[0]!;
    await turnChangesOneWord(rt, ENCODERS[alpha.knob]!);
    await holdTrig(rt, true);
    try {
      await pollUntil(
        rt.clock,
        async () => ({ sound: await soundWord(session, alpha.slot), engine: await voiceWord(session, HELD_VOICE, alpha.slot) }),
        (w) => w.sound === w.engine,
        { withinMs: 3000, message: 'the held voice engine word equals the Sound word of Dyn Alpha', intervalMs: 50 },
      );
    } finally {
      await holdTrig(rt, false);
    }

    // the ranges clamp: Beta at its top, Gamma at its bottom and top (the values of the old proof: 0x3f00, 0x100, 0x7f00); further events change nothing
    const beta = layout[1]!;
    const gamma = layout[2]!;
    expect(await turnPastLimit(rt, ENCODERS[beta.knob]!, beta.slot, 4, 0x3f00), 'Dyn Beta stops at its maximum').toBe(0x3f00);
    expect(await turnPastLimit(rt, ENCODERS[gamma.knob]!, gamma.slot, -4, 0x100), 'Dyn Gamma stops at its minimum').toBe(0x100);
    expect(await turnPastLimit(rt, ENCODERS[gamma.knob]!, gamma.slot, 4, 0x7f00), 'Dyn Gamma stops at its maximum').toBe(0x7f00);

    // the empty knobs move no Sound word, up or down
    for (const knob of empty) {
      for (const detents of [12, -12]) {
        expect(await turnChangesNothing(rt, ENCODERS[knob]!, detents), `${ENCODERS[knob]} (empty) ${detents > 0 ? 'up' : 'down'} moves nothing`).toEqual([]);
      }
    }
    await session.expectNoFault('DynSyn selected, page shown, knobs turned');
  }, 900_000);
});
