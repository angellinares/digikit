import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { afterAll, expect, it } from 'vitest';
import { packSourceInputs } from '../pack-inputs.mjs';

const root = mkdtempSync(join(tmpdir(), 'digikit-pack-inputs-'));
afterAll(() => rmSync(root, { recursive: true, force: true }));
const write = (file: string, text: string) => {
  mkdirSync(dirname(join(root, file)), { recursive: true });
  writeFileSync(join(root, file), text);
};
const file = 'modules/example/module.toml';
const modules = new Map([
  ['example/module', { file, manifest: {}, objects: [{ sources: ['src/Cargo.toml', '../../shared/dsp.s'] }] }],
]);

it('includes Cargo path-dependency project sources and exact unowned object sources', () => {
  write(
    'modules/example/src/Cargo.toml',
    '[package]\nname = "module"\n[dependencies]\nhelper = { path = "../../../library" }\n',
  );
  write('library/Cargo.toml', '[package]\nname = "helper"\n');
  write('library/project.json', '{"name":"helper"}');
  expect(packSourceInputs(root, modules, [file])).toEqual([
    { input: 'production', projects: ['example/module', 'helper'] },
    '{workspaceRoot}/shared/dsp.s',
  ]);
});

it('rejects inherited or external Cargo dependency paths instead of omitting their inputs', () => {
  write('modules/example/src/Cargo.toml', '[dependencies]\nhelper = { workspace = true }\n');
  expect(() => packSourceInputs(root, modules, [file])).toThrow('inherits workspace configuration');
  write('modules/example/src/Cargo.toml', '[dependencies]\nhelper = { path = "../../../../external" }\n');
  expect(() => packSourceInputs(root, modules, [file])).toThrow('outside the workspace');
});

it('keeps explicit sources outside module and Cargo owners file-level with private headers', () => {
  write('sdk/project.json', '{"name":"sdk"}');
  const external = new Map([['example/module', { file, objects: [{ sources: ['../../sdk/src/picker.c'] }] }]]);
  expect(packSourceInputs(root, external, [file])).toEqual([
    { input: 'production', projects: ['example/module'] },
    '{workspaceRoot}/sdk/src/**/*.{h,hh,hpp,hxx}',
    '{workspaceRoot}/sdk/src/picker.c',
  ]);
});
