import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vitest/config';

// Each worker runs one coupled daemon (two cores); the Nx launcher sets REGRESSION_JOBS from its budget.
// stock, runtime and conformance files run in parallel up to maxWorkers; fx-* stay serial.
// Vitest 3.2 reads only singleFork per project; worker caps are root-level.
const serial = { poolOptions: { forks: { singleFork: true } } };

const at = (p: string) => fileURLToPath(new URL(p, import.meta.url));

// Files under modules/ and tests/conformance/ have no node_modules: resolve the workspace packages here.
const alias = [
  { find: /^@digi\/regression\/(.*)$/, replacement: `${at('.').replace(/\/$/, '')}/$1` },
  { find: /^@digi\/test-support$/, replacement: at('../../packages/test-support/src/index.ts') },
  { find: /^@digi\/control-client$/, replacement: at('../../packages/control-client/src/index.ts') },
  { find: /^@digi\/dkbs$/, replacement: at('../../packages/dkbs/src/index.ts') },
  { find: /^@digi\/dk-session$/, replacement: at('../../packages/dk-session/src/index.ts') },
  { find: /^@digi\/dk-session\/(.*)$/, replacement: `${at('../../packages/dk-session/src')}/$1.ts` },
];

export default defineConfig({
  root: at('.'),
  resolve: { alias },
  test: {
    // Removes the temp dirs of the file's fixtures after its last test.
    setupFiles: [at('./setup-temp.ts')],
    // Caps the shared pool: workers at once in stock, runtime and conformance.
    maxWorkers: Number(process.env.REGRESSION_JOBS ?? 2),
    // REGRESSION_JUNIT_FILE (verify's failed-unit record) names the report; REGRESSION_JUNIT=1 alone keeps the default.
    reporters:
      process.env.REGRESSION_JUNIT === '1' || process.env.REGRESSION_JUNIT_FILE
        ? [
            'default',
            [
              'junit',
              {
                outputFile: process.env.REGRESSION_JUNIT_FILE ?? '../../out/regression/junit.xml',
              },
            ],
          ]
        : ['default'],
    projects: [
      {
        extends: true,
        test: {
          name: 'stock',
          include: ['dt2/**/*.test.ts'],
          pool: 'forks',
          testTimeout: 120_000,
          hookTimeout: 120_000,
        },
      },
      {
        extends: true,
        test: {
          name: 'runtime',
          include: [
            'emulator/**/*.test.ts',
            '../../modules/**/tests/**/*.device.test.ts',
            '../fixtures/modules/**/tests/**/*.device.test.ts',
          ],
          pool: 'forks',
          // Hang detection is the no-progress watchdog (DIGI_NO_PROGRESS_MS, default 120 s). These limits are
          // backstops, so host load cannot fail a slow but healthy test.
          testTimeout: 1_800_000,
          hookTimeout: 600_000,
        },
      },
      {
        // Named sampler and DSP proofs (tests/regression/runtime/*.test.ts). Not part of `runtime`: run one by name,
        // `pnpm exec vitest run --project proofs runtime/FILE.test.ts`. New device tests live in their module (`*.device.test.ts`).
        extends: true,
        test: {
          name: 'proofs',
          include: ['runtime/**/*.test.ts'],
          pool: 'forks',
          testTimeout: 300_000,
          hookTimeout: 150_000,
        },
      },
      {
        extends: true,
        test: {
          name: 'conformance',
          include: ['../conformance/**/*.device.test.ts'],
          pool: 'forks',
          // Backstops; see the runtime project.
          testTimeout: 1_800_000,
          hookTimeout: 600_000,
        },
      },
      {
        extends: true,
        test: {
          name: 'fx-dn2',
          include: ['scenarios/dn2-*.test.ts'],
          pool: 'forks',
          ...serial,
          testTimeout: 30_000,
          hookTimeout: 1_900_000,
        },
      },
      {
        extends: true,
        test: {
          name: 'fx-dt2',
          include: ['scenarios/dt2-fx.test.ts'],
          pool: 'forks',
          ...serial,
          testTimeout: 30_000,
          hookTimeout: 1_900_000,
        },
      },
    ],
  },
});
