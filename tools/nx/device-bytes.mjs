// Writes dist/bytes/core-objects.sha256: one line per committed core object (modules/core/**/build/*.dko.json) with the
// SHA-256 of its content without `source_sha256` and `toolchain.cflags`, the fields that follow the source text and the flag list
// (a comment edit changes the first and no other field; checked for Rust, C and assembly sources; a flag that changes code changes the sections). The patched image is a function of these objects, the core
// manifests, the device files and ports, the patchkit binary and the firmware; device tasks hash all of them, so every
// `--modules` combination of core roots is covered and a rebuild with the same object bytes changes no key.
import { globSync, mkdirSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { objectHash } from './object-hash.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const lines = globSync('modules/core/**/build/*.dko.json', { cwd: root })
  .sort()
  .map((file) => `${file} ${objectHash(join(root, file))}`);
mkdirSync(join(root, 'dist/bytes'), { recursive: true });
writeFileSync(join(root, 'dist/bytes/core-objects.sha256'), `${lines.join('\n')}\n`);
