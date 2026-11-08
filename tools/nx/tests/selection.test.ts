import { execFileSync } from 'node:child_process';
import { existsSync, globSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { parse } from 'smol-toml';
import { createDependencies } from '../modules.mjs';
import { manifestClosure } from '../module-manifests.mjs';
import { changedFiles, gateCommands, menuOnly, MENU_SOURCE, TIERS, uiOnly, UI_SOURCE } from '../gate.mjs';

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
          {
            options?: { command?: string };
            inputs?: unknown[];
            cache?: boolean;
          }
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

const selected = (file: string, targets: string) => {
  expect(existsSync(join(root, file)), `selection fixture exists: ${file}`).toBe(true);
  return plan(['affected', `--files=${file}`, `--targets=${targets}`]).tasks;
};

let workspace: ProjectGraph;
beforeAll(() => {
  workspace = plan(['graph'], '--file').graph;
});

// Let Vitest flush worker updates between synchronous Nx subprocesses.
afterEach(async () => {
  await new Promise<void>((resolve) => setImmediate(resolve));
});

function imageConsumer(excluded?: string): string {
  return Object.entries(workspace.nodes).find(
    ([id, node]) =>
      id !== excluded && node.data.root.startsWith('modules/machines/') && node.data.targets['test-conformance'],
  )![0];
}

function otherImageConsumer(): string {
  return Object.entries(workspace.nodes).find(
    ([id, node]) =>
      id !== imageConsumer() && node.data.root.startsWith('modules/machines/') && node.data.targets['test-conformance'],
  )![0];
}

const manifestOf = (id: string) => `${workspace.nodes[id].data.root}/module.toml`;

function pickerConsumer(): string {
  const consumers = Object.entries(workspace.nodes).filter(([id, node]) => {
    if (!node.data.targets.pack) return false;
    const manifest = parse(readFileSync(join(root, manifestOf(id)), 'utf8')) as {
      object?: { sources?: string[] }[];
    };
    return manifest.object?.some((object) =>
      object.sources?.some((source) => resolve(root, node.data.root, source) === join(root, 'sdk/src/sample_picker.c')),
    );
  });
  expect(consumers).toHaveLength(1);
  return consumers[0][0];
}

describe('default gate fixture scheduling policy', () => {
  beforeEach(() => vi.stubEnv('NX_HEAD', ''));
  afterEach(() => vi.unstubAllEnvs());
  const targets = 'pack,test-device,test-conformance,rust-tests';
  const requestedTargets = (command: string[]) => {
    const index = command.indexOf('-t');
    return new Set(command[index + 1].split(','));
  };
  const commandsFor = (files: string[]) =>
    gateCommands(
      ['--base=main', '-t', targets],
      files.map((file) => ({ file, status: 'M' })),
    );
  // Supply hypothetical changed files to Nx only after the gate classifies the default comparison.
  const gatePlan = (files: string[]) =>
    commandsFor(files).map(
      (command: string[]) =>
        plan([...command.filter((arg) => arg !== '--base=main'), `--files=${files.join(',')}`]).tasks,
    );
  const withoutConformance = targets
    .split(',')
    .filter((target) => target !== 'test-conformance')
    .join(',');

  it('keeps comparisons without one explicit nonempty base on the broad path', () => {
    for (const comparison of [[], ['--base='], ['--base=main', '--base=HEAD'], ['--base=main', '--head=HEAD']]) {
      expect(changedFiles(comparison)).toEqual([]);
    }
    const command = gateCommands(['-t', targets], [{ file: MENU_SOURCE, status: 'M' }])[0];
    expect(command).toEqual(['affected', '-t', targets]);
  });

  it('keeps an Nx environment head override on the broad path', () => {
    vi.stubEnv('NX_HEAD', 'another-commit');
    expect(changedFiles(['--base=main'])).toEqual([]);
    expect(gateCommands(['--base=main', '-t', targets], [{ file: MENU_SOURCE, status: 'M' }])).toEqual([
      ['affected', '--base=main', '-t', targets],
    ]);
  });

  it('keeps alternate file and stdin selectors on the broad path', () => {
    for (const selector of [['--stdin'], ['--stdin=true']]) expect(menuOnly(changedFiles(selector))).toBe(false);
    for (const selector of [
      ['--files', MENU_SOURCE, 'sdk/include/dk/api.h'],
      ['--files', MENU_SOURCE, '--files', 'sdk/include/dk/api.h'],
      [`--files=${MENU_SOURCE}`, '--files=sdk/include/dk/api.h'],
      [`--files=${MENU_SOURCE},sdk/include/dk/api.h`],
      [`--files=${MENU_SOURCE}`],
    ]) {
      expect(changedFiles(selector)).toEqual([]);
      // Even a supplied menu-only classification cannot override an alternate selector.
      const commands = gateCommands([...selector, '-t', targets], [{ file: MENU_SOURCE, status: 'M' }]);
      expect(commands).toEqual([['affected', ...selector, '-t', targets]]);
    }
  });

  it('drops only conformance for the menu fixture and keeps the base comparison', () => {
    expect(commandsFor([MENU_SOURCE])).toEqual([['affected', '--base=main', '-t', withoutConformance]]);
  });

  it('drops only conformance for the UI fixture and keeps the base comparison', () => {
    expect(commandsFor([UI_SOURCE])).toEqual([['affected', '--base=main', '-t', withoutConformance]]);
  });

  it('keeps UI policy fail-closed for unsupported selectors, comparisons and incomplete target sets', () => {
    const changes = [{ file: UI_SOURCE, status: 'M' }];
    const selections = [
      [],
      ['--base='],
      ['--base=main', '--base=HEAD'],
      ['--base=main', '--head=HEAD'],
      ...['--files', '--stdin', '--uncommitted', '--untracked', '--all'].map((selector) => [
        '--base=main',
        `${selector}=true`,
      ]),
    ];
    for (const args of selections) {
      expect(changedFiles(args)).toEqual([]);
      const command = gateCommands([...args, '-t', targets], changes)[0];
      expect(requestedTargets(command)).toEqual(new Set(targets.split(',')));
    }
    vi.stubEnv('NX_HEAD', 'another-commit');
    expect(requestedTargets(gateCommands(['--base=main', '-t', targets], changes)[0])).toEqual(
      new Set(targets.split(',')),
    );
    vi.stubEnv('NX_HEAD', '');
    for (const missing of targets.split(',')) {
      const incomplete = targets
        .split(',')
        .filter((target) => target !== missing)
        .join(',');
      expect(gateCommands(['--base=main', '-t', incomplete], changes)).toEqual([
        ['affected', '--base=main', '-t', incomplete],
      ]);
    }
  });

  it('keeps normal affected selection for mixed and unknown changes', () => {
    for (const files of [
      [UI_SOURCE, 'sdk/include/dk/api.h'],
      [UI_SOURCE, 'modules/core/runtime/src/rs/src/loader.rs'],
      ['unknown-file'],
    ]) {
      const graphs = gatePlan(files);
      expect(graphs).toHaveLength(1);
      expect(graphs[0]).toEqual(selectedPlan(files));
      if (files[0] !== 'unknown-file')
        expect(Object.keys(graphs[0].tasks)).toContain(`${imageConsumer()}:test-conformance`);
    }
    function selectedPlan(files: string[]) {
      return plan(['affected', `--files=${files.join(',')}`, '-t', targets]).tasks;
    }
  }, 60_000);

  it('rejects rename, deletion and uncertain change metadata', () => {
    for (const status of ['A', 'D', 'R100', '?']) expect(menuOnly([{ file: MENU_SOURCE, status }])).toBe(false);
    expect(menuOnly([])).toBe(false);
    for (const status of ['A', 'D', 'R100', '?']) {
      const changes = [{ file: UI_SOURCE, status }];
      expect(uiOnly(changes)).toBe(false);
      expect(requestedTargets(gateCommands(['--base=main', '-t', targets], changes)[0])).toEqual(
        new Set(targets.split(',')),
      );
    }
    expect(uiOnly([])).toBe(false);
  });

  it.each([
    [MENU_SOURCE, menuOnly],
    [UI_SOURCE, uiOnly],
  ] as const)('uses Git status to classify modifications to %s', (source, isolatedOnly) => {
    const repo = join(scratch, `git-policy-${source === MENU_SOURCE ? 'menu' : 'ui'}`);
    mkdirSync(join(repo, source, '..'), { recursive: true });
    const git = (...args: string[]) => execFileSync('git', args, { cwd: repo, stdio: 'pipe' });
    git('init', '-b', 'main');
    writeFileSync(join(repo, source), 'original');
    git('add', '.');
    git('-c', 'user.name=Gate Test', '-c', 'user.email=gate@example.invalid', 'commit', '-m', 'fixture');
    const changes = () => changedFiles(['--base=main'], repo);
    writeFileSync(join(repo, source), 'modified');
    expect(isolatedOnly(changes())).toBe(true);
    writeFileSync(join(repo, 'unknown'), 'new');
    expect(isolatedOnly(changes())).toBe(false);
    rmSync(join(repo, 'unknown'));
    git('mv', source, 'renamed.c');
    expect(isolatedOnly(changes())).toBe(false);
    expect(isolatedOnly(changedFiles([`--files=${source}`], repo))).toBe(false);
    git('reset', '--hard', 'HEAD');
    rmSync(join(repo, source));
    expect(isolatedOnly(changes())).toBe(false);
  });
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
  expect(reaches('core/runtime', 'core/runtime-capacity-tests')).toBe(false);
});

it('keeps every gated runtime device file in exactly one owner command', () => {
  const base = workspace.nodes['core/runtime-tests'].data;
  const owners = Object.values(workspace.nodes)
    .map((node) => node.data)
    .filter(
      (owner) =>
        owner.tags?.includes('test-only') && (owner.root === base.root || owner.root.startsWith(base.root + '/')),
    );
  const actual = owners
    .flatMap((owner) => {
      const targets = Object.entries(owner.targets)
        .filter(([name, target]) => /^test-device(-full)?(--|$)/.test(name) && target.options?.command)
        .map(([, target]) => target);
      const files = targets.flatMap((target) =>
        (target.options.command.match(/\S+\.device\.test\.ts/g) ?? []).map((file) =>
          resolve(root, 'tests/regression', file),
        ),
      );
      const inputs = targets.flatMap((target) => target.inputs);
      const sources = new Set((owner.metadata?.runtimeAcceptance ?? []).flatMap((rec) => rec.sources ?? []));
      for (const source of sources) expect(inputs).toContain(`{workspaceRoot}/${source}`);
      for (const rec of owner.metadata?.runtimeAcceptance ?? [])
        for (const file of rec.files ?? []) expect(files).toContain(join(root, owner.root, file));
      return files;
    })
    .sort();
  const expected = globSync('**/*.device.test.ts', { cwd: join(root, base.root) })
    .filter((file) => !file.endsWith('.manual.device.test.ts'))
    .map((file) => join(root, base.root, file))
    .sort();
  expect(actual).toEqual(expected);
});

it('derives content runtime edges from the unique runtime role', () => {
  const fixture = mkdtempSync(join(scratch, 'runtime-role-'));
  const projects = Object.fromEntries(
    [
      ['renamed/runtime', 'modules/core/renamed', 'runtime'],
      ['example/content', 'modules/tools/content', undefined],
    ].map(([name, dir, role]) => {
      mkdirSync(join(fixture, dir!), { recursive: true });
      writeFileSync(
        join(fixture, dir!, 'module.toml'),
        `[module]\nid = "${name}"\ntype = "service"\n${role ? `role = "${role}"\n` : ''}`,
      );
      return [name, { name, root: dir, tags: ['module'] }];
    }),
  );
  const dependencies = createDependencies({}, { workspaceRoot: fixture, projects });
  expect(dependencies.filter((dep) => dep.source === 'example/content').map((dep) => dep.target)).toEqual([
    'renamed/runtime',
  ]);
  writeFileSync(
    join(fixture, 'modules/tools/content/module.toml'),
    '[module]\nid = "example/content"\nrole = "runtime"\n',
  );
  expect(() => createDependencies({}, { workspaceRoot: fixture, projects })).toThrow(
    'Expected one module with role=runtime, found 2',
  );
});

it('forms explicit manifest libraries from selected modules and transitive requirements only', () => {
  const manifest = (id: string, requires: string[] = []) =>
    [id, { file: `modules/${id}/module.toml`, manifest: { requires } }] as const;
  const modules = new Map([
    manifest('example/content', ['example/adapter']),
    manifest('example/adapter', ['example/runtime']),
    manifest('example/runtime'),
    manifest('example/unrelated'),
  ]);
  expect(manifestClosure(modules, ['example/content', 'example/adapter'])).toEqual([
    'modules/example/adapter/module.toml',
    'modules/example/content/module.toml',
    'modules/example/runtime/module.toml',
  ]);
  expect(() => manifestClosure(modules, ['example/missing'])).toThrow('Unknown scoped module example/missing');
});

describe('workspace selection', () => {
  it('keeps full-only module fixtures out of quick capacity selection', () => {
    const manifest = manifestOf(otherImageConsumer());
    expect(selected(manifest, 'test-device').tasks['core/runtime-capacity-tests:test-device']).toBeUndefined();
    const full = selected(manifest, 'test-device-full');
    expect(full.tasks['core/runtime-inferred-needs-tests:test-device-full']).toBeDefined();
    expect(full.tasks['core/runtime-capacity-tests:test-device']).toBeUndefined();
    const quick = selected('tests/fixtures/modules/dyn-fx-gain/src/module.c', 'test-device');
    expect(quick.tasks['core/runtime-capacity-tests:test-device']).toBeDefined();
  }, 30_000);

  it('leaves manual device audits out of inferred gate commands', () => {
    const manual = 'modules/core/runtime/tests/memaudit-module.manual.device.test.ts';
    expect(existsSync(join(root, manual))).toBe(true);
    for (const { data } of Object.values(workspace.nodes))
      for (const target of Object.values(data.targets))
        expect(target.options?.command ?? '').not.toContain('.manual.device.test.ts');
  });

  it('retains core manifest selection for producers and their image consumers', () => {
    const graph = selected('modules/core/runtime/module.toml', 'build-objects,test-conformance');
    expect(graph.tasks['core/runtime:build-objects']).toBeDefined();
    expect(graph.tasks[`${imageConsumer()}:test-conformance`]).toBeDefined();
    expect(graph.tasks[`${otherImageConsumer()}:test-conformance`]).toBeDefined();
  }, 30_000);

  it('packs affected modules without device tests through the default gate', () => {
    const targets = TIERS.merge.targets.join(',');
    const [owner, node] = Object.entries(workspace.nodes).find(
      ([, node]) =>
        node.data.targets.pack && !node.data.targets['test-device'] && !node.data.targets['test-conformance'],
    )!;
    const source = globSync('src/**/*.{c,rs}', { cwd: join(root, node.data.root) }).sort()[0];
    expect(source).toBeDefined();
    const graph = selected(`${node.data.root}/${source}`, targets);
    expect(graph.tasks[`${owner}:pack`]).toBeDefined();
    expect(node.data.targets.pack.cache).toBe(false);
  }, 30_000);

  it.each(['modules/core/adapters/ui/src/ui.c'])(
    'selects image consumers when %s changes',
    (file) => {
      const graph = selected(file, 'test-device,test-conformance');
      expect(graph.tasks['core/runtime-tests:test-device']).toBeDefined();
      expect(graph.tasks[`${imageConsumer()}:test-conformance`]).toBeDefined();
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

  it('selects hermetic Rust tests when the release profile digest recipe changes', () => {
    const graph = selected('tools/nx/cargo-profile-env.mjs', 'rust-tests');
    expect(graph.tasks['dkneeds:rust-tests']).toBeDefined();
    expect(graph.tasks['device-profile:rust-tests']).toBeDefined();
    expect(graph.tasks['patchkit:rust-tests']).toBeUndefined();
  }, 30_000);
});

it.each(['sdk/src/sample_picker.c'])(
  'limits private SDK source %s to its pack and host consumers',
  (file) => {
    const picker = pickerConsumer();
    const unrelated = imageConsumer(picker);
    const graph = selected(file, 'pack,rust-tests,test-device,test-conformance');
    expect(graph.tasks[`${picker}:pack`]).toBeDefined();
    expect(graph.tasks['dkui-host:rust-tests']).toBeDefined();
    const conformanceOwners = Object.values(graph.tasks)
      .filter((task) => task.target.target === 'test-conformance')
      .map((task) => task.target.project);
    expect(conformanceOwners).toEqual(workspace.nodes[picker].data.targets['test-conformance'] ? [picker] : []);
    for (const target of [`${picker}:pack`, 'dkui-host:rust-tests']) {
      const inputs = JSON.parse(nx(['show', 'target', 'inputs', target, '--json']));
      expect(inputs.files, target).toContain(file);
    }
    for (const target of [`${unrelated}:pack`, `${unrelated}:test-conformance`, 'core/runtime:build-objects']) {
      const inputs = JSON.parse(nx(['show', 'target', 'inputs', target, '--json']));
      expect(inputs.files, target).not.toContain(file);
    }
    expect(graph.tasks[`${unrelated}:pack`]).toBeUndefined();
    expect(graph.tasks[`${picker}:test-device`]).toBeDefined();
  },
  30_000,
);

it.each(['sdk/include/dk/api.h'])(
  'retains broad module and device selection for shared SDK contract %s',
  (file) => {
    const picker = pickerConsumer();
    const unrelated = imageConsumer(picker);
    const graph = selected(file, 'pack,test-device,test-conformance');
    for (const target of [
      `${picker}:pack`,
      `${unrelated}:pack`,
      `${unrelated}:test-conformance`,
      'core/runtime-tests:test-device',
    ])
      expect(graph.tasks[target], target).toBeDefined();
  },
  30_000,
);
