// Writes dist/bytes/images.sha256: `<device>-<version> <set> image <sha256> plan <sha256> addrmap <sha256>` for the image, the plan and the address map that `patchkit apply` makes from each
// acceptance stock firmware (CELLS of device-inputs.mjs) with each patch set of its device profile (devices/*.toml). This is the
// code path the daemon takes at boot (patchkit Library, Registry and apply::apply). The device tasks of a merge key hash this file
// and not the patchkit binary: a patchkit edit that leaves every packed image byte alone changes no key. A missing firmware
// file writes no line for it. Lines `roots:<id>` are the static `--modules` sets of tests/regression/runtime/image-roots.json (the tasks hash the firmware itself in device-inputs.mjs).
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { createHash } from 'node:crypto';
import { existsSync, globSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parse } from 'smol-toml';
import { CELLS } from './device-inputs.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const pk = join(root, 'dist/patchkit/patchkit');
const setsOf = new Map(
  globSync('devices/*.toml', { cwd: root })
    .map((f) => parse(readFileSync(join(root, f), 'utf8')))
    .filter((t) => t.device?.short)
    .map((t) => [t.device.short, Object.keys(t.patch_sets ?? {}).sort()]),
);
const scratch = mkdtempSync(join(tmpdir(), 'digi-images-'));
const run = promisify(execFile);
// Static module root lists (`apply --modules`) that tests use: tests/regression/runtime/image-roots.json, also read by runtime-plan.ts.
// Their image, plan and address map are kept under dist/bytes/images/<device>/<id>.* for the tests to copy.
// Single projects keep the lists only their tests use in modules/**/tests/image-roots.json (image-roots.ts reads the same files): their lines go to
// dist/bytes/project-images.sha256, so a project's list changes no key of another test.
const readSets = (file) => JSON.parse(readFileSync(join(root, file), 'utf8')).sets;
const sharedSets = readSets('tests/regression/runtime/image-roots.json');
const projectSets = globSync('modules/**/tests/image-roots.json', { cwd: root }).sort().flatMap(readSets);
const ids = [...sharedSets, ...projectSets].map((set) => set.id);
const repeated = ids.find((id, i) => ids.indexOf(id) !== i);
if (repeated) throw new Error(`root set ${repeated} is listed twice`);
const rootSets = [...sharedSets, ...projectSets.map((set) => ({ ...set, project: true }))];
const cells = CELLS.filter((cell) => existsSync(join(root, cell.syx)));
const jobs = cells.flatMap((cell) => [
  ...(setsOf.get(cell.dev) ?? []).map((set) => ({ cell, label: set, select: ['--set', set] })),
  ...rootSets
    .filter((set) => !set.devices || set.devices.includes(cell.dev))
    .map((set) => ({
      cell,
      label: `roots:${set.id}`,
      project: set.project === true,
      select: ['--modules', set.roots.join(',')],
      keep: join(root, 'dist/bytes/images', cell.dev, set.id),
    })),
]);
rmSync(join(root, 'dist/bytes/images'), { recursive: true, force: true });
for (const dev of new Set(cells.map((cell) => cell.dev)))
  mkdirSync(join(root, 'dist/bytes/images', dev), { recursive: true });
// The jobs only read; four at a time keep the machine usable. Lines keep the order of the jobs.
const lines = Array.from({ length: jobs.length }, () => '');
const isProject = (index) => jobs[index].project === true;
let next = 0;
try {
  await Promise.all(
    Array.from({ length: 4 }, async () => {
      while (next < jobs.length) {
        const index = next++;
        const { cell, label, select, keep } = jobs[index];
        const base = keep ?? join(scratch, `${cell.dev}-${label.replace(':', '-')}`);
        const out = `${base}.syx`;
        const plan = `${base}.plan.json`;
        const addrmap = `${base}.addrmap.json`;
        // the plan and the address map are what tests/regression/runtime/runtime-plan.ts reads (offline image preparation)
        const args = [
          '--root',
          root,
          'apply',
          '--firmware',
          cell.syx,
          ...select,
          '-o',
          out,
          '--plan',
          plan,
          '--addrmap',
          addrmap,
        ];
        await run(pk, args, { cwd: root });
        const sha = (file) => createHash('sha256').update(readFileSync(file)).digest('hex');
        lines[index] = `${cell.dev}-${cell.ver} ${label} image ${sha(out)} plan ${sha(plan)} addrmap ${sha(addrmap)}`;
      }
    }),
  );
} finally {
  rmSync(scratch, { recursive: true, force: true });
}
mkdirSync(join(root, 'dist/bytes'), { recursive: true });
const written = (project) => `${lines.filter((_, i) => isProject(i) === project).join('\n')}\n`;
writeFileSync(join(root, 'dist/bytes/images.sha256'), written(false));
writeFileSync(join(root, 'dist/bytes/project-images.sha256'), written(true));
