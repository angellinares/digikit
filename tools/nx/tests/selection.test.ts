import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { afterAll, describe, expect, it } from 'vitest';

const root = fileURLToPath(new URL('../../../', import.meta.url));
const scratch = mkdtempSync(join(tmpdir(), 'digikit-nx-selection-'));
afterAll(() => rmSync(scratch, { recursive: true, force: true }));

type TaskGraph = {
  tasks: Record<string, { target: { project: string; target: string } }>;
  dependencies: Record<string, { id: string }[]>;
};

function selected(file: string, targets: string, legacy: boolean): TaskGraph {
  const output = join(scratch, 'tasks.json');
  execFileSync('pnpm', ['exec', 'nx', 'affected', `--files=${file}`, `--targets=${targets}`, `--graph=${output}`], {
    cwd: root,
    env: {
      ...process.env,
      NX_DAEMON: 'false',
      NX_NO_CLOUD: 'true',
      NX_ISOLATE_PLUGINS: 'false',
      NX_WORKSPACE_DATA_DIRECTORY: join(scratch, 'workspace-data'),
      NX_LEGACY_AFFECTED: String(legacy),
    },
    timeout: 30_000,
    stdio: 'pipe',
  });
  return JSON.parse(readFileSync(output, 'utf8')).tasks;
}

describe.each([true, false])('workspace selection (legacy=%s)', (legacy) => {
  it('runs the standalone runtime unit tests without build or daemon prerequisites', () => {
    const graph = selected('modules/core/runtime/src/rs/src/lib.rs', 'rust-tests', legacy);
    expect(graph.tasks['core/runtime:rust-tests']).toBeDefined();
    expect(graph.dependencies['core/runtime:rust-tests']).toEqual([]);
  }, 30_000);

  it('keeps desktop source edits out of unrelated host suites', () => {
    const graph = selected('apps/desktop/ui/src/card-core.ts', 'test-host', legacy);
    // Legacy selection also includes web's host suite because its check/Node targets read UI files.
    expect(Object.keys(graph.tasks).sort()).toEqual(
      legacy ? ['@digi/desktop:test-host', '@digi/web:test-host'] : ['@digi/desktop:test-host'],
    );
  }, 30_000);

  it.each(['modules/core/runtime/sdk/src/lib.rs', 'modules/core/runtime/fit/src/lib.rs'])(
    'selects the runtime consumer unit tests when %s changes',
    (file) => {
      const graph = selected(file, 'rust-tests', legacy);
      expect(graph.tasks['core/runtime:rust-tests']).toBeDefined();
      expect(graph.dependencies['core/runtime:rust-tests']).toEqual([]);
    },
    30_000,
  );

  it('retains a leaf module’s own conformance and pack prerequisites', () => {
    const graph = selected('modules/machines/pluck/src/module.c', 'test-conformance', legacy);
    expect(graph.tasks['digikit/pluck:test-conformance']).toBeDefined();
    expect(graph.dependencies['digikit/pluck:test-conformance'].map((dep) => dep.id)).toContain('digikit/pluck:pack');
  }, 30_000);
});
