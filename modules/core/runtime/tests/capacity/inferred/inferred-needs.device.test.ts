import fs from 'node:fs';
import { expect, test } from 'vitest';
import { cardRigs } from '@digi/regression/runtime/card-rig.ts';
import { needsOf, recordBytesOf } from '@digi/regression/runtime/dkm.ts';
import { moduleArtifact } from '@digi/regression/runtime/install.ts';
import { block, geometryOf, LOADED } from '@digi/regression/runtime/synth.ts';
import { bootEntries, COUPLED_ROOTS, expectBoot, popcount, SYMBOLS, type Entry } from '../capacity-helpers.ts';

/**
 * The needs that `patchkit needs` infers from a packed module's contents are what the runtime takes of each resource once the module is loaded:
 * the arena block, the DSP range, the voice, machine, parameter and page rows and the effect rows. The shipped debug modules and the fixtures
 * that stand for the other shapes of a module (a raw ColdFire module, a machine, an effect). One boot per module: full tier.
 */
const INFERRED_NEEDS_MODULES = [
  'digikit/debug',
  'digikit/probe',
  'digikit/remote',
  'test/c-demo',
  'test/commit-demo',
  'test/dyn-dn2',
  'test/dyn-dn2-pages',
  'test/dyn-fx-gain',
  'test/sine',
];
/** Fixtures that build for the Digitone II only. */
const DN2_ONLY = new Set(['test/dyn-dn2-pages']);

cardRigs('inferred needs', { dsp: 'sharc', roots: COUPLED_ROOTS, hold: true, full: true, symbols: SYMBOLS }, (rig) => {
  for (const id of INFERRED_NEEDS_MODULES)
    test.skipIf(DN2_ONLY.has(id) && rig.device !== 'dn2')(
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
        const kinds = [
          'ARENA',
          'DSP_AREA',
          'MACHINE_ROWS',
          'VOICE_ROWS',
          'PARAM_ROWS',
          'PAGE_ROWS',
          'FX_SLOTS',
        ] as const;
        expect.soft(
          Object.fromEntries(kinds.map((k) => [k, got.cap[k]!.used])),
          `taken (CAPACITY), inferred ${JSON.stringify(n)}`,
        ).toEqual(Object.fromEntries(kinds.map((k) => [k, n![k]])));
        const rec = mods[0]!;
        // ARENA is the module's image block and the record block its `init` keeps (`record_bytes`: the RES_RECORD_BYTES item of the USES entry in the tagged keys table, header pair 0x90/0x94)
        const records = recordBytesOf(fs.readFileSync(file));
        expect.soft(
          [block(rec.cf_size) + (records ? block(records) : 0), rec.dsp_len, popcount(rec.rows)],
          'the MODULES record: arena blocks (image, records), DSP range, voice rows',
        ).toEqual([n!.ARENA, n!.DSP_AREA, n!.VOICE_ROWS]);
      },
    );
});
