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
