// Build and pack: the patchkit command, the build input lists and the build-objects, pack and pack-aliases targets.
// It does not hold device-test cache keys (device-test-keys.mjs) or the shape of test targets (device-tests.mjs).
import { dkm, dkmMap, manifestClosure, manifestArgs, packDevices } from './module-manifests.mjs';
import { packSourceInputs } from './pack-inputs.mjs';

// Commands run with cwd '.' (the workspace root); Nx accepts {workspaceRoot} only at the start of an option.
export const PK = 'dist/patchkit/patchkit --root .';
// Shared compiler/contract surfaces. Optional implementation sources are named by their consumers.
export const SHARED_SDK_INPUTS = [
  '{workspaceRoot}/sdk/sdk.toml',
  '{workspaceRoot}/sdk/include/**/*',
  '{workspaceRoot}/sdk/dsp/contract/**/*',
  '{workspaceRoot}/sdk/dsp/**/*',
  '{workspaceRoot}/modules/core/runtime/vectors/**/*',
];
// Build/pack producers do not read acceptance tests. Nx's implicit ^default input would
// otherwise turn a harness-only edit into a changed image output for every consumer.
// Plugin sources are not inputs: Nx hashes each project's resolved configuration, so a plugin edit reaches only targets whose definition changes.
// A pack or object build reads the SDK headers its sources include, not every header: packSourceInputs lists those.
const SDK_INCLUDE_GLOB = '{workspaceRoot}/sdk/include/**/*';
export const BUILD_INPUTS = [
  'production',
  '^production',
  'jsToolchain',
  { dependentTasksOutputFiles: '**/*', transitive: true },
  // gen reads the library manifests and device facts; builders read shared SDK headers and pins.
  '{workspaceRoot}/modules/**/module.toml',
  '{workspaceRoot}/tests/fixtures/modules/**/module.toml',
  // pack and build-objects run the Rust patchkit binary, which reads devices/*.toml and the toolchain pins;
  // pk_toolchains.py pins the m68k compiler; there is no Python on this path.
  '{workspaceRoot}/devices/*.toml',
  ...SHARED_SDK_INPUTS,
  '{workspaceRoot}/tools/patchkit/toolchain/**/*',
  '{workspaceRoot}/tools/patchkit/pk_toolchains.py',
  '{workspaceRoot}/Cargo.toml',
  '{workspaceRoot}/Cargo.lock',
  '{workspaceRoot}/.cargo/**/*',
  '{workspaceRoot}/rust-toolchain*',
  '{workspaceRoot}/mise.toml',
];

// These commands receive an explicit library; shared devices, SDK and tool inputs stay conservative.
export const ALL_MANIFESTS = new Set([
  '{workspaceRoot}/modules/**/module.toml',
  '{workspaceRoot}/tests/fixtures/modules/**/module.toml',
]);
export const scopedManifestInputs = (inputs, manifests) => [
  ...inputs.filter((input) => !ALL_MANIFESTS.has(input)),
  ...manifests.map((file) => `{workspaceRoot}/${file}`),
];

// Core generation reads the allocation providers and declarations of the complete core library.
// Build inputs follow manifest/source ownership, never the module's acceptance relationships.
export const packInputs = (workspaceRoot, byId, manifests) => [
  ...scopedManifestInputs(
    BUILD_INPUTS.filter((input) => input !== '^production' && input !== SDK_INCLUDE_GLOB),
    manifests,
  ),
  ...packSourceInputs(workspaceRoot, byId, manifests),
];

// Like pack, build-objects reads the sources of the module's manifest closure (packSourceInputs) and the patchkit binary it
// runs, not the production files of every graph dependency (`^production` reached the test harness and its packages).
export const buildObjectsTarget = (workspaceRoot, byId, m, root, objects, coreBuilds, coreManifests) => ({
  executor: 'nx:run-commands',
  cache: false,
  inputs: [
    ...scopedManifestInputs(
      BUILD_INPUTS.filter((input) => input !== '^production' && input !== SDK_INCLUDE_GLOB),
      coreManifests,
    ),
    ...packSourceInputs(workspaceRoot, byId, manifestClosure(byId, [m.id])),
    // The build is not cached: a patchkit source edit must still run it (a changed generator changes the objects).
    { input: 'production', projects: ['patchkit'] },
  ],
  description: 'Check generated sources, then incrementally build committed core objects; may update tracked outputs.',
  dependsOn: [
    'sdk:check-generated',
    'patchkit:cli',
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
});

export const packTarget = (workspaceRoot, byId, m, devices) => {
  const manifests = manifestClosure(byId, [m.id]);
  return {
    executor: 'nx:run-commands',
    cache: false,
    inputs: packInputs(workspaceRoot, byId, manifests),
    dependsOn: ['patchkit:cli'],
    outputs: devices.flatMap((d) => [`{workspaceRoot}/${dkm(d, m.id)}`, `{workspaceRoot}/${dkmMap(d, m.id)}`]),
    options: {
      commands: [
        'mkdir -p dist/modules/dt2 dist/modules/dn2',
        ...devices.map(
          (d) =>
            `${PK} pack ${m.id} ${manifestArgs(manifests)} --device ${d} --addrmap ${dkmMap(d, m.id)} -o ${dkm(d, m.id)}`,
        ),
      ],
      parallel: false,
      cwd: '.',
    },
  };
};

// Returns undefined when no record names an alias with a known source module.
export const packAliasesTarget = (workspaceRoot, byId, recs) => {
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
  if (!aliasCmds.length) return undefined;
  return {
    executor: 'nx:run-commands',
    cache: false,
    inputs: packInputs(workspaceRoot, byId, manifestClosure(byId, [...aliasSources])),
    dependsOn: ['patchkit:cli', ...[...aliasSources].map((s) => `${s}:pack`)],
    outputs: aliasOutputs,
    options: {
      commands: ['mkdir -p dist/modules/dt2 dist/modules/dn2', ...aliasCmds],
      parallel: false,
      cwd: '.',
    },
  };
};
