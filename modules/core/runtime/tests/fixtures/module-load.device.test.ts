import { expectModuleLoad } from '../../../../../tests/conformance/assert-load.ts';
import type { RuntimeModule } from '../../../../../tests/conformance/manifests.ts';
import { moduleArtifact } from '@digi/regression/runtime/install.ts';
import { runtimeTests } from '@digi/regression/runtime/ready.ts';

// Representative SDK artifacts keep the runtime loader boundary independent of shipped modules.
// Service registration is covered by core/runtime-service-tests with its zero-DSP fixture.
// Machines are a service file (machines.dks, R12-6): the harness puts it on the card with the machine fixtures, so these loads go through it. `test/dyn-dn2` is the machine
// with its own parameter pages and voice of the Digitone II.
const fixtures: readonly (RuntimeModule & { runOn: ('dt2' | 'dn2')[] })[] = [
  { id: 'test/sine', type: 'machine', contract: 1, dsp: true, runOn: ['dt2', 'dn2'] },
  { id: 'test/dyn-dn2', type: 'machine', contract: 1, dsp: true, runOn: ['dn2'] },
  { id: 'test/dyn-fx-gain', type: 'fx', contract: 2, dsp: true, runOn: ['dt2', 'dn2'] },
];

for (const fixture of fixtures) {
  runtimeTests(
    `loads and registers ${fixture.id} through USB without runtime faults`,
    (runtime) => expectModuleLoad(runtime, fixture, moduleArtifact(runtime.session.options.device, fixture.id)),
    { dsp: 'sharc', devices: fixture.runOn },
  );
}
