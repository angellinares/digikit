// The stock firmware kept for binding (src/firmware/stock-store.ts) and the service binder over it (src/modules/dkbind.ts), behind fakes.
import assert from 'node:assert/strict';
import { test } from 'vitest';
import { memoryBlobStore } from '../src/blob-store.ts';
import { createStockStore } from '../src/firmware/stock-store.ts';
import { createServiceBinder, type DkBind } from '../src/modules/dkbind.ts';

const bytes = (s: string) => new TextEncoder().encode(s);

test('the stock store keeps the newest image per device and drops the replaced one', async () => {
  const blobs = memoryBlobStore();
  const stock = createStockStore(blobs);
  assert.equal(await stock.hashOf('dt2'), undefined);
  await stock.remember('dt2', 'aaa', bytes('one'));
  await stock.remember('dn2', 'bbb', bytes('two'));
  assert.equal(await stock.hashOf('dt2'), 'aaa');
  assert.deepEqual(await stock.bytesOf('aaa'), bytes('one'));
  await stock.remember('dt2', 'ccc', bytes('three'));
  assert.equal(await stock.hashOf('dt2'), 'ccc');
  assert.equal(await stock.bytesOf('aaa'), undefined);
  assert.deepEqual(await stock.bytesOf('bbb'), bytes('two'));
});

test('the service binder binds against the kept bytes of the hash, loads the wasm once and says when the image is not kept', async () => {
  const stock = createStockStore(memoryBlobStore());
  await stock.remember('dt2', 'aaa', bytes('stock'));
  let made = 0;
  const seen: string[] = [];
  const fake = {
    bind: (dks: Uint8Array, fw: Uint8Array) => {
      if (fw.length === 0) throw new Error('no image');
      seen.push(`${new TextDecoder().decode(dks)}+${new TextDecoder().decode(fw)}`);
      return { file: bytes('bound'), hooks: [] };
    },
  } as unknown as DkBind;
  const binder = createServiceBinder(async () => {
    made++;
    return fake;
  }, stock);
  assert.deepEqual(await binder.bind(bytes('fx'), 'aaa'), bytes('bound'));
  await binder.bind(bytes('lfo'), 'aaa');
  assert.deepEqual(seen, ['fx+stock', 'lfo+stock']);
  assert.equal(made, 1);
  await assert.rejects(binder.bind(bytes('fx'), 'zzz'), /not kept here/);
});

test('a failed load of the binder is tried again at the next use', async () => {
  const stock = createStockStore(memoryBlobStore());
  await stock.remember('dt2', 'aaa', bytes('stock'));
  let tries = 0;
  const binder = createServiceBinder(async () => {
    if (++tries === 1) throw new Error('wasm missing');
    return { bind: () => ({ file: bytes('ok'), hooks: [] }) } as unknown as DkBind;
  }, stock);
  await assert.rejects(binder.bind(bytes('x'), 'aaa'), /wasm missing/);
  assert.deepEqual(await binder.bind(bytes('x'), 'aaa'), bytes('ok'));
});
