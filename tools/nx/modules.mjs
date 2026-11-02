// Nx plugin: one project per module.toml. Edges: [module].requires plus [[object]].sources paths
// that resolve into another module directory. tools/devtools/verify_route.py reads them from the graph.
import { globSync, readFileSync } from 'node:fs';
import { dirname, join, normalize } from 'node:path';
import { DependencyType } from '@nx/devkit';
import { parse } from 'smol-toml';

const read = (root, toml) => parse(readFileSync(join(root, toml), 'utf8'));

export const createNodesV2 = [
  '{modules,tests/fixtures/modules}/**/module.toml',
  (files, _opts, ctx) =>
    files.map((f) => {
      const { module: m } = read(ctx.workspaceRoot, f);
      const root = dirname(f);
      const project = { name: m.id, root, tags: ['module', `type:${m.type}`] };
      const tests = globSync('tests/**/*.test.ts', { cwd: join(ctx.workspaceRoot, root) });
      const hostTests = tests.filter((t) => !t.endsWith('.device.test.ts'));
      const deviceTests = tests.filter((t) => t.endsWith('.device.test.ts')).sort();
      const shipped = root.startsWith('modules/');
      const targets = {};
      if (shipped && hostTests.length)
        targets['test-host'] = {
          executor: 'nx:run-commands',
          cache: false,
          inputs: ['hostTests'],
          outputs: [],
          options: {
            command: `pnpm exec vitest run --config vitest.host.config.ts --project module-tests ${root}/`,
            cwd: '.',
          },
        };
      if (shipped && deviceTests.length)
        targets['test-device'] = {
          executor: 'nx:run-commands',
          cache: false,
          outputs: [],
          options: {
            command: `uv run python -m tools.devtools.verify --tests ${deviceTests.map((t) => join(root, t)).join(' ')}`,
            cwd: '.',
          },
        };
      // Same rule as RUNTIME_UPLOADS in tools/devtools/verify_route.py: the shared conformance suite loads these.
      if (shipped && (m.dynamic || m.host || m.type === 'service'))
        targets['test-conformance'] = {
          executor: 'nx:run-commands',
          cache: false,
          outputs: [],
          options: { command: `uv run python -m tools.devtools.verify --conformance ${m.id}`, cwd: '.' },
        };
      if (Object.keys(targets).length) project.targets = targets;
      return [f, { projects: { [root]: project } }];
    }),
];

export const createDependencies = (_opts, ctx) => {
  const mods = Object.values(ctx.projects).filter((p) => p.tags?.includes('module'));
  const owner = (path) =>
    mods
      .filter((p) => path === p.root || path.startsWith(p.root + '/'))
      .sort((a, b) => b.root.length - a.root.length)[0]?.name;
  // Shipped content modules (not core/*, not fixtures) need the runtime and the adapters of their type.
  const implicit = (p, type) =>
    !p.root.startsWith('modules/') || p.root.startsWith('modules/core/')
      ? []
      : [
          'core/runtime',
          ...(type === 'machine' ? ['core/dynreg', 'core/dsp-voice'] : []),
          ...(type === 'fx' ? ['core/dynfx', 'core/fxpages'] : []),
        ];
  return mods.flatMap((p) => {
    const t = read(ctx.workspaceRoot, join(p.root, 'module.toml'));
    const deps = [...(t.module.requires ?? []), ...implicit(p, t.module.type)];
    for (const o of t.object ?? []) for (const s of o.sources ?? []) deps.push(owner(normalize(join(p.root, s))) ?? '');
    return [...new Set(deps)]
      .filter((d) => d && d !== p.name)
      .map((target) => ({
        source: p.name,
        target,
        sourceFile: join(p.root, 'module.toml'),
        type: DependencyType.static,
      }));
  });
};
