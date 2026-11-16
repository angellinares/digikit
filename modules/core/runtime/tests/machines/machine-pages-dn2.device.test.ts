import { pollUntil } from '@digi/test-support';
import { expect, test } from 'vitest';
import { cardRig, RT } from '@digi/regression/runtime/card-rig.ts';
import { FIRMWARE, describeOn } from '@digi/regression/runtime/devices.ts';
import { pickSine, REDRAW, word, type Rt } from '@digi/regression/runtime/fx-live.ts';
import { decodeTables, dynType } from '@digi/regression/runtime/project-kit.ts';
import {
  DN2_AUDIO_MIRROR_112,
  ENCODERS,
  expectLoaded,
  namedKnobs,
  soundWords,
  tableSymbols,
  turnChangesNothing,
  turnChangesOneWord,
  turnPastLimit,
} from './machines-kit.ts';

/**
 * The SYN key cycles the pages of a run-time machine with two pages on the Digitone II (`test/dyn-dn2-pages`: machine DynPages, pages PGA with ALPH BETA GAMA and
 * PGB with EPSL ZETA). The page rows are the declared ones. Which page is current shows in what a knob edits. SYN once gives PGA, twice PGB, three times PGA again.
 * On each page: every mapped knob edits exactly the Sound word of its parameter (the mirror index of the parameter row), the Sound words hold the declared defaults after
 * the first TRIG, a range clamps, and the empty knobs move no word. The fixture (`tests/fixtures/modules/dyn-dn2-pages/src/module.c`) declares values 0..127, one value is
 * 0x100: Alpha 32, Beta 16 (0..63), Gamma 112 (1..127), Epsilon 8, Zeta 127. No pixels. One boot; SHARC for the module's DSP part.
 */
const ID = 'test/dyn-dn2-pages';
/** The expectations of module.c, per parameter: range and default in Sound word units (value x 0x100). */
const DECLARED = {
  'Pg Alpha': { min: 0, max: 0x7f00, default: 0x2000 },
  'Pg Beta': { min: 0, max: 0x3f00, default: 0x1000 },
  'Pg Gamma': { min: 0x100, max: 0x7f00, default: 0x7000 },
  'Pg Epsilon': { min: 0, max: 0x7f00, default: 0x800 },
  'Pg Zeta': { min: 0, max: 0x7f00, default: 0x7f00 },
} as const;
type Declared = keyof typeof DECLARED;

interface Knob {
  knob: number;
  name: Declared;
  slot: number;
  mirror: number;
}

describeOn('dn2', `DN2 ${FIRMWARE.dn2} machines.dks: two pages of a run-time machine`, () => {
  const run = cardRig('dn2', 'machine-pages-dn2', {
    dsp: 'sharc',
    roots: RT,
    symbols: tableSymbols('dn2'),
    initial: () => ({ modules: [ID], selection: ['modules/dyn-dn2-pages.dkm'] }),
  });

  /** Everything one page must do; `clamp` is the knob (index into `knobs`) whose range end is checked, `up` the end it is turned to. */
  async function exercisePage(rt: Rt, label: string, knobs: Knob[], emptyKnobs: number[], clamp: { which: number; up: boolean }) {
    // each mapped knob edits exactly the word of its parameter (a knob at its top is turned down)
    for (const k of knobs) {
      const d = DECLARED[k.name];
      const edit = await turnChangesOneWord(rt, ENCODERS[k.knob]!, d.default === d.max ? -12 : 12);
      expect(edit.index, `${label} ${k.name} edits the Sound word of its mirror index`).toBe(k.mirror);
      if (d.default === d.max) expect(edit.after, `${label} ${k.name} moved down`).toBeLessThan(edit.before);
      else expect(edit.after, `${label} ${k.name} moved up`).toBeGreaterThan(edit.before);
    }
    // the range clamps: turned to its end, further events change nothing
    const c = knobs[clamp.which]!;
    const end = clamp.up ? DECLARED[c.name].max : DECLARED[c.name].min;
    expect(await turnPastLimit(rt, ENCODERS[c.knob]!, c.mirror, clamp.up ? 4 : -4, end), `${label} ${c.name} stops at its ${clamp.up ? 'maximum' : 'minimum'}`).toBe(end);
    // the empty knobs move no word, up or down
    for (const knob of emptyKnobs)
      for (const detents of [12, -12])
        expect(await turnChangesNothing(rt, ENCODERS[knob]!, detents), `${label} ${ENCODERS[knob]} (empty) ${detents > 0 ? 'up' : 'down'} moves nothing`).toEqual([]);
  }

  test('SYN goes PGA, PGB, PGA; each page edits its own parameters, has its defaults, clamps and leaves its empty knobs alone', async () => {
    const rt = await run.first();
    const { session } = rt;
    await expectLoaded(rt, run);

    const tables = await decodeTables(session, run.plan);
    expect(tables.machines['DynPages'], 'the machine row').toMatchObject({ short: 'DPG', clone_of: 0, own_dsp: 1 });
    const pages = tables.pages.filter((p) => p.machine === 'DynPages').sort((x, y) => (x.name < y.name ? -1 : 1));
    expect(pages.map((p) => [p.name, p.title])).toEqual([
      ['PGA', 'Pages A'],
      ['PGB', 'Pages B'],
    ]);
    const layout = pages.map((p) =>
      namedKnobs(p.knobs).map((k) => ({
        ...k,
        slot: p.slots[k.knob]!,
        mirror: (tables.params[k.name] as { mirror: number }).mirror,
      })),
    ) as Knob[][];
    expect(layout[0]!.map((k) => k.name)).toEqual(['Pg Alpha', 'Pg Beta', 'Pg Gamma']);
    expect(layout[1]!.map((k) => k.name)).toEqual(['Pg Epsilon', 'Pg Zeta']);
    expect(new Set(layout.flat().map((k) => k.slot)).size, 'five parameters, five slots').toBe(5);
    expect(new Set(layout.flat().map((k) => k.mirror)).size, 'five parameters, five Sound words').toBe(5);
    for (const k of layout.flat()) expect(tables.params[k.name], `${k.name}: the declared range and default`).toMatchObject(DECLARED[k.name]);
    const empty = pages.map((p) => p.knobs.flatMap((k, knob) => (k === 0 ? [knob] : [])));
    for (const e of empty) expect(e.length, 'each page leaves knobs empty').toBeGreaterThan(0);

    const type = await dynType(session, run.plan, ID, 'dyn_pages');
    // FUNC+SRC, five DOWN from FM TONE (FM TONE, FM DRUM, WAVETONE, SWARMER, MIDI, then the first run-time machine; the hint of the round-trip test), YES, YES
    await pickSine(rt, 5);
    await rt.ui.tap('TRIG 1', { until: { drained: true } });
    const mirror = await pollUntil(rt.clock, () => word(session, DN2_AUDIO_MIRROR_112), (w) => w >>> 24 >= 16, {
      withinMs: 3000,
      message: 'the audio mirror holds a run-time type after the first TRIG',
      intervalMs: 50,
    });
    expect(mirror >>> 24, 'the audio mirror holds the committed type').toBe(type);

    // the declared defaults of both pages are in the Sound words after the selection
    const words = await pollUntil(rt.clock, () => soundWords(session), (w) => layout.flat().every((k) => w[k.mirror] === DECLARED[k.name].default), {
      withinMs: 3000,
      message: 'the Sound words hold the defaults of both pages',
      intervalMs: 50,
    });
    expect(
      layout.flat().map((k) => [k.name, words[k.mirror]]),
      'the Sound words of the five parameters are their defaults',
    ).toEqual(layout.flat().map((k) => [k.name, DECLARED[k.name].default]));

    const syn = () => rt.ui.tap('SYN', { until: { ms: 400, why: REDRAW } });

    await syn(); // page A
    await exercisePage(rt, 'page A', layout[0]!, empty[0]!, { which: 1, up: true });
    await syn(); // page B
    await exercisePage(rt, 'page B', layout[1]!, empty[1]!, { which: 1, up: true });
    expect(layout[1]![0]!.mirror - layout[0]![0]!.mirror, 'the words are as far apart as the slots').toBe(layout[1]![0]!.slot - layout[0]![0]!.slot);
    await syn(); // page A again
    const a0 = layout[0]![0]!;
    expect((await turnChangesOneWord(rt, ENCODERS[a0.knob]!)).index, 'on the third SYN the first knob edits ALPH again').toBe(a0.mirror);
    await session.expectNoFault('two pages cycled and edited');
  }, 900_000);
});
