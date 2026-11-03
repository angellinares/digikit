import { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { afterAll, expect, it } from 'vitest';
import { testImportInputs } from '../test-imports.mjs';

const root = mkdtempSync(join(tmpdir(), 'digikit-test-imports-'));
const repository = fileURLToPath(new URL('../../../', import.meta.url));
afterAll(() => rmSync(root, { recursive: true, force: true }));
const write = (file: string, content: string) => {
  mkdirSync(dirname(join(root, file)), { recursive: true });
  writeFileSync(join(root, file), content);
};
write('tests/regression/package.json', '{}');
symlinkSync(join(repository, 'tests/regression/node_modules'), join(root, 'tests/regression/node_modules'), 'dir');
write(
  'tests/regression/tsconfig.json',
  JSON.stringify({
    compilerOptions: {
      module: 'NodeNext',
      moduleResolution: 'NodeNext',
      baseUrl: '.',
      paths: { '@harness/*': ['./runtime/*'] },
    },
  }),
);

it('follows aliases, reexports, type imports, literal dynamic imports and cycles', () => {
  write('suite/test.ts', "import '@harness/start.ts'; import type { T } from './types.ts';");
  write('suite/types.ts', 'export type T = number;');
  write('tests/regression/runtime/start.ts', "export * from './middle.ts';");
  write('tests/regression/runtime/middle.ts', "import './start.ts'; import('./leaf.ts');");
  write('tests/regression/runtime/leaf.ts', "import 'node:fs';");
  const inputs = testImportInputs(root)(['suite/test.ts']);
  expect(inputs).toEqual(
    [
      'suite/test.ts',
      'suite/types.ts',
      'tests/regression/runtime/start.ts',
      'tests/regression/runtime/middle.ts',
      'tests/regression/runtime/leaf.ts',
    ]
      .map((file) => `{workspaceRoot}/${file}`)
      .sort(),
  );
});

it('retains deleted explicit helpers and recomputes a newly added transitive import', () => {
  write('suite/deletion.ts', "import '@harness/deleted.ts'; import './deleted.ts';");
  expect(testImportInputs(root)(['suite/deletion.ts'])).toEqual(
    ['suite/deletion.ts', 'suite/deleted.ts', 'tests/regression/runtime/deleted.ts']
      .map((file) => `{workspaceRoot}/${file}`)
      .sort(),
  );
  write('suite/deletion.ts', "import '@harness/new.ts';");
  write('tests/regression/runtime/new.ts', "import './new-leaf.ts';");
  expect(testImportInputs(root)(['suite/deletion.ts'])).toContain(
    '{workspaceRoot}/tests/regression/runtime/new-leaf.ts',
  );
});

it('fails closed for computed or ambiguous unresolved local imports', () => {
  write('suite/computed.ts', 'import(`./${name}.ts`);');
  expect(() => testImportInputs(root)(['suite/computed.ts'])).toThrow('Computed test import');
  write('suite/unresolved.ts', "import './missing';");
  expect(() => testImportInputs(root)(['suite/unresolved.ts'])).toThrow('Cannot resolve local test import');
  write('suite/alias.ts', "import '@unknown/helper.ts';");
  expect(() => testImportInputs(root)(['suite/alias.ts'])).toThrow('Cannot resolve test import');
});
