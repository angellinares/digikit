import { mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { expect, it } from 'vitest';
// @ts-expect-error plain module
import { galleryGaps, read } from '../module-manifests.mjs';

const repo = fileURLToPath(new URL('../../../', import.meta.url));

// Modules whose gallery lacks a device until `module:check` regenerates it in the next device window: module id to its gaps, in the data file next to this test.
// Remove an entry when it is fixed.
const PENDING = new Map<string, string[]>(
  Object.entries(
    JSON.parse(readFileSync(join(import.meta.dirname, 'gallery-gaps-pending.json'), 'utf8')) as Record<
      string,
      string[]
    >,
  ),
);

function scratchGallery(gallery: unknown) {
  const dir = mkdtempSync(join(tmpdir(), 'gallery-gaps-'));
  mkdirSync(join(dir, 'build/gen'), { recursive: true });
  writeFileSync(join(dir, 'build/gen/gallery.json'), JSON.stringify(gallery));
  return dir;
}

it('reports a device the gallery lacks and a device without parameters', () => {
  const manifest = { machine: [{ id: 'm' }], targets: { 'dt2-1.17': 'x', 'dn2-1.12': 'x' } };
  const dir = scratchGallery({
    machines: [{ id: 'm', devices: { dn2: {} } }],
    params: { dn2: [], dt2: [{ name: 'a' }] },
  });
  try {
    const gaps = galleryGaps(dir, manifest);
    expect(gaps).toHaveLength(2);
    expect(gaps[0]).toMatch(/^dn2: machine m has no parameter/);
    expect(gaps[1]).toMatch(/^dt2: machine m is not listed/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

it('finds no gap when every device lists the machine with parameters', () => {
  const manifest = { machine: [{ id: 'm' }], module: { ports: ['dn2-1.12'] } };
  const dir = scratchGallery({ machines: [{ id: 'm', devices: { dn2: {} } }], params: { dn2: [{ name: 'a' }] } });
  try {
    expect(galleryGaps(dir, manifest)).toEqual([]);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

it('lists every device of its manifest in the build/gen/gallery.json of each module', () => {
  const found: Record<string, string[]> = {};
  const walk = (dir: string) => {
    for (const e of readdirSync(join(repo, dir), { withFileTypes: true })) {
      if (!e.isDirectory() || ['build', 'node_modules', 'src', 'tests'].includes(e.name)) continue;
      const sub = join(dir, e.name);
      if (existsSync(join(repo, sub, 'module.toml'))) {
        const manifest = read(repo, join(sub, 'module.toml'));
        const gaps = galleryGaps(join(repo, sub), manifest).map((g: string) => g.replace(repo, ''));
        if (gaps.length) found[manifest.module.id] = gaps;
      }
      walk(sub);
    }
  };
  walk('modules');
  expect(found).toEqual(Object.fromEntries(PENDING));
});
