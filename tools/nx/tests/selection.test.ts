import { execFileSync } from 'node:child_process';
import { globSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';

const root = fileURLToPath(new URL('../../../', import.meta.url));
const scratch = mkdtempSync(join(tmpdir(), 'digikit-nx-selection-'));
afterAll(() => rmSync(scratch, { recursive: true, force: true }));

type TaskGraph = {
  tasks: Record<string, { target: { project: string; target: string } }>;
  dependencies: Record<string, { id: string }[]>;
};

type ProjectGraph = {
  nodes: Record<
    string,
    {
      data: {
        root: string;
        targets: Record<string, { options?: { command?: string }; inputs?: unknown[] }>;
        metadata?: { runtimeAcceptance?: { files?: string[]; sources?: string[] }[] };
      };
    }
  >;
  dependencies: Record<string, { target: string }[]>;
};

function plan(args: string[], outputFlag = '--graph'): { tasks: TaskGraph; graph: ProjectGraph } {
  const output = join(scratch, 'tasks.json');
  const env: Record<string, string | undefined> = {
    ...process.env,
    NX_DAEMON: 'false',
    NX_NO_CLOUD: 'true',
    NX_ISOLATE_PLUGINS: 'false',
    NX_WORKSPACE_DATA_DIRECTORY: join(scratch, 'workspace-data'),
  };
  delete env.NX_LEGACY_AFFECTED;
  execFileSync('pnpm', ['exec', 'nx', ...args, `${outputFlag}=${output}`], {
    cwd: root,
    env,
    timeout: 30_000,
    stdio: 'pipe',
  });
  return JSON.parse(readFileSync(output, 'utf8'));
}

const selected = (file: string, targets: string) => plan(['affected', `--files=${file}`, `--targets=${targets}`]).tasks;

let workspace: ProjectGraph;
beforeAll(() => {
  workspace = plan(['graph'], '--file').graph;
});

it('keeps runtime and shipped content outside dependency cycles', () => {
  const reaches = (from: string, target: string, seen = new Set<string>()): boolean => {
    if (seen.has(from)) return false;
    seen.add(from);
    return (workspace.dependencies[from] ?? []).some(
      (dep) => dep.target === target || reaches(dep.target, target, seen),
    );
  };
  for (const [name, node] of Object.entries(workspace.nodes)) {
    if (name === 'core/runtime' || /^modules\/(machines|fx|tools)\//.test(node.data.root))
      expect(reaches(name, name), name).toBe(false);
  }
  expect(reaches('core/runtime', 'core/runtime-tests')).toBe(false);
});

it('keeps every runtime device file in the new owner command', () => {
  const owner = workspace.nodes['core/runtime-tests'].data;
  const command = owner.targets['test-device'].options?.command ?? '';
  const actual = (command.match(/\S+\.device\.test\.ts/g) ?? [])
    .map((file) => resolve(root, 'tests/regression', file))
    .sort();
  const expected = globSync('**/*.device.test.ts', { cwd: join(root, owner.root) })
    .map((file) => join(root, owner.root, file))
    .sort();
  expect(actual).toEqual(expected);
  const sources = new Set((owner.metadata?.runtimeAcceptance ?? []).flatMap((rec) => rec.sources ?? []));
  for (const source of sources) expect(owner.targets['test-device'].inputs).toContain(`{workspaceRoot}/${source}`);
  for (const rec of owner.metadata?.runtimeAcceptance ?? [])
    for (const file of rec.files ?? []) expect(actual).toContain(join(root, owner.root, file));
});

describe('workspace selection', () => {
  it('runs the standalone runtime unit tests without build or daemon prerequisites', () => {
    const graph = selected('modules/core/runtime/src/rs/src/lib.rs', 'rust-tests,test-device');
    expect(graph.tasks['core/runtime:rust-tests']).toBeDefined();
    expect(graph.dependencies['core/runtime:rust-tests']).toEqual([]);
    expect(graph.tasks['core/runtime-tests:test-device']).toBeDefined();
  }, 30_000);

  it('keeps desktop source edits out of unrelated host suites', () => {
    const graph = selected('apps/desktop/ui/src/card-core.ts', 'test-host');
    expect(Object.keys(graph.tasks)).toEqual(['@digi/desktop:test-host']);
  }, 30_000);

  it.each(['modules/core/runtime/sdk/src/lib.rs', 'modules/core/runtime/fit/src/lib.rs'])(
    'selects the runtime consumer unit tests when %s changes',
    (file) => {
      const graph = selected(file, 'rust-tests');
      expect(graph.tasks['core/runtime:rust-tests']).toBeDefined();
      expect(graph.dependencies['core/runtime:rust-tests']).toEqual([]);
    },
    30_000,
  );

  it('retains a leaf module’s own conformance and pack prerequisites', () => {
    const fixtures = new Set(workspace.dependencies['core/runtime-tests'].map((dep) => dep.target));
    const [owner, node] = Object.entries(workspace.nodes)
      .filter(
        ([name, node]) =>
          node.data.root.startsWith('modules/machines/') && node.data.targets['test-conformance'] && fixtures.has(name),
      )
      .sort(([a], [b]) => a.localeCompare(b))[0];
    const source = globSync('src/**/*.{c,rs}', { cwd: join(root, node.data.root) }).sort()[0];
    expect(source).toBeDefined();
    const file = `${node.data.root}/${source}`;
    const graph = selected(file, 'test-conformance,test-device');
    expect(graph.tasks[`${owner}:test-conformance`]).toBeDefined();
    expect(graph.dependencies[`${owner}:test-conformance`].map((dep) => dep.id)).toContain(`${owner}:pack`);
    expect(graph.tasks['core/runtime-tests:test-device']).toBeDefined();
    expect(
      Object.values(graph.tasks)
        .filter((task) => task.target.target === 'test-conformance')
        .map((task) => task.target.project),
    ).toEqual([owner]);
  }, 30_000);

  it('keeps runtime acceptance edits out of production consumers', () => {
    const graph = selected(
      'modules/core/runtime/tests/capacity.device.test.ts',
      'rust-tests,test-device,test-conformance',
    );
    expect(graph.tasks['core/runtime-tests:test-device']).toBeDefined();
    expect(graph.tasks['core/runtime:rust-tests']).toBeUndefined();
    expect(Object.values(graph.tasks).filter((task) => task.target.target === 'test-conformance')).toEqual([]);
  }, 30_000);

  it('selects acceptance for its declared Python helper source', () => {
    const graph = selected('tools/device/dktransfer.py', 'test-device');
    expect(graph.tasks['core/runtime-tests:test-device']).toBeDefined();
  }, 30_000);
});
