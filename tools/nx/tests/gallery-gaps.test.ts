import { readFileSync, readdirSync, existsSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { expect, it } from 'vitest';
// @ts-expect-error plain module
import { galleryGaps, read } from '../module-manifests.mjs';

const repo = fileURLToPath(new URL('../../../', import.meta.url));

it('lists every device of its manifest in the build/gen/gallery.json of each module', () => {
  const found: Record<string, string[]> = {};
  // A module whose gallery lacks a device until `module:check` regenerates it in the next device window lists its gaps in `gallery-pending.json` in its own
  // directory (a JSON array of the messages). Delete the file when the gallery is fixed.
  const pending: Record<string, string[]> = {};
  const walk = (dir: string) => {
    for (const e of readdirSync(join(repo, dir), { withFileTypes: true })) {
      if (!e.isDirectory() || ['build', 'node_modules', 'src', 'tests'].includes(e.name)) continue;
      const sub = join(dir, e.name);
      if (existsSync(join(repo, sub, 'module.toml'))) {
        const manifest = read(repo, join(sub, 'module.toml'));
        const gaps = galleryGaps(join(repo, sub), manifest).map((g: string) => g.replace(repo, ''));
        if (gaps.length) found[manifest.module.id] = gaps;
        const marker = join(repo, sub, 'gallery-pending.json');
        if (existsSync(marker)) pending[manifest.module.id] = JSON.parse(readFileSync(marker, 'utf8')) as string[];
      }
      walk(sub);
    }
  };
  walk('modules');
  expect(found).toEqual(pending);
});
