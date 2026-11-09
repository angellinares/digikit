// Discover importing test files without collecting tests or executing their hooks.
import { relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createVitest } from 'vitest/node';

const root = fileURLToPath(new URL('..', import.meta.url));
const [project, ...sources] = process.argv.slice(2);
const context = await createVitest('test', {
  root,
  config: resolve(root, 'vitest.config.ts'),
  project: [project],
  watch: false,
});
try {
  const files = {};
  for (const source of sources) {
    context.config.related = [resolve(root, '../..', source)];
    files[source] = (await context.getRelevantTestSpecifications())
      .map((specification) => relative(root, specification.moduleId).replaceAll('\\', '/'))
      .sort();
  }
  process.stdout.write(`${JSON.stringify(files)}\n`);
} finally {
  await context.close();
}
