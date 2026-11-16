// Writes dist/bytes/owners/: the hash of each owner of the runtime object (docs/TESTING.md "Owner hashes"; no device test key reads them yet).
//   <tag>.sha256        one per owner of `modules/core/runtime/owners.toml` and `shared.sha256` (`patchkit owners`)
//   other-objects.sha256  the other objects of the runtime module (tramp, device-dt2, device-dn2), part of every key
//   layout.sha256       the whole runtime object, for the tests tagged C:layout (they depend on addresses)
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { globSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { objectHash } from './object-hash.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const out = join(root, 'dist/bytes/owners');
const runtime = 'modules/core/runtime/build/runtime.dko.json';
rmSync(out, { recursive: true, force: true });
mkdirSync(out, { recursive: true });
execFileSync(
  join(root, 'dist/patchkit/patchkit'),
  ['owners', runtime, '--owners', 'modules/core/runtime/owners.toml', '--out', out],
  { cwd: root, stdio: ['ignore', 'inherit', 'inherit'] },
);
const others = globSync('modules/core/runtime/build/*.dko.json', { cwd: root })
  .filter((file) => file !== runtime)
  .sort()
  .map((file) => `${file} ${objectHash(join(root, file))}`);
writeFileSync(join(out, 'other-objects.sha256'), `${createHash('sha256').update(others.join('\n')).digest('hex')}\n`);
writeFileSync(join(out, 'layout.sha256'), `${objectHash(join(root, runtime))}\n`);
