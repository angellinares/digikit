import { fileURLToPath } from 'node:url';
import { configDefaults, defineConfig } from 'vitest/config';
// tools/devtools/verify.py reads this file to treat host tests as device-free.
import projects from './vitest.host.projects.json';

const root = fileURLToPath(new URL('.', import.meta.url));

export default defineConfig({
  root,
  test: {
    maxWorkers: 1,
    // Host tests also read desktop source/CSS via fs: related cannot discover
    // those edges. This native trigger conservatively reruns every host project.
    forceRerunTriggers: [...configDefaults.forceRerunTriggers, '**/apps/desktop/ui/src/**'],
    // desktop-host: the Solid client build (the server build runs no effects), for the tests of reactive code
    projects: projects.map((p: { name: string; dir: string; include: string; conditions?: string[] }) => ({
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
