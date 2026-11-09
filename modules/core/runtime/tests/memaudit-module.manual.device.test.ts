import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import type { Device } from '@digi/test-support';
import { test } from 'vitest';
import { patchkit } from '@digi/regression/runtime/patchkit.ts';
import {
  AUDIT_SWEEP,
  auditSet,
  hasMachine,
  moduleManifests,
  resolveModule,
} from '@digi/regression/runtime/memaudit-kit.ts';
import { defineMemaudit } from '@digi/regression/runtime/memaudit-suite.ts';

/**
 * `mise run module:audit NAME [--device dt2|dn2]`: the ColdFire memory audit of one run-time module (tools/devtools/module_audit.sh sets DIGI_AUDIT_MODULE).
 * The module is packed with the run-time modules it requires and loaded on a ColdFire-only emulator. The UI is driven for a bounded time: the
 * DIGIKIT menu, the machine list (each of the first entries is chosen on track 1 and its page turned, which reaches a machine the module adds),
 * a project save and a load. The write guard reports every ColdFire store into the free regions outside the module blocks by code other than the
 * patch objects: pc, address and the code it belongs to. A module the runtime cannot load (a patched-in module) is skipped.
 * Without DIGI_AUDIT_MODULE the file has nothing to run.
 */
const name = process.env.DIGI_AUDIT_MODULE;

if (!name) test.skip('set DIGI_AUDIT_MODULE=<module id> (mise run module:audit NAME)', () => {});
else {
  const manifests = moduleManifests();
  const id = resolveModule(name, manifests);
  const targets = new Set(Object.keys(manifests.get(id)?.targets ?? {}).map((k) => k.split('-')[0]));
  const only = process.env.DIGI_AUDIT_DEVICE as Device | undefined;
  const modules = auditSet(id, manifests);
  // The packed files outlive the first daemon: the load pass boots a card built from the same files.
  const packed = fs.mkdtempSync(path.join(os.tmpdir(), 'digi-module-audit-'));
  process.on('exit', () => fs.rmSync(packed, { recursive: true, force: true }));

  defineMemaudit(
    (device) =>
      targets.has(device)
        ? {
            title: `module ${id}`,
            modules,
            sweep: modules.some((m) => hasMachine(m, manifests)) ? AUDIT_SWEEP : 0,
          }
        : undefined,
    {
      devices: only ? [only] : undefined,
      artifact: async (rig, module) => {
        const last = module.split('/').at(-1)!;
        const file = path.join(packed, `${rig.device}-${last}.dkm`);
        try {
          await patchkit(['pack', module, '--device', rig.device, '-o', file], { timeout: 300_000 });
        } catch (error) {
          const text = String((error as { stderr?: string }).stderr || error);
          return { skip: `${module} does not pack for ${rig.device} (not a run-time module): ${text.trim().split('\n').at(-1)}` };
        }
        return { name: `modules/${last}.dkm`, file };
      },
    },
  );
}
