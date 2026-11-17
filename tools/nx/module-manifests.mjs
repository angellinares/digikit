// Shared module manifest and acceptance metadata helpers.
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { parse } from 'smol-toml';

export const read = (root, toml) => parse(readFileSync(join(root, toml), 'utf8'));
// The devices a module packs for: no ports means both devices.
export const packDevices = (m) => {
  if (!m.ports?.length) return ['dt2', 'dn2'];
  return ['dt2', 'dn2'].filter((d) => m.ports.some((p) => p.startsWith(`${d}-`)));
};
// Shipped module that is loaded at run time: it gets test-conformance.
export const isUploadable = (root, m) => root.startsWith('modules/') && !!(m.dynamic || m.host || m.type === 'service');
const leaf = (id) => id.split('/').at(-1);
export const dkm = (dev, id) => `dist/modules/${dev}/${leaf(id)}.dkm`;
// The module's address map (docs/ADDRMAP.md), written by `patchkit pack --addrmap` next to the .dkm.
export const dkmMap = (dev, id) => dkm(dev, id).replace(/\.dkm$/, '.addrmap.json');
// Pack output of a module, given its parsed [module] table: a service packs to .dks, every other module to .dkm.
export const packOut = (dev, manifest, id = manifest.id) =>
  manifest.role === 'service' ? dkm(dev, id).replace(/\.dkm$/, '.dks') : dkm(dev, id);
export const packMap = (dev, manifest) => packOut(dev, manifest).replace(/\.dk[ms]$/, '.addrmap.json');
export const records = (root) => {
  const file = join(root, 'project.json');
  return existsSync(file) ? (JSON.parse(readFileSync(file, 'utf8')).metadata?.runtimeAcceptance ?? []) : [];
};
// Device test files (relative to the project root) whose merge target keeps full inputs.
export const mergeTests = (root) => {
  const file = join(root, 'project.json');
  return existsSync(file) ? (JSON.parse(readFileSync(file, 'utf8')).metadata?.mergeTests ?? []) : [];
};

// Explicit patchkit libraries include only selected modules and their declared requirements.
export const manifestClosure = (byId, ids) => {
  const files = new Set();
  const visit = (id) => {
    const module = byId.get(id);
    if (!module) throw new Error(`Unknown scoped module ${id}`);
    if (files.has(module.file)) return;
    files.add(module.file);
    for (const required of module.manifest.requires ?? []) visit(required);
  };
  for (const id of ids) visit(id);
  return [...files].sort();
};
export const manifestArgs = (files) => files.map((file) => `--manifest ${file}`).join(' ');

// Gaps between a module's manifest and its generated gallery (build/gen/gallery.json): a device of [targets], [module] ports or an
// object's `devices` that a machine does not list, or lists with no parameter (unless `[uses.DEVICE] param_rows = 0` declares none), has no knob slots there. `module:check` writes the
// gallery, so a gap is fixed by running it again. Returns one message per gap; a module without machines or gallery has none.
export const galleryGaps = (root, manifest) => {
  const file = join(root, 'build/gen/gallery.json');
  if (!manifest.machine?.length || !existsSync(file)) return [];
  const gallery = JSON.parse(readFileSync(file, 'utf8'));
  const fromPort = (t) => t.split('-')[0];
  const wanted = new Set([
    ...Object.keys(manifest.targets ?? {}).map(fromPort),
    ...(manifest.module?.ports ?? []).map(fromPort),
    ...(manifest.object ?? []).flatMap((o) => o.devices ?? []),
  ]);
  const gaps = [];
  for (const m of gallery.machines ?? [])
    for (const d of [...wanted].sort()) {
      if (!m.devices?.[d]) gaps.push(`${d}: machine ${m.id} is not listed in ${file}`);
      else if (!(gallery.params?.[d] ?? []).length && manifest.uses?.[d]?.param_rows !== 0) gaps.push(`${d}: machine ${m.id} has no parameter in ${file}`);
    }
  return gaps;
};
