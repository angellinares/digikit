import fs from 'node:fs';
import { expect, test } from 'vitest';
import { cardRigs } from '@digi/regression/runtime/card-rig.ts';
import { needsOf, recordBytesOf } from '@digi/regression/runtime/dkm.ts';
import { moduleArtifact } from '@digi/regression/runtime/install.ts';
import { boundPath, neededServices } from '@digi/regression/runtime/service-files.ts';
import { block, geometryOf, LOADED } from '@digi/regression/runtime/synth.ts';
import { bootEntries, COUPLED_ROOTS, expectBoot, popcount, SYMBOLS, type Entry } from '../capacity-helpers.ts';

/** The arena of the providers of a module's services: their image blocks and the blocks of their keys tables (loader/role.rs). */
const providerArena = async (device: Parameters<typeof boundPath>[0], file: string) => {
  let sum = 0;
  for (const name of neededServices([file])) {
    const service = boundPath(device, name);
    const [n] = await needsOf([service]);
    sum += n!.ARENA! + block(fs.readFileSync(service).readUInt32BE(0x94));
  }
  return sum;
};

/**
 * The needs that `patchkit needs` infers from a packed module's contents are what the runtime takes of each resource once the module is loaded:
 * the arena block, the DSP range, the voice, machine, parameter and page rows and the effect rows. The shipped debug modules and the fixtures
 * that stand for the other shapes of a module (a raw ColdFire module, a machine, an effect). One boot per module: full tier.
 *
 * A module that needs a service file (machines.dks, fx.dks) also takes the arena of the provider the runtime loads with it: the provider's image block
 * (`patchkit needs` of the bound file) and the block that keeps its keys table (BIND, EXPORTS and SLOTS; loader/role.rs). ARENA is compared with both added.
 */
const INFERRED_NEEDS_MODULES = [
  'digikit/debug',
  'digikit/probe',
  'digikit/remote',
  'test/c-demo',
  'test/commit-demo',
  'test/dyn-dn2',
  'test/dyn-fx-gain',
  'test/sine',
];
cardRigs('inferred needs', { dsp: 'sharc', roots: COUPLED_ROOTS, hold: true, full: true, symbols: SYMBOLS }, (rig) => {
  for (const id of INFERRED_NEEDS_MODULES)
    test(
      `${id}: the resources taken are the needs inferred from its contents`,
      async () => {
        await rig.first();
        const g = geometryOf(rig.device, rig.plan);
        const file = moduleArtifact(rig.device, id);
        const [n] = await needsOf([file]);
        const provided = await providerArena(rig.device, file);
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
        ).toEqual(Object.fromEntries(kinds.map((k) => [k, k === 'ARENA' ? n![k]! + provided : n![k]])));
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
