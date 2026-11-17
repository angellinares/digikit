// Nx plugin: one project per module.toml. Edges: [module].requires plus [[object]].sources paths
// that resolve into another module directory. tools/devtools/nx_graph.py reads them from the graph.
// Targets: pack (build stale objects and pack to dist/modules/<dev>/<leaf>.dkm), pack-aliases (renamed copies named by
// metadata.runtimeAcceptance), test-host, test-device and test-conformance (vitest directly, after the packs).
// This file wires the plugin. Build and pack targets are in module-build.mjs, device-test cache keys in
// device-test-keys.mjs, and the shape of device, conformance and test-only targets in device-tests.mjs.
import { globSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { read, records, mergeTests, packDevices, isUploadable } from './module-manifests.mjs';
import { buildObjectsTarget, packTarget, packAliasesTarget, bindTarget } from './module-build.mjs';
import { deviceTargets, checkMergeTests, conformanceTarget, testOnlyNodes, regressionNode } from './device-tests.mjs';
import { serviceLeaves, isBindable, bindableIds } from './device-test-keys.mjs';
import { checkAcceptanceTags } from './acceptance-tags.mjs';
import { rigTarget } from './rig-tests.mjs';
import { testImportInputs } from './ts-imports.mjs';
import { pythonImportInputs } from './py-imports.mjs';

export { createDependencies } from './module-dependencies.mjs';

export const createNodesV2 = [
  '{modules,tests/fixtures/modules,tests/regression}/**/{module.toml,project.json}',
  (matched, _opts, ctx) => {
    // Lazy for manifests without device targets (including small plugin fixtures).
    let imports;
    const importInputs = (files) => (imports ??= testImportInputs(ctx.workspaceRoot))(files);
    let py;
    const pythonInputs = (seeds) => (py ??= pythonImportInputs(ctx.workspaceRoot))(seeds);
    const files = matched.filter((f) => f.endsWith('/module.toml'));
    const testOwners = matched
      .filter((f) => f.endsWith('/project.json'))
      .map((f) => [f, JSON.parse(readFileSync(join(ctx.workspaceRoot, f), 'utf8'))])
      .filter(([, p]) => p.tags?.includes('test-only'));
    // id -> manifest, so an alias can find the devices of its source module.
    const byId = new Map(
      files.map((f) => {
        const parsed = read(ctx.workspaceRoot, f);
        return [
          parsed.module.id,
          { file: f, manifest: parsed.module, objects: parsed.object ?? [], hooks: parsed.hook ?? [] },
        ];
      }),
    );
    // id -> { category, root }, for the module tags of metadata.runtimeAcceptance (acceptance-tags.mjs).
    const moduleInfo = new Map(
      [...byId].map(([id, { file, manifest }]) => [id, { category: manifest.category, root: dirname(file) }]),
    );
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
    // The patch sets of the device profiles; arbitrary overrides need separate input review.
    for (const profile of globSync('devices/*.toml', { cwd: ctx.workspaceRoot }).sort()) {
      const sets = read(ctx.workspaceRoot, profile).patch_sets ?? {};
      // every set: tools/nx/device-bytes.mjs hashes each one, so their objects are built first
      for (const ids of Object.values(sets)) for (const id of ids) includeImageModule(id);
    }
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
      checkAcceptanceTags(dir, records(join(ctx.workspaceRoot, dir)), [], moduleInfo);
      for (const rec of records(join(ctx.workspaceRoot, dir))) {
        for (const id of rec.modules ?? []) packable.add(id);
        for (const source of Object.values(rec.aliases ?? {})) packable.add(source);
      }
    }
    const nodes = files.map((f) => {
      const { module: m, object: objects = [], hook: hooks = [] } = read(ctx.workspaceRoot, f);
      const root = dirname(f);
      const project = { name: m.id, root, tags: ['module', `type:${m.type}`] };
      if (root.startsWith('modules/') && m.category !== 'core') project.tags.push('release-module');
      if (coreBuilds.has(m.id) && imageModules.has(m.id)) project.tags.push('acceptance-image');
      const tests = globSync('tests/**/*.test.ts', { cwd: join(ctx.workspaceRoot, root) }).filter(
        (t) =>
          !t.endsWith('.manual.device.test.ts') &&
          !testOwners.some(([pf]) => join(root, t).startsWith(dirname(pf) + '/')),
      );
      const hostTests = tests.filter((t) => !t.endsWith('.device.test.ts') && !t.endsWith('.rig.test.ts'));
      const rigTests = tests.filter((t) => t.endsWith('.rig.test.ts'));
      const deviceTests = tests.filter((t) => t.endsWith('.device.test.ts')).sort();
      const shipped = root.startsWith('modules/');
      const targets = {};
      const uploadable = isUploadable(root, m);
      const recs = records(join(ctx.workspaceRoot, root));
      const devices = packDevices(m);
      if (coreBuilds.has(m.id))
        targets['build-objects'] = buildObjectsTarget(
          ctx.workspaceRoot,
          byId,
          m,
          root,
          objects,
          coreBuilds,
          coreManifests,
        );
      if (devices.length && packable.has(m.id)) targets.pack = packTarget(ctx.workspaceRoot, byId, m, devices);
      if (devices.length && packable.has(m.id) && isBindable(m, hooks))
        targets.bind = bindTarget(ctx.workspaceRoot, m, devices);
      const aliasesTarget = packAliasesTarget(ctx.workspaceRoot, byId, recs);
      if (aliasesTarget) targets['pack-aliases'] = aliasesTarget;
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
      checkMergeTests(ctx.workspaceRoot, root, deviceTests, false);
      checkAcceptanceTags(root, recs, deviceTests, moduleInfo);
      if (shipped && deviceTests.length)
        Object.assign(
          targets,
          deviceTargets('test-device', root, deviceTests, recs, null, importInputs, pythonInputs, uploadable, {
            twin: true,
            moduleId: m.id,
            services: serviceLeaves(byId),
            bindable: bindableIds(byId),
            projectDevices: devices,
            mergeTests: mergeTests(join(ctx.workspaceRoot, root)),
            base: ctx.workspaceRoot,
          }),
        );
      // sound checks on the SHARC-only rig (packages/sharc-rig-kit, docs/TESTING.md "Sound checks"): tests/rig/*.toml run by tests/*.rig.test.ts
      if (rigTests.length) targets['test-rig'] = rigTarget(root);
      if (uploadable) targets['test-conformance'] = conformanceTarget(ctx.workspaceRoot, root, m, f, importInputs);
      if (Object.keys(targets).length) project.targets = targets;
      return [f, { projects: { [root]: project } }];
    });
    testOnlyNodes(ctx.workspaceRoot, testOwners, byId, moduleInfo, coreBuilds, importInputs, pythonInputs, nodes);
    if (matched.includes('tests/regression/project.json')) nodes.push(regressionNode(ctx.workspaceRoot, importInputs));
    return nodes;
  },
];
