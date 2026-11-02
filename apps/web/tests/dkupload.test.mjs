// The module upload client (dkupload.wasm + the TS driver) against the wasm mock device, and over a mock Web MIDI
// access that answers like the device. Skips when dkupload.wasm is not built (pnpm dev builds it).
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { Dk, DkClient, DkError, MockTransport, repackMissing, storePath, hexSha256 } from '@digi/dk-session';
import { WebMidiTransport, listPorts, portLabel } from '../src/modules/webmidi.ts';

const wasm = join(resolve(dirname(fileURLToPath(import.meta.url)), '..'), 'public/dkupload.wasm');
const skip = !existsSync(wasm);
const data = (n, salt) => Uint8Array.from({ length: n }, (_, i) => ((i * 2654435761) >>> 24) ^ salt);
const setup = async (capacity = 1 << 20) => {
  const dk = await Dk.create(readFileSync(wasm));
  const t = new MockTransport(dk, capacity);
  return { dk, t, c: new DkClient(dk, t, { timeoutMs: 20 }) };
};

test('info, put, list, delete', { skip }, async () => {
  const { c } = await setup();
  const info = await c.info();
  assert.equal(info.protocol, 1);
  assert.equal(info.max_chunk, 32768);
  const sizes = [0, 1, 512, 32769, 100000];
  for (const [k, n] of sizes.entries()) await c.put(`modules/f${k}.dkm`, data(n, k));
  const list = await c.list();
  assert.equal(list.length, 5);
  for (const [k, n] of sizes.entries()) {
    const e = list.find((x) => x.path === `modules/f${k}.dkm`);
    assert.equal(e.size, n);
    assert.equal(e.sha256, await hexSha256(data(n, k)));
  }
  await c.delete('modules/f1.dkm');
  assert.equal((await c.list()).length, 4);
  await assert.rejects(c.delete('modules/f1.dkm'), (e) => e instanceof DkError && e.status === 'E_NOFILE');
});

test('overlapping requests both succeed; a quick INFO to a silent device fails at once', { skip }, async () => {
  const { c } = await setup();
  const [info, list] = await Promise.all([c.info(), c.list()]);
  assert.equal(info.protocol, 1);
  assert.deepEqual(list, []);
  const dk = await Dk.create(readFileSync(wasm));
  let sends = 0;
  const silent = { send: () => void sends++, recv: (ms) => new Promise((r) => setTimeout(() => r(null), ms)) };
  const quiet = new DkClient(dk, silent, { timeoutMs: 5000, retries: 2 });
  const t0 = Date.now();
  await assert.rejects(quiet.info(true), (e) => e instanceof DkError && e.kind === 'timeout');
  assert.equal(sends, 1, 'no retries');
  assert.ok(Date.now() - t0 < 1500, 'about 1 s, not 15 s');
});

test('lost reply and lost request are retried', { skip }, async () => {
  const { dk, t, c } = await setup();
  const d = data(5 * 32768 + 7, 9);
  for (const [kind, n] of [
    [1, 1],
    [0, 1],
    [1, 3],
  ]) {
    dk.mockCtl(t.handle, kind, n);
    await c.put(`modules/k${kind}${n}.dkm`, d);
  }
  const files = dk.mockFiles(t.handle);
  assert.equal(files.length, 3);
  assert.ok(files.every((f) => f.size === d.length));
});

test('E_FULL carries the free bytes; delete frees space; repack; progress and cancel', { skip }, async () => {
  const { c } = await setup(100_000);
  const a = data(30_000, 1),
    b = data(30_000, 2),
    big = data(40_000, 3);
  await c.put('modules/a.dkm', a);
  await c.put('modules/b.dkm', b);
  // a replacement takes the old file's space (store format 3)
  await c.put('modules/a.dkm', a);
  await c.put('modules/b.dkm', b);
  await assert.rejects(c.put('modules/big.dkm', big), (e) => e.status === 'E_FULL' && e.freeBytes > 0);
  await c.delete('modules/a.dkm');
  await c.put('modules/big.dkm', big);
  const listing = await c.list();
  const lib = [{ path: 'modules/b.dkm', sha256: await hexSha256(b) }];
  assert.deepEqual(repackMissing(listing, lib), ['modules/big.dkm']);
  await c.repack([
    { path: 'modules/a.dkm', data: a },
    { path: 'modules/b.dkm', data: b },
  ]);
  assert.equal((await c.list()).length, 2);
  let seen = 0;
  await assert.rejects(
    c.put('modules/c.dkm', data(20_000, 3), ({ done, total }) => {
      assert.ok(done <= total);
      return ++seen < 2;
    }),
    (e) => e.kind === 'cancelled',
  );
  assert.equal((await c.info()).upload_open, false);
});

test('bad paths are refused before sending; names map to store paths', { skip }, async () => {
  const { c } = await setup();
  await assert.rejects(c.put('a/../b', data(4, 0)), (e) => e.kind === 'request');
  assert.equal(storePath('Dyn Pluck.dkm'), 'modules/Dyn Pluck.dkm');
  assert.equal(storePath('é/x.dkm'), 'modules/__x.dkm');
});

/** A Web MIDI access whose output feeds the wasm mock device and whose input gets the replies in pieces. */
function midiAccess(dk, mock) {
  const input = { id: 'in1', name: 'Mock In', manufacturer: 'mock', onmidimessage: null, open: async () => {} };
  const sent = [];
  const output = {
    id: 'out1',
    name: 'Mock Out',
    open: async () => {},
    send(m) {
      sent.push(m.length);
      const r = dk.mockHandle(mock, Uint8Array.from(m));
      if (!r) return;
      // deliver in pieces, with a real-time byte in the middle
      const cut = [0, 5, r.length - 3, r.length];
      cut.slice(1).forEach((end, i) =>
        setTimeout(() => {
          const piece = r.slice(cut[i], end);
          input.onmidimessage?.({ data: i === 1 ? Uint8Array.from([0xf8, ...piece]) : piece });
        }, i),
      );
    },
  };
  return { access: { inputs: new Map([['in1', input]]), outputs: new Map([['out1', output]]) }, sent };
}

test('over Web MIDI: sysex pieces are assembled', { skip }, async () => {
  const dk = await Dk.create(readFileSync(wasm));
  const mock = dk.mockNew(1 << 20);
  const { access, sent } = midiAccess(dk, mock);
  const { inputs, outputs } = listPorts(access);
  assert.equal(portLabel(inputs[0]), 'mock Mock In');
  const t = await WebMidiTransport.open(inputs[0], outputs[0]);
  const c = new DkClient(dk, t, { timeoutMs: 200 });
  assert.equal((await c.info()).port, 'mock-0.0');
  const d = data(70_000, 4);
  await c.put('modules/web.dkm', d);
  assert.deepEqual(
    (await c.list()).map((e) => e.path),
    ['modules/web.dkm'],
  );
  assert.ok(sent.every((n) => n <= 40 * 1024));
  assert.equal(dk.mockFiles(mock)[0].size, d.length);
  t.close();
});
