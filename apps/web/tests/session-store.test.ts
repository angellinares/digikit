// The target's read-only store facade (workbench-core src/target) over a real `DkSession` on the byte-exact fake device.
import assert from 'node:assert/strict';
import { DkSession } from '@digi/dk-session';
import { sessionStoreOps } from '@digi/workbench-core/target-ops/session-store.ts';
import { test } from 'vitest';
import { FakeDevice } from '@digi/test-support/fakedevice';
import { dkm } from './support/dkm.ts';

const setup = () => {
  const dev = new FakeDevice();
  dev.storeKib = 2048;
  dev.files.set('modules/a.dkm', dkm('x/a', { size: 1000, salt: 1 }));
  dev.files.set('modules/b.dkm', dkm('x/b', { size: 2000, salt: 2, role: 1 }));
  dev.files.set(
    'data/x/state.bin',
    Uint8Array.from({ length: 1700 }, (_, i) => (i * 7) & 255),
  );
  const s = new DkSession(dev, { timeoutMs: 20, retries: 1 });
  return { dev, ops: sessionStoreOps(async () => s) };
};

test('session store: snapshot lists the whole store and groups the usage by category', async () => {
  const { ops } = setup();
  const snap = await ops.snapshot();
  assert.deepEqual(
    snap.entries.map((e) => [e.path, e.size]),
    [
      ['data/x/state.bin', 1700],
      ['modules/a.dkm', 1000],
      ['modules/b.dkm', 2000],
    ],
  );
  assert.deepEqual(
    snap.usage.used.map((u) => [u.key, u.bytes, u.files]),
    [
      ['modules', 3000, 2],
      ['data', 1700, 1],
    ],
  );
  assert.equal(snap.usage.total, 2048 * 1024);
  assert.equal(snap.usage.free, 1024 * 1024);
  assert.equal(snap.facts.device, 'dt2');
  assert.equal(snap.facts.runtime, '0.2.4');
  assert.deepEqual(snap.states, []);
});

test('session store: header decodes name, role and version', async () => {
  const { ops } = setup();
  const a = await ops.header('modules/a.dkm');
  assert.deepEqual([a?.name, a?.role, a?.version], ['x/a', 'module', '0.1.2']);
  assert.equal((await ops.header('modules/b.dkm'))?.role, 'service');
});

test('session store: readAll returns the exact bytes in chunks, with progress', async () => {
  const { dev, ops } = setup();
  const calls: [number, number][] = [];
  const got = await ops.readAll('data/x/state.bin', (done, size) => calls.push([done, size]));
  assert.deepEqual(got, dev.files.get('data/x/state.bin'));
  assert.deepEqual(calls, [
    [768, 1700],
    [1536, 1700],
    [1700, 1700],
  ]);
});
