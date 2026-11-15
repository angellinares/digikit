// The `test-rig` target of a module: its sound checks (tests/rig/*.toml, tests/*.rig.test.ts) on the SHARC-only rig. No daemon, firmware or
// emulator. The key holds the module's own files (the DSP sources and the tables of the checks), the kit, the two adapters the rig wraps
// the code in, the SDK, the device profiles, the hashes of the two binaries it runs and the hash of the stock DSP section (or "absent":
// the stock-image suites skip without the firmware). The synthetic shadow-replay fixtures of tests/fixtures/dsp are inputs of the stereo proof's
// shadow replay test.
const STEREO_PROOF_ROOT = 'modules/core/adapters/dsp-stereo-proof';

export const rigTarget = (root) => ({
  executor: 'nx:run-commands',
  cache: true,
  dependsOn: ['patchkit:cli', 'sharc-rig:build'],
  inputs: [
    'jsToolchain',
    `{projectRoot}/**/*`,
    '{workspaceRoot}/packages/sharc-rig-kit/src/**/*',
    '{workspaceRoot}/packages/sharc-rig-kit/package.json',
    '{workspaceRoot}/vitest.host.config.ts',
    '{workspaceRoot}/vitest.host.projects.json',
    '{workspaceRoot}/modules/core/adapters/dsp-voice/**/*',
    '{workspaceRoot}/modules/core/adapters/fx/**/*',
    '{workspaceRoot}/modules/core/adapters/machines/types.toml',
    ...(root === STEREO_PROOF_ROOT ? ['{workspaceRoot}/tests/fixtures/dsp/stereo-*shadow-replay.s'] : []),
    '{workspaceRoot}/sdk/**/*',
    '{workspaceRoot}/devices/*.toml',
    '{workspaceRoot}/tools/patchkit/toolchain/pins.toml',
    { runtime: 'node tools/nx/stock-section-hash.mjs' },
    { dependentTasksOutputFiles: '**/*' },
  ],
  outputs: [],
  options: {
    forwardAllArgs: false,
    command: `pnpm exec vitest run --config vitest.host.config.ts --project module-rig ${root}/tests/`,
    cwd: '.',
  },
});
