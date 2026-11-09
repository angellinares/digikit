// Nx plugin: one project per module.toml. Edges: [module].requires plus [[object]].sources paths
// that resolve into another module directory. tools/devtools/nx_graph.py reads them from the graph.
// Targets: pack (build stale objects and pack to dist/modules/<dev>/<leaf>.dkm), pack-aliases (renamed copies named by
// metadata.runtimeAcceptance), test-host, test-device and test-conformance (vitest directly, after the packs).
import { globSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { read, records, packDevices, isUploadable, dkm, manifestClosure, manifestArgs } from './module-manifests.mjs';
import {
  PK,
  IMAGE_BUILD,
  BUILD_INPUTS,
  DEVICE_INPUTS,
  EMU_SLOT,
  EMU_SLOT_INPUT,
  VITEST_SETUP,
  deviceTargets,
  scopedManifestInputs,
} from './module-targets.mjs';
import { packSourceInputs } from './pack-inputs.mjs';
import { testImportInputs } from './test-imports.mjs';

export { createDependencies } from './module-dependencies.mjs';

export const createNodesV2 = [
  '{modules,tests/fixtures/modules,tests/regression}/**/{module.toml,project.json}',
  (matched, _opts, ctx) => {
    // Lazy for manifests without device targets (including small plugin fixtures).
    let imports;
    const importInputs = (files) => (imports ??= testImportInputs(ctx.workspaceRoot))(files);
    const files = matched.filter((f) => f.endsWith('/module.toml'));
    const testOwners = matched
      .filter((f) => f.endsWith('/project.json'))
      .map((f) => [f, JSON.parse(readFileSync(join(ctx.workspaceRoot, f), 'utf8'))])
      .filter(([, p]) => p.tags?.includes('test-only'));
    // id -> manifest, so an alias can find the devices of its source module.
    const byId = new Map(
      files.map((f) => {
        const parsed = read(ctx.workspaceRoot, f);
        return [parsed.module.id, { file: f, manifest: parsed.module, objects: parsed.object ?? [] }];
      }),
    );
    // Core generation reads the allocation providers and declarations of the complete core library.
    // Build inputs follow manifest/source ownership, never the module's acceptance relationships.
    const packInputs = (manifests) => [
      ...scopedManifestInputs(
        BUILD_INPUTS.filter((input) => input !== '^production'),
        manifests,
      ),
      ...packSourceInputs(ctx.workspaceRoot, byId, manifests),
    ];
    const coreManifests = [...byId.values()]
      .filter(({ manifest }) => manifest.category === 'core')
      .map(({ file }) => file)
      .sort();
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
      for (const set of ['emulator', 'sample-tracks', 'overdrive-tracks'])
        for (const id of sets[set] ?? []) includeImageModule(id);
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
        (t) =>
          !t.endsWith('.manual.device.test.ts') &&
          !testOwners.some(([pf]) => join(root, t).startsWith(dirname(pf) + '/')),
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
          inputs: scopedManifestInputs(BUILD_INPUTS, coreManifests),
          description:
            'Check generated sources, then incrementally build committed core objects; may update tracked outputs.',
          dependsOn: [
            'sdk:check-generated',
            ...(m.requires ?? []).filter((id) => coreBuilds.has(id)).map((id) => `${id}:build-objects`),
          ],
          outputs: objects.filter((o) => o.sources?.length).map((o) => `{workspaceRoot}/${root}/${o.path}`),
          options: {
            commands: [
              `${PK} gen --modules ${m.id} ${manifestArgs(coreManifests)} --check --scope modules`,
              `${PK} build ${root}`,
            ],
            parallel: false,
            cwd: '.',
          },
        };
      if (devices.length && packable.has(m.id)) {
        const manifests = manifestClosure(byId, [m.id]);
        targets.pack = {
          executor: 'nx:run-commands',
          cache: false,
          inputs: packInputs(manifests),
          dependsOn: ['patchkit:cli'],
          outputs: devices.map((d) => `{workspaceRoot}/${dkm(d, m.id)}`),
          options: {
            commands: [
              'mkdir -p dist/modules/dt2 dist/modules/dn2',
              ...devices.map((d) => `${PK} pack ${m.id} ${manifestArgs(manifests)} --device ${d} -o ${dkm(d, m.id)}`),
            ],
            parallel: false,
            cwd: '.',
          },
        };
      }
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
            aliasCmds.push(
              `${PK} pack ${source} ${manifestArgs(manifestClosure(byId, [source]))} --device ${d} --name ${alias} -o ${dkm(d, alias)}`,
            );
            aliasOutputs.push(`{workspaceRoot}/${dkm(d, alias)}`);
          }
        }
      if (aliasCmds.length)
        targets['pack-aliases'] = {
          executor: 'nx:run-commands',
          cache: false,
          inputs: packInputs(manifestClosure(byId, [...aliasSources])),
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
        Object.assign(
          targets,
          deviceTargets('test-device', root, deviceTests, recs, imageInput, importInputs, uploadable),
        );
      // The rule is `isUploadable` in this file: the shared conformance suite loads these.
      if (uploadable)
        targets['test-conformance'] = {
          executor: 'nx:run-commands',
          cache: true,
          inputs: [
            ...DEVICE_INPUTS,
            'production',
            EMU_SLOT_INPUT,
            imageInput,
            ...importInputs([
              ...globSync('tests/conformance/**/*.device.test.ts', { cwd: ctx.workspaceRoot }),
              VITEST_SETUP,
            ]),
          ],
          dependsOn: ['pack', 'digikit/debug:pack', 'digi-control:daemons', IMAGE_BUILD],
          outputs: [],
          options: {
            command: `${EMU_SLOT} pnpm --filter @digi/regression test:conformance`,
            env: { DIGI_CONFORMANCE_MODULES: m.id, DIGI_CONFORMANCE_MANIFEST: f },
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
            !t.endsWith('.manual.device.test.ts') &&
            !testOwners.some(
              ([pf]) => dirname(pf).startsWith(root + '/') && join(root, t).startsWith(dirname(pf) + '/'),
            ),
        )
        .sort();
      if (!tests.length) continue;
      const recs = records(join(ctx.workspaceRoot, root));
      if (recs.some((rec) => Object.keys(rec.aliases ?? {}).length))
        throw new Error(`Test-only project ${config.name} needs explicit pack-aliases support`);
      // Explicit fixture images build only their manifest requirements; ordinary owners keep the default image.
      const patchRoots = [...new Set(recs.flatMap((rec) => rec.patchModules ?? []))];
      const patchIds = patchRoots.length
        ? manifestClosure(byId, patchRoots).map((file) => read(ctx.workspaceRoot, file).module.id)
        : undefined;
      const targetName = config.tags.includes('tier:full') ? 'test-device-full' : 'test-device';
      const targets = deviceTargets(
        targetName,
        root,
        tests,
        recs,
        patchIds ? { input: 'production', projects: patchIds } : imageInput,
        importInputs,
      );
      for (const target of Object.values(targets)) {
        if (!target.options) continue; // the no-op parent
        if (patchIds)
          target.dependsOn = target.dependsOn.flatMap((dependency) =>
            dependency === IMAGE_BUILD
              ? patchIds.filter((id) => coreBuilds.has(id)).map((id) => `${id}:build-objects`)
              : [dependency],
          );
        if (targetName === 'test-device-full') target.options.env = { DIGIKIT_TIER: 'full' };
      }
      nodes.push([file, { projects: { [root]: { name: config.name, root, tags: config.tags, targets } } }]);
    }
    if (matched.includes('tests/regression/project.json'))
      nodes.push([
        'tests/regression/project.json',
        {
          projects: {
            'tests/regression': {
              targets: {
                'test-device': {
                  inputs: [
                    ...DEVICE_INPUTS,
                    imageInput,
                    '{projectRoot}/project.json',
                    ...importInputs([
                      ...globSync('tests/regression/emulator/**/*.test.ts', { cwd: ctx.workspaceRoot }),
                      VITEST_SETUP,
                    ]),
                  ],
                },
              },
            },
          },
        },
      ]);
    return nodes;
  },
];
