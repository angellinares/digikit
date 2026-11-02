import { test } from 'vitest';
import { expectRuntimeBoot, startPatchedSession } from '@digi/regression/support.ts';
import { timePhase, timingContext } from '@digi/regression/timing.ts';
import { DEVICES, FIRMWARE, liveOptions, moduleArtifact } from '@digi/regression/runtime/live.ts';
import { expectModuleLoad, runtimeModules, type RuntimeModule } from './load.ts';

/** `DIGI_CONFORMANCE_MODULES`: comma-separated module ids; unset or empty runs every run-time module. */
function selectedModules(): readonly RuntimeModule[] {
  const ids = (process.env.DIGI_CONFORMANCE_MODULES ?? '')
    .split(',')
    .map((id) => id.trim())
    .filter(Boolean);
  if (ids.length === 0) return runtimeModules;
  const unknown = ids.filter((id) => !runtimeModules.some((module) => module.id === id));
  if (unknown.length > 0) throw new Error(`DIGI_CONFORMANCE_MODULES: unknown run-time module ${unknown.join(', ')}`);
  return runtimeModules.filter((module) => ids.includes(module.id));
}

// Every cell owns a fresh coupled daemon, card and module selection. No patched restart/reset pooling.
for (const module of selectedModules()) {
  for (const device of DEVICES) {
    const name = `${device} ${FIRMWARE[device]} loads and registers ${module.id} through USB without runtime faults`;
    test(name, async () => {
      const options = liveOptions(device, { mirror: true });
      const artifact = moduleArtifact(device, module.id);
      const runtime = await startPatchedSession(options, name);
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
