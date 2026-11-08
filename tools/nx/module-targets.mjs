// Module build and acceptance target inputs and factories.
import { basename, join } from 'node:path';

// Commands run with cwd '.' (the workspace root); Nx accepts {workspaceRoot} only at the start of an option.
export const PK = 'dist/patchkit/patchkit --root .';
export const IMAGE_BUILD = { target: 'build-objects', projects: ['tag:acceptance-image'] };
// Shared compiler/contract surfaces. Optional implementation sources are named by their consumers.
export const SHARED_SDK_INPUTS = [
  '{workspaceRoot}/sdk/sdk.toml',
  '{workspaceRoot}/sdk/include/**/*',
  '{workspaceRoot}/sdk/asm/**/*',
  '{workspaceRoot}/sdk/dsp/**/*',
  '{workspaceRoot}/sdk/vectors/**/*',
];
// Build/pack producers do not read acceptance tests. Nx's implicit ^default input would
// otherwise turn a harness-only edit into a changed image output for every consumer.
// Plugin sources are not inputs: Nx hashes each project's resolved configuration, so a plugin edit reaches only targets whose definition changes.
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

// These lists hold the module-level import closure of the tools device tests spawn, plus the files in
// tools/patchkit and tools/device that those tools import from functions. Other tools/coldfire and
// tools/sharc_dev files are imported only from functions and are left out.
// Extend a list when a test spawns a new tool.
export const PYTHON_CLIENT_SOURCES = [
  'tools/__init__.py',
  'tools/control/digi_client.py',
  'tools/devtools/__init__.py',
  'tools/devtools/portable_flock.py',
  'tools/devtools/stablebin.py',
  'tools/emulator/__init__.py',
  'tools/emulator/digi_env.py',
  'tools/emulator/native_target.py',
  'tools/firmware/*.py',
  'tools/patchkit/__init__.py',
  'tools/patchkit/dsp_patch.py',
  'tools/patchkit/modroots.py',
  'tools/patchkit/patchkit.py',
].map((path) => `{workspaceRoot}/${path}`);

export const PYTHON_TOOL_SOURCES = [
  ...PYTHON_CLIENT_SOURCES,
  ...[
    'tools/coldfire/__init__.py',
    'tools/coldfire/arena_safety.py',
    'tools/coldfire/panel_script.py',
    'tools/coldfire/regions.py',
    'tools/control/__init__.py',
    'tools/device/*.py',
    'tools/devtools/benchlock.py',
    'tools/devtools/render_cache.py',
    'tools/emulator/card_check.py',
    'tools/emulator/card_overlay.py',
    'tools/emulator/coupled.py',
    'tools/emulator/dt2_sample_check.py',
    'tools/emulator/native_pgo.py',
    'tools/emulator/snapread.py',
    'tools/patchkit/hook_ab.py',
    'tools/patchkit/pk_device.py',
    'tools/patchkit/pk_digikit_*.py',
    'tools/patchkit/pk_dkrt*.py',
    'tools/patchkit/pk_dsp_budget.py',
    'tools/patchkit/pk_emud_pgo.py',
    'tools/patchkit/pk_harness.py',
    'tools/patchkit/pk_mem.py',
  ].map((path) => `{workspaceRoot}/${path}`),
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
  ...SHARED_SDK_INPUTS,
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
  { env: 'UPDATE_GOLDENS' },
  '{workspaceRoot}/packages/test-support/**/*',
  '{workspaceRoot}/packages/control-client/**/*',
  '{workspaceRoot}/packages/dk-session/**/*',
  '{workspaceRoot}/tools/nx/device-inputs.mjs',
  '{workspaceRoot}/tools/emulator/fast_regions.json',
];
// Device and conformance tasks run behind this wrapper: it takes one machine-wide emulator slot.
export const EMU_SLOT = 'node tools/devtools/emu-slot.mjs --';
export const EMU_SLOT_INPUT = '{workspaceRoot}/tools/devtools/emu-slot.mjs';
export const deviceTarget = (root, tests, recs, imageInput, importInputs, uploadable = false) => {
  const dependsOn = ['patchkit:cli', 'digi-control:daemons', IMAGE_BUILD];
  if (uploadable) dependsOn.push('pack');
  for (const rec of recs) {
    for (const id of rec.modules ?? []) dependsOn.push(`${id}:pack`);
    if (Object.keys(rec.aliases ?? {}).length) dependsOn.push('pack-aliases');
    if (rec.plusdrive) dependsOn.push('plusdrive-format:cli');
    if (rec.dksampleWasm) dependsOn.push('dksample:wasm');
  }
  return {
    executor: 'nx:run-commands',
    cache: true,
    inputs: [
      ...DEVICE_INPUTS,
      EMU_SLOT_INPUT,
      imageInput,
      ...importInputs(tests.map((test) => join(root, test))),
      ...new Set(recs.flatMap((rec) => (rec.sources ?? []).map((s) => `{workspaceRoot}/${s}`))),
      ...(recs.some((rec) => rec.pythonTools === true)
        ? PYTHON_TOOL_SOURCES
        : recs.some((rec) => rec.pythonTools === 'client')
          ? PYTHON_CLIENT_SOURCES
          : []),
    ],
    dependsOn: [...new Set(dependsOn)],
    outputs: [],
    options: {
      command: `${EMU_SLOT} pnpm --filter @digi/regression test:runtime ${tests.map((t) => `../../${root}/${t}`).join(' ')}`,
      cwd: '.',
    },
  };
};

// One cached target per test file, so an edit or a failure in one file reruns only that file.
// `name` stays the target everything selects: a no-op parent of the per-file targets.
export const deviceTargets = (name, root, tests, recs, imageInput, importInputs, uploadable = false) => {
  if (tests.length === 1) return { [name]: deviceTarget(root, tests, recs, imageInput, importInputs, uploadable) };
  const targets = {};
  for (const test of tests) {
    const child = `${name}--${basename(test, '.device.test.ts')}`;
    if (targets[child]) throw new Error(`Two device tests of ${root} map to ${child}`);
    const own = recs.filter((rec) => rec.files?.includes(test));
    const target = deviceTarget(root, [test], own.length ? own : recs, imageInput, importInputs, uploadable);
    // A sibling test file is not an input. This file's own closure comes from importInputs.
    target.inputs.push('!{projectRoot}/**/*.device.test.ts');
    targets[child] = target;
  }
  // Nx selects a target by its inputs, so the parent keeps the inputs of the whole project.
  const { inputs } = deviceTarget(root, tests, recs, imageInput, importInputs, uploadable);
  targets[name] = { executor: 'nx:noop', inputs, dependsOn: Object.keys(targets) };
  return targets;
};
