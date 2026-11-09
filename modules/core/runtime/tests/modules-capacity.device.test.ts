import fs from 'node:fs';
import { hasCap } from '@digi/dk-session';
import { CMD, LOG_KIND } from '@digi/dk-session/gen_proto';
import { expect } from 'vitest';
import type { PatchedSession } from '@digi/regression/support.ts';
import { deviceProfile } from '@digi/regression/runtime/device-data.ts';
import { word } from '@digi/regression/runtime/fx-live.ts';
import { liveArtifacts } from '@digi/regression/runtime/artifacts.ts';
import { DEBUG_MODULE, installModules, moduleArtifact } from '@digi/regression/runtime/install.ts';
import { expectRuntimeReady, runtimeTests } from '@digi/regression/runtime/ready.ts';
import { ofKind } from '@digi/regression/runtime/runtime-menu.ts';
import { planSymbol, prepareRuntimePlan } from '@digi/regression/runtime/runtime-plan.ts';

/** MODULES (protocol command 3): after a boot with two modules with DSP code (dyn-params and sine, then the debug module) the reply lists them with ranges
 * inside the arena and the DSP area, the contract bytes and version of the file and the assigned machine types; it pages by start index and is not
 * logged as an UPLOAD event. CAPACITY lists every kind with the device file's limit. */
async function expectModulesReply(runtime: PatchedSession): Promise<void> {
  const { session, dk } = runtime;
  await expectRuntimeReady(session);
  const device = session.options.device;
  const plan = await prepareRuntimePlan(session, liveArtifacts(session, 'modules-capacity'), ['DKRT_STATE']);
  const ids = ['test/dyn-params', 'test/sine', DEBUG_MODULE]; // the debug module last: it owns no machine
  await installModules(
    dk,
    ids.map((id) => ({ id, artifact: moduleArtifact(device, id) })),
  );
  const info = await dk.info();
  expect(hasCap(info, 'MODULES') && hasCap(info, 'CAPACITY'), 'INFO sets the MODULES and CAPACITY capabilities').toBe(
    true,
  );
  const uploadsBefore = ofKind(await dk.readLog(), LOG_KIND.UPLOAD).length;
  const modules = await dk.modules();
  const [pl, xs] = modules as [(typeof modules)[0], (typeof modules)[0]];
  expect(modules.map((m) => [m.index, m.name])).toEqual(ids.map((id, i) => [i, id]));
  ids.slice(0, 2).forEach((id, i) => {
    const raw = fs.readFileSync(moduleArtifact(device, id));
    expect(modules[i]!.version, `${id}: the file's version`).toBe(raw.readUInt32BE(0x34));
    expect(Buffer.from(modules[i]!.contracts.flat()), `${id}: the contract bytes`).toEqual(raw.subarray(0x38, 0x40));
  });

  const cap = await dk.capacity();
  const state = planSymbol(plan, 'DKRT_STATE');
  const [arenaLo, arenaHi] = [await word(session, state + 28), await word(session, state + 32)];
  const disjoint = (a: [number, number], b: [number, number]) => a[0] + a[1] <= b[0] || b[0] + b[1] <= a[0];
  for (const m of modules) {
    expect(m.cf_size, `${m.name}: a ColdFire range`).toBeGreaterThan(0);
    expect(m.cf_base).toBeGreaterThanOrEqual(arenaLo);
    expect(m.cf_base + m.cf_size, 'the range lies inside the arena').toBeLessThanOrEqual(arenaHi);
  }
  expect(disjoint([pl.cf_base, pl.cf_size], [xs.cf_base, xs.cf_size]), 'the ColdFire ranges do not overlap').toBe(true);
  const arena = cap.ARENA!;
  const arenaFree = arena.limit - arena.used;
  expect(arena.used, 'the arena use covers the modules').toBeGreaterThanOrEqual(
    modules.reduce((n, m) => n + m.cf_size, 0),
  );
  expect(arena.largest).toBeGreaterThan(0);
  expect(arena.largest).toBeLessThanOrEqual(arenaFree);

  const dsp = cap.DSP_AREA!;
  expect(pl.dsp_len).toBeGreaterThan(0);
  expect(xs.dsp_len).toBeGreaterThan(0);
  expect(dsp.limit - dsp.used, 'the free DSP area leaves room').toBeGreaterThan(0);
  expect(dsp.largest).toBeGreaterThan(0);
  expect(dsp.largest).toBeLessThanOrEqual(dsp.limit - dsp.used);
  expect(disjoint([pl.dsp_off, pl.dsp_len], [xs.dsp_off, xs.dsp_len]), 'the DSP ranges do not overlap').toBe(true);

  const popcount = (n: number) => n.toString(2).replace(/0/g, '').length;
  expect([pl.types, xs.types], 'the lowest free machine types, in load order').toEqual([[16], [17]]);
  expect([popcount(pl.rows), popcount(xs.rows)], 'one voice row each').toEqual([1, 1]);
  for (const m of [pl, xs]) expect([m.faults, m.guard_off], `${m.name}: no guard fault`).toEqual([0, false]);

  const file = deviceProfile(device);
  const limits = Object.fromEntries(Object.entries(cap).map(([k, v]) => [k, v.limit]));
  expect(limits, "CAPACITY lists the device file's limits").toMatchObject({
    MODULES: file.limits.modules,
    ARENA: arenaHi - arenaLo,
    STAGE: file.limits.stage_bytes,
    DSP_AREA: file.digikit.dsp_area_bytes,
    MACHINE_ROWS: file.limits.machine_rows,
    VOICE_ROWS: file.limits.machine_rows,
    PARAM_ROWS: file.limits.param_rows,
    PAGE_ROWS: file.limits.page_rows,
    SETTINGS: 16 * 512,
  });
  const used = Object.fromEntries(Object.entries(cap).map(([k, v]) => [k, v.used]));
  expect(used, 'CAPACITY use agrees with MODULES: three modules, two voice rows and machine types').toMatchObject({
    MODULES: 3,
    SELECTION: 3,
    VOICE_ROWS: 2,
    MACHINE_ROWS: 2,
    MAP_ENTRIES: 2,
  });
  expect(used.PARAM_ROWS).toBeGreaterThan(0);

  // paging: from record index 1 the sine and the debug module, past the end nothing; the total stays 3
  const page = async (start: number) => (await dk.request(CMD.MODULES, Uint8Array.of(start))).body;
  const [from1, from9] = [await page(1), await page(9)];
  expect([from1[16], from1[17], from1[18]], 'MODULES pages by start index').toEqual([3, 2, 1]);
  expect([from9[16], from9[17]]).toEqual([3, 0]);
  expect(ofKind(await dk.readLog(), LOG_KIND.UPLOAD), 'MODULES is not logged as an UPLOAD event').toHaveLength(
    uploadsBefore,
  );
  await session.expectNoFault('modules reply');
}

runtimeTests('MODULES and CAPACITY list the loaded modules, ranges, types and limits', expectModulesReply, { timeout: 900_000,
  devices: ['dt2'],
});
