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
