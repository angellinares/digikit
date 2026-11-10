import path from 'node:path';
import { repoRoot, type Device } from '@digi/test-support';
import { test } from 'vitest';
import { expectRuntimeBoot, startPatchedSession } from '@digi/regression/support.ts';
import { timePhase, timingContext } from '@digi/regression/timing.ts';
import { FIRMWARE, liveOptions, selectDevices } from '@digi/regression/runtime/devices.ts';
import { moduleArtifact } from '@digi/regression/runtime/install.ts';

import { loadRuntimeModules, scanRuntimeModules as scanManifests, supportsDevice, type RuntimeModule } from './manifests.ts';

import { expectModuleLoad } from './assert-load.ts';

export { expectModuleLoad } from './assert-load.ts';
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
  const modules = selectDevices([device]).length
    ? runtimeModules.filter((module) => supportsDevice(module, device))
    : [];
  if (modules.length === 0) {
    // a module without a port for this device (e.g. a DT2-only machine): nothing to load, but the file must hold a test
    test.skip(`${device}: no module of this run has a ${device} port`, () => {});
    return;
  }
  for (const module of modules) {
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
