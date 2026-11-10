import fs from 'node:fs';
import path from 'node:path';
import type { DkModule } from '@digi/dk-session';
import { expect, test } from 'vitest';
import { cardRigs, RT, type CardRig } from '@digi/regression/runtime/card-rig.ts';
import { needsOf, parseDspSection, relocate } from '@digi/regression/runtime/dkm.ts';
import { dspBytes } from '@digi/regression/runtime/fx-live.ts';
import { moduleArtifact } from '@digi/regression/runtime/install.ts';
import { bootEntries, COUPLED_ROOTS, expectBoot, SYMBOLS, type Entry } from './capacity-helpers.ts';
import {
  block,
  CODE,
  DKM_HEADER,
  DSP_GRANULE,
  E_NOMEM,
  fullSet,
  geometryOf,
  LOADED,
  packSynth,
  stageFilePad,
  usageOf,
  type Geometry,
  type SynthSpec,
  type Want,
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
 */

const E_FORMAT = -5;
const E_LOADED = -10;
const E_TOO_BIG = -13;

/** Pack the synthetic modules SPECS (with their wanted results) for the rig's device into the card entries. */
async function synthEntries(rig: CardRig, tag: string, mods: { spec: SynthSpec; want: Want }[]): Promise<Entry[]> {
  const dir = path.join(rig.artifacts, tag);
  const out: Entry[] = [];
  for (const [i, m] of mods.entries())
    out.push({ name: m.spec.name, file: await packSynth(rig.device, m.spec, dir, `${i}.dkm`), want: m.want });
  return out;
}

// ---- the full set: every limit used at once ------------------------------------------------------------------------------------

/** Every voice row has an owner, every machine type is in use, every row of CAPACITY is used up, and no whole DSP granule is free. */
function expectFull(
  g: Geometry,
  mods: DkModule[],
  cap: Record<string, { limit: number; used: number; largest: number }>,
) {
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

// Keep DSP transfers pending: zero mode advances SSI blocks and times out without a SHARC peer.
cardRigs('capacity, ColdFire only', { roots: RT, dsp: 'cold', symbols: SYMBOLS }, (rig) => {
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
      expect(got.cap.ARENA!.used, 'the arena holds the two loaded blocks and nothing of the refused files').toBe(
        mods.reduce((n, m) => n + block(m.cf_size), 0),
      );
    },
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
  );
});

// ---- coupled: the reference module's DSP bytes, and the needs the runtime takes ------------------------------------------------

/**
 * The reference module of the full set: a module with DSP code and no DSP imports, so that its packed image can be relocated without the runtime's
 * import table (`relocate` of dkm.ts). Every machine module imports from the runtime; an effect does not.
 */
const REFERENCE = 'test/dyn-fx-gain';

cardRigs('capacity, coupled', { dsp: 'sharc', roots: COUPLED_ROOTS, hold: true, symbols: SYMBOLS }, (rig) => {
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
  );
});
