// The shape of device-test targets: dependsOn, command and env, the dt2/dn2 halves and the per-file targets, and the
// conformance, test-only and regression nodes. The cache keys are in device-test-keys.mjs; build and pack targets are in module-build.mjs.
import { globSync } from 'node:fs';
import { basename, dirname, join } from 'node:path';
import { checkAcceptanceTags } from './acceptance-tags.mjs';
import { mergeTests, manifestClosure, read, records } from './module-manifests.mjs';
import {
  DEVICE_INPUTS,
  VITEST_SETUP,
  builtSourceExclusions,
  deviceInputs,
  serviceLeaves,
} from './device-test-keys.mjs';

export const IMAGE_BUILD = { target: 'build-objects', projects: ['tag:acceptance-image'] };
// Writes the hash of every committed core object without its source hash (tools/nx/device-bytes.mjs).
export const DEVICE_BYTES = 'patchkit:device-bytes';
// Writes the hash of the image, plan and address map that `patchkit apply` makes for each acceptance firmware and patch set
// (tools/nx/device-images.mjs).
export const DEVICE_IMAGES = 'patchkit:device-images';
// Writes the `needs` verdict of the packs in tests/regression/runtime/needs-packs.json (tools/nx/pack-verdicts.mjs).
export const PACK_VERDICTS = 'patchkit:pack-verdicts';
// The host assembly tests of the runtime (modules/core/runtime): the seams between the real packed outputs. No device task starts while they fail.
export const TEST_ASSEMBLY = 'core/runtime:test-assembly';
// Device and conformance tasks run behind this wrapper: it takes one machine-wide emulator slot. It only schedules and is
// no input: it cannot change a result, and Nx does not cache failures.
export const EMU_SLOT = 'node tools/devtools/emu-slot.mjs --';

// `opts`: see deviceInputs (device-test-keys.mjs).
export const deviceTarget = (
  root,
  tests,
  recs,
  imageInput,
  importInputs,
  pythonInputs,
  uploadable = false,
  opts = {},
) => {
  const dependsOn = [
    'patchkit:cli',
    'digi-control:daemons',
    IMAGE_BUILD,
    DEVICE_BYTES,
    DEVICE_IMAGES,
    PACK_VERDICTS,
    TEST_ASSEMBLY,
  ];
  if (uploadable) dependsOn.push('pack');
  for (const rec of recs) {
    for (const id of rec.modules ?? []) {
      dependsOn.push(`${id}:pack`);
      if (opts.bindable?.has(id)) dependsOn.push(`${id}:bind`); // a service with hooks: the bound file the tests copy to the card
    }
    if (Object.keys(rec.aliases ?? {}).length) dependsOn.push('pack-aliases');
    if (rec.plusdrive) dependsOn.push('plusdrive-format:cli');
    if (rec.dksampleWasm) dependsOn.push('dksample:wasm');
    if (rec.dkprofWasm) dependsOn.push('dkprof:wasm');
  }
  const { inputs, patchkitLive } = deviceInputs(
    root,
    tests,
    recs,
    imageInput,
    importInputs,
    pythonInputs,
    uploadable,
    opts,
  );
  return {
    executor: 'nx:run-commands',
    cache: true,
    inputs,
    dependsOn: [...new Set(dependsOn)],
    outputs: [],
    options: {
      command: `${EMU_SLOT} pnpm --filter @digi/regression test:runtime ${tests.map((t) => `../../${root}/${t}`).join(' ')}`,
      cwd: '.',
      // the key holds the patchkit binary (a full key, or a live closure): runtime/patchkit-exec.ts runs patchkit only then
      env: {
        DIGI_PATCHKIT_LIVE: (!opts.narrow && !opts.merge) || patchkitLive ? '1' : '',
        // the `-all` twins run the full tier: `full: true` tests (tests/regression/runtime/devices.ts FULL_TIER) run too
        ...(opts.fullTier ? { DIGIKIT_TIER: 'full' } : {}),
      },
    },
  };
};

// One cached target per test file, so an edit or a failure in one file reruns only that file.
// `name` stays the target everything selects: a no-op parent of the per-file targets.
// With `opts.twin`, `name` is the merge target (narrow keys for files outside `opts.mergeTests`) and `${name}-all`
// runs every file with full inputs and DIGIKIT_TIER=full (the gate's full tier).
// Each run target is split in halves, `<name>--dt2` and `<name>--dn2` (env DIGI_DEVICE), so they run in
// parallel. A file gets only the halves of the devices it runs on (`fileDevices`): a half of another device would queue for
// an emulator slot and run nothing. `<name>` stays as a no-op parent with the inputs of the run target.
export const deviceTargets = (
  name,
  root,
  tests,
  recs,
  imageInput,
  importInputs,
  pythonInputs,
  uploadable = false,
  opts = {},
) => {
  const targets = fileTargets(name, root, tests, recs, imageInput, importInputs, pythonInputs, uploadable, opts);
  const devicesOf = (key) => {
    // a run target of one file: the file is the last path in its command
    const files = [...(targets[key].options.command.match(/\S+\.device\.test\.ts/g) ?? [])].map((f) =>
      f.slice(`../../${root}/`.length),
    );
    return unionDevices(files.map((file) => fileDevices(file, recs, opts.projectDevices)));
  };
  const split = {};
  for (const [key, target] of Object.entries(targets)) {
    if (!target.options?.command) {
      split[key] = target;
      continue;
    }
    const devices = devicesOf(key);
    const halves = devices.map((device) => `${key}--${device}`);
    split[key] = { executor: 'nx:noop', inputs: target.inputs, dependsOn: halves };
    devices.forEach((device, i) => {
      split[halves[i]] = {
        ...target,
        inputs: [...target.inputs, { env: 'DIGI_DEVICE' }],
        options: { ...target.options, env: { ...target.options.env, DIGI_DEVICE: device } },
      };
    });
  }
  return split;
};

const ALL_DEVICES = ['dt2', 'dn2'];
const unionDevices = (lists) => ALL_DEVICES.filter((d) => lists.some((list) => list.includes(d)));

// The devices a device test file runs on (docs/TESTING.md "Nx gate"): the `devices` of the runtimeAcceptance records that
// list the file, else `projectDevices` (the pack devices of a module project), else both. A file that the records do not list
// runs on `projectDevices` or both. tools/nx/tests/device-pins.test.ts checks the declarations against the test sources.
export const fileDevices = (file, recs, projectDevices) => {
  const own = recs.filter((rec) => rec.files?.includes(file));
  const declared = own.filter((rec) => rec.devices !== undefined);
  for (const rec of declared)
    if (!Array.isArray(rec.devices) || !rec.devices.length || rec.devices.some((d) => !ALL_DEVICES.includes(d)))
      throw new Error(`metadata.runtimeAcceptance devices of ${file} must be a list of dt2 and dn2`);
  const list = declared.length ? unionDevices(declared.map((rec) => rec.devices)) : (projectDevices ?? ALL_DEVICES);
  if (!list.length) throw new Error(`${file} has no device to run on`);
  return list;
};

const fileTargets = (
  name,
  root,
  tests,
  recs,
  imageInput,
  importInputs,
  pythonInputs,
  uploadable = false,
  opts = {},
) => {
  const allName = `${name}-all`;
  const merge = opts.mergeTests ?? [];
  const isMerge = (test) => !opts.twin || merge.includes(test);
  // `merge`: the key of the merge target (the target everything selects); the `-all` twins keep the full key.
  const build = (list, narrow, recList = recs, merge = false, fullTier = false) =>
    deviceTarget(root, list, recList, imageInput, importInputs, pythonInputs, uploadable, {
      ...opts,
      narrow,
      merge,
      fullTier,
    });
  if (tests.length === 1) {
    const [test] = tests;
    const targets = { [name]: build(tests, !isMerge(test), recs, Boolean(opts.twin)) };
    if (opts.twin) targets[allName] = build(tests, false, recs, false, true); // the full key for every test, a merge test included
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
    const make = (isNarrow, merge = false, fullTier = false) => {
      const target = build([test], isNarrow, own.length ? own : recs, merge, fullTier);
      target.inputs.push('!{projectRoot}/**/*.device.test.ts');
      return target;
    };
    targets[child] = make(narrow, Boolean(opts.twin));
    if (opts.twin) all[`${allName}--${suffix}`] = make(false, false, true);
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

// A marker that names a missing file would silently give that file a narrow key.
export const checkMergeTests = (workspaceRoot, root, deviceTests, fullTier) => {
  const listed = mergeTests(join(workspaceRoot, root));
  if (!Array.isArray(listed) || listed.some((t) => typeof t !== 'string'))
    throw new Error(`metadata.mergeTests of ${root} must be an array of strings`);
  if (!listed.length) return;
  if (fullTier) throw new Error(`metadata.mergeTests of ${root} has no meaning on a tier:full project`);
  const unknown = listed.filter((t) => !deviceTests.includes(t));
  if (unknown.length)
    throw new Error(`metadata.mergeTests of ${root} names files that are not its device tests: ${unknown.join(', ')}`);
};

// The shared conformance suite loads every uploadable module (`isUploadable` in module-manifests.mjs).
export const conformanceTarget = (workspaceRoot, root, m, f, importInputs) => ({
  executor: 'nx:run-commands',
  cache: true,
  inputs: [
    ...DEVICE_INPUTS,
    'production',
    ...builtSourceExclusions(root, workspaceRoot),
    ...importInputs([...globSync('tests/conformance/**/*.device.test.ts', { cwd: workspaceRoot }), VITEST_SETUP]),
  ],
  dependsOn: ['pack', 'digikit/debug:pack', 'digi-control:daemons', IMAGE_BUILD, DEVICE_BYTES, TEST_ASSEMBLY],
  outputs: [],
  options: {
    command: `${EMU_SLOT} pnpm --filter @digi/regression test:conformance`,
    env: { DIGI_CONFORMANCE_MODULES: m.id, DIGI_CONFORMANCE_MANIFEST: f, DIGI_PATCHKIT_LIVE: '1' },
    cwd: '.',
  },
});

// Nodes of the test-only projects (their device targets); appended to `nodes`.
export const testOnlyNodes = (
  workspaceRoot,
  testOwners,
  byId,
  moduleInfo,
  coreBuilds,
  importInputs,
  pythonInputs,
  nodes,
) => {
  for (const [file, config] of testOwners) {
    const root = dirname(file);
    const tests = globSync('**/*.device.test.ts', { cwd: join(workspaceRoot, root) })
      .filter(
        (t) =>
          !t.endsWith('.manual.device.test.ts') &&
          !testOwners.some(([pf]) => dirname(pf).startsWith(root + '/') && join(root, t).startsWith(dirname(pf) + '/')),
      )
      .sort();
    checkMergeTests(workspaceRoot, root, tests, config.tags.includes('tier:full'));
    if (!tests.length) continue;
    const recs = records(join(workspaceRoot, root));
    checkAcceptanceTags(root, recs, tests, moduleInfo);
    if (recs.some((rec) => Object.keys(rec.aliases ?? {}).length))
      throw new Error(`Test-only project ${config.name} needs explicit pack-aliases support`);
    // Explicit fixture images build only their manifest requirements; ordinary owners keep the default image.
    const patchRoots = [...new Set(recs.flatMap((rec) => rec.patchModules ?? []))];
    const patchIds = patchRoots.length
      ? manifestClosure(byId, patchRoots).map((file) => read(workspaceRoot, file).module.id)
      : undefined;
    const targetName = config.tags.includes('tier:full') ? 'test-device-full' : 'test-device';
    if (targetName === 'test-device' && !config.implicitDependencies?.length && !patchRoots.length)
      throw new Error(
        `Test-only project ${config.name}: the narrow merge key needs the subject module; name it in implicitDependencies`,
      );
    const targets = deviceTargets(
      targetName,
      root,
      tests,
      recs,
      patchIds ? { input: 'production', projects: patchIds } : null,
      importInputs,
      pythonInputs,
      false,
      {
        twin: targetName === 'test-device',
        subjects: config.implicitDependencies,
        services: serviceLeaves(byId),
        base: workspaceRoot,
        mergeTests: mergeTests(join(workspaceRoot, root)),
      },
    );
    for (const target of Object.values(targets)) {
      if (!target.options) continue; // the no-op parent
      if (patchIds)
        target.dependsOn = target.dependsOn.flatMap((dependency) =>
          dependency === IMAGE_BUILD
            ? patchIds.filter((id) => coreBuilds.has(id)).map((id) => `${id}:build-objects`)
            : [dependency],
        );
      if (targetName === 'test-device-full') target.options.env = { ...target.options.env, DIGIKIT_TIER: 'full' };
    }
    nodes.push([file, { projects: { [root]: { name: config.name, root, tags: config.tags, targets } } }]);
  }
};

// The node that adds the emulator tests to tests/regression.
export const regressionNode = (workspaceRoot, importInputs) => [
  'tests/regression/project.json',
  {
    projects: {
      'tests/regression': {
        targets: {
          'test-device': {
            inputs: [
              ...DEVICE_INPUTS,
              '{projectRoot}/project.json',
              ...importInputs([
                ...globSync('tests/regression/emulator/**/*.test.ts', { cwd: workspaceRoot }),
                VITEST_SETUP,
              ]),
            ],
          },
          'test-device-all': { executor: 'nx:noop', dependsOn: ['test-device'] },
        },
      },
    },
  },
];
