import fs from 'node:fs';
import path from 'node:path';
import { repoRoot, resolveInputs, type Device } from '@digi/test-support';
import { FIRMWARE } from './devices.ts';
import { neededServiceHashes, nameHash } from './dkm-parse.ts';

/**
 * Services that are files of the card (`services/<name>.dks`, docs/plans/runtime-and-services.md section 13.2): the module that provides each. A module that needs one
 * of them has a hard NEEDS item for its hash (the packer adds it for a record kind of that service), and the runtime finds the file by its name at boot. The harness
 * puts the file on the card of every test that installs such a module, so a test never lists the service by hand.
 */
export const SERVICE_MODULES: Record<string, string> = {
  machines: 'core/services/machines',
  fx: 'core/services/fx',
};

/** The names of the services (of `SERVICE_MODULES`) that the packed `.dkm` files need, in the order of `SERVICE_MODULES`. */
export function neededServices(files: string[]): string[] {
  const needed = new Set(files.flatMap((f) => neededServiceHashes(fs.readFileSync(f))));
  return Object.keys(SERVICE_MODULES).filter((name) => needed.has(nameHash(name)));
}

/** The store path of a service file. */
export const servicePath = (name: string) => `services/${name}.dks`;

/**
 * The packed service bound to the STOCK firmware of the device (`patchkit bind`): a service with hooks has trampolines and the stock bytes of its sites, so the packed
 * file is not enough. The Nx target `<module id>:bind` writes it under dist/ (keyed on the packed bytes and the firmware image; the file holds firmware bytes and is never
 * committed); a test only copies it, so no test that puts a service on a card is a live-patchkit task. `moduleFile` is the packed `.dks` (the `bind` input).
 */
export async function boundService(device: Device, name: string, moduleFile: string): Promise<string> {
  const out = boundPath(device, name);
  const rerun = `run nx run ${SERVICE_MODULES[name]}:bind`;
  if (!fs.existsSync(out)) throw new Error(`missing ${out}: ${rerun} (it binds ${moduleFile} to the stock firmware)`);
  // an older bound file was made from other bytes: the Nx target is keyed on both, a copy left in dist/ is not
  const age = fs.statSync(out).mtimeMs;
  const inputs = [moduleFile, resolveInputs(device, FIRMWARE[device]).syx];
  for (const input of inputs)
    if (fs.existsSync(input) && fs.statSync(input).mtimeMs > age)
      throw new Error(`${out} is older than ${input}: ${rerun}`);
  return out;
}

/** The bound service file `dist/svc-bound/<device>/<name>.dks` (the Nx target `bind` writes it). */
export const boundPath = (device: Device, name: string) =>
  path.join(repoRoot(), 'dist/svc-bound', device, `${name}.dks`);

/** The address map of the bound file (the packed map moved by what bind appended; `patchkit bind --addrmap`). */
export const boundMapPath = (device: Device, name: string) =>
  boundPath(device, name).replace(/\.dks$/, '.addrmap.json');
