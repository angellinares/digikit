// patchkit.wasm and the patchkit command line apply the same modules to the same stock image and must
// produce byte-identical output (and the same plan). Skips when the wasm, a stock image or the CLI is absent.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync, readdirSync, mkdtempSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');
const wasmPath = join(root, 'apps/web/public/patchkit.wasm');
const images = {
  dt2: join(root, 'firmware', 'Digitakt_II_OS1.17.syx'),
  dn2: join(root, 'firmware', 'Digitone_II_OS1.12.syx'),
};
const need = [wasmPath, ...Object.values(images)].every(existsSync);
const py = (module, ...args) =>
  execFileSync('uv', ['run', 'python', '-m', module, ...args], {
    cwd: root,
    env: { ...process.env, PYTHONPATH: root },
    encoding: 'utf8',
    maxBuffer: 1 << 28,
  });

async function load() {
  const { instance } = await WebAssembly.instantiate(readFileSync(wasmPath), {});
  const x = instance.exports;
  const put = (bytes) => {
    const p = x.pk_alloc(bytes.length);
    new Uint8Array(x.memory.buffer, p, bytes.length).set(bytes);
    return [p, bytes.length];
  };
  const get = (ptr, len) => new Uint8Array(x.memory.buffer.slice(ptr(), ptr() + len()));
  const text = (ptr, len) => new TextDecoder().decode(get(ptr, len));
  return { x, put, get, text };
}

test('patchkit.wasm is small and carries no firmware', () => {
  if (!existsSync(wasmPath)) return;
  const bytes = readFileSync(wasmPath);
  assert.ok(statSync(wasmPath).size < 2.5 * 1024 * 1024); // carries the Selache assembler, the SHARC+ build
  // no stock-image hash and no port: those are passed in at run time
  for (const f of readdirSync(join(root, 'devices/ports'))) {
    const sha = /syx_sha256 = "([0-9a-f]{64})"/.exec(readFileSync(join(root, 'devices/ports', f), 'utf8'))[1];
    assert.equal(Buffer.from(bytes).includes(Buffer.from(sha)), false, `${f} sha embedded`);
  }
});

test('wasm and CLI produce byte-identical output for core/fx', { skip: !need }, async () => {
  const work = mkdtempSync(join(tmpdir(), 'pk-wasm-'));
  const bundlePath = join(work, 'bundle.json');
  py('tools.patchkit.patchkit', 'bundle', '-o', bundlePath);
  const bundle = readFileSync(bundlePath);
  const { x, put, get, text } = await load();
  for (const [name, path] of Object.entries(images)) {
    const fw = readFileSync(path);
    const out = join(work, `${name}.syx`);
    const plan = join(work, `${name}.plan.json`);
    py('tools.patchkit.patchkit', 'patch', '--firmware', path, '--modules', 'core/fx', '-o', out, '--plan', plan);
    const [fp, fl] = put(fw);
    const [bp, bl] = put(bundle);
    const [ip, il] = put(Buffer.from('core/fx'));
    const rc = x.pk_apply(fp, fl, bp, bl, ip, il);
    assert.equal(rc, 0, text(x.pk_err_ptr, x.pk_err_len));
    const wasmOut = Buffer.from(get(x.pk_out_ptr, x.pk_out_len));
    const cliOut = readFileSync(out);
    assert.equal(wasmOut.length, cliOut.length, `${name}: length`);
    assert.ok(wasmOut.equals(cliOut), `${name}: wasm and CLI output differ`);
    assert.equal(text(x.pk_plan_ptr, x.pk_plan_len), readFileSync(plan, 'utf8'), `${name}: plan.json differs`);
    // the page can verify what it made
    const [op, ol] = put(wasmOut);
    const [fbp, fbl] = put(fw);
    assert.equal(x.pk_verify(op, ol, bp, bl, fbp, fbl), 0, text(x.pk_err_ptr, x.pk_err_len));
    const report = JSON.parse(text(x.pk_plan_ptr, x.pk_plan_len));
    assert.equal(report.device, name);
    assert.ok(report.record.modules[0].startsWith('core/fx@'));
  }
});

test(
  'wasm and CLI produce byte-identical output for the machine framework (two machines)',
  { skip: !need },
  async () => {
    const work = mkdtempSync(join(tmpdir(), 'pk-wasm-'));
    const bundlePath = join(work, 'bundle.json');
    py('tools.patchkit.patchkit', 'bundle', '-o', bundlePath);
    const bundle = readFileSync(bundlePath);
    const { x, put, get, text } = await load();
    const mods = 'core/machines,test/machine-clone';
    const fw = readFileSync(images.dt2);
    const out = join(work, 'dt2-machines.syx');
    const plan = join(work, 'dt2-machines.plan.json');
    py('tools.patchkit.patchkit', 'patch', '--firmware', images.dt2, '--modules', mods, '-o', out, '--plan', plan);
    const [fp, fl] = put(fw);
    const [bp, bl] = put(bundle);
    const [ip, il] = put(Buffer.from(mods));
    assert.equal(x.pk_apply(fp, fl, bp, bl, ip, il), 0, text(x.pk_err_ptr, x.pk_err_len));
    assert.ok(Buffer.from(get(x.pk_out_ptr, x.pk_out_len)).equals(readFileSync(out)), 'wasm and CLI output differ');
    const report = JSON.parse(text(x.pk_plan_ptr, x.pk_plan_len));
    const entries = (name) => report.registries.find((r) => r.name === name).entries.map((e) => [e.id, e.index]);
    assert.deepEqual(entries('machines'), [['xclone', 8]]); // Digitone II has core/machines, but XClone clones a Digitakt machine (MANUAL SLICE, type 6)
    const [dp, dl] = put(readFileSync(images.dn2));
    const [mp, ml] = put(Buffer.from('core/machines,test/machine-clone'));
    assert.equal(x.pk_apply(dp, dl, bp, bl, mp, ml), 1);
    assert.match(
      text(x.pk_err_ptr, x.pk_err_len),
      /not compatible|outside the anchor|no registry|no number in the allocation table/,
    );
  },
);

test('wasm and CLI produce byte-identical output for core/params and PLUCK', { skip: !need }, async () => {
  const work = mkdtempSync(join(tmpdir(), 'pk-wasm-'));
  const bundlePath = join(work, 'bundle.json');
  py('tools.patchkit.patchkit', 'bundle', '-o', bundlePath);
  const bundle = readFileSync(bundlePath);
  const { x, put, get, text } = await load();
  const mods = 'core/dsp-voice,core/machines,core/params,test/patched-pluck';
  const fw = readFileSync(images.dt2);
  const out = join(work, 'dt2-pluck.syx');
  const plan = join(work, 'dt2-pluck.plan.json');
  py('tools.patchkit.patchkit', 'patch', '--firmware', images.dt2, '--modules', mods, '-o', out, '--plan', plan);
  const [fp, fl] = put(fw);
  const [bp, bl] = put(bundle);
  const [ip, il] = put(Buffer.from(mods));
  assert.equal(x.pk_apply(fp, fl, bp, bl, ip, il), 0, text(x.pk_err_ptr, x.pk_err_len));
  assert.ok(Buffer.from(get(x.pk_out_ptr, x.pk_out_len)).equals(readFileSync(out)), 'wasm and CLI output differ');
  const report = JSON.parse(text(x.pk_plan_ptr, x.pk_plan_len));
  const entries = (name) => report.registries.find((r) => r.name === name).entries.map((e) => [e.id, e.index]);
  // PLUCK's permanent type number is 9; 7 and 8 are gap rows (not machines)
  assert.deepEqual(entries('machines'), [['pluck', 9]]);
  assert.deepEqual(report.registries.find((r) => r.name === 'machines').gaps, [7, 8]);
  assert.deepEqual(entries('params'), [
    ['pluck_decay', 275],
    ['pluck_pick', 276],
    ['pluck_bright', 277],
    ['pluck_color', 278],
  ]);
  // PLUCK builds for Digitone II too (device-filtered entries and objects): the same parity there
  const outDn2 = join(work, 'dn2-pluck.syx');
  py('tools.patchkit.patchkit', 'patch', '--firmware', images.dn2, '--modules', mods, '-o', outDn2);
  const [dp, dl] = put(readFileSync(images.dn2));
  const [mp, ml] = put(Buffer.from(mods));
  assert.equal(x.pk_apply(dp, dl, bp, bl, mp, ml), 0, text(x.pk_err_ptr, x.pk_err_len));
  assert.ok(
    Buffer.from(get(x.pk_out_ptr, x.pk_out_len)).equals(readFileSync(outDn2)),
    'DN2: wasm and CLI output differ',
  );
});

test(
  'wasm and CLI produce byte-identical output for the primitives (registry, section, hook)',
  { skip: !need },
  async () => {
    const work = mkdtempSync(join(tmpdir(), 'pk-wasm-'));
    const bundlePath = join(work, 'bundle.json');
    py('tools.patchkit.patchkit', 'bundle', '-o', bundlePath);
    const bundle = readFileSync(bundlePath);
    const { x, put, get, text } = await load();
    const out = join(work, 'dt2-rd.syx');
    const plan = join(work, 'dt2-rd.plan.json');
    py(
      'tools.patchkit.patchkit',
      'patch',
      '--firmware',
      images.dt2,
      '--modules',
      'test/registry-demo',
      '-o',
      out,
      '--plan',
      plan,
    );
    const [fp, fl] = put(readFileSync(images.dt2));
    const [bp, bl] = put(bundle);
    const [ip, il] = put(Buffer.from('test/registry-demo'));
    assert.equal(x.pk_apply(fp, fl, bp, bl, ip, il), 0, text(x.pk_err_ptr, x.pk_err_len));
    assert.ok(Buffer.from(get(x.pk_out_ptr, x.pk_out_len)).equals(readFileSync(out)), 'wasm and CLI output differ');
    const wasmPlan = text(x.pk_plan_ptr, x.pk_plan_len);
    assert.equal(wasmPlan, readFileSync(plan, 'utf8'), 'plan.json differs');
    const j = JSON.parse(wasmPlan);
    assert.equal(j.registries[0].count, 2);
    assert.equal(j.sections[0].appended_rows, 2);
    assert.equal(j.hooks[0].displaced, 'run');
    // the demo is Digitakt only: its anchor does not exist on the Digitone II
    const [dp, dl] = put(readFileSync(images.dn2));
    assert.equal(x.pk_apply(dp, dl, bp, bl, ip, il), 0, text(x.pk_err_ptr, x.pk_err_len));
    assert.equal(JSON.parse(text(x.pk_plan_ptr, x.pk_plan_len)).hooks, undefined);
  },
);

test('wasm adds the requirements of the selection like the CLI', { skip: !need }, async () => {
  const work = mkdtempSync(join(tmpdir(), 'pk-wasm-'));
  const bundlePath = join(work, 'bundle.json');
  py('tools.patchkit.patchkit', 'bundle', '-o', bundlePath);
  const { x, put, get, text } = await load();
  const [bp, bl] = put(readFileSync(bundlePath));
  for (const dev of ['dt2', 'dn2']) {
    const out = join(work, `${dev}-pluck-only.syx`);
    // the CLI prints what it added
    const said = py(
      'tools.patchkit.patchkit',
      'patch',
      '--firmware',
      images[dev],
      '--modules',
      'test/patched-pluck',
      '-o',
      out,
    );
    assert.match(said, /added by requires: .*core\/machines/);
    const [fp, fl] = put(readFileSync(images[dev]));
    const [ip, il] = put(Buffer.from('test/patched-pluck'));
    assert.equal(x.pk_apply(fp, fl, bp, bl, ip, il), 0, text(x.pk_err_ptr, x.pk_err_len));
    assert.ok(
      Buffer.from(get(x.pk_out_ptr, x.pk_out_len)).equals(readFileSync(out)),
      `${dev}: wasm and CLI output differ`,
    );
  }
  // a missing module is refused with the same words
  const [fp, fl] = put(readFileSync(images.dt2));
  const [ip, il] = put(Buffer.from('machines/nope'));
  assert.equal(x.pk_apply(fp, fl, bp, bl, ip, il), 1);
  assert.match(text(x.pk_err_ptr, x.pk_err_len), /unknown module machines\/nope/);
});

test('wasm refuses what the CLI refuses', { skip: !need }, async () => {
  const work = mkdtempSync(join(tmpdir(), 'pk-wasm-'));
  const bundlePath = join(work, 'bundle.json');
  // the DN2-only oracle is generated under out/modules (it holds a firmware word, so it is not committed)
  py('tools.patchkit.pk_oracle_dn2_half');
  py('tools.patchkit.patchkit', 'bundle', '-o', bundlePath, '--modules-dir', join(root, 'out', 'modules'));
  const { x, put, text } = await load();
  const [fp, fl] = put(readFileSync(images.dt2));
  const [bp, bl] = put(readFileSync(bundlePath));
  const [ip, il] = put(Buffer.from('oracle/dn2-master-half'));
  assert.equal(x.pk_apply(fp, fl, bp, bl, ip, il), 1);
  assert.match(text(x.pk_err_ptr, x.pk_err_len), /not compatible/);
});

// The SHARC+ build runs in the page: the sources of a module as texts in, the `.dko.json` text out, the bytes `patchkit build` writes.
test(
  'wasm and CLI build byte-identical SHARC+ objects (a machine and an effect)',
  { skip: !existsSync(wasmPath) },
  async () => {
    const { x, put, text } = await load();
    const work = mkdtempSync(join(tmpdir(), 'pk-wasm-sharc-'));
    const modules = [
      ['tests/fixtures/modules/dyn-fx-gain', 'build/dyn-fx-gain-dsp.dko.json', 'src/dsp/dyn_fx_gain.s'],
      ['tests/fixtures/modules/sine', 'build/sine-dsp.dko.json', 'src/dsp/sine.s'],
    ];
    for (const [mod, object, source] of modules) {
      py('tools.patchkit.patchkit', 'build', join(root, mod), '--force', '--out', work);
      const expected = readFileSync(join(work, object), 'utf8');
      // the page has no directories: it passes every file the sources include, by the name they use
      const dirs = [
        join(root, mod, 'build/gen'),
        join(root, mod, 'src'),
        join(root, 'sdk/asm'),
        join(root, 'sdk'),
        dirname(join(root, mod, source)),
      ];
      const includes = {};
      const collect = (t) => {
        for (const m of t.matchAll(/^#include\s+"([^"]+)"[ \t]*$/gm)) {
          if (m[1] in includes) continue;
          const f = dirs.map((d) => join(d, m[1])).find((f) => existsSync(f));
          assert.ok(f, `${m[1]} not found`);
          includes[m[1]] = readFileSync(f, 'utf8');
          collect(includes[m[1]]);
        }
      };
      const src = readFileSync(join(root, mod, source), 'utf8');
      collect(src);
      const [ip, il] = put(Buffer.from(JSON.stringify({ sources: [src], includes, defines: [] })));
      assert.equal(x.pk_sharc_object(ip, il), 0, text(x.pk_err_ptr, x.pk_err_len));
      assert.equal(text(x.pk_plan_ptr, x.pk_plan_len), expected, `${mod}: wasm and native objects differ`);
      assert.equal(JSON.parse(expected).arch, 'sharc');
    }
    // a refusal is a message, not a trap
    const [bp, bl] = put(Buffer.from(JSON.stringify({ sources: ['.SECTION/PM seg_pmco;\nDM(15, I4) = I0;\n'] })));
    assert.equal(x.pk_sharc_object(bp, bl), 1);
    assert.match(text(x.pk_err_ptr, x.pk_err_len), /the ISA does not allow/);
  },
);
