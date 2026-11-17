// The library of the Files page preview: the module and service files of the demo store, with one hash changed ("other version") and one file
// left out ("not in library").
import type { StoreOps } from '@digi/workbench-core/target-ops/ops.ts';

export async function demoLibrary(store: StoreOps): Promise<{ path: string; hash: string }[]> {
  const files = (await store.snapshot()).entries
    .filter((e) => /^(modules|services)\//.test(e.path))
    .map((e) => ({ path: e.path, hash: e.hash }));
  // the first file differs from the card, the last one is not built
  return files
    .slice(0, -1)
    .map((f, i) => (i === 0 ? { ...f, hash: f.hash.replace(/^./, (c) => (c === 'f' ? '0' : 'f')) } : f));
}
