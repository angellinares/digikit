// Module build and acceptance target inputs and factories.
import { join } from 'node:path';

// Commands run with cwd '.' (the workspace root); Nx accepts {workspaceRoot} only at the start of an option.
export const PK = 'dist/patchkit/patchkit --root .';
export const IMAGE_BUILD = { target: 'build-objects', projects: ['tag:acceptance-image'] };
// Build/pack producers do not read acceptance tests. Nx's implicit ^default input would
// otherwise turn a harness-only edit into a changed image output for every consumer.
export const BUILD_INPUTS = [
  'production',
  '^production',
  'jsToolchain',
  { dependentTasksOutputFiles: '**/*', transitive: true },
  '{workspaceRoot}/tools/nx/modules.mjs',
  '{workspaceRoot}/tools/nx/module-dependencies.mjs',
  '{workspaceRoot}/tools/nx/module-manifests.mjs',
  '{workspaceRoot}/tools/nx/module-targets.mjs',
  // gen reads the library manifests and device facts; builders read shared SDK headers and pins.
  '{workspaceRoot}/modules/**/module.toml',
  '{workspaceRoot}/tests/fixtures/modules/**/module.toml',
  '{workspaceRoot}/devices/**/*',
  '{workspaceRoot}/sdk/**/*',
  '{workspaceRoot}/tools/patchkit/**/*',
  '{workspaceRoot}/Cargo.toml',
  '{workspaceRoot}/Cargo.lock',
  '{workspaceRoot}/.cargo/**/*',
  '{workspaceRoot}/rust-toolchain*',
  '{workspaceRoot}/mise.toml',
  '{workspaceRoot}/pyproject.toml',
  '{workspaceRoot}/uv.lock',
  '{workspaceRoot}/tools/__init__.py',
  '{workspaceRoot}/tools/devtools/benchlock.py',
  '{workspaceRoot}/tools/devtools/portable_flock.py',
];

// These commands receive an explicit library; shared devices, SDK and tool inputs stay conservative.
const ALL_MANIFESTS = new Set([
  '{workspaceRoot}/modules/**/module.toml',
  '{workspaceRoot}/tests/fixtures/modules/**/module.toml',
]);
export const scopedManifestInputs = (inputs, manifests) => [
  ...inputs.filter((input) => !ALL_MANIFESTS.has(input)),
  ...manifests.map((file) => `{workspaceRoot}/${file}`),
];

// Cache inputs of test-device and test-conformance: the module and its graph deps (core modules, harness via
// regression/conformance, daemon and patchkit crate sources) with their `production` files (sources without
// tests/ and docs; regression's runtime helpers come from each suite's import closure), the outputs of
// the pack, CLI and daemons targets (dist/daemons.json names the content-keyed daemon), and the firmware and
// ready dirs via device-inputs.mjs.
// Image core sources/objects are explicit production inputs, without acceptance-fixture graph edges.
// Also named explicitly: the harness packages (test-support, control-client, dk-session) and
// tools/emulator/fast_regions.json (sets SHARC_* env). Manifests follow production and declared
// acceptance module dependencies; conformance also names its scoped selected manifest.
export const DEVICE_INPUTS = [
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
  '{workspaceRoot}/tools/nx/device-inputs.mjs',
  '{workspaceRoot}/tools/nx/modules.mjs',
  '{workspaceRoot}/tools/nx/module-dependencies.mjs',
  '{workspaceRoot}/tools/nx/module-manifests.mjs',
  '{workspaceRoot}/tools/nx/module-targets.mjs',
  '{workspaceRoot}/tools/nx/test-imports.mjs',
  '{workspaceRoot}/tools/emulator/fast_regions.json',
];
export const deviceTarget = (root, tests, recs, imageInput, importInputs, uploadable = false) => {
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
      ...importInputs(tests.map((test) => join(root, test))),
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
