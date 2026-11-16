// Writes dist/bytes/pack-verdicts.json: `needs:<sha256 of the pack>` -> the `patchkit needs --json` verdict, for each pack in
// tests/regression/runtime/needs-packs.json on each acceptance device (CELLS of device-inputs.mjs). Device tests that read a
// committed pack's needs read this file (tests/regression/runtime/pack-needs.ts), so a patchkit edit that changes no verdict
// changes no key. Keys are sorted: the output is deterministic.
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { CELLS } from './device-inputs.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const { modules } = JSON.parse(readFileSync(join(root, 'tests/regression/runtime/needs-packs.json'), 'utf8'));
const verdicts = {};
for (const { dev } of CELLS) {
  for (const id of modules) {
    const base = join(root, 'dist/modules', dev, id.split('/').at(-1));
    const file = !existsSync(`${base}.dkm`) && existsSync(`${base}.dks`) ? `${base}.dks` : `${base}.dkm`;
    if (!existsSync(file)) throw new Error(`missing ${file}: run nx run ${id}:pack`);
    const out = execFileSync(join(root, 'dist/patchkit/patchkit'), ['--root', root, 'needs', '--json', file], {
      cwd: root,
      encoding: 'utf8',
    });
    const verdict = JSON.parse(out)[file];
    if (!verdict || 'error' in verdict) throw new Error(`patchkit needs ${file}: ${verdict?.error ?? 'no verdict'}`);
    verdicts[`needs:${createHash('sha256').update(readFileSync(file)).digest('hex')}`] = verdict;
  }
}
const sorted = Object.fromEntries(Object.entries(verdicts).sort(([a], [b]) => a.localeCompare(b)));
mkdirSync(join(root, 'dist/bytes'), { recursive: true });
writeFileSync(join(root, 'dist/bytes/pack-verdicts.json'), `${JSON.stringify(sorted, null, 2)}\n`);
