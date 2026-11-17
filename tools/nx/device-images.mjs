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
import { CELLS, syxPath } from './device-inputs.mjs';

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
const { sets: rootSets } = JSON.parse(readFileSync(join(root, 'tests/regression/runtime/image-roots.json'), 'utf8'));
const cells = CELLS.filter((cell) => existsSync(syxPath(cell)));
const jobs = cells.flatMap((cell) => [
  ...(setsOf.get(cell.dev) ?? []).map((set) => ({ cell, label: set, select: ['--set', set] })),
  ...rootSets
    .filter((set) => !set.devices || set.devices.includes(cell.dev))
    .map((set) => ({
      cell,
      label: `roots:${set.id}`,
      select: ['--modules', set.roots.join(',')],
      keep: join(root, 'dist/bytes/images', cell.dev, set.id),
    })),
]);
rmSync(join(root, 'dist/bytes/images'), { recursive: true, force: true });
for (const dev of new Set(cells.map((cell) => cell.dev)))
  mkdirSync(join(root, 'dist/bytes/images', dev), { recursive: true });
/**
 * `dist/bytes/images/<device>/set-<set>.info`: one line per entry of the image's info table (`dkrt_info_table`): name, name hash, kind (0 data table, 1 slot
 * word), address and the words of a data entry, all but the name and kind in hex. The runtime's assembly test builds its `Platform::info` from it.
 */
const writeInfo = (dev, set, info) =>
  writeFileSync(
    join(root, 'dist/bytes/images', dev, `set-${set}.info`),
    info
      .map((e) =>
        [e.name, e.hash.toString(16), e.kind, e.addr.toString(16), ...e.words.map((w) => w.toString(16))].join(' '),
      )
      .join('\n') + '\n',
  );
// The jobs only read; four at a time keep the machine usable. Lines keep the order of the jobs.
const lines = Array.from({ length: jobs.length }, () => '');
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
          syxPath(cell),
          ...select,
          '-o',
          out,
          '--plan',
          plan,
          '--addrmap',
          addrmap,
        ];
        await run(pk, args, { cwd: root });
        // The info table of a patch set's image, as text for the host assembly tests (modules/core/runtime, `test-assembly`)
        if (!keep) writeInfo(cell.dev, label, JSON.parse(readFileSync(plan, 'utf8')).info ?? []);
        const sha = (file) => createHash('sha256').update(readFileSync(file)).digest('hex');
        lines[index] = `${cell.dev}-${cell.ver} ${label} image ${sha(out)} plan ${sha(plan)} addrmap ${sha(addrmap)}`;
      }
    }),
  );
} finally {
  rmSync(scratch, { recursive: true, force: true });
}
mkdirSync(join(root, 'dist/bytes'), { recursive: true });
writeFileSync(join(root, 'dist/bytes/images.sha256'), `${lines.join('\n')}\n`);
