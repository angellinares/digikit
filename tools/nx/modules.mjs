// Nx plugin: one project per module.toml. Edges: [module].requires plus [[object]].sources paths
// that resolve into another module directory. tools/devtools/nx_graph.py reads them from the graph.
// Targets: pack (build stale objects and pack to dist/modules/<dev>/<leaf>.dkm), pack-aliases (renamed copies named by
// metadata.runtimeAcceptance), test-host, test-device and test-conformance (vitest directly, after the packs).
import { existsSync, globSync, readFileSync } from 'node:fs';
import { dirname, join, normalize } from 'node:path';
import { DependencyType } from '@nx/devkit';
import { parse } from 'smol-toml';

const read = (root, toml) => parse(readFileSync(join(root, toml), 'utf8'));
// Commands run with cwd '.' (the workspace root); Nx accepts {workspaceRoot} only at the start of an option.
const PK = 'dist/patchkit/patchkit --root .';
const IMAGE_BUILD = { target: 'build-objects', projects: ['tag:acceptance-image'] };

// The devices a module packs for: no ports means both devices.
const packDevices = (m) => {
  if (!m.ports?.length) return ['dt2', 'dn2'];
  return ['dt2', 'dn2'].filter((d) => m.ports.some((p) => p.startsWith(`${d}-`)));
};
// Shipped module that is loaded at run time: it gets test-conformance.
const isUploadable = (root, m) => root.startsWith('modules/') && !!(m.dynamic || m.host || m.type === 'service');
// Cache inputs of test-device and test-conformance: the module and its graph deps (core modules, harness via
// regression/conformance, daemon and patchkit crate sources) with their `production` files (sources without
// tests/ and docs; for regression only the harness files, see tests/regression/project.json), the outputs of
// the pack, CLI and daemons targets (dist/daemons.json names the content-keyed daemon), and the firmware and
// ready dirs via device-inputs.mjs.
// Image core sources/objects are explicit production inputs, without acceptance-fixture graph edges.
// Also named explicitly: the harness packages (test-support, control-client, dk-session), every module.toml
// (the conformance suite scans manifests to select modules) and tools/emulator/fast_regions.json (sets SHARC_* env).
const DEVICE_INPUTS = [
  'default',
  '^production',
  'jsToolchain',
  { dependentTasksOutputFiles: '**/*', transitive: true },
  { runtime: 'node tools/nx/device-inputs.mjs' },
  { env: 'DIGI_EMU_FAST' },
  { env: 'DIGI_NO_PROGRESS_MS' },
  // quick or full tier (tests/regression/runtime/devices.ts FULL_TIER): selects which tests run.
  { env: 'DIGIKIT_TIER' },
  { env: 'DIGI_PK_READY_DT2_117' },
  { env: 'DIGI_PK_READY_DN2_112' },
  { env: 'SHARC_FAST_REGIONS' },
  { env: 'SHARC_BLOCKTRANS' },
  { env: 'SHARC_FAST_FALLBACK_REGIONS' },
  { env: 'SHARC_FAST_AUTO' },
  '{workspaceRoot}/packages/test-support/**/*',
  '{workspaceRoot}/packages/control-client/**/*',
  '{workspaceRoot}/packages/dk-session/**/*',
  '{workspaceRoot}/modules/**/module.toml',
  '{workspaceRoot}/tools/nx/device-inputs.mjs',
  '{workspaceRoot}/tools/emulator/fast_regions.json',
];
const leaf = (id) => id.split('/').at(-1);
const dkm = (dev, id) => `dist/modules/${dev}/${leaf(id)}.dkm`;
const records = (root) => {
  const file = join(root, 'project.json');
  return existsSync(file) ? (JSON.parse(readFileSync(file, 'utf8')).metadata?.runtimeAcceptance ?? []) : [];
};

const deviceTarget = (root, tests, recs, imageInput, uploadable = false) => {
  const dependsOn = ['patchkit:cli', 'digi-control:daemons', IMAGE_BUILD];
  if (uploadable) dependsOn.push('pack');
  for (const rec of recs) {
    for (const id of rec.modules ?? []) dependsOn.push(`${id}:pack`);
    if (Object.keys(rec.aliases ?? {}).length) dependsOn.push('pack-aliases');
    if (rec.plusdrive) dependsOn.push('plusdrive-format:cli');
  }
  return {
    executor: 'nx:run-commands',
    cache: true,
    inputs: [
      ...DEVICE_INPUTS,
      imageInput,
      ...new Set(recs.flatMap((rec) => (rec.sources ?? []).map((s) => `{workspaceRoot}/${s}`))),
    ],
    dependsOn: [...new Set(dependsOn)],
    outputs: [],
    options: {
      env: { REGRESSION_JOBS: '2' },
      command: `pnpm --filter @digi/regression test:runtime ${tests.map((t) => `../../${root}/${t}`).join(' ')}`,
      cwd: '.',
    },
  };
};

export const createNodesV2 = [
  '{modules,tests/fixtures/modules}/**/{module.toml,project.json}',
  (matched, _opts, ctx) => {
    const files = matched.filter((f) => f.endsWith('/module.toml'));
    const testOwners = matched
      .filter((f) => f.endsWith('/project.json'))
      .map((f) => [f, JSON.parse(readFileSync(join(ctx.workspaceRoot, f), 'utf8'))])
      .filter(([, p]) => p.tags?.includes('test-only'));
    // id -> manifest, so an alias can find the devices of its source module.
    const byId = new Map(
      files.map((f) => {
        const parsed = read(ctx.workspaceRoot, f);
        return [parsed.module.id, { manifest: parsed.module, objects: parsed.object ?? [] }];
      }),
    );
    const coreBuilds = new Set(
      [...byId]
        .filter(([, { manifest, objects }]) => manifest.category === 'core' && objects.some((o) => o.sources?.length))
        .map(([id]) => id),
    );
    // Patch image composition follows module.requires, not acceptance-fixture graph edges.
    const imageModules = new Set();
    const includeImageModule = (id) => {
      if (imageModules.has(id)) return;
      const manifest = byId.get(id)?.manifest;
      if (!manifest) throw new Error(`Unknown acceptance patch-set module ${id}`);
      imageModules.add(id);
      for (const required of manifest.requires ?? []) includeImageModule(required);
    };
    // The supported acceptance image sets; arbitrary overrides need separate input review.
    for (const profile of globSync('devices/*.toml', { cwd: ctx.workspaceRoot }).sort()) {
      const sets = read(ctx.workspaceRoot, profile).patch_sets ?? {};
      for (const set of ['emulator', 'sample-tracks']) for (const id of sets[set] ?? []) includeImageModule(id);
    }
    const imageInput = { input: 'production', projects: [...imageModules].sort() };
    // Only the modules the gate packs get a pack target: patched-in core modules and static fixtures
    // (several ColdFire objects, no dkm_ symbols) cannot be packed as .dkm.
    const packable = new Set();
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
    const nodes = files.map((f) => {
      const { module: m, object: objects = [] } = read(ctx.workspaceRoot, f);
      const root = dirname(f);
      const project = { name: m.id, root, tags: ['module', `type:${m.type}`] };
      if (root.startsWith('modules/') && m.category !== 'core') project.tags.push('release-module');
      if (coreBuilds.has(m.id) && imageModules.has(m.id)) project.tags.push('acceptance-image');
      const tests = globSync('tests/**/*.test.ts', { cwd: join(ctx.workspaceRoot, root) }).filter(
        (t) => !testOwners.some(([pf]) => join(root, t).startsWith(dirname(pf) + '/')),
      );
      const hostTests = tests.filter((t) => !t.endsWith('.device.test.ts'));
      const deviceTests = tests.filter((t) => t.endsWith('.device.test.ts')).sort();
      const shipped = root.startsWith('modules/');
      const targets = {};
      const uploadable = isUploadable(root, m);
      const recs = records(join(ctx.workspaceRoot, root));
      const devices = packDevices(m);
      if (coreBuilds.has(m.id))
        targets['build-objects'] = {
          executor: 'nx:run-commands',
          cache: false,
          description:
            'Check generated sources, then incrementally build committed core objects; may update tracked outputs.',
          dependsOn: [
            'patchkit:cli',
            ...(m.requires ?? []).filter((id) => coreBuilds.has(id)).map((id) => `${id}:build-objects`),
          ],
          outputs: objects.filter((o) => o.sources?.length).map((o) => `{workspaceRoot}/${root}/${o.path}`),
          options: {
            commands: [
              `${PK} gen --modules ${m.id} --check`,
              `uv run python -m tools.devtools.benchlock run --shared --label module-build -- ${PK} build ${root}`,
            ],
            parallel: false,
            cwd: '.',
          },
        };
      if (devices.length && packable.has(m.id))
        targets.pack = {
          executor: 'nx:run-commands',
          cache: false,
          dependsOn: ['patchkit:cli'],
          outputs: devices.map((d) => `{workspaceRoot}/${dkm(d, m.id)}`),
          options: {
            commands: [
              'mkdir -p dist/modules/dt2 dist/modules/dn2',
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
      if (shipped && deviceTests.length)
        targets['test-device'] = deviceTarget(root, deviceTests, recs, imageInput, uploadable);
      // The rule is `isUploadable` in this file: the shared conformance suite loads these.
      if (uploadable)
        targets['test-conformance'] = {
          executor: 'nx:run-commands',
          cache: true,
          inputs: [...DEVICE_INPUTS, imageInput],
          dependsOn: ['pack', 'digikit/debug:pack', 'digi-control:daemons', IMAGE_BUILD],
          outputs: [],
          options: {
            command: 'pnpm --filter @digi/regression test:conformance',
            env: { DIGI_CONFORMANCE_MODULES: m.id, REGRESSION_JOBS: '2' },
            cwd: '.',
          },
        };
      if (Object.keys(targets).length) project.targets = targets;
      return [f, { projects: { [root]: project } }];
    });
    for (const [file, config] of testOwners) {
      const root = dirname(file);
      const tests = globSync('**/*.device.test.ts', { cwd: join(ctx.workspaceRoot, root) })
        .filter(
          (t) =>
            !testOwners.some(
              ([pf]) => dirname(pf).startsWith(root + '/') && join(root, t).startsWith(dirname(pf) + '/'),
            ),
        )
        .sort();
      if (!tests.length) continue;
      const recs = records(join(ctx.workspaceRoot, root));
      if (recs.some((rec) => Object.keys(rec.aliases ?? {}).length))
        throw new Error(`Test-only project ${config.name} needs explicit pack-aliases support`);
      const target = deviceTarget(root, tests, recs, imageInput);
      // This new target identity awaits input/output and cache-behavior evidence.
      target.cache = false;
      target.description =
        'Runtime acceptance; cache temporarily disabled pending input/output and invalidation/restoration evidence.';
      nodes.push([
        file,
        { projects: { [root]: { name: config.name, root, tags: config.tags, targets: { 'test-device': target } } } },
      ]);
    }
    return nodes;
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
  return all
    .filter((p) => p.tags?.includes('module') || p.tags?.includes('test-only'))
    .flatMap((p) => {
      const isModule = p.tags?.includes('module');
      const t = isModule ? read(ctx.workspaceRoot, join(p.root, 'module.toml')) : { module: {} };
      const deps = isModule ? [...(t.module.requires ?? []), ...implicit(p, t.module.type)] : [];
      for (const o of t.object ?? [])
        for (const s of o.sources ?? []) deps.push(owner(normalize(join(p.root, s))) ?? '');
      const statics = [...new Set(deps)]
        .filter((d) => d && d !== p.name)
        .map((target) => ({
          source: p.name,
          target,
          sourceFile: join(p.root, 'module.toml'),
          type: DependencyType.static,
        }));
      // Implicit edges: a module with device tests depends on the harness, the daemon and the packs;
      // only a module with test-conformance also depends on the conformance suite.
      const hasDeviceTests = !!(p.targets?.['test-device']?.options?.command || p.targets?.['test-conformance']);
      const implicitDeps = hasDeviceTests ? ['regression', 'digi-control', 'patchkit', 'devices', 'sdk'] : [];
      // Runtime acceptance records name the fixture/content modules loaded by device tests.
      // Their source changes must invalidate the consumer's device-test cache.
      for (const rec of records(join(ctx.workspaceRoot, p.root))) {
        implicitDeps.push(...(rec.modules ?? []), ...Object.values(rec.aliases ?? {}));
      }
      if (p.targets?.['test-conformance']) implicitDeps.push('conformance');
      // Projects that live inside the module directory (for example core/ui and dkui-host).
      for (const q of all)
        if (q.name !== p.name && q.root.startsWith(p.root + '/') && !q.tags?.includes('test-only'))
          implicitDeps.push(q.name);
      const implicits = [...new Set(implicitDeps)]
        .filter((d) => d !== p.name && ctx.projects[d])
        .map((target) => ({ source: p.name, target, type: DependencyType.implicit }));
      return [...statics, ...implicits];
    });
};
