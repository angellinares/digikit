import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { expect, it } from 'vitest';

const root = fileURLToPath(new URL('../../../', import.meta.url));

it('compiles public machine and FX declarations and preserves their parameter units and routes', () => {
  const scratch = mkdtempSync(join(tmpdir(), 'digikit-sdk-contract-'));
  try {
    const binary = join(scratch, 'declarations');
    execFileSync(
      'cc',
      ['-std=c11', '-Wall', '-Wextra', '-Werror', '-Isdk/include', 'sdk/tests/declarations.c', '-o', binary],
      {
        cwd: root,
        timeout: 15_000,
        stdio: 'pipe',
      },
    );
    const records = JSON.parse(execFileSync(binary, { encoding: 'utf8', timeout: 5000 }));
    expect(records).toEqual({
      machine: {
        id: 'sine',
        render: '_sn_render',
        note_on: '_sn_note_on',
        parameter: { owner: 'sine', word: 0, min: 0, max: 32512, default: 32512 },
      },
      fx: {
        id: 'gain',
        process: '_gbt_process',
        mix: 127,
        route: 1,
        routes: 3,
        parameter: { owner: 'gain', word: 0, min: 0, max: 1.27, default: 0.5 },
      },
    });
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}, 20_000);
