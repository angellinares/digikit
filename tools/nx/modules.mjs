// Nx plugin: one project per module.toml. Edges: [module].requires plus [[object]].sources paths
// that resolve into another module directory. tools/devtools/verify_route.py reads them from the graph.
// Targets: pack (build + pack to dist/modules/<dev>/<leaf>.dkm), pack-aliases (renamed copies named by
// metadata.runtimeAcceptance), test-host, test-device and test-conformance (vitest directly, after the packs).
import { existsSync, globSync, readFileSync } from 'node:fs';
import { dirname, join, normalize } from 'node:path';
import { DependencyType } from '@nx/devkit';
import { parse } from 'smol-toml';

const read = (root, toml) => parse(readFileSync(join(root, toml), 'utf8'));
// Commands run with cwd '.' (the workspace root); Nx accepts {workspaceRoot} only at the start of an option.
const PK = 'dist/patchkit/patchkit --root .';

// Same as pack_devices in tools/devtools/verify.py: no ports means both devices.
const packDevices = (m) => {
  if (!m.ports?.length) return ['dt2', 'dn2'];
  return ['dt2', 'dn2'].filter((d) => m.ports.some((p) => p.startsWith(`${d}-`)));
};
// Shipped module that is loaded at run time: it gets test-conformance.
const isUploadable = (root, m) => root.startsWith('modules/') && !!(m.dynamic || m.host || m.type === 'service');
const leaf = (id) => id.split('/').at(-1);
const dkm = (dev, id) => `dist/modules/${dev}/${leaf(id)}.dkm`;
const records = (root) => {
  const file = join(root, 'project.json');
  return existsSync(file) ? (JSON.parse(readFileSync(file, 'utf8')).metadata?.runtimeAcceptance ?? []) : [];
};

export const createNodesV2 = [
  '{modules,tests/fixtures/modules}/**/module.toml',
  (files, _opts, ctx) => {
    // id -> manifest, so an alias can find the devices of its source module.
    const byId = new Map(
      files.map((f) => [read(ctx.workspaceRoot, f).module.id, { manifest: read(ctx.workspaceRoot, f).module }]),
    );
    // Only the modules the gate packs get a pack target: patched-in core modules and static fixtures
    // (several ColdFire objects, no dkm_ symbols) cannot be packed as .dkm.
    const packable = new Set(['digikit/debug']);
    for (const [id, { manifest: m }] of byId) {
      const root = files.find((f) => read(ctx.workspaceRoot, f).module.id === id);
      if (root?.startsWith('modules/') && (m.dynamic || m.host || m.type === 'service')) packable.add(id);
    }
    const projectFiles = [
      ...globSync('{modules,tests/fixtures/modules}/**/project.json', { cwd: ctx.workspaceRoot }),
      'tests/regression/project.json',
    ];
    for (const pf of projectFiles) {
      const dir = dirname(pf);
      for (const rec of records(join(ctx.workspaceRoot, dir))) {
        for (const id of rec.modules ?? []) packable.add(id);
        for (const source of Object.values(rec.aliases ?? {})) packable.add(source);
      }
    }
    return files.map((f) => {
      const { module: m } = read(ctx.workspaceRoot, f);
      const root = dirname(f);
      const project = { name: m.id, root, tags: ['module', `type:${m.type}`] };
      const tests = globSync('tests/**/*.test.ts', { cwd: join(ctx.workspaceRoot, root) });
      const hostTests = tests.filter((t) => !t.endsWith('.device.test.ts'));
      const deviceTests = tests.filter((t) => t.endsWith('.device.test.ts')).sort();
      const shipped = root.startsWith('modules/');
      const targets = {};
      const uploadable = isUploadable(root, m);
      const recs = records(join(ctx.workspaceRoot, root));
      const devices = packDevices(m);
      if (devices.length && packable.has(m.id))
        targets.pack = {
          executor: 'nx:run-commands',
          cache: false,
          dependsOn: ['patchkit:cli'],
          outputs: devices.map((d) => `{workspaceRoot}/${dkm(d, m.id)}`),
          options: {
            commands: [
              'mkdir -p dist/modules/dt2 dist/modules/dn2',
              `${PK} build ${root}`,
              ...devices.map((d) => `${PK} pack ${m.id} --device ${d} -o ${dkm(d, m.id)}`),
            ],
            parallel: false,
            cwd: '.',
          },
        };
      const aliasCmds = [];
      const aliasOutputs = [];
      const aliasSources = new Set();
      for (const rec of recs)
        for (const [alias, source] of Object.entries(rec.aliases ?? {})) {
          // A source outside the scanned module.toml files has no manifest: its devices are unknown, skip it.
          const src = byId.get(source);
          if (!src) continue;
          aliasSources.add(source);
          for (const d of packDevices(src.manifest)) {
            aliasCmds.push(`${PK} pack ${source} --device ${d} --name ${alias} -o ${dkm(d, alias)}`);
            aliasOutputs.push(`{workspaceRoot}/${dkm(d, alias)}`);
          }
        }
      if (aliasCmds.length)
        targets['pack-aliases'] = {
          executor: 'nx:run-commands',
          cache: false,
          dependsOn: ['patchkit:cli', ...[...aliasSources].map((s) => `${s}:pack`)],
          outputs: aliasOutputs,
          options: {
            commands: ['mkdir -p dist/modules/dt2 dist/modules/dn2', ...aliasCmds],
            parallel: false,
            cwd: '.',
          },
        };
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
      if (shipped && deviceTests.length) {
        const dependsOn = ['patchkit:cli', 'digi-control:daemons'];
        if (uploadable) dependsOn.push('pack');
        for (const rec of recs) {
          for (const id of rec.modules ?? []) dependsOn.push(`${id}:pack`);
          if (Object.keys(rec.aliases ?? {}).length) dependsOn.push('pack-aliases');
          if (rec.plusdrive) dependsOn.push('plusdrive-format:cli');
        }
        targets['test-device'] = {
          executor: 'nx:run-commands',
          cache: false,
          parallelism: false,
          dependsOn: [...new Set(dependsOn)],
          outputs: [],
          options: {
            command: `pnpm --filter @digi/regression test:runtime ${deviceTests.map((t) => `../../${root}/${t}`).join(' ')}`,
            cwd: '.',
          },
        };
      }
      // Same rule as RUNTIME_UPLOADS in tools/devtools/verify_route.py: the shared conformance suite loads these.
      if (uploadable)
        targets['test-conformance'] = {
          executor: 'nx:run-commands',
          cache: false,
          parallelism: false,
          dependsOn: ['pack', 'digikit/debug:pack', 'digi-control:daemons'],
          outputs: [],
          options: {
            command: 'pnpm --filter @digi/regression test:conformance',
            env: { DIGI_CONFORMANCE_MODULES: m.id },
            cwd: '.',
          },
        };
      if (Object.keys(targets).length) project.targets = targets;
      return [f, { projects: { [root]: project } }];
    });
  },
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
  const all = Object.values(ctx.projects);
  return mods.flatMap((p) => {
    const t = read(ctx.workspaceRoot, join(p.root, 'module.toml'));
    const deps = [...(t.module.requires ?? []), ...implicit(p, t.module.type)];
    for (const o of t.object ?? []) for (const s of o.sources ?? []) deps.push(owner(normalize(join(p.root, s))) ?? '');
    const statics = [...new Set(deps)]
      .filter((d) => d && d !== p.name)
      .map((target) => ({
        source: p.name,
        target,
        sourceFile: join(p.root, 'module.toml'),
        type: DependencyType.static,
      }));
    // Implicit edges: a module with device or conformance tests depends on the harness, the daemon and the packs.
    const hasDeviceTests = !!(p.targets?.['test-device'] || p.targets?.['test-conformance']);
    const implicitDeps = hasDeviceTests ? ['regression', 'conformance', 'digi-control', 'patchkit'] : [];
    // Projects that live inside the module directory (for example core/ui and dkui-host).
    for (const q of all) if (q.name !== p.name && q.root.startsWith(p.root + '/')) implicitDeps.push(q.name);
    const implicits = [...new Set(implicitDeps)]
      .filter((d) => d !== p.name && ctx.projects[d])
      .map((target) => ({ source: p.name, target, type: DependencyType.implicit }));
    return [...statics, ...implicits];
  });
};
