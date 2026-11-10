// The gallery's build step (scripts/gen-modules.mjs), the shipped modules and the patch worker's core.
// The apply tests compare with the patchkit command line and skip when the wasm or a stock image is absent.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync, mkdtempSync, readdirSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { createHash } from 'node:crypto';
import { join, resolve, dirname, relative, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parse } from 'smol-toml';
import { buildModuleData, shipModules } from '../scripts/gen-modules.mjs';
import { isShipped, shippedDevices } from '../src/data/shipped.mjs';
import { PatchKit, patchedFileName } from '../src/patch/patchkit-core.ts';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');
const wasmPath = join(root, 'apps/web/public/patchkit.wasm');
const images = {
  dt2: join(root, 'firmware', 'Digitakt_II_OS1.17.syx'),
  dn2: join(root, 'firmware', 'Digitone_II_OS1.12.syx'),
};
const have = [wasmPath, ...Object.values(images)].every(existsSync);
const py = (module, ...args) =>
  execFileSync('uv', ['run', 'python', '-m', module, ...args], {
    cwd: root,
    env: { ...process.env, PYTHONPATH: root },
    encoding: 'utf8',
    maxBuffer: 1 << 28,
  });

const manifestFiles = (dir = join(root, 'modules')) =>
  readdirSync(dir, { withFileTypes: true }).flatMap((e) =>
    e.isDirectory() ? manifestFiles(join(dir, e.name)) : e.name === 'module.toml' ? [join(dir, e.name)] : [],
  );

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

test('the published bundle (no test fixtures) stays within its size budget', () => {
  // What the site ships: DIGI_SITE_NO_FIXTURES leaves out tests/fixtures/modules (proof modules like test/file-write that only the tests use).
  // 4,131,187 characters at the usb-fast lane (the runtime module is 1.8 MB of it); keep a margin for module growth.
  const before = process.env.DIGI_SITE_NO_FIXTURES;
  process.env.DIGI_SITE_NO_FIXTURES = '1';
  try {
    const json = JSON.stringify(buildModuleData(root).bundle);
    const maxChars = 4.5 * 1024 * 1024;
    assert.ok(json.length < maxChars, `bundle is ${json.length} characters (budget: ${maxChars})`);
  } finally {
    if (before === undefined) delete process.env.DIGI_SITE_NO_FIXTURES;
    else process.env.DIGI_SITE_NO_FIXTURES = before;
  }
});

test('bundle carries the device knob models and the gallery JSON of typed modules', () => {
  const { bundle } = buildModuleData(root);
  assert.equal(bundle.devices.length, 2);
  assert.ok(bundle.devices.every((t) => 'modules' in parse(t)));
  const sine = bundle.modules.find((m) => m.manifest.includes('id = "test/sine"'));
  assert.ok(!sine.manifest.includes('[[entry]]'));
  const g = JSON.parse(sine.files['build/gen/gallery.json']);
  assert.equal(g.module, 'test/sine');
  const m = g.machines[0];
  assert.deepEqual(Object.keys(m.devices).sort(), ['dn2', 'dt2']);
  assert.equal(m.devices.dt2.knobs[1].param, 'sine_level');
  assert.equal(m.devices.dn2.knobs[1].slot, 26);
  assert.deepEqual(
    g.params.dt2.map((p) => p.short),
    ['LEVEL'],
  );
  for (const p of g.params.dt2) assert.ok(p.min <= p.default && p.default <= p.max, p.id);
});

test('public module summaries are one short sentence', () => {
  for (const file of manifestFiles()) {
    const { module: m } = parse(readFileSync(file, 'utf8'));
    if (m.category === 'core' || m.category === 'test') continue;
    assert.ok(m.summary.length <= 120, `${m.id}: summary is longer than 120 characters`);
    assert.ok(m.summary.endsWith('.'), `${m.id}: summary does not end with a full stop`);
  }
});

test('bundle equals tools/patchkit/patchkit.py bundle', { skip: !existsSync(join(root, '.venv')) }, () => {
  const out = join(mkdtempSync(join(tmpdir(), 'gal-')), 'bundle.json');
  py('tools.patchkit.patchkit', 'bundle', '-o', out);
  assert.deepEqual(buildModuleData(root).bundle, JSON.parse(readFileSync(out, 'utf8')));
});

test('the shipped modules: the index describes them, the sources hold no build output', () => {
  const out = mkdtempSync(join(tmpdir(), 'ship-'));
  const { modules } = shipModules(out, root);
  // what ships is the rule of isShipped over the manifests of modules/{machines,fx,tools} (a device-less module is left out)
  const expected = ['machines', 'fx', 'tools']
    .flatMap((r) => manifestFiles(join(root, 'modules', r)))
    .map((file) => ({ file, d: parse(readFileSync(file, 'utf8')) }))
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

test('patched file names', () => {
  assert.equal(patchedFileName('Digitakt II', '1.17', ['core/fx']), 'Digitakt_II_OS1.17+core-fx.syx');
});

test('worker core: check and apply equal the command line (core/fx)', { skip: !have }, async () => {
  const work = mkdtempSync(join(tmpdir(), 'gal-'));
  const { bundle } = buildModuleData(root);
  const kit = await PatchKit.create(readFileSync(wasmPath), JSON.stringify(bundle));
  for (const [name, path] of Object.entries(images)) {
    const fw = new Uint8Array(readFileSync(path));
    const checked = kit.check(fw);
    assert.equal(checked.device, name);
    assert.ok(checked.port?.startsWith(name));
    assert.equal(checked.modules.find((m) => m.id === 'core/fx').ok, true);
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

test(
  'the runtime set of each device file patches the stock image, byte for byte as the command line does',
  { skip: !have },
  async () => {
    const work = mkdtempSync(join(tmpdir(), 'gal-'));
    const { bundle } = buildModuleData(root);
    const kit = await PatchKit.create(readFileSync(wasmPath), JSON.stringify(bundle));
    const files = { dt2: 'digitakt-ii', dn2: 'digitone-ii' };
    for (const [name, path] of Object.entries(images)) {
      const set = parse(readFileSync(join(root, 'devices', `${files[name]}.toml`), 'utf8')).patch_sets.device;
      const fw = new Uint8Array(readFileSync(path));
      const checked = kit.check(fw);
      assert.ok(
        set.every((id) => checked.modules.find((m) => m.id === id)?.ok === true),
        `${name}: the set fits`,
      );
      const out = join(work, `${name}-runtime.syx`);
      py('tools.patchkit.patchkit', 'patch', '--firmware', path, '-o', out); // no --modules: the `device` set
      assert.ok(Buffer.from(kit.apply(fw, set).syx).equals(readFileSync(out)), `${name}: differs from the CLI`);
    }
  },
);

test('worker core: a firmware of the other device, and garbage, are refused', { skip: !have }, async () => {
  const { bundle } = buildModuleData(root);
  const kit = await PatchKit.create(readFileSync(wasmPath), JSON.stringify(bundle));
  assert.throws(() => kit.check(new Uint8Array(1000)), /./);
  const old = join(root, 'firmware', 'Digitakt_II_OS1.15C.syx');
  if (existsSync(old)) {
    const c = kit.check(new Uint8Array(readFileSync(old)));
    assert.equal(c.port, null);
    assert.ok(c.modules.every((m) => !m.ok && /no port/.test(m.error)));
  }
});

test('built site (when present) carries no firmware', { skip: !existsSync(join(root, 'apps/web/dist')) }, () => {
  const walk = (d) =>
    readdirSync(d, { withFileTypes: true }).flatMap((e) =>
      e.isDirectory() ? walk(join(d, e.name)) : [join(d, e.name)],
    );
  for (const f of walk(join(root, 'apps/web/dist'))) {
    assert.ok(!/\.(syx|snap|mstate)$/i.test(f), f);
    assert.ok(statSync(f).size < 5 * 1024 * 1024 || f.endsWith('emulator-core.wasm'), f);
  }
});
