// patchkit.wasm and the patchkit command line apply the same modules to the same stock image and must
// produce byte-identical output (and the same plan). Skips when the wasm, a stock image or the CLI is absent.
import assert from 'node:assert/strict';
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { test, vi } from 'vitest';
import { haveWasmAndImages as need, images, py, root, wasmPath, workDir, writeBundle } from './support/patchkit-env.ts';

// Most tests here run the patchkit command line with execFileSync (two full patches per image for core/fx).
// Vitest checks the limit only after a synchronous call returns, so the project's 120 s limit failed a correct
// test that took 408 s on a loaded machine. The limit fits that work.
vi.setConfig({ testTimeout: 900_000 });

type Registry = { name: string; entries: { id: string; index: number }[]; gaps?: number[]; count?: number };

/** The raw exports of patchkit.wasm (`pk_*`), with the helpers that pass bytes in and read results out. */
async function load() {
  const { instance } = await WebAssembly.instantiate(new Uint8Array(readFileSync(wasmPath)), {});
  const x = instance.exports as Record<string, any> & { memory: WebAssembly.Memory };
  const put = (bytes: ArrayLike<number>): [number, number] => {
    const p = x.pk_alloc(bytes.length);
    new Uint8Array(x.memory.buffer, p, bytes.length).set(bytes);
    return [p, bytes.length];
  };
  const get = (ptr: () => number, len: () => number) => new Uint8Array(x.memory.buffer.slice(ptr(), ptr() + len()));
  const text = (ptr: () => number, len: () => number) => new TextDecoder().decode(get(ptr, len));
  return { x, put, get, text };
}

test('patchkit.wasm carries no firmware', () => {
  if (!existsSync(wasmPath)) return;
  const bytes = readFileSync(wasmPath);
  // no stock-image hash and no port: those are passed in at run time
  for (const f of readdirSync(join(root, 'devices/ports'))) {
    const sha = /syx_sha256 = "([0-9a-f]{64})"/.exec(readFileSync(join(root, 'devices/ports', f), 'utf8'))![1]!;
    assert.equal(Buffer.from(bytes).includes(Buffer.from(sha)), false, `${f} sha embedded`);
  }
});

test.skipIf(!need)('wasm and CLI produce byte-identical output for core/fx', async () => {
  const work = workDir('pk-wasm-');
  const bundle = readFileSync(writeBundle(work));
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

test.skipIf(!need)('wasm and CLI produce byte-identical output for the machine framework (two machines)', async () => {
  const work = workDir('pk-wasm-');
  const bundle = readFileSync(writeBundle(work));
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
  const entries = (name: string) =>
    (report.registries as Registry[]).find((r) => r.name === name)!.entries.map((e) => [e.id, e.index]);
  assert.deepEqual(entries('machines'), [['xclone', 8]]); // Digitone II has core/machines, but XClone clones a Digitakt machine (MANUAL SLICE, type 6)
  const [dp, dl] = put(readFileSync(images.dn2));
  const [mp, ml] = put(Buffer.from('core/machines,test/machine-clone'));
  assert.equal(x.pk_apply(dp, dl, bp, bl, mp, ml), 1);
  assert.match(
    text(x.pk_err_ptr, x.pk_err_len),
    /not compatible|outside the anchor|no registry|no number in the allocation table/,
  );
});

test.skipIf(!need)(
  'wasm and CLI produce byte-identical output for core/params and a machine on each device',
  async () => {
    const work = workDir('pk-wasm-');
    const bundle = readFileSync(writeBundle(work));
    const { x, put, get, text } = await load();
    const mods = 'core/dsp-voice,core/machines,core/params,test/machine-clone';
    const fw = readFileSync(images.dt2);
    const out = join(work, 'dt2-clone.syx');
    const plan = join(work, 'dt2-clone.plan.json');
    py('tools.patchkit.patchkit', 'patch', '--firmware', images.dt2, '--modules', mods, '-o', out, '--plan', plan);
    const [fp, fl] = put(fw);
    const [bp, bl] = put(bundle);
    const [ip, il] = put(Buffer.from(mods));
    assert.equal(x.pk_apply(fp, fl, bp, bl, ip, il), 0, text(x.pk_err_ptr, x.pk_err_len));
    assert.ok(Buffer.from(get(x.pk_out_ptr, x.pk_out_len)).equals(readFileSync(out)), 'wasm and CLI output differ');
    const report = JSON.parse(text(x.pk_plan_ptr, x.pk_plan_len));
    const entries = (name: string) =>
      (report.registries as Registry[]).find((r) => r.name === name)!.entries.map((e) => [e.id, e.index]);
    // the clone's permanent type number is 8; 7 is a gap row (not a machine)
    assert.deepEqual(entries('machines'), [['xclone', 8]]);
    assert.deepEqual((report.registries as Registry[]).find((r) => r.name === 'machines')!.gaps, [7]);
    assert.deepEqual(entries('params'), []);
    // the Digitone II machine with its own page and parameters: the same parity there
    const modsDn2 = 'core/dsp-voice,core/machines,core/params,test/dn2-pages';
    const outDn2 = join(work, 'dn2-pages.syx');
    const planDn2 = join(work, 'dn2-pages.plan.json');
    py(
      'tools.patchkit.patchkit',
      'patch',
      '--firmware',
      images.dn2,
      '--modules',
      modsDn2,
      '-o',
      outDn2,
      '--plan',
      planDn2,
    );
    const [dp, dl] = put(readFileSync(images.dn2));
    const [mp, ml] = put(Buffer.from(modsDn2));
    assert.equal(x.pk_apply(dp, dl, bp, bl, mp, ml), 0, text(x.pk_err_ptr, x.pk_err_len));
    assert.ok(
      Buffer.from(get(x.pk_out_ptr, x.pk_out_len)).equals(readFileSync(outDn2)),
      'DN2: wasm and CLI output differ',
    );
    const reportDn2 = JSON.parse(text(x.pk_plan_ptr, x.pk_plan_len));
    const entriesDn2 = (name: string) =>
      (reportDn2.registries as Registry[]).find((r) => r.name === name)!.entries.map((e) => [e.id, e.index]);
    assert.deepEqual(entriesDn2('machines'), [['pages', 6]]);
    assert.deepEqual((reportDn2.registries as Registry[]).find((r) => r.name === 'machines')!.gaps, [5]);
    assert.deepEqual(entriesDn2('params'), [
      ['pg_alpha', 321],
      ['pg_beta', 322],
      ['pg_gamma', 323],
      ['pg_delta', 324],
      ['pg_eps', 325],
      ['pg_zeta', 326],
    ]);
  },
);

test.skipIf(!need)(
  'wasm and CLI produce byte-identical output for the primitives (registry, section, hook)',
  async () => {
    const work = workDir('pk-wasm-');
    const bundle = readFileSync(writeBundle(work));
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

test.skipIf(!need)('wasm adds the requirements of the selection like the CLI', async () => {
  const work = workDir('pk-wasm-');
  const { x, put, get, text } = await load();
  const [bp, bl] = put(readFileSync(writeBundle(work)));
  for (const [dev, mod] of [
    ['dt2', 'test/machine-clone'],
    ['dn2', 'test/dn2-pages'],
  ] as const) {
    const out = join(work, `${dev}-machine-only.syx`);
    // the CLI prints what it added
    const said = py('tools.patchkit.patchkit', 'patch', '--firmware', images[dev], '--modules', mod, '-o', out);
    assert.match(said, /added by requires: .*core\/machines/);
    const [fp, fl] = put(readFileSync(images[dev]));
    const [ip, il] = put(Buffer.from(mod));
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

test.skipIf(!need)('wasm refuses what the CLI refuses', async () => {
  const work = workDir('pk-wasm-');
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
test.skipIf(!existsSync(wasmPath))(
  'wasm and CLI build byte-identical SHARC+ objects (a machine and an effect)',
  async () => {
    const { x, put, text } = await load();
    const work = workDir('pk-wasm-sharc-');
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
        join(root, 'sdk/dsp/contract'),
        join(root, 'sdk'),
        dirname(join(root, mod, source)),
      ];
      const includes: Record<string, string> = {};
      const collect = (t: string) => {
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
    assert.match(text(x.pk_err_ptr, x.pk_err_len), /^selas failed:/);
  },
);
