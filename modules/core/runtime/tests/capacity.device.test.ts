import fs from 'node:fs';
import path from 'node:path';
import type { DkModule } from '@digi/dk-session';
import { expect, test } from 'vitest';
import { bootInfo, E_LOADING } from '@digi/regression/runtime/card-kit.ts';
import { CARD_SYMBOLS, cardRigs, RT, type CardRig } from '@digi/regression/runtime/card-rig.ts';
import { CARD_TIMEOUT } from '@digi/regression/runtime/debug-kit.ts';
import { needsOf, parseDspSection, relocate } from '@digi/regression/runtime/dkm.ts';
import { dspBytes, word } from '@digi/regression/runtime/fx-live.ts';
import { moduleArtifact } from '@digi/regression/runtime/install.ts';
import { ringEvents } from '@digi/regression/runtime/runtime-menu.ts';
import { planSymbol } from '@digi/regression/runtime/runtime-plan.ts';
import {
  block,
  CODE,
  DKM_HEADER,
  DSP_GRANULE,
  geometryOf,
  MAX_VOICES,
  packSynth,
  pinned,
  stageFilePad,
  type Geometry,
  type SynthSpec,
} from '@digi/regression/runtime/synth.ts';

/**
 * Capacity of the runtime on the real memory map (docs/plans/capacity-isolation.md; ledger docs/plans/ledgers/debug-card-safe.md, sections capacity,
 * inferred-needs; port of tools/patchkit/pk_digikit_capacity.py and tools/patchkit/pk_capacity.py). The pure limit logic is unit-tested in dkrt-sdk
 * (capacity.rs, sticky.rs); these tests load synthetic modules (`patchkit synth-dkm`) and real ones through the real loader:
 *
 * - full set: one module per limit takes it to the last unit (machine types, parameter and page rows, voice rows, the DSP area to its last whole granule,
 *   the arena to the last byte), and the next module of each kind is refused with the limit's own code. ColdFire only, then coupled with a real reference
 *   module whose DSP bytes in L2 are compared with its packed image.
 * - files and arena: a file of the staging buffer's size loads and one 4 bytes over does not; a bss that fills the arena, one above it.
 * - inferred needs: what `patchkit needs` infers from a module's file is what the runtime takes.
 */

const E_FORMAT = -5;
const E_NOMEM = -8;
const E_LOADED = -10;
const E_TOO_BIG = -13;
const E_DSP_NOSPACE = -33;
const E_DSP_ROWS = -34;
const LOADED = 'loaded';
type Want = number | typeof LOADED;

/** One module of a card in boot order: its file, the name it reports and the result the loader must give. */
interface Entry {
  name: string;
  file: string;
  want: Want;
}

const SYMBOLS = [...CARD_SYMBOLS, 'DKRT_ARENA', '_dl_area', 'DKRT_TABLE_BAD'];
const popcount = (n: number) => n.toString(2).replace(/0/g, '').length;
const resultOk = (got: number, want: Want) => (want === LOADED ? got === 0 || got === E_LOADING : got === want);

/** Pack the synthetic modules SPECS (with their wanted results) for the rig's device into the card entries. */
async function synthEntries(rig: CardRig, tag: string, mods: { spec: SynthSpec; want: Want }[]): Promise<Entry[]> {
  const dir = path.join(rig.artifacts, tag);
  const out: Entry[] = [];
  for (const [i, m] of mods.entries())
    out.push({ name: m.spec.name, file: await packSynth(rig.device, m.spec, dir, `${i}.dkm`), want: m.want });
  return out;
}

/** Boot a card of ENTRIES (selected in order) and read what the runtime says. */
async function bootEntries(rig: CardRig, tag: string, entries: Entry[]) {
  const rt = await rig.load(tag, {
    modules: entries.map((e, i) => ({ name: `modules/m${i}.dkm`, file: e.file })),
    selection: entries.map((_, i) => `modules/m${i}.dkm`),
  });
  const info = await bootInfo(rt.session, rig.plan);
  const { mods, cap } = await rig.usb(rt, async (dk) => ({ mods: await dk.modules(), cap: await dk.capacity() }));
  return { rt, info, mods, cap };
}

/** Every nonzero range [off, off + len) lies in [lo, hi) and no two overlap. */
function rangesOk(ranges: [number, number][], lo: number, hi: number): boolean {
  const rs = ranges.filter(([, len]) => len).map(([off, len]) => [off, off + len] as const).sort((a, b) => a[0] - b[0]);
  return rs.every(([a, b]) => lo <= a && b <= hi) && rs.every((r, i) => i === rs.length - 1 || r[1] <= rs[i + 1]![0]);
}

/** The checks every boot of a set shares. Returns the loaded modules. */
async function expectBoot(rig: CardRig, g: Geometry, entries: Entry[], got: Awaited<ReturnType<typeof bootEntries>>) {
  const { rt, info, mods, cap } = got;
  expect(info.count, 'the boot saw every selected module').toBe(entries.length);
  entries.forEach((e, i) =>
    expect(resultOk(info.results[i]!, e.want), `${e.name}: result ${info.results[i]}, wanted ${e.want}`).toBe(true),
  );
  const loaded = entries.filter((e) => e.want === LOADED).map((e) => e.name);
  expect(
    mods.map((m) => m.name),
    'MODULES lists exactly the loaded modules, in order',
  ).toEqual(loaded);
  expect(
    rangesOk(
      mods.map((m) => [m.cf_base, m.cf_size]),
      g.arenaLo,
      g.arenaHi,
    ),
    'ColdFire blocks lie in the arena and do not overlap',
  ).toBe(true);
  expect(
    rangesOk(
      mods.map((m) => [m.dsp_off, m.dsp_len]),
      0,
      g.dspArea,
    ),
    'DSP ranges lie in the area and do not overlap',
  ).toBe(true);
  const rows = mods.reduce((n, m) => n | m.rows, 0);
  expect(cap.MODULES!, 'CAPACITY modules').toMatchObject({ limit: g.modules, used: loaded.length });
  expect(cap.SELECTION!.used, 'CAPACITY selection').toBe(entries.length);
  expect(cap.ARENA!.limit, 'CAPACITY arena').toBe(g.arenaFree);
  expect(cap.STAGE!, 'CAPACITY stage').toMatchObject({ limit: g.stageBytes, largest: g.stageBytes });
  expect(cap.DSP_AREA!.limit, 'CAPACITY DSP area').toBe(g.dspArea);
  expect(cap.VOICE_ROWS!, 'CAPACITY voice rows are the owned rows').toMatchObject({
    limit: g.machineRows,
    used: popcount(rows),
  });
  expect(
    [cap.MACHINE_ROWS!.limit, cap.PARAM_ROWS!.limit, cap.PAGE_ROWS!.limit],
    'CAPACITY registry rows',
  ).toEqual([g.machineRows, g.paramRows, g.pageRows]);
  const bad = (await word(rt.session, planSymbol(rig.plan, 'DKRT_TABLE_BAD'))) >>> 24;
  expect(
    [bad, (await ringEvents(rt.session, rig.plan, 'TABLE_BAD')).length],
    'the table check never failed: a refused module left nothing behind',
  ).toEqual([0, 0]);
  return mods;
}

// ---- the full set: every limit used at once ------------------------------------------------------------------------------------

/** What a packed module takes of each resource (`patchkit needs`). */
interface Usage {
  arena: number;
  arenaXfer: number;
  dspLen: number;
  dspAlign: number;
  rows: number;
  machines: number;
  params: number;
  pages: number;
}

function usageOf(n: Record<string, number>): Usage {
  return {
    arena: n.ARENA!,
    arenaXfer: n.ARENA_XFER!,
    dspLen: n.DSP_AREA!,
    dspAlign: n.DSP_ALIGN!,
    rows: n.VOICE_ROWS!,
    machines: n.MACHINE_ROWS!,
    params: n.PARAM_ROWS!,
    pages: n.PAGE_ROWS!,
  };
}

/**
 * The modules that, behind the reference modules REFS (loaded first), use every limit at once, followed by three that do not fit:
 * M takes the machine types, parameter rows and page rows the references leave; V the voice rows (pinned types, behind the references' hashed ones);
 * D the DSP area, to its last whole granule; F the arena, to the last byte (the references' and V's and D's blocks and the arena copies of their DSP
 * images counted: a ColdFire-only rig keeps those until a transfer that never comes). Then X (a raw module: E_NOMEM), Y (a voice: E_DSP_ROWS) and
 * Z (a DSP module: E_DSP_NOSPACE).
 */
function fullSet(g: Geometry, refs: Usage[]): { spec: SynthSpec; want: Want }[] {
  const sum = (key: keyof Usage) => refs.reduce((n, r) => n + r[key], 0);
  const typesLeft = g.machineRows - sum('machines');
  const rowsLeft = g.machineRows - sum('rows');
  const paramsLeft = g.paramRows - sum('params');
  const pagesLeft = g.pageRows - sum('pages');
  if (refs.length + 7 > g.modules) throw new Error(`the full set needs ${refs.length + 7} modules, the device has ${g.modules}`);
  if (rowsLeft > MAX_VOICES || rowsLeft < 1 || typesLeft < 1)
    throw new Error(`the references leave ${rowsLeft} rows and ${typesLeft} types`);
  if (pagesLeft > typesLeft) throw new Error(`${pagesLeft} page rows left, but only ${typesLeft} machines to put them on`);
  const firstPinned = g.typeFirst + sum('rows');
  // the DSP area: references, then V (one granule), then D takes every whole granule left (first fit from the area's start, each range at the
  // first address its alignment allows)
  let cursor = 0;
  for (const [length, align] of [...refs.filter((r) => r.dspLen).map((r) => [r.dspLen, r.dspAlign] as const), [DSP_GRANULE, 8] as const]) {
    const start = Math.ceil((g.dspBase + cursor) / align) * align - g.dspBase;
    cursor = start + length;
  }
  const dStart = Math.ceil((g.dspBase + cursor) / 8) * 8 - g.dspBase;
  const dLen = Math.floor((g.dspArea - dStart) / DSP_GRANULE) * DSP_GRANULE;
  if (dLen < DSP_GRANULE) throw new Error('the references leave no DSP area for D');
  // every raw block is `block(CODE)`; V's and D's DSP images keep an arena copy of `block(64)` each (as does every reference's)
  const used = refs.reduce((n, r) => n + r.arena + r.arenaXfer, 0) + 3 * block(CODE) + 2 * block(DSP_GRANULE);
  const fTotal = g.arenaFree - used - 16;
  if (fTotal < CODE) throw new Error('the references leave no arena for F');
  return [
    { spec: { name: 'cap/full-m', machines: typesLeft, params: paramsLeft, pages: pagesLeft }, want: LOADED },
    {
      spec: {
        name: 'cap/full-v',
        dsp: { image: 64, voices: pinned(Array.from({ length: rowsLeft }, (_, i) => firstPinned + i)) },
      },
      want: LOADED,
    },
    { spec: { name: 'cap/full-d', dsp: { image: 64, bss: dLen - 64 } }, want: LOADED },
    { spec: { name: 'cap/full-f', bss: fTotal - CODE }, want: LOADED },
    { spec: { name: 'cap/full-x' }, want: E_NOMEM },
    { spec: { name: 'cap/full-y', dsp: { image: 64, voices: pinned([firstPinned]) } }, want: E_DSP_ROWS },
    { spec: { name: 'cap/full-z', dsp: { image: 64 } }, want: E_DSP_NOSPACE },
  ];
}

/** Every voice row has an owner, every machine type is in use, every row of CAPACITY is used up, and no whole DSP granule is free. */
function expectFull(g: Geometry, mods: DkModule[], cap: Record<string, { limit: number; used: number; largest: number }>) {
  const owned = mods.reduce((n, m) => n | m.rows, 0);
  expect(owned, 'every voice row has an owner').toBe(2 ** g.machineRows - 1);
  expect(
    [...new Set(mods.flatMap((m) => m.types))].sort((a, b) => a - b),
    'every machine type is in use',
  ).toEqual(Array.from({ length: g.machineRows }, (_, i) => g.typeFirst + i));
  expect(
    ['MACHINE_ROWS', 'VOICE_ROWS', 'PARAM_ROWS', 'PAGE_ROWS'].map((k) => cap[k]!.used),
    'CAPACITY counts every machine, voice, parameter and page row as used',
  ).toEqual([g.machineRows, g.machineRows, g.paramRows, g.pageRows]);
  expect(cap.DSP_AREA!.largest, 'the DSP area has no whole granule left').toBeLessThan(DSP_GRANULE);
}

cardRigs(
  'capacity, ColdFire only',
  { roots: RT, dsp: 'zero', symbols: SYMBOLS },
  (rig) => {
    test(
      'full set: every limit is filled to its last unit and the next module of each kind is refused',
      async () => {
        await rig.first();
        const g = geometryOf(rig.device, rig.plan);
        const entries = await synthEntries(rig, 'full', fullSet(g, []));
        const got = await bootEntries(rig, 'full', entries);
        const mods = await expectBoot(rig, g, entries, got);
        expectFull(g, mods, got.cap);
        expect(
          [got.cap.ARENA!.used, got.cap.ARENA!.largest],
          'the arena has no free byte (the DSP images keep their arena copy on a ColdFire-only rig)',
        ).toEqual([g.arenaFree, 0]);
      },
      CARD_TIMEOUT,
    );

    test(
      'files: a file of the staging buffer size loads with its whole image, 4 bytes more is E_TOO_BIG, a repeated name is E_LOADED',
      async () => {
        await rig.first();
        const g = geometryOf(rig.device, rig.plan);
        const entries = await synthEntries(rig, 'files', [
          { spec: { name: 'cap/f-first' }, want: LOADED },
          { spec: { name: 'cap/f-first', pad: 4 }, want: E_LOADED },
          { spec: { name: 'cap/f-exact', pad: stageFilePad(g.stageBytes), fill: 0xa5 }, want: LOADED },
          { spec: { name: 'cap/f-big', pad: stageFilePad(g.stageBytes, 4), oversize: true }, want: E_TOO_BIG },
        ]);
        expect(
          entries.map((e) => fs.statSync(e.file).size),
          'the fixtures are exactly the staging buffer size and 4 bytes over',
        ).toEqual([expect.any(Number), expect.any(Number), g.stageBytes, g.stageBytes + 4]);
        const got = await bootEntries(rig, 'files', entries);
        const mods = await expectBoot(rig, g, entries, got);
        expect(mods[1]!.cf_size, 'the module of the staging buffer size is in the arena with its whole image').toBe(
          g.stageBytes - DKM_HEADER,
        );
        expect(
          got.cap.ARENA!.used,
          'the arena holds the two loaded blocks and nothing of the refused files',
        ).toBe(mods.reduce((n, m) => n + block(m.cf_size), 0));
      },
      CARD_TIMEOUT,
    );

    test(
      'arena: filled to the last byte; a raw module, a DSP module and a bss as large as the arena are E_NOMEM, a bss above the arena is E_FORMAT',
      async () => {
        await rig.first();
        const g = geometryOf(rig.device, rig.plan);
        const fTotal = g.arenaFree - block(CODE) - 16; // the block of F is its payload rounded up and the header: it takes the rest
        const entries = await synthEntries(rig, 'arena', [
          { spec: { name: 'cap/a-r' }, want: LOADED },
          { spec: { name: 'cap/a-f', bss: fTotal - CODE }, want: LOADED },
          { spec: { name: 'cap/a-x' }, want: E_NOMEM },
          { spec: { name: 'cap/a-y', dsp: { image: 64 } }, want: E_NOMEM },
          { spec: { name: 'cap/a-z', bss: g.arenaBytes - 16 }, want: E_NOMEM },
          { spec: { name: 'cap/a-w', bss: g.arenaBytes + 4 }, want: E_FORMAT },
        ]);
        const got = await bootEntries(rig, 'arena', entries);
        const mods = await expectBoot(rig, g, entries, got);
        expect(
          [got.cap.ARENA!.used, got.cap.ARENA!.largest, mods.reduce((n, m) => n + block(m.cf_size), 0)],
          'every byte of the arena is in use, no free block',
        ).toEqual([g.arenaFree, 0, g.arenaFree]);
        expect(
          [got.cap.DSP_AREA!.used, got.cap.VOICE_ROWS!.used],
          'the refused DSP module left no range in the DSP area and no row',
        ).toEqual([0, 0]);
      },
      CARD_TIMEOUT,
    );
  },
);

// ---- coupled: the reference module's DSP bytes, and the needs the runtime takes ------------------------------------------------

/**
 * The reference module of the full set: a module with DSP code and no DSP imports, so that its packed image can be relocated without the runtime's
 * import table (`relocate` of dkm.ts). Every machine module imports from the runtime; an effect does not.
 */
const REFERENCE = 'test/dyn-fx-gain';

/** The module core/input, core/dynfx and core/fxpages are patched in for the modules that import from them or take rows in them. */
const COUPLED_ROOTS = [...RT, 'core/input', 'core/dynfx', 'core/fxpages'];

cardRigs(
  'capacity, coupled',
  { roots: COUPLED_ROOTS, hold: true, symbols: SYMBOLS },
  (rig) => {
    test(
      'full set behind a real module: the reference keeps its ranges and its DSP bytes in L2, every row is used up',
      async () => {
        await rig.first();
        const g = geometryOf(rig.device, rig.plan);
        const refFile = moduleArtifact(rig.device, REFERENCE);
        const [refNeeds] = await needsOf([refFile]);
        const fill = await synthEntries(rig, 'coupled-full', fullSet(g, [usageOf(refNeeds!)]));
        const entries: Entry[] = [{ name: REFERENCE, file: refFile, want: LOADED }, ...fill];
        const got = await bootEntries(rig, 'coupled-full', entries);
        const mods = await expectBoot(rig, g, entries, got);
        expectFull(g, mods, got.cap);
        // the DSP part of every module is done: the arena holds the blocks of the modules only
        expect(got.cap.ARENA!.used, 'the copies of the DSP images in the arena are freed').toBeLessThan(g.arenaFree);
        const ref = mods[0]!;
        const sec = parseDspSection(fs.readFileSync(refFile));
        const base = g.dspBase + ref.dsp_off;
        const want = relocate(sec, base);
        expect(
          await dspBytes(got.rt.session, base, want.length),
          "the reference's DSP bytes in L2 equal its packed image relocated at its range",
        ).toEqual(want);
      },
      CARD_TIMEOUT,
    );
  },
);

/**
 * The needs that `patchkit needs` infers from a packed module's contents are what the runtime takes of each resource once the module is loaded:
 * the arena block, the DSP range, the voice, machine, parameter and page rows and the effect rows. Real modules of the repository and the fixtures
 * that stand for the other shapes of a module (a raw ColdFire module, a machine, an effect, a service). One boot per module: full tier.
 */
const INFERRED_NEEDS_MODULES = [
  'digikit/debug',
  'digikit/probe',
  'digikit/remote',
  'digikit/freqshift',
  'digikit/pluck',
  'fadedlimes/analog',
  'fadedlimes/bd',
  'fadedlimes/pd',
  'test/c-demo',
  'test/dyn-params',
  'test/dyn-fx-gain',
  'test/sine',
];

/** Modules with no Digitone II port. */
const DT2_ONLY = ['test/dyn-params'];

cardRigs(
  'inferred needs',
  { roots: COUPLED_ROOTS, hold: true, full: true, symbols: SYMBOLS },
  (rig) => {
    for (const id of INFERRED_NEEDS_MODULES.filter((m) => !(rig.device === 'dn2' && DT2_ONLY.includes(m))))
      test(
        `${id}: the resources taken are the needs inferred from its contents`,
        async () => {
          await rig.first();
          const g = geometryOf(rig.device, rig.plan);
          const file = moduleArtifact(rig.device, id);
          const [n] = await needsOf([file]);
          const entries: Entry[] = [{ name: id, file, want: LOADED }];
          const got = await bootEntries(rig, `needs-${id.replace(/\//g, '-')}`, entries);
          const mods = await expectBoot(rig, g, entries, got);
          // the DSP part is done: the arena holds the module's block only (the DSP image's copy is freed) and every row is in the registry
          const kinds = ['ARENA', 'DSP_AREA', 'MACHINE_ROWS', 'VOICE_ROWS', 'PARAM_ROWS', 'PAGE_ROWS', 'FX_SLOTS'] as const;
          expect(
            Object.fromEntries(kinds.map((k) => [k, got.cap[k]!.used])),
            `taken (CAPACITY), inferred ${JSON.stringify(n)}`,
          ).toEqual(Object.fromEntries(kinds.map((k) => [k, n![k]])));
          const rec = mods[0]!;
          expect(
            [block(rec.cf_size), rec.dsp_len, popcount(rec.rows)],
            'the MODULES record: arena block, DSP range, voice rows',
          ).toEqual([n!.ARENA, n!.DSP_AREA, n!.VOICE_ROWS]);
        },
        CARD_TIMEOUT,
      );
  },
);
