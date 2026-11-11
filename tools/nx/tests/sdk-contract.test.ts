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
        timeout: 25_000,
        stdio: 'pipe',
      },
    );
    const records = JSON.parse(execFileSync(binary, { encoding: 'utf8', timeout: 10_000 }));
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
}, 40_000);

function runHostC(source: string, ...extra: string[]) {
  const scratch = mkdtempSync(join(tmpdir(), 'digikit-sdk-host-'));
  try {
    const binary = join(scratch, 'host');
    execFileSync('cc', ['-std=c11', '-Wall', '-Wextra', '-Werror', '-Isdk/include', source, ...extra, '-o', binary], {
      cwd: root,
      timeout: 25_000,
      stdio: 'pipe',
    });
    execFileSync(binary, { timeout: 10_000, stdio: 'pipe' });
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}

it('draws the page widgets inside their bounds', () => {
  runHostC('sdk/tests/page/widgets.c');
}, 40_000);

it('draws and clips the sample view', () => {
  runHostC('sdk/tests/sample/view.c');
}, 40_000);

it('reads and writes big-endian 32-bit fields', () => {
  runHostC('sdk/tests/core/bytes.c');
}, 40_000);

it('bounds every text helper to its buffer and formats numbers', () => {
  runHostC('sdk/tests/core/text.c', 'sdk/src/core/text.c');
}, 40_000);

it('joins, splits and names paths within the public limit', () => {
  runHostC('sdk/tests/core/path.c', 'sdk/src/core/path.c', 'sdk/src/core/text.c');
}, 40_000);
