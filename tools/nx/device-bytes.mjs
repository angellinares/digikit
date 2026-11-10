// Writes dist/bytes/core-objects.sha256: one line per committed core object (modules/core/**/build/*.dko.json) with the
// SHA-256 of its content without `source_sha256`, the only field that follows the source text (a comment edit changes it and
// no other field; checked for Rust, C and assembly sources). The patched image is a function of these objects, the core
// manifests, the device files and ports, the patchkit binary and the firmware; device tasks hash all of them, so every
// `--modules` combination of core roots is covered and a rebuild with the same object bytes changes no key.
import { createHash } from 'node:crypto';
import { globSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const lines = globSync('modules/core/**/build/*.dko.json', { cwd: root })
  .sort()
  .map((file) => {
    const object = JSON.parse(readFileSync(join(root, file), 'utf8'));
    delete object.source_sha256;
    return `${file} ${createHash('sha256').update(JSON.stringify(object)).digest('hex')}`;
  });
mkdirSync(join(root, 'dist/bytes'), { recursive: true });
writeFileSync(join(root, 'dist/bytes/core-objects.sha256'), `${lines.join('\n')}\n`);
