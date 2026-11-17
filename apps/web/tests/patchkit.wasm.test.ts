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

test.skipIf(!need)(
  'wasm and CLI produce byte-identical output for core/machines and core/params on each device',
  async () => {
    const work = workDir('pk-wasm-');
    const bundle = readFileSync(writeBundle(work));
    const { x, put, get, text } = await load();
    const mods = 'core/machines,core/params';
    for (const dev of ['dt2', 'dn2'] as const) {
      const out = join(work, `${dev}-machines.syx`);
      const plan = join(work, `${dev}-machines.plan.json`);
      py('tools.patchkit.patchkit', 'patch', '--firmware', images[dev], '--modules', mods, '-o', out, '--plan', plan);
      const [fp, fl] = put(readFileSync(images[dev]));
      const [bp, bl] = put(bundle);
      const [ip, il] = put(Buffer.from(mods));
      assert.equal(x.pk_apply(fp, fl, bp, bl, ip, il), 0, text(x.pk_err_ptr, x.pk_err_len));
      assert.ok(
        Buffer.from(get(x.pk_out_ptr, x.pk_out_len)).equals(readFileSync(out)),
        `${dev}: wasm and CLI output differ`,
      );
      const wasmPlan = text(x.pk_plan_ptr, x.pk_plan_len);
      assert.equal(wasmPlan, readFileSync(plan, 'utf8'), `${dev}: plan.json differs`);
      // no module adds rows at patch time: the registries exist and are empty
      const registries = JSON.parse(wasmPlan).registries as Registry[];
      for (const name of ['machines', 'params']) {
        assert.deepEqual(
          registries.find((r) => r.name === name)?.entries,
          [],
          `${dev}: ${name} has no patch-time rows`,
        );
      }
    }
  },
);

test.skipIf(!need)(
  'wasm adds core/machines and core/params as requirements of core/dynreg like the CLI and the plan reserves the run-time rows',
  async () => {
    const work = workDir('pk-wasm-');
    const { x, put, get, text } = await load();
    const [bp, bl] = put(readFileSync(writeBundle(work)));
    // the first parameter code and the reserved page rows differ by device (the machine rows are the types 16 to 23 on both)
    const want = {
      dt2: { gaps: [7, 8, 9, 10, 11, 12, 13, 14, 15], firstParam: 275, pages: 0 },
      dn2: { gaps: [5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15], firstParam: 321, pages: 8 },
    } as const;
    for (const dev of ['dt2', 'dn2'] as const) {
      const out = join(work, `${dev}-dynreg.syx`);
      const plan = join(work, `${dev}-dynreg.plan.json`);
      // the CLI prints what it added
      const said = py(
        'tools.patchkit.patchkit',
        'patch',
        '--firmware',
        images[dev],
        '--modules',
        'core/dynreg',
        '-o',
        out,
        '--plan',
        plan,
      );
      assert.match(said, /added by requires: .*core\/machines/);
      const [fp, fl] = put(readFileSync(images[dev]));
      const [ip, il] = put(Buffer.from('core/dynreg'));
      assert.equal(x.pk_apply(fp, fl, bp, bl, ip, il), 0, text(x.pk_err_ptr, x.pk_err_len));
      assert.ok(
        Buffer.from(get(x.pk_out_ptr, x.pk_out_len)).equals(readFileSync(out)),
        `${dev}: wasm and CLI output differ`,
      );
      const wasmPlan = text(x.pk_plan_ptr, x.pk_plan_len);
      assert.equal(wasmPlan, readFileSync(plan, 'utf8'), `${dev}: plan.json differs`);
      const registries = JSON.parse(wasmPlan).registries as Registry[];
      const find = (name: string) => registries.find((r) => r.name === name)!;
      const machines = find('machines');
      assert.deepEqual(
        machines.entries.map((e) => [e.id, e.index]),
        Array.from({ length: 8 }, (_, i) => [`dyn${16 + i}`, 16 + i]),
        `${dev}: eight reserved run-time machine rows`,
      );
      assert.deepEqual(machines.gaps, want[dev].gaps, `${dev}: the rows between the stock and the run-time types`);
      const params = find('params').entries;
      assert.equal(params.length, 16, `${dev}: sixteen reserved parameter rows`);
      assert.deepEqual(
        [params[0]!.id, params[0]!.index],
        ['dp0', want[dev].firstParam],
        `${dev}: first reserved parameter code`,
      );
      assert.equal(
        registries.find((r) => r.name === 'pages')?.entries.length ?? 0,
        want[dev].pages,
        `${dev}: reserved page rows`,
      );
    }
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
    ['dt2', 'core/dynfx'],
    ['dn2', 'core/dynfx'],
  ] as const) {
    const out = join(work, `${dev}-machine-only.syx`);
    // the CLI prints what it added
    const said = py('tools.patchkit.patchkit', 'patch', '--firmware', images[dev], '--modules', mod, '-o', out);
    assert.match(said, /added by requires: .*core\/fx/);
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
