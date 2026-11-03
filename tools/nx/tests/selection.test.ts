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
        tags?: string[];
        targets: Record<
          string,
          { options?: { command?: string }; inputs?: unknown[]; outputs?: string[]; cache?: boolean }
        >;
        metadata?: { runtimeAcceptance?: { files?: string[]; sources?: string[] }[] };
      };
    }
  >;
  dependencies: Record<string, { target: string }[]>;
};

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

function plan(args: string[], outputFlag = '--graph'): { tasks: TaskGraph; graph: ProjectGraph } {
  const output = join(scratch, 'tasks.json');
  nx([...args, `${outputFlag}=${output}`]);
  return JSON.parse(readFileSync(output, 'utf8'));
}

const selected = (file: string, targets: string) => plan(['affected', `--files=${file}`, `--targets=${targets}`]).tasks;

let workspace: ProjectGraph;
beforeAll(() => {
  workspace = plan(['graph'], '--file').graph;
});

function imageConsumer(): string {
  return Object.entries(workspace.nodes).find(
    ([, node]) => node.data.root.startsWith('modules/machines/') && node.data.targets['test-conformance'],
  )![0];
}

it('prepares precisely the declared image core objects before patched-image consumers', () => {
  const consumers = [
    'core/runtime-tests:test-device',
    'core/runtime-service-tests:test-device',
    `${imageConsumer()}:test-conformance`,
    'regression:test-device',
  ];
  const image = workspace.nodes['core/runtime-tests'].data.targets['test-device'].inputs!.find(
    (input): input is { input: string; projects: string[] } =>
      typeof input === 'object' && input !== null && 'projects' in input,
  )!;
  const expected = image.projects.filter((id) => workspace.nodes[id].data.targets['build-objects']).sort();
  expect(expected.length).toBeGreaterThan(0);
  for (const consumer of consumers) {
    const graph = plan(['run', consumer]).tasks;
    const producers = Object.values(graph.tasks).filter((task) => task.target.target === 'build-objects');
    expect(producers.map((task) => task.target.project).sort(), consumer).toEqual(expected);
    expect(
      graph.dependencies[consumer].map((dep) => dep.id),
      consumer,
    ).toEqual(expect.arrayContaining(expected.map((id) => `${id}:build-objects`)));
    for (const producer of producers) {
      const id = `${producer.target.project}:build-objects`;
      expect(graph.dependencies[id].map((dep) => dep.id)).toContain('patchkit:cli');
      expect(workspace.nodes[producer.target.project].data.targets['build-objects'].cache).toBe(false);
    }
    expect(graph.dependencies['core/runtime:build-objects'].map((dep) => dep.id)).toContain(
      'core/expansion:build-objects',
    );
  }
}, 30_000);

it('keeps stock and runtime host units independent of core-object preparation', () => {
  for (const target of ['gate-stock:stock', 'core/runtime:rust-tests']) {
    const graph = plan(['run', target]).tasks;
    expect(
      Object.values(graph.tasks).filter((task) => task.target.target === 'build-objects'),
      target,
    ).toEqual([]);
  }
}, 30_000);

it('selects core preparation for source edits and declares external Rust build sources', () => {
  const graph = selected('modules/core/apps/digikit-menu/src/menu.c', 'build-objects');
  expect(graph.tasks['core/digikit-menu:build-objects']).toBeDefined();
  expect(Object.values(graph.tasks).some((task) => task.target.target === 'test-device')).toBe(false);
  const inputs = JSON.parse(nx(['show', 'target', 'inputs', 'core/runtime:build-objects', '--json']));
  for (const source of [
    'modules/core/runtime/src/rs/src/lib.rs',
    'modules/core/runtime/sdk/src/device.rs',
    'modules/core/fw/rs/src/lib.rs',
    'packages/dkneeds/src/lib.rs',
  ])
    expect(inputs.files, source).toContain(source);
  const outputs = JSON.parse(nx(['show', 'target', 'outputs', 'core/ui:build-objects', '--json']));
  expect(outputs.outputPaths).toEqual(['modules/core/adapters/ui/build/ui.dko.json']);
  expect(outputs.expandedOutputs).toEqual(outputs.outputPaths);
}, 30_000);

it('declares patch-image sources and objects without consumer acceptance files', () => {
  const conformance = JSON.parse(nx(['show', 'target', 'inputs', `${imageConsumer()}:test-conformance`, '--json']));
  const acceptance = JSON.parse(nx(['show', 'target', 'inputs', 'core/runtime-tests:test-device', '--json']));
  for (const file of [
    'modules/core/apps/digikit-menu/src/menu.c',
    'modules/core/apps/digikit-menu/build/digikit-menu.dko.json',
    'modules/core/adapters/ui/src/ui.c',
    'modules/core/adapters/ui/build/ui.dko.json',
    // DSP-link is a transitive requirement, not an emulator-set root.
    'modules/core/adapters/dsp-link/src/cf/dsp-link.c',
    'modules/core/adapters/dsp-link/build/dsp-link-cf.dko.json',
  ]) {
    expect(conformance.files, file).toContain(file);
    expect(acceptance.files, file).toContain(file);
  }
  expect(conformance.files).not.toContain('modules/core/runtime/tests/capacity.device.test.ts');
  expect(conformance.files).not.toContain('tests/fixtures/modules/dyn-demo-stray/src/rs/src/lib.rs');
}, 30_000);

it('declares patchkit external compile sources in the native input plan', () => {
  const inputs = JSON.parse(nx(['show', 'target', 'inputs', 'patchkit:rust-tests', '--json']));
  for (const file of [
    'sdk/sdk.toml',
    'sdk/vectors/fit-vectors.json',
    'sdk/vectors/settings-vectors.json',
    'devices/digitakt-ii.toml',
    'devices/digitone-ii.toml',
    'tools/patchkit/toolchain/pins.toml',
    'tools/patchkit/toolchain/m68k-cf-none-elf-a2.json',
    'tools/patchkit/pk_toolchains.py',
  ])
    expect(inputs.files, file).toContain(file);
}, 30_000);

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

it('keeps every runtime device file in exactly one owner command', () => {
  const base = workspace.nodes['core/runtime-tests'].data;
  const owners = Object.values(workspace.nodes)
    .map((node) => node.data)
    .filter(
      (owner) =>
        owner.tags?.includes('test-only') && (owner.root === base.root || owner.root.startsWith(base.root + '/')),
    );
  const actual = owners
    .flatMap((owner) => {
      const command = owner.targets['test-device'].options?.command ?? '';
      const files = (command.match(/\S+\.device\.test\.ts/g) ?? []).map((file) =>
        resolve(root, 'tests/regression', file),
      );
      const sources = new Set((owner.metadata?.runtimeAcceptance ?? []).flatMap((rec) => rec.sources ?? []));
      for (const source of sources) expect(owner.targets['test-device'].inputs).toContain(`{workspaceRoot}/${source}`);
      for (const rec of owner.metadata?.runtimeAcceptance ?? [])
        for (const file of rec.files ?? []) expect(files).toContain(join(root, owner.root, file));
      return files;
    })
    .sort();
  const expected = globSync('**/*.device.test.ts', { cwd: join(root, base.root) })
    .map((file) => join(root, base.root, file))
    .sort();
  expect(actual).toEqual(expected);
});

describe('workspace selection', () => {
  it.each([
    'modules/core/runtime/tests/service-commands/service-commands.device.test.ts',
    'tests/fixtures/modules/svc-echo/src/c/svc_echo.c',
  ])(
    'selects only the CF owner for %s',
    (file) => {
      const graph = selected(file, 'test-device,test-conformance');
      expect(
        Object.values(graph.tasks)
          .filter((task) => task.target.target === 'test-device')
          .map((task) => task.target.project),
      ).toEqual(['core/runtime-service-tests']);
      expect(Object.values(graph.tasks).filter((task) => task.target.target === 'test-conformance')).toEqual([]);
    },
    30_000,
  );

  it('keeps the CF file out of parent inputs while preserving owner metadata selection', () => {
    const file = 'modules/core/runtime/tests/service-commands/service-commands.device.test.ts';
    const inputs = JSON.parse(nx(['show', 'target', 'inputs', 'core/runtime-tests:test-device', '--json']));
    expect(inputs.files).not.toContain(file);
    expect(
      selected('modules/core/runtime/tests/project.json', 'test-device').tasks['core/runtime-tests:test-device'],
    ).toBeDefined();
    expect(
      selected('modules/core/runtime/tests/service-commands/project.json', 'test-device').tasks[
        'core/runtime-service-tests:test-device'
      ],
    ).toBeDefined();
  }, 30_000);
  it.each(['modules/core/apps/digikit-menu/src/menu.c', 'modules/core/adapters/ui/src/ui.c'])(
    'selects image consumers when %s changes',
    (file) => {
      const graph = selected(file, 'test-device,test-conformance');
      expect(graph.tasks['core/runtime-tests:test-device']).toBeDefined();
      expect(graph.tasks[`${imageConsumer()}:test-conformance`]).toBeDefined();
    },
    30_000,
  );

  it('selects a Rust dependency’s own and consumer tests', () => {
    const graph = selected('packages/dksample/src/lib.rs', 'rust-tests');
    expect(graph.tasks['dksample:rust-tests']).toBeDefined();
    expect(graph.tasks['plusdrive-format:rust-tests']).toBeDefined();
    expect(graph.tasks['patchkit:rust-tests']).toBeDefined();
  }, 30_000);

  it.each(['sdk/sdk.toml', 'devices/digitakt-ii.toml', 'tools/patchkit/toolchain/pins.toml'])(
    'selects patchkit Rust tests for external compile source %s',
    (file) => {
      expect(selected(file, 'rust-tests').tasks['patchkit:rust-tests']).toBeDefined();
    },
    30_000,
  );

  it('selects cached device consumers when the device digest recipe changes', () => {
    const graph = selected('tools/nx/device-inputs.mjs', 'test-device,test-conformance,stock');
    expect(graph.tasks['core/runtime-tests:test-device']).toBeDefined();
    expect(graph.tasks['core/runtime-service-tests:test-device']).toBeDefined();
    expect(graph.tasks[`${imageConsumer()}:test-conformance`]).toBeDefined();
    expect(graph.tasks['gate-stock:stock']).toBeDefined();
  }, 30_000);

  it('keeps an acceptance-only fixture out of unrelated conformance', () => {
    const graph = selected('tests/fixtures/modules/dyn-demo-stray/src/rs/src/lib.rs', 'test-device,test-conformance');
    expect(graph.tasks['core/runtime-tests:test-device']).toBeDefined();
    expect(Object.values(graph.tasks).filter((task) => task.target.target === 'test-conformance')).toEqual([]);
  }, 30_000);

  it('runs the standalone runtime unit tests without build or daemon prerequisites', () => {
    const graph = selected('modules/core/runtime/src/rs/src/lib.rs', 'rust-tests,test-device');
    expect(graph.tasks['core/runtime:rust-tests']).toBeDefined();
    expect(graph.dependencies['core/runtime:rust-tests']).toEqual([]);
    expect(graph.tasks['core/runtime-tests:test-device']).toBeDefined();
    expect(graph.tasks['core/runtime-service-tests:test-device']).toBeDefined();
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
