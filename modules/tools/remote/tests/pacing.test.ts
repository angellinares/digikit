import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { test } from 'vitest';

test('remote pacing unit uses the C handler with a controlled block clock', () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'remote-pacing-'));
  try {
    const binary = path.join(directory, 'remote-pacing');
    execFileSync(
      'cc',
      [
        '-std=c11',
        '-Wall',
        '-Wextra',
        '-Werror',
        '-I',
        path.join(import.meta.dirname, '../../../../sdk/include'),
        path.join(import.meta.dirname, 'remote-pacing.c'),
        path.join(import.meta.dirname, '../../../../sdk/src/core/text.c'),
        '-o',
        binary,
      ],
      { timeout: 25_000 },
    );
    execFileSync(binary, { timeout: 10_000 });
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
});
