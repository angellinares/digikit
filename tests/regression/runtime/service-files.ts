import { createHash } from 'node:crypto';
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
  // the sidecar of `patchkit bind` names what the file was bound from: a copy left in dist/ from other bytes is stale (mtimes say nothing under an Nx cache restore)
  const stale = staleBinding(out, moduleFile, resolveInputs(device, FIRMWARE[device]).syx);
  if (stale) throw new Error(`${out} ${stale}: ${rerun}`);
  return out;
}

const sha256 = (file: string) => createHash('sha256').update(fs.readFileSync(file)).digest('hex');

/** Why the bound file `out` does not match the current packed `.dks` and firmware image (its `.src.json` sidecar), or undefined when it does. */
export function staleBinding(out: string, moduleFile: string, firmware: string): string | undefined {
  const sidecar = out.replace(/\.dks$/, '.src.json');
  if (!fs.existsSync(sidecar)) return 'has no .src.json sidecar (bound by an older patchkit)';
  const src = JSON.parse(fs.readFileSync(sidecar, 'utf8')) as { unbound_sha256?: string; firmware_sha256?: string };
  if (src.unbound_sha256 !== sha256(moduleFile)) return `was bound from another ${path.basename(moduleFile)}`;
  if (fs.existsSync(firmware) && src.firmware_sha256 !== sha256(firmware)) return 'was bound to another firmware image';
  return undefined;
}

/** The bound service file `dist/svc-bound/<device>/<name>.dks` (the Nx target `bind` writes it). */
export const boundPath = (device: Device, name: string) =>
  path.join(repoRoot(), 'dist/svc-bound', device, `${name}.dks`);

/** The address map of the bound file (the packed map moved by what bind appended; `patchkit bind --addrmap`). */
export const boundMapPath = (device: Device, name: string) =>
  boundPath(device, name).replace(/\.dks$/, '.addrmap.json');
