import fs from 'node:fs';
import path from 'node:path';
import { hexSha256, type DkLogEvent } from '@digi/dk-session';
import { LOG_KIND } from '@digi/dk-session/gen_proto';
import { repoRoot, type Device } from '@digi/test-support';
import { expect, test } from 'vitest';
import { expectRuntimeBoot, startPatchedSession, type PatchedSession } from '@digi/regression/support.ts';
import { timePhase, timingContext } from '@digi/regression/timing.ts';
import { FIRMWARE, liveOptions } from '@digi/regression/runtime/devices.ts';
import { liveArtifacts, record } from '@digi/regression/runtime/artifacts.ts';
import { DEBUG_MODULE, installWithDebug, moduleArtifact } from '@digi/regression/runtime/install.ts';
import { expectRuntimeReady } from '@digi/regression/runtime/ready.ts';
import { waitEmulated } from '@digi/regression/runtime/waits.ts';

import { loadRuntimeModules, scanRuntimeModules as scanManifests, supportsDevice, type RuntimeModule } from './manifests.ts';

export type { RuntimeModule } from './manifests.ts';
export function scanRuntimeModules(root = path.join(repoRoot(), 'modules')): RuntimeModule[] {
  return scanManifests(root);
}
export const runtimeModules: readonly RuntimeModule[] = loadRuntimeModules(repoRoot(), {
  manifest: process.env.DIGI_CONFORMANCE_MANIFEST,
  ids: process.env.DIGI_CONFORMANCE_MODULES,
});

// Each cell starts its own daemon. Sessions of a worker reuse one cached run dir (fast boot);
// the store starts empty in every cell.
export function conformanceSuite(device: Device): void {
  for (const module of runtimeModules.filter((module) => supportsDevice(module, device))) {
    const name = `${device} ${FIRMWARE[device]} loads and registers ${module.id} through USB without runtime faults`;
    test(name, async () => {
      const options = liveOptions(device);
      const artifact = moduleArtifact(device, module.id);
      const runtime = await startPatchedSession({ ...options, dsp: module.dsp ? 'sharc' : 'zero' }, name);
      try {
        await expectRuntimeBoot(runtime);
        await timePhase(
          'scenario.body',
          () => expectModuleLoad(runtime, module, artifact),
          timingContext(runtime.session),
        );
      } finally {
        await runtime.close();
      }
    });
  }
}

/** Real USB upload, persisted bytes, selected reboot, registration and fault acceptance.
 * DSP allocation/registration does not establish rendered audio correctness. */
export async function expectModuleLoad(
  runtime: PatchedSession,
  module: RuntimeModule,
  artifact: string,
): Promise<void> {
  const { session, dk } = runtime;
  const bytes = fs.readFileSync(artifact);
  const storePath = `modules/${module.id.split('/')[1]}.dkm`;
  // The debug module is loaded next to every module (the cell reads its LOG); for the debug module itself, once.
  const names = [...new Set([DEBUG_MODULE, module.id])];
  const paths = [...new Set(['modules/debug.dkm', storePath])].sort();
  const hash = await hexSha256(bytes);
  const artifacts = path.join(liveArtifacts(session, 'module-load'), module.id.split('/')[1]!);
  fs.mkdirSync(artifacts, { recursive: true });

  await expectRuntimeReady(session);
  expect(await dk.list(), 'fresh daemon has an empty store').toEqual([]);
  expect(await dk.modules(), 'fresh runtime has no uploaded modules').toEqual([]);
  const epoch = (await session.status()).epoch;
  if (typeof epoch !== 'number') throw new Error('daemon status must report a numeric epoch');
  // the debug module is selected first: the log (LOG) exists only while it is loaded, and this cell reads it
  await installWithDebug(runtime, module.id);
  expect(
    (await dk.modules()).map((entry) => entry.name),
    'only the uploaded module and the debug module are registered',
  ).toEqual(names);
  expect((await session.status()).epoch, 'protocol restart boots another epoch').toBeGreaterThan(epoch);
  const entries = await dk.list();
  record(artifacts, 'store', entries);
  expect(entries.find((entry) => entry.path === storePath)).toEqual({
    path: storePath,
    size: bytes.length,
    sha256: hash,
  });
  expect(entries.map((entry) => entry.path).sort(), 'the store holds the two module files (any order)').toEqual(paths);
  expect(await dk.verifyFile(storePath, bytes), 'module bytes survive reboot').toBe(true);

  try {
    if (module.dsp) {
      // MOD_LOAD publishes the CF module before its DSP part completes over UI-loop passes.
      // This cell selects the debug module (index 0) and then this module's path, so boot's selection index is one (not MODULES' registry index).
      let completionLog: DkLogEvent[] = [];
      try {
        await expect
          .poll(
            async () => {
              completionLog = await dk.readLog();
              return completionLog.some((event) => event.kind === LOG_KIND.MOD_DSP && event.args[0] === 1);
            },
            { timeout: 30_000, interval: 500, message: 'selected module DSP completion' },
          )
          .toBe(true);
      } finally {
        record(artifacts, 'dsp-completion-log', completionLog);
      }
      expect(
        completionLog
          .filter((event) => event.kind === LOG_KIND.MOD_DSP && event.args[0] === 1)
          .map((event) => event.args[1]),
        'DSP loader completes the selected module successfully',
      ).toEqual([0]);
    }
    const loaded = (await dk.modules()).find((entry) => entry.name === module.id)!;
    expect(loaded.version, 'loader reports the prepared module version').toBe(bytes.readUInt32BE(0x34));
    const contracts = Array.from({ length: 4 }, (_, index) => [bytes[0x38 + 2 * index], bytes[0x39 + 2 * index]]);
    expect(loaded.contracts, 'loader reports the prepared SDK contracts').toEqual(contracts);
    expect(loaded.contracts[module.contract]![0], 'intended module contract is active').toBeGreaterThan(0);
    if (module.dsp) {
      expect(loaded.dsp_len, 'module DSP image was allocated').toBeGreaterThan(0);
      if (module.type === 'machine') {
        expect(loaded.rows, 'machine has a published DSP voice row').toBeGreaterThan(0);
        expect(loaded.types, 'machine has a published firmware type').toHaveLength(1);
      }
    } else {
      expect(loaded.dsp_len).toBe(0);
    }
    // DSP registrations need time to run on the coupled DSP before checking guard counters.
    // CF-only registration is already complete; UI and fault acceptance below still apply.
    if (module.dsp) {
      expect((await session.status()).dsp, 'DSP conformance remains coupled').toBe('sharc');
      await waitEmulated(session, 2000);
    } else {
      const status = await session.status();
      expect(status.dsp, 'CF-only conformance retains the zero SSI peer').toBe('zero');
      expect(status.audio, 'CF-only conformance has no SHARC audio session').toBeNull();
    }
    const modules = await dk.modules();
    record(artifacts, 'modules', modules);
    expect(modules.map(({ name, faults, guard_off }) => ({ name, faults, guard_off }))).toEqual(
      names.map((name) => ({ name, faults: 0, guard_off: false })),
    );
    await expect
      .poll(async () => (await dk.readLog()).some((event) => event.kind === LOG_KIND.UI_FIRST), {
        timeout: 30_000,
        interval: 500,
      })
      .toBe(true);
    const log = await dk.readLog();
    record(artifacts, 'boot-log', log);
    expect(
      log.some((event) => event.kind === LOG_KIND.UI_FIRST),
      'selected module boot reaches the firmware UI',
    ).toBe(true);
    const failures = new Set<number>([
      LOG_KIND.SAFE,
      LOG_KIND.DSP_STOPPED,
      LOG_KIND.CRASH_FOUND,
      LOG_KIND.DSP_ABSENT,
      LOG_KIND.MACHINE_FAULT,
      LOG_KIND.MACHINE_DISABLED,
      LOG_KIND.TABLE_BAD,
    ]);
    expect(
      log.filter((event) => failures.has(event.kind)),
      'no runtime crash, missing DSP or disabled registration',
    ).toEqual([]);
    await session.expectNoFault(`loaded ${module.id}`);
  } finally {
    record(artifacts, 'final-modules', await dk.modules());
    record(artifacts, 'final-status', await session.status());
  }
}
