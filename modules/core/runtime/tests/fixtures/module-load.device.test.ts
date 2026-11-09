import { expectModuleLoad } from '../../../../../tests/conformance/assert-load.ts';
import type { RuntimeModule } from '../../../../../tests/conformance/manifests.ts';
import { moduleArtifact } from '@digi/regression/runtime/install.ts';
import { runtimeTests } from '@digi/regression/runtime/ready.ts';

// Representative SDK artifacts keep the runtime loader boundary independent of shipped modules.
// Service registration is covered by core/runtime-service-tests with its zero-DSP fixture.
const fixtures: readonly RuntimeModule[] = [
  { id: 'test/sine', type: 'machine', contract: 1, dsp: true },
  { id: 'test/dyn-fx-gain', type: 'fx', contract: 2, dsp: true },
];

for (const fixture of fixtures) {
  runtimeTests(
    `loads and registers ${fixture.id} through USB without runtime faults`,
    (runtime) => expectModuleLoad(runtime, fixture, moduleArtifact(runtime.session.options.device, fixture.id)),
    { dsp: 'sharc', devices: ['dt2', 'dn2'] },
  );
}
