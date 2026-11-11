import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { test } from 'vitest';

test('the adapter hash (pdw_hash.h) equals the content hash of dksample, in any piece size', () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'pdw-hash-'));
  try {
    const binary = path.join(directory, 'pdw-hash-host');
    const module = path.join(import.meta.dirname, '..');
    execFileSync(
      'cc',
      ['-std=c11', '-Wall', '-Wextra', '-Werror', '-I', path.join(module, 'src'), path.join(module, 'tests/pdw-hash-host.c'), '-o', binary],
      { timeout: 25_000 },
    );
    execFileSync(binary, { timeout: 10_000 });
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
});
