import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { test } from 'vitest';

test('spp_window_feed puts every frame in exactly one column, in any chunking and from any first frame', () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'pool-window-'));
  try {
    const binary = path.join(directory, 'window-host');
    const module = path.join(import.meta.dirname, '..');
    execFileSync(
      'cc',
      [
        '-std=c11',
        '-Wall',
        '-Wextra',
        '-Werror',
        '-I',
        path.join(module, 'src'),
        path.join(module, 'tests/window-host.c'),
        '-o',
        binary,
      ],
      { timeout: 25_000 },
    );
    execFileSync(binary, { timeout: 10_000 });
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
}, 40_000);
