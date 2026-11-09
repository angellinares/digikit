import { fileURLToPath } from 'node:url';
import { configDefaults, defineConfig } from 'vitest/config';
// The Nx test-host targets read this file; host tests are device-free.
import projects from './vitest.host.projects.json';

const root = fileURLToPath(new URL('.', import.meta.url));

export default defineConfig({
  root,
  test: {
    maxWorkers: 1,
    // desktop-host: the Solid client build (the server build runs no effects), for the tests of reactive code
    projects: projects.map((p: { name: string; dir: string; include: string; conditions?: string[] }) => ({
      // modules/ has no node_modules: its tests reach the workspace packages by path
      resolve: { alias: { '@digi/dkbs': fileURLToPath(new URL('packages/dkbs/src/index.ts', import.meta.url)) } },
      ...(p.conditions
        ? { resolve: { conditions: p.conditions }, ssr: { resolve: { conditions: p.conditions } } }
        : {}),
      test: {
        ...(p.conditions ? { server: { deps: { inline: ['solid-js'] } } } : {}),
        name: p.name,
        root: fileURLToPath(new URL(p.dir, import.meta.url)),
        include: [p.include],
        exclude: [...configDefaults.exclude, '**/*.device.test.ts'],
        testTimeout: 5000,
      },
    })),
  },
});
