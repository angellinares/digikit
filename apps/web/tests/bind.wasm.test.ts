// `pk_bind` of patchkit.wasm through the binder of the Transfer page (`DkBind`, packages/workbench-core/src/modules/dkbind.ts): the built svc-test.dks (a service with a hook) is
// bound to a stock image. Skips when the wasm, the stock images or the packed service are absent (the `.dks` pack of test/svc-test).
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'vitest';
import { fileHashHex } from '@digi/dk-session';
import { DkBind } from '@digi/workbench-core/modules/dkbind.ts';
import { nameHash, readNeeds, serviceFileOf } from '@digi/workbench-core/modules/services.ts';
import { buildBindBundle } from '../scripts/gen-modules.mjs';
import { haveWasmAndImages, images, readBytes, root, wasmPath } from './support/patchkit-env.ts';

const dks = (dev: 'dt2' | 'dn2') => join(root, 'dist', 'modules', dev, 'svc-test.dks');
const have = haveWasmAndImages && (['dt2', 'dn2'] as const).every((d) => existsSync(dks(d)));

test('the bind bundle holds ports and firmware declarations, no firmware', () => {
  const b = buildBindBundle(root);
  assert.ok(b.ports.length > 0 && b.fw.length > 0);
});

test.skipIf(!have)(
  'svc-test.dks binds to each stock image: same service, more bytes, the same bytes again, no second bind',
  async () => {
    const binder = await DkBind.create(readBytes(wasmPath), JSON.stringify(buildBindBundle(root)));
    for (const dev of ['dt2', 'dn2'] as const) {
      const unbound = readBytes(dks(dev));
      const stock = new Uint8Array(readFileSync(images[dev]));
      const a = binder.bind(unbound, stock);
      const b = binder.bind(unbound, stock);
      assert.equal(fileHashHex(a.file), fileHashHex(b.file), `${dev}: binding is deterministic`);
      assert.ok(a.file.length > unbound.length, `${dev}: trampolines were added`);
      assert.ok(a.hooks.length > 0, `${dev}: a hook was bound`);
      const before = serviceFileOf('svc-test', unbound, 'h')!;
      const after = serviceFileOf('svc-test', a.file, 'h')!;
      assert.equal(after.hash32, nameHash('svc-test'));
      assert.equal(after.version, before.version);
      assert.equal(after.abi, before.abi, `${dev}: the ABI fingerprint survives binding`);
      assert.deepEqual(readNeeds(a.file), readNeeds(unbound));
      // a bound file is not bound again
      assert.throws(() => binder.bind(a.file, stock), /already bound/);
    }
    // a file that is no stock image: patchkit's reason comes back
    assert.throws(() => binder.bind(readBytes(dks('dt2')), new Uint8Array(64)), /.+/);
  },
);

test.skipIf(!haveWasmAndImages || !existsSync(join(root, 'dist', 'modules', 'dt2', 'lfo.dks')))(
  'lfo.dks hooks no firmware site: bind returns it as built',
  async () => {
    const binder = await DkBind.create(readBytes(wasmPath), JSON.stringify(buildBindBundle(root)));
    const lfo = readBytes(join(root, 'dist', 'modules', 'dt2', 'lfo.dks'));
    const r = binder.bind(lfo, new Uint8Array(readFileSync(images.dt2)));
    assert.equal(fileHashHex(r.file), fileHashHex(lfo));
    assert.deepEqual(r.hooks, []);
  },
);
