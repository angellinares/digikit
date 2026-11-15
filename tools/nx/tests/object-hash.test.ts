import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { expect, it } from 'vitest';
import { objectHash } from '../object-hash.mjs';

const hashOf = (object: unknown) => {
  const dir = mkdtempSync(join(tmpdir(), 'object-hash-'));
  try {
    writeFileSync(join(dir, 'a.dko.json'), JSON.stringify(object));
    return objectHash(join(dir, 'a.dko.json'));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
};

it('ignores the source hash and the flag list, and follows the sections', () => {
  const base = { sections: { text: 'aa' }, source_sha256: 'x', toolchain: { cflags: ['-O1'], name: 't' } };
  const comment = { ...base, source_sha256: 'y', toolchain: { cflags: ['-O2'], name: 't' } };
  expect(hashOf(comment)).toBe(hashOf(base));
  expect(hashOf({ ...base, sections: { text: 'bb' } })).not.toBe(hashOf(base));
});
