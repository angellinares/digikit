import fs from 'node:fs';
import path from 'node:path';
import { repoRoot } from '@digi/test-support';

/** The static `--modules` lists of image-roots.json: the one source of patchkit:device-images (tools/nx/device-images.mjs) and the tests. */
export interface RootSet {
  id: string;
  roots: string[];
  /** Devices that have the image; absent: every device. */
  devices?: string[];
}

export function rootSets(): RootSet[] {
  const file = path.join(repoRoot(), 'tests/regression/runtime/image-roots.json');
  return (JSON.parse(fs.readFileSync(file, 'utf8')) as { sets: RootSet[] }).sets;
}

/** The roots of the set `id` (throws for an id that is not in the file). */
export function rootsOf(id: string): string[] {
  const found = rootSets().find((set) => set.id === id);
  if (!found) throw new Error(`no root set ${id} in tests/regression/runtime/image-roots.json`);
  return [...found.roots];
}
