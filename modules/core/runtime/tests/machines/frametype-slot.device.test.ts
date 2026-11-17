import { expect } from 'vitest';
import { liveArtifacts } from '@digi/regression/runtime/artifacts.ts';
import { word } from '@digi/regression/runtime/fx-live.ts';
import { DEBUG_MODULE, installModules, moduleArtifact } from '@digi/regression/runtime/install.ts';
import { expectRuntimeReady, runtimeTests } from '@digi/regression/runtime/ready.ts';
import { planSymbol, prepareRuntimePlan } from '@digi/regression/runtime/runtime-plan.ts';
import { nameHash } from '../refusal/dkm-patch.ts';

/**
 * The first proof of a pointer slot on a device (docs/plans/runtime-and-services.md, R12-6): the interrupt site of the Digitone II's frame builder (`frametype`) jumps to an
 * image stub that loads the slot word `frametype`. The word is 0 after a boot without the machines service (the stub runs the stock instructions), and the loader fills it
 * with the address of the service file's routine when `services/machines.dks` loads. The slot is found as a service finds it: by the name hash in the image's one info table
 * `dkrt_info_table` (`{magic "DKIT", count}`, then `{hash, address, kind}`). The machine fixture needs the service, so the harness puts the bound file on the card.
 *
 * Two boots: the plain runtime, then one with the service file and `test/dyn-dn2`. No SHARC audio is judged here (the machine suites do that); the DSP is on because the
 * fixture has a DSP part.
 */
const ID = 'test/dyn-dn2';
const INFO_MAGIC = 0x444b4954;

async function slotWord(session: Parameters<typeof word>[0], table: number, hash: number): Promise<number> {
  expect(await word(session, table), 'the info table has its magic').toBe(INFO_MAGIC);
  const count = await word(session, table + 4);
  for (let i = 0; i < count; i++) {
    if ((await word(session, table + 8 + 12 * i)) === hash) return word(session, await word(session, table + 12 + 12 * i));
  }
  throw new Error(`no entry ${hash.toString(16)} in the info table`);
}

runtimeTests(
  'the frame type slot is empty on a plain boot and holds the service routine when machines.dks is loaded',
  async (runtime) => {
    const { session, dk } = runtime;
    await expectRuntimeReady(session);
    const device = session.options.device;
    const plan = await prepareRuntimePlan(session, liveArtifacts(session, 'frametype-slot'), ['dkrt_info_table']);
    // `dkrt_info_table` is the image's table itself (a data symbol)
    const table = planSymbol(plan, 'dkrt_info_table');
    const hash = nameHash('frametype');
    expect(await slotWord(session, table, hash), 'no service: the stub falls through to the stock code').toBe(0);

    await installModules(dk, [
      { id: ID, artifact: moduleArtifact(device, ID) },
      { id: DEBUG_MODULE, artifact: moduleArtifact(device, DEBUG_MODULE) },
    ], { device, session });
    expect(await dk.loadFailures(), 'the service and the machine loaded').toEqual([]);
    expect(
      (await dk.modules()).map((m) => m.name),
      'the machine fixture is loaded',
    ).toContain(ID);
    const filled = await slotWord(session, table, hash);
    expect(filled, 'the slot holds an address inside the card-loaded service (above the image)').toBeGreaterThan(0x4000_0000);
  },
  { dsp: 'sharc', devices: ['dn2'] },
);
