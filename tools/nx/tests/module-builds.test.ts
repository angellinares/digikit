import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { afterAll, expect, it } from 'vitest';
import { createNodesV2 } from '../modules.mjs';

const root = mkdtempSync(join(tmpdir(), 'digikit-module-builds-'));
afterAll(() => rmSync(root, { recursive: true, force: true }));

it('infers core producers from category and source-backed objects, independently of IDs and types', () => {
  const modules = [
    {
      dir: 'modules/arbitrary/owner',
      id: 'example/owner',
      category: 'core',
      requires: ['example/dep', 'core/content'],
      sources: ['src/a.c'],
    },
    { dir: 'modules/arbitrary/dep', id: 'example/dep', category: 'core', sources: ['src/b.c'] },
    { dir: 'modules/arbitrary/content', id: 'core/content', category: 'test', sources: ['src/c.c'] },
    { dir: 'modules/arbitrary/prebuilt', id: 'example/prebuilt', category: 'core', sources: [] },
    { dir: 'modules/arbitrary/unrelated', id: 'example/unrelated', category: 'core', sources: ['src/d.c'] },
    { dir: 'tests/fixtures/modules/release-example', id: 'fixture/example', category: 'tools', sources: ['src/f.c'] },
  ];
  const files = modules.map((m) => {
    const file = `${m.dir}/module.toml`;
    mkdirSync(join(root, dirname(file)), { recursive: true });
    writeFileSync(
      join(root, file),
      `[module]\nid = "${m.id}"\ncategory = "${m.category}"\ntype = "raw"\nrequires = ${JSON.stringify(m.requires ?? [])}\n[[object]]\npath = "build/object.dko.json"\nsources = ${JSON.stringify(m.sources)}\n`,
    );
    return file;
  });
  mkdirSync(join(root, 'devices'));
  writeFileSync(join(root, 'devices/example.toml'), '[patch_sets]\nemulator = ["example/owner"]\n');
  const inferred = createNodesV2[1](files, {}, { workspaceRoot: root });
  const projects = Object.assign({}, ...inferred.map(([, result]) => result.projects));
  const owner = projects[modules[0].dir];
  expect(owner.tags).toEqual(expect.arrayContaining(['module', 'type:raw', 'acceptance-image']));
  const producer = owner.targets['build-objects'];
  expect(producer.cache).toBe(false);
  for (const module of modules) {
    const input = `{workspaceRoot}/${module.dir}/module.toml`;
    if (module.category === 'core') expect(producer.inputs).toContain(input);
    else expect(producer.inputs).not.toContain(input);
  }
  expect(producer.inputs).not.toContain('{workspaceRoot}/modules/**/module.toml');
  expect(producer.inputs).not.toContain('{workspaceRoot}/tests/fixtures/modules/**/module.toml');

  expect(producer.outputs).toEqual([`{workspaceRoot}/${modules[0].dir}/build/object.dko.json`]);
  expect(producer.dependsOn).toEqual(['sdk:check-generated', 'example/dep:build-objects']);
  expect(producer.options.parallel).toBe(false);
  expect(producer.options.commands).toHaveLength(2);
  expect(projects[modules[1].dir].tags).toContain('acceptance-image');
  expect(projects[modules[2].dir].targets?.['build-objects']).toBeUndefined();
  expect(projects[modules[3].dir].targets?.['build-objects']).toBeUndefined();
  expect(projects[modules[4].dir].targets['build-objects']).toBeDefined();
  expect(projects[modules[4].dir].tags).not.toContain('acceptance-image');
  expect(projects[modules[2].dir].tags).toContain('release-module'); // shipped raw content, no uploadability flags
  expect(owner.tags).not.toContain('release-module');
  expect(projects[modules[5].dir].tags).not.toContain('release-module');
});

it('scopes explicit acceptance images without changing the default image producer set', () => {
  mkdirSync(join(root, 'tests/regression'), { recursive: true });
  writeFileSync(
    join(root, 'tests/regression/tsconfig.json'),
    JSON.stringify({ compilerOptions: {}, include: ['../fixtures/**/*.ts'] }),
  );
  const owners = [{ name: 'scoped-ui-tests', patchModules: ['example/dep'] }, { name: 'default-ui-tests' }];
  const files = owners.map(({ name, patchModules }) => {
    const dir = `tests/fixtures/modules/${name}/tests/zero`;
    mkdirSync(join(root, dir), { recursive: true });
    writeFileSync(join(root, dir, 'ui.device.test.ts'), 'export {};');
    writeFileSync(
      join(root, dir, 'project.json'),
      JSON.stringify({
        name,
        tags: ['test-only'],
        ...(patchModules ? {} : { implicitDependencies: ['example/owner'] }),
        metadata: { runtimeAcceptance: [{ files: ['ui.device.test.ts'], patchModules }] },
      }),
    );
    return `${dir}/project.json`;
  });
  const manifests = ['owner', 'dep', 'content', 'prebuilt', 'unrelated'].map(
    (name) => `modules/arbitrary/${name}/module.toml`,
  );
  const inferred = createNodesV2[1]([...manifests, ...files], {}, { workspaceRoot: root });
  const projects = Object.assign({}, ...inferred.map(([, result]) => result.projects));
  const scoped = projects['tests/fixtures/modules/scoped-ui-tests/tests/zero'].targets['test-device'];
  const normal = projects['tests/fixtures/modules/default-ui-tests/tests/zero'].targets['test-device'];
  expect(scoped.dependsOn).toContain('example/dep:build-objects');
  expect(scoped.dependsOn).not.toContain('example/owner:build-objects');
  expect(scoped.inputs).toContainEqual({ input: 'production', projects: ['example/dep'] });
  expect(normal.dependsOn).toContainEqual({ target: 'build-objects', projects: ['tag:acceptance-image'] });
});
