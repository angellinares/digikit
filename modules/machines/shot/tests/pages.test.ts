import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { test } from 'vitest';

test('pages draw the region, cache failed peak fetches per token and show the zoom', () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'shot-pages-'));
  try {
    const binary = path.join(directory, 'pages-host');
    const module = path.join(import.meta.dirname, '..');
    execFileSync(
      'cc',
      [
        '-std=c11',
        '-Wall',
        '-Wextra',
        '-Werror',
        '-I',
        path.join(module, '../../../sdk/include'),
        '-I',
        path.join(module, 'src'),
        path.join(module, 'tests/pages-host.c'),
        path.join(module, 'src/pages.c'),
        path.join(module, 'src/sample_view.c'),
        '-o',
        binary,
      ],
      { timeout: 10_000 },
    );
    execFileSync(binary, { timeout: 2000 });
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
});
