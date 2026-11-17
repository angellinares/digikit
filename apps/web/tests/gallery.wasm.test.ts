// The patch worker's core (`PatchKit`, patchkit-core.ts) over patchkit.wasm against the patchkit command line, the gallery bundle (its shape, and against the
// command line's) and the shipped modules (`shipModules` packs them with the CLI). Skips when the wasm, a stock image or the CLI is absent. The rest
// of the gallery's build step is gallery.test.ts.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { dirname, join, relative, sep } from 'node:path';
import { parse } from 'smol-toml';
import { test } from 'vitest';
import { buildModuleData, shipModules } from '../scripts/gen-modules.mjs';
import { isShipped, shippedDevices } from '../src/data/shipped.mjs';
import { PatchKit } from '@digi/workbench-core/patch/patchkit-core.ts';
import {
  firmwareDir,
  haveWasmAndImages as have,
  images,
  manifestFiles,
  py,
  readBytes,
  root,
  wasmPath,
  workDir,
} from './support/patchkit-env.ts';

test('bundle lists the committed modules and ports', () => {
  const { bundle } = buildModuleData(root);
  assert.equal(
    bundle.modules.length,
    manifestFiles().length + manifestFiles(join(root, 'tests/fixtures/modules')).length,
  );
  assert.ok(bundle.modules.some((m) => m.manifest.includes('id = "core/fx"')));
  assert.equal(bundle.ports.length, readdirSync(join(root, 'devices/ports')).length);
  // Module content only: no firmware blobs or out/modules oracles.
  assert.ok(!JSON.stringify(bundle).includes('out/modules'));
});

test('bundle carries the device knob models and the gallery JSON of typed modules', () => {
  const { bundle } = buildModuleData(root);
  assert.equal(bundle.devices.length, 2);
  assert.ok(bundle.devices.every((t: string) => 'modules' in parse(t)));
  const sine = bundle.modules.find((m: { manifest: string }) => m.manifest.includes('id = "test/sine"'))!;
  assert.ok(!sine.manifest.includes('[[entry]]'));
  const g = JSON.parse((sine.files as Record<string, string>)['build/gen/gallery.json']);
  assert.equal(g.module, 'test/sine');
  const m = g.machines[0];
  assert.deepEqual(Object.keys(m.devices).sort(), ['dn2', 'dt2']);
  assert.equal(m.devices.dt2.knobs[1].param, 'level');
  assert.equal(m.devices.dn2.knobs[1].slot, 26);
  assert.deepEqual(
    g.params.dt2.map((p: { short: string }) => p.short),
    ['LEVEL'],
  );
  for (const p of g.params.dt2 as { min: number; default: number; max: number; id: string }[])
    assert.ok(p.min <= p.default && p.default <= p.max, p.id);
});

test.skipIf(!existsSync(join(root, '.venv')))('bundle equals tools/patchkit/patchkit.py bundle', () => {
  const out = join(workDir('gal-'), 'bundle.json');
  py('tools.patchkit.patchkit', 'bundle', '-o', out);
  assert.deepEqual(buildModuleData(root).bundle, JSON.parse(readFileSync(out, 'utf8')));
});

test('the shipped modules: the index describes them, the sources hold no build output', () => {
  const out = workDir('ship-');
  const { modules } = shipModules(out, root);
  // what ships is the rule of isShipped over the manifests of modules/{machines,fx,tools} (a device-less module is left out)
  const expected = ['machines', 'fx', 'tools']
    .flatMap((r) => manifestFiles(join(root, 'modules', r)))
    .map((file) => ({ file, d: parse(readFileSync(file, 'utf8')) as { module: { id: string } } }))
    .filter(
      ({ file, d }) => isShipped(d, relative(root, dirname(file)).split(sep).join('/')) && shippedDevices(d).length,
    )
    .map(({ d }) => d.module.id)
    .sort();
  assert.ok(expected.length > 0);
  assert.deepEqual(modules.map((m) => m.id).sort(), expected);
  assert.deepEqual(JSON.parse(readFileSync(join(out, 'index.json'), 'utf8')).modules, modules);
  for (const m of modules) {
    assert.ok(m.source.includes('module.toml') && m.source.includes('README.md'), m.id);
    assert.ok(
      m.source.every((f) => /^(module\.toml|README\.md|LICENSE[^/]*|src\/|examples\/)/.test(f)),
      m.id,
    );
    for (const f of m.source) assert.ok(existsSync(join(out, 'src', m.id, f)), `${m.id}/${f}`);
    assert.ok(!existsSync(join(out, 'src', m.id, 'build')));
    for (const d of Object.values(m.devices)) {
      const bytes = readFileSync(join(out, d.file));
      assert.equal(bytes.length, d.size);
      assert.equal(createHash('sha256').update(bytes).digest('hex'), d.sha256);
    }
  }
});

test.skipIf(!have)('worker core: check and apply equal the command line (core/fx)', async () => {
  const work = workDir('gal-');
  const { bundle } = buildModuleData(root);
  const kit = await PatchKit.create(readBytes(wasmPath), JSON.stringify(bundle));
  for (const [name, path] of Object.entries(images)) {
    const fw = new Uint8Array(readFileSync(path));
    const checked = kit.check(fw);
    assert.equal(checked.device, name);
    assert.ok(checked.port?.startsWith(name));
    assert.equal(checked.modules.find((m) => m.id === 'core/fx')!.ok, true);
    const out = join(work, `${name}.syx`);
    py('tools.patchkit.patchkit', 'patch', '--firmware', path, '--modules', 'core/fx', '-o', out);
    const r = kit.apply(fw, ['core/fx']);
    assert.ok(Buffer.from(r.syx).equals(readFileSync(out)), `${name}: output differs from the CLI`);
    assert.equal(r.plan.record.modules[0].split('@')[0], 'core/fx');
    assert.ok(r.verify.checks.length > 0);
    assert.equal(r.sha256.length, 64);
    // always from stock: an already patched image is refused with patchkit's text
    assert.throws(() => kit.apply(r.syx, ['core/fx']), /./);
  }
});

test.skipIf(!have)(
  'the runtime set of each device file patches the stock image, byte for byte as the command line does',
  async () => {
    const work = workDir('gal-');
    const { bundle } = buildModuleData(root);
    const kit = await PatchKit.create(readBytes(wasmPath), JSON.stringify(bundle));
    const files: Record<string, string> = { dt2: 'digitakt-ii', dn2: 'digitone-ii' };
    for (const [name, path] of Object.entries(images)) {
      const set = (
        parse(readFileSync(join(root, 'devices', `${files[name]!}.toml`), 'utf8')) as {
          patch_sets: { device: string[] };
        }
      ).patch_sets.device;
      const fw = new Uint8Array(readFileSync(path));
      const checked = kit.check(fw);
      assert.ok(
        set.every((id: string) => checked.modules.find((m) => m.id === id)?.ok === true),
        `${name}: the set fits`,
      );
      const out = join(work, `${name}-runtime.syx`);
      py('tools.patchkit.patchkit', 'patch', '--firmware', path, '-o', out); // no --modules: the `device` set
      assert.ok(Buffer.from(kit.apply(fw, set).syx).equals(readFileSync(out)), `${name}: differs from the CLI`);
    }
  },
);

test.skipIf(!have)('worker core: a firmware of the other device, and garbage, are refused', async () => {
  const { bundle } = buildModuleData(root);
  const kit = await PatchKit.create(readBytes(wasmPath), JSON.stringify(bundle));
  assert.throws(() => kit.check(new Uint8Array(1000)), /./);
  const old = join(firmwareDir, 'Digitakt_II_OS1.15C.syx');
  if (existsSync(old)) {
    const c = kit.check(new Uint8Array(readFileSync(old)));
    assert.equal(c.port, null);
    assert.ok(c.modules.every((m) => !m.ok && /no port/.test(m.error ?? '')));
  }
});
