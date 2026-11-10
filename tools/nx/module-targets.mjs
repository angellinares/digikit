// Module build and acceptance target inputs and factories.
import { existsSync } from 'node:fs';
import { basename, join } from 'node:path';

// Commands run with cwd '.' (the workspace root); Nx accepts {workspaceRoot} only at the start of an option.
export const PK = 'dist/patchkit/patchkit --root .';
export const IMAGE_BUILD = { target: 'build-objects', projects: ['tag:acceptance-image'] };
// Writes the hash of every committed core object without its source hash (tools/nx/device-bytes.mjs).
export const DEVICE_BYTES = 'patchkit:device-bytes';
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

// Files that tests read by path and do not import. The import walk (test-imports.mjs) cannot see them.
// Extend this list when a test reads a new file; a file only one record's tests read goes in that record's `sources`.
// SHARED_READS are files that every device test may read and the built bytes do not carry.
const SHARED_READS = [
  // tests/regression/runtime/device-data.ts and card-cache.ts read the profiles and the firmware ports
  '{workspaceRoot}/devices/**/*.toml',
  // the manifests of the core modules: `patchkit apply` reads them with the objects
  '{workspaceRoot}/modules/core/**/module.toml',
  // upload-kit.ts and files-api.test.ts read the runtime version and the [api] table
  '{workspaceRoot}/sdk/sdk.toml',
  // synth.ts reads the machine types
  '{workspaceRoot}/modules/core/adapters/machines/types.toml',
];
const TEST_READS = [
  // fixture files
  '{workspaceRoot}/modules/core/runtime/tests/fixtures/safe-*.bin',
  '{workspaceRoot}/tests/fixtures/sharc/**/*.json',
  // the Vitest run itself
  '{workspaceRoot}/tests/regression/vitest.config.ts',
  '{workspaceRoot}/tests/regression/tsconfig.json',
  '{workspaceRoot}/tests/regression/package.json',
  '{workspaceRoot}/tools/emulator/ready_inputs.json',
];
export const READ_BY_TESTS = [...SHARED_READS, ...TEST_READS];

// Core-change policy. A change runs all device tests of the projects it edits. A device test that a change reaches
// only through shared inputs (other core modules, SDK, device profiles, patchkit, the daemon) runs only if it is
// listed in `metadata.mergeTests` of its project.json (paths relative to the project root). Every other file gets a
// narrow merge key (its own folder, test code and loaded packs), so Nx replays its last result. `mise run test:full`
// runs every file with full inputs (`test-device-all`); run it before each push or PR to main.
// Vitest runs this file before each test file (`setupFiles`), so no test imports it.
export const VITEST_SETUP = 'tests/regression/setup-temp.ts';

// Cache inputs of test-device, test-device-all and test-conformance. Two sets exist:
// - full (DEVICE_INPUTS plus the image input and every pack output): test-conformance, `test-device-all`, and the
//   merge target of a file in `metadata.mergeTests`;
// - narrow (narrowDeviceInputs): the merge target of any other file. It drops the SDK, device profiles, committed
//   core objects, the image input and the blanket pack outputs, and keeps the packs the test loads (and, for a
//   test-only project, `production` of its subject modules).
// A key covers what the run consumes:
// - the imports of the test files and of the Vitest setup file (importInputs, a closure over TypeScript sources);
// - the outputs of the pack, CLI and daemons targets (dependentTasksOutputFiles; dist/daemons.json names the
//   content-keyed daemon), and the production files of the core modules of the patched image (imageInput);
// - the module itself (`production` of the project: its sources, without tests/ and docs);
// - the files in READ_BY_TESTS, the SDK, config/emulator.json (sets SHARC_* env) and each record's `sources`;
// - the firmware and ready dirs via device-inputs.mjs, and the env switches below.
// A package or a harness file that no test imports is not an input.
// Inputs of both sets.
const DEVICE_COMMON = [
  'jsToolchain',
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
  '{workspaceRoot}/config/emulator.json',
];
// Full set: what the run consumes is the built bytes (the image hashes, the packs, the tool binaries), not the sources
// they are made from. A source edit that leaves these bytes alone changes no key.
export const BUILT_OUTPUTS = [
  'dist/bytes/core-objects.sha256',
  'dist/modules/*/*.dkm',
  'dist/patchkit/patchkit',
  'dist/daemons.json',
  'dist/plusdrive/plusdrive',
  'apps/web/public/dksample.wasm',
].map((glob) => ({ dependentTasksOutputFiles: glob, transitive: true }));
export const DEVICE_INPUTS = [...READ_BY_TESTS, ...BUILT_OUTPUTS, ...DEVICE_COMMON];
// Tests that run `patchkit build` or `pack` at run time, or patch with explicit fixture modules, compile sources:
// they keep the SDK and the toolchain as inputs (record flag `compiles`, `patchModules`, or any `pythonTools`: the Python devtools may build or pack).
export const COMPILE_INPUTS = [...SHARED_SDK_INPUTS, '{workspaceRoot}/tools/patchkit/toolchain/**/*'];
// Device and conformance tasks run behind this wrapper: it takes one machine-wide emulator slot. It only schedules and is
// no input: it cannot change a result, and Nx does not cache failures.
export const EMU_SLOT = 'node tools/devtools/emu-slot.mjs --';
// Directories of a module project that its objects are built from (see the pack and image outputs).
export const builtSourceExclusions = (root, base = '.') =>
  ['src', 'include', 'fit', 'sdk']
    .filter((dir) => existsSync(join(base, root, dir)) && !root.includes('/tests'))
    .map((dir) => `!{projectRoot}/${dir}/**/*`);
// `opts`: `narrow` selects the narrow input set; `moduleId` is the project's module id (its pack is loaded when
// `uploadable`); `subjects` are the projects a test-only project tests.
export const deviceTarget = (root, tests, recs, imageInput, importInputs, uploadable = false, opts = {}) => {
  const dependsOn = ['patchkit:cli', 'digi-control:daemons', IMAGE_BUILD, DEVICE_BYTES];
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
      ...(opts.narrow
        ? [
            ...DEVICE_COMMON,
            ...TEST_READS,
            // the packs the test loads, not every pack output
            // (the own module, the records' modules and their alias packs)
            ...[
              ...new Set(
                [
                  ...(uploadable && opts.moduleId ? [opts.moduleId] : []),
                  ...recs.flatMap((r) => [...(r.modules ?? []), ...Object.keys(r.aliases ?? {})]),
                ].map((id) => id.split('/').at(-1)),
              ),
            ].map((leaf) => ({
              dependentTasksOutputFiles: `dist/modules/*/${leaf}.dkm`,
              transitive: true,
            })),
            // tools the test runs directly
            ...(recs.some((r) => r.plusdrive)
              ? [{ dependentTasksOutputFiles: 'dist/plusdrive/plusdrive', transitive: true }]
              : []),
            ...(recs.some((r) => r.dksampleWasm)
              ? [{ dependentTasksOutputFiles: 'apps/web/public/dksample.wasm', transitive: true }]
              : []),
            // an edit of the subject module reruns a test-only project
            ...(opts.subjects?.length ? [{ input: 'production', projects: opts.subjects }] : []),
            // fixture image modules that a record names are chosen by the test itself
            ...(recs.some((r) => r.patchModules?.length)
              ? [{ input: 'production', projects: [...new Set(recs.flatMap((r) => r.patchModules ?? []))] }]
              : []),
          ]
        : [
            ...DEVICE_INPUTS,
            // an explicit fixture image is built from the sources of its modules at run time
            ...(imageInput ? [imageInput] : []),
          ]),
      // the module itself (its sources, without tests/ and docs). Full set: the sources that become objects reach the
      // run through the pack outputs and the image hashes.
      ...(opts.narrow ? ['production'] : ['production', ...builtSourceExclusions(root, opts.base)]),
      ...importInputs([...tests.map((test) => join(root, test)), VITEST_SETUP]),
      ...(!opts.narrow && recs.some((rec) => rec.compiles || rec.patchModules?.length || rec.pythonTools)
        ? COMPILE_INPUTS
        : []),
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
// With `opts.twin`, `name` is the merge target (narrow keys for files outside `opts.mergeTests`) and `${name}-all`
// runs every file with full inputs.
// Each run target is split in two halves, `<name>--dt2` and `<name>--dn2` (env DIGI_DEVICE), so they run in
// parallel. `<name>` stays as a no-op parent with the inputs of the run target.
export const deviceTargets = (name, root, tests, recs, imageInput, importInputs, uploadable = false, opts = {}) => {
  const targets = fileTargets(name, root, tests, recs, imageInput, importInputs, uploadable, opts);
  const split = {};
  for (const [key, target] of Object.entries(targets)) {
    if (!target.options?.command) {
      split[key] = target;
      continue;
    }
    const halves = ['dt2', 'dn2'].map((device) => `${key}--${device}`);
    split[key] = { executor: 'nx:noop', inputs: target.inputs, dependsOn: halves };
    halves.forEach((half, i) => {
      split[half] = {
        ...target,
        inputs: [...target.inputs, { env: 'DIGI_DEVICE' }],
        options: { ...target.options, env: { ...target.options.env, DIGI_DEVICE: ['dt2', 'dn2'][i] } },
      };
    });
  }
  return split;
};

const fileTargets = (name, root, tests, recs, imageInput, importInputs, uploadable = false, opts = {}) => {
  const allName = `${name}-all`;
  const merge = opts.mergeTests ?? [];
  const isMerge = (test) => !opts.twin || merge.includes(test);
  const build = (list, narrow, recList = recs) =>
    deviceTarget(root, list, recList, imageInput, importInputs, uploadable, { ...opts, narrow });
  if (tests.length === 1) {
    const [test] = tests;
    const targets = { [name]: build(tests, !isMerge(test)) };
    if (opts.twin)
      targets[allName] = isMerge(test)
        ? { executor: 'nx:noop', inputs: targets[name].inputs, dependsOn: [name] }
        : build(tests, false);
    return targets;
  }
  const targets = {};
  const all = {};
  for (const test of tests) {
    const suffix = basename(test, '.device.test.ts');
    const child = `${name}--${suffix}`;
    if (targets[child]) throw new Error(`Two device tests of ${root} map to ${child}`);
    const own = recs.filter((rec) => rec.files?.includes(test));
    const narrow = !isMerge(test);
    // A sibling test file is not an input. This file's own closure comes from importInputs.
    const make = (isNarrow) => {
      const target = build([test], isNarrow, own.length ? own : recs);
      target.inputs.push('!{projectRoot}/**/*.device.test.ts');
      return target;
    };
    targets[child] = make(narrow);
    if (opts.twin) all[narrow ? `${allName}--${suffix}` : child] = narrow ? make(false) : child;
  }
  // Nx selects a target by its inputs, so the parents keep the inputs of the whole project.
  const { inputs } = build(tests, false);
  const children = Object.keys(targets);
  if (opts.twin) {
    for (const [key, value] of Object.entries(all)) if (typeof value !== 'string') targets[key] = value;
    targets[allName] = { executor: 'nx:noop', inputs, dependsOn: Object.keys(all) };
  }
  targets[name] = { executor: 'nx:noop', inputs, dependsOn: children };
  return targets;
};
