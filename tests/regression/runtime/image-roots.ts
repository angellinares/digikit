import fs from 'node:fs';
import path from 'node:path';
import { repoRoot } from '@digi/test-support';

/** The static `--modules` lists: the one source of patchkit:device-images (tools/nx/device-images.mjs) and the tests. */
export interface RootSet {
  id: string;
  roots: string[];
  /** Devices that have the image; absent: every device. */
  devices?: string[];
}

/** The shared file, and the files of single projects (`modules/**` `/tests/image-roots.json`) for the lists only that project's tests use. */
const SHARED = 'tests/regression/runtime/image-roots.json';

/** The paths (relative to the repository) of the per-project files. */
export function projectRootFiles(root = repoRoot()): string[] {
  return fs.globSync('modules/**/tests/image-roots.json', { cwd: root }).sort();
}

export function rootSets(): RootSet[] {
  const root = repoRoot();
  const sets: RootSet[] = [];
  const owner = new Map<string, string>();
  for (const file of [SHARED, ...projectRootFiles(root)]) {
    const read = JSON.parse(fs.readFileSync(path.join(root, file), 'utf8')) as { sets: RootSet[] };
    for (const set of read.sets) {
      if (owner.has(set.id)) throw new Error(`root set ${set.id} is in both ${owner.get(set.id)} and ${file}`);
      owner.set(set.id, file);
      sets.push(set);
    }
  }
  return sets;
}

/** The roots of the set `id` (throws for an id that is in no image-roots.json). */
export function rootsOf(id: string): string[] {
  const found = rootSets().find((set) => set.id === id);
  if (!found) throw new Error(`no root set ${id} in ${SHARED} or a project's tests/image-roots.json`);
  return [...found.roots];
}
