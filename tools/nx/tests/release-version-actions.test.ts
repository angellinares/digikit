import { createTreeWithEmptyWorkspace } from '@nx/devkit/testing';
import { expect, it } from 'vitest';
import TomlVersionActions from '../release-version-actions.cjs';

const modulePath = 'modules/example/module.toml';
const moduleText =
  '# owner\r\n[module]\r\nid = "example/owner"\r\nversion = \'0.5.4\'  # keep me\r\n\r\n[other]\r\nversion = "9.9.9"\r\n';
const sdkText = '[runtime]\nversion = "0.5.4" # runtime release\n[contract]\napi = [1, 6]\n';

async function actions(tree: ReturnType<typeof createTreeWithEmptyWorkspace>, runtime = false) {
  const action = new TomlVersionActions(
    { name: runtime ? 'runtime' : 'modules' },
    { name: 'example/owner', data: { root: 'modules/example' } },
    {
      manifestRootsToUpdate: runtime ? [{ path: 'sdk', preserveLocalDependencyProtocols: false }] : [],
      versionActionsOptions: runtime
        ? { manifestFilename: 'sdk.toml', versionTable: 'runtime', mirrorProjectManifest: true }
        : {},
      preserveLocalDependencyProtocols: false,
      adjustSemverBumpsForZeroMajorVersion: false,
    },
  );
  await action.init(tree);
  return action;
}

it('changes only the owner version token, preserving comments, quotes, line endings and other tables', async () => {
  const tree = createTreeWithEmptyWorkspace();
  tree.write(modulePath, moduleText);
  const action = await actions(tree);
  await action.validate(tree);
  expect(await action.readCurrentVersionFromSourceManifest(tree)).toEqual({
    currentVersion: '0.5.4',
    manifestPath: modulePath,
  });
  await action.updateProjectVersion(tree, '0.5.5');
  expect(tree.read(modulePath, 'utf8')).toBe(moduleText.replace("'0.5.4'", "'0.5.5'"));
  expect(action.excludeManifestsFromFormatting).toBe(true);
});

it('versions the canonical runtime and its mirror together without changing SDK contracts or another module', async () => {
  const tree = createTreeWithEmptyWorkspace();
  tree.write(modulePath, moduleText);
  tree.write('sdk/sdk.toml', sdkText);
  tree.write('modules/other/module.toml', '[module]\nversion = "0.1.0"\n');
  const action = await actions(tree, true);
  const bump = await action.calculateNewVersion('0.5.4', 'minor', 'USER_SPECIFIER', {}, '');
  expect(bump.newVersion).toBe('0.6.0');
  expect((await action.readCurrentVersionFromSourceManifest(tree)).manifestPath).toBe('sdk/sdk.toml');
  await action.updateProjectVersion(tree, bump.newVersion);
  expect(tree.read('sdk/sdk.toml', 'utf8')).toBe(sdkText.replace('0.5.4', '0.6.0'));
  expect(tree.read(modulePath, 'utf8')).toBe(moduleText.replace('0.5.4', '0.6.0'));
  expect(tree.read('modules/other/module.toml', 'utf8')).toBe('[module]\nversion = "0.1.0"\n');
  expect(action.manifestsToUpdate.map((m: { manifestPath: string }) => m.manifestPath)).toEqual([
    'sdk/sdk.toml',
    modulePath,
  ]);
});

it.each(['0.5.5-beta.1', '0.5.5+build', '256.0.0', '0.0.256', '01.2.3'])(
  'refuses unsupported packed version %s before editing either runtime manifest',
  async (version) => {
    const tree = createTreeWithEmptyWorkspace();
    tree.write(modulePath, moduleText);
    tree.write('sdk/sdk.toml', sdkText);
    const action = await actions(tree, true);
    await expect(action.updateProjectVersion(tree, version)).rejects.toThrow('Unsupported packed version');
    expect(tree.read(modulePath, 'utf8')).toBe(moduleText);
    expect(tree.read('sdk/sdk.toml', 'utf8')).toBe(sdkText);
  },
);

it.each(['missing', 'mismatched'])(
  'refuses a %s runtime mirror without changing the canonical manifest',
  async (kind) => {
    const tree = createTreeWithEmptyWorkspace();
    tree.write('sdk/sdk.toml', sdkText);
    if (kind === 'mismatched') tree.write(modulePath, moduleText.replace('0.5.4', '0.1.0'));
    const action = await actions(tree, true);
    await expect(action.validate(tree)).rejects.toThrow();
    await expect(action.updateProjectVersion(tree, '0.5.5')).rejects.toThrow();
    expect(tree.read('sdk/sdk.toml', 'utf8')).toBe(sdkText);
    expect(tree.read(modulePath, 'utf8')).toBe(kind === 'missing' ? null : moduleText.replace('0.5.4', '0.1.0'));
  },
);

it('fails closed on a TOML layout that would require serialization', async () => {
  const tree = createTreeWithEmptyWorkspace();
  const text = '[module]\nversion = """0.5.4"""\n';
  tree.write(modulePath, text);
  const action = await actions(tree);
  await expect(action.updateProjectVersion(tree, '0.5.5')).rejects.toThrow('refusing to reformat TOML');
  expect(tree.read(modulePath, 'utf8')).toBe(text);
});

it('does not treat build graph edges as versioned dependency declarations', async () => {
  const tree = createTreeWithEmptyWorkspace();
  tree.write(modulePath, moduleText);
  const action = await actions(tree);
  expect(
    await action.readDependencies(tree, {
      dependencies: { 'example/owner': [{ target: 'example/build-tool', type: 'static' }] },
    }),
  ).toEqual([]);
  await expect(action.updateProjectDependencies(tree, {}, { 'example/build-tool': '1.0.0' })).rejects.toThrow(
    'not configured',
  );
  expect(tree.read(modulePath, 'utf8')).toBe(moduleText);
});
