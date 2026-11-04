import { execFileSync } from 'node:child_process';
import { globSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { afterAll, expect, it } from 'vitest';

const root = fileURLToPath(new URL('../../../', import.meta.url));
const scratch = mkdtempSync(join(tmpdir(), 'digikit-sdk-selection-'));
afterAll(() => rmSync(scratch, { recursive: true, force: true }));

function nx(args: string[]): string {
  const env: Record<string, string | undefined> = {
    ...process.env,
    NX_DAEMON: 'false',
    NX_NO_CLOUD: 'true',
    NX_ISOLATE_PLUGINS: 'false',
    NX_WORKSPACE_DATA_DIRECTORY: join(scratch, 'workspace-data'),
  };
  delete env.NX_LEGACY_AFFECTED;
  return execFileSync('pnpm', ['exec', 'nx', ...args], {
    cwd: root,
    env,
    timeout: 30_000,
    stdio: 'pipe',
    encoding: 'utf8',
  });
}

function selected(file: string) {
  const output = join(scratch, 'tasks.json');
  nx(['affected', `--files=${file}`, '--targets=test-host', `--graph=${output}`]);
  return JSON.parse(readFileSync(output, 'utf8')).tasks.tasks;
}

it.each([
  'sdk/include/dk/machine.h',
  'sdk/include/dk/fx.h',
  'tests/fixtures/modules/sine/src/module.c',
  'tests/fixtures/modules/dyn-fx-gain-both/src/module.c',
])(
  'selects the SDK host contract for %s',
  (file) => {
    expect(selected(file)['sdk:test-host']).toBeDefined();
  },
  30_000,
);

it('declares SDK fixture inputs and excludes shipped leaf module edits', () => {
  const inputs = JSON.parse(nx(['show', 'target', 'inputs', 'sdk:test-host', '--json']));
  for (const file of [
    'sdk/tests/declarations.c',
    'sdk/include/dk/page.h',
    'tools/nx/tests/sdk-contract.test.ts',
    'tests/fixtures/modules/sine/src/module.c',
    'tests/fixtures/modules/dyn-fx-gain-both/src/module.c',
  ])
    expect(inputs.files, file).toContain(file);
  const source = globSync('modules/machines/*/src/**/*.c', { cwd: root }).sort()[0];
  expect(source).toBeDefined();
  expect(inputs.files).not.toContain(source);
  for (const file of ['sdk/src/picker_logic.c', 'sdk/src/picker_logic.h']) {
    expect(inputs.files).not.toContain(file);
    expect(selected(file)['sdk:test-host']).toBeUndefined();
  }
  expect(selected(source)['sdk:test-host']).toBeUndefined();
}, 30_000);

it('keeps shared generation inputs complete without optional SDK implementation sources', () => {
  const inputs = JSON.parse(nx(['show', 'target', 'inputs', 'sdk:check-generated', '--json']));
  for (const file of [
    'sdk/sdk.toml',
    'sdk/include/dk/gen/api.h',
    'sdk/asm/dk_machine.inc',
    'modules/core/runtime/abi.toml',
    'modules/core/runtime/sdk/src/gen_abi.rs',
    'modules/core/fw/reference.json',
    'packages/dk-session/src/gen_proto.ts',
  ])
    expect(inputs.files, file).toContain(file);
  for (const file of ['sdk/src/picker_logic.c', 'sdk/src/picker_logic.h']) expect(inputs.files).not.toContain(file);
}, 30_000);
