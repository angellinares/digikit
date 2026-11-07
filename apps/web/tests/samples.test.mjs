// The samples API (samples.ts) over both backends with fake transports: the backend follows the profile's `[storage] samples` value, and the same calls give the same
// shapes on a fake +Drive (stock MidiRpc, fakedrive.mjs) and a fake digikit store session.
import assert from 'node:assert/strict';
import test from 'node:test';
import { DkError, DkSession, fileHashHex } from '@digi/dk-session';
import { VERIFY_PIECE, openSamples } from '@digi/dk-session/samples';
import { FakeDrive, hashOf } from './fakedrive.mjs';

const sha = fileHashHex;
const ZERO = '0'.repeat(64);
const FOLDERS = 1 << 10;
const CAPACITY = 1 << 6;

/** The part of `DkSession` the digikit backend uses, on an in-memory store; `calls` records the commands. */
class FakeStore {
  constructor({ caps = FOLDERS | CAPACITY, limitKib = 1000 } = {}) {
    this.caps = caps;
    this.limitKib = limitKib;
    this.files = new Map();
    this.dirs = new Set();
    this.calls = [];
    this.corrupt = null; // [path, offset]: the card holds a flipped byte there
  }
  async info() {
    this.calls.push('info');
    return { caps: this.caps, free_bytes: 123 };
  }
  async list(prefix) {
    this.calls.push(prefix === undefined ? 'list' : `list ${prefix}`);
    const rows = [
      ...[...this.files].map(([path, b]) => ({ path, size: b.length, hash: sha(b) })),
      ...[...this.dirs].map((path) => ({ path, size: 0, hash: ZERO })),
    ];
    return rows.filter((e) => !prefix || e.path.startsWith(prefix));
  }
  async mkdir(path) {
    this.calls.push(`mkdir ${path}`);
    this.dirs.add(path);
  }
  async put(path, data, onProgress = () => {}) {
    this.calls.push(`put ${path}`);
    for (let at = 0; at < data.length; at += 100) onProgress(Math.min(data.length, at + 100), data.length);
    this.files.set(path, Uint8Array.from(data));
    for (const d of this.dirs) if (path.startsWith(d + '/')) this.dirs.delete(d);
  }
  async delete(path) {
    this.calls.push(`rm ${path}`);
    if (!this.files.delete(path) && !this.dirs.delete(path))
      throw new DkError({ error: 'rm: E_NOFILE', kind: 'device', status: 'E_NOFILE' });
  }
  async verify(path, offset, length) {
    this.calls.push(`verify ${path}${offset === undefined ? '' : ` ${offset} ${length}`}`);
    const b = this.files.get(path);
    if (!b) throw new DkError({ error: 'verify: E_NOFILE', kind: 'device', status: 'E_NOFILE' });
    const card = Uint8Array.from(b);
    if (this.corrupt && this.corrupt[0] === path) card[this.corrupt[1]] ^= 1;
    return sha(offset === undefined ? card : card.subarray(offset, offset + length));
  }
  async capacity() {
    const used = Math.ceil([...this.files.values()].reduce((n, b) => n + b.length, 0) / 1024);
    return { STORE: { limit: this.limitKib, used, largest: 0 } };
  }
}

const bytes = (n, salt = 1) => Uint8Array.from({ length: n }, (_, i) => (i * 31 + salt) & 255);
/** What `prepareUpload` returns for a sample: the bytes over MidiRpc and the file the store holds (with the 16-byte trailer). */
const prepared = (name, n, salt) => {
  const b = bytes(n, salt);
  const stored = new Uint8Array(n + 16);
  stored.set(b);
  return { name, bytes: b, stored, kind: 'converted' };
};

/** One session that serves both backends and records which side was used. */
const session = ({ store = new FakeStore() } = {}) => {
  const drive = new FakeDrive();
  const rpc = new DkSession(drive, { timeoutMs: 50 });
  const used = { rpc: 0, store: 0 };
  const s = { rpc: (...a) => (used.rpc++, rpc.rpc(...a)) };
  for (const m of ['info', 'list', 'mkdir', 'put', 'delete', 'verify', 'capacity'])
    s[m] = (...a) => (used.store++, store[m](...a));
  return { s, drive, store, used };
};

test('the backend follows the profile value; an unknown value or a missing hash is refused', async () => {
  const a = session();
  const rpc = openSamples('stock-rpc', { session: a.s, hashOf });
  assert.equal(rpc.kind, 'stock-rpc');
  await rpc.list('');
  assert.deepEqual(a.used.store, 0);
  assert.ok(a.used.rpc > 0);
  const b = session();
  const store = openSamples('digikit', { session: b.s });
  assert.equal(store.kind, 'digikit');
  await store.list('');
  assert.equal(b.used.rpc, 0);
  assert.ok(b.used.store > 0);
  assert.throws(() => openSamples('stock-rpc', { session: a.s }), /content hash/);
  assert.throws(() => openSamples('flash', { session: a.s, hashOf }), /unknown samples backend "flash"/);
});

for (const kind of ['stock-rpc', 'digikit']) {
  test(`${kind}: folder, upload with progress, list, space, verify (ok, other bytes, missing), delete`, async () => {
    const { s } = session();
    const dev = openSamples(kind, { session: s, hashOf });
    const n = 5000;
    const f = prepared('kick', n, 1);
    assert.equal(await dev.canMkdir(), true);
    await dev.mkdir('drums');
    assert.deepEqual(
      await dev.list(''),
      kind === 'stock-rpc'
        ? [
            { name: 'factory', kind: 'folder', size: 0 },
            { name: 'drums', kind: 'folder', size: 0 },
          ]
        : [{ name: 'drums', kind: 'folder', size: 0 }],
    );
    const before = await dev.space();
    const seen = [];
    await dev.put('drums', f, (sent, total) => seen.push([sent, total]));
    assert.equal(seen.at(-1)[0], seen.at(-1)[1]);
    assert.ok(seen.length >= 3, 'several chunks');
    // the size the device reports: without the trailer on the +Drive, the stored file on the store
    assert.deepEqual(await dev.list('drums'), [
      { name: 'kick', kind: 'file', size: kind === 'stock-rpc' ? n : n + 16 },
    ]);
    assert.deepEqual(
      (await dev.list('')).filter((e) => e.name === 'drums'),
      [{ name: 'drums', kind: 'folder', size: 0 }],
    );
    if (kind === 'digikit') {
      assert.equal((await dev.space()).total, 1000 * 1024);
      assert.ok((await dev.space()).free < before.free, 'the upload took space');
    }

    const ok = await dev.verify('drums', f);
    assert.equal(ok.ok, true);
    assert.equal(ok.message, kind === 'stock-rpc' ? 'device content hash matches' : 'device hash matches');
    assert.equal(ok.device, ok.host);
    assert.equal(ok.sent, kind === 'stock-rpc' ? n : n + 16);
    assert.equal(ok.mismatchAt, null);
    const other = { ...prepared('kick', n, 9) };
    const bad = await dev.verify('drums', other);
    assert.deepEqual([bad.ok, bad.device === bad.host, bad.device !== null], [false, false, true]);
    assert.match(bad.message, /differs/);
    const none = await dev.verify('drums', prepared('snare', 10, 1));
    assert.deepEqual(
      [none.ok, none.device, none.size, none.message],
      [false, null, null, 'the device does not have the file'],
    );

    await dev.remove('drums', 'kick');
    assert.deepEqual(await dev.list('drums'), []);
    if (kind === 'digikit') assert.equal((await dev.space()).free, before.free, 'the delete freed the space');
    // a folder that held a file has no entry of its own once it is empty: only a made folder is removed
    await dev.mkdir('tmp');
    await dev.rmdir('tmp');
    assert.deepEqual(
      (await dev.list('')).filter((e) => e.name === 'tmp'),
      [],
    );
  });
}

test('digikit: the same verify result keys as the +Drive', async () => {
  const a = session();
  const keys = [];
  for (const kind of ['stock-rpc', 'digikit']) {
    const dev = openSamples(kind, { session: a.s, hashOf });
    const f = prepared('x', 40, 3);
    await dev.put('k', f);
    keys.push(Object.keys(await dev.verify('k', f)).sort());
  }
  assert.deepEqual(keys[0], keys[1]);
});

test('digikit: samples/ is the folder, a prefix matches whole names, folders are listed one level at a time', async () => {
  const { s, store } = session();
  const dev = openSamples('digikit', { session: s });
  await dev.put('drums', prepared('a', 20));
  await dev.put('drums/kicks', prepared('b', 20));
  await dev.put('drumsticks', prepared('c', 20));
  await dev.mkdir('empty');
  assert.deepEqual([...store.files.keys()].sort(), [
    'samples/drums/a',
    'samples/drums/kicks/b',
    'samples/drumsticks/c',
  ]);
  assert.deepEqual((await dev.list('')).map((e) => `${e.kind}:${e.name}`).sort(), [
    'folder:drums',
    'folder:drumsticks',
    'folder:empty',
  ]);
  assert.deepEqual((await dev.list('drums')).map((e) => `${e.kind}:${e.name}`).sort(), ['file:a', 'folder:kicks']);
  assert.ok(store.calls.includes('list samples/drums/'), 'LIST by prefix, with the slash');
  await assert.rejects(dev.put('x', { ...prepared('a/b', 1) }), /not a file name/);
  await assert.rejects(dev.remove('drums', 'nothing'), (e) => e.status === 'E_NOFILE');
});

test('digikit without the FOLDERS capability: no mkdir, the host filters a full LIST', async () => {
  const { s, store } = session({ store: new FakeStore({ caps: 0 }) });
  const dev = openSamples('digikit', { session: s });
  assert.equal(await dev.canMkdir(), false);
  await assert.rejects(dev.mkdir('x'), /no folder entries/);
  await dev.put('drums', prepared('a', 20));
  store.files.set('modules/m.dkm', bytes(5));
  assert.deepEqual(await dev.list('drums'), [{ name: 'a', kind: 'file', size: 36 }]);
  assert.ok(store.calls.includes('list') && !store.calls.some((c) => c.startsWith('list samples')));
});

test('digikit: a store above 4 GiB reports its free space in bytes (KiB from CAPACITY, no 32-bit saturation)', async () => {
  const { s } = session({ store: new FakeStore({ limitKib: 20_000_000 }) });
  const space = await openSamples('digikit', { session: s }).space();
  assert.deepEqual(space, { free: 20_000_000 * 1024, total: 20_000_000 * 1024 });
  assert.ok(space.free > 2 ** 32);
});

test('digikit: a large file is verified in ranges and reports where the first one differs', async () => {
  const { s, store } = session();
  const dev = openSamples('digikit', { session: s });
  const f = prepared('long', 2.5 * VERIFY_PIECE, 5);
  await dev.put('', f);
  const ok = await dev.verify('', f);
  assert.equal(ok.ok, true);
  assert.equal(store.calls.filter((c) => c.startsWith('verify samples/long ')).length, 3);
  store.corrupt = ['samples/long', 2 * VERIFY_PIECE + 5];
  const bad = await dev.verify('', f);
  assert.deepEqual([bad.ok, bad.mismatchAt], [false, 2 * VERIFY_PIECE]);
  assert.notEqual(bad.device, bad.host);
  // a file of another size fails even when the range sent matches
  store.corrupt = null;
  store.files.set('samples/long', new Uint8Array([...store.files.get('samples/long'), 1]));
  const longer = await dev.verify('', f);
  assert.deepEqual([longer.ok, longer.message], [false, 'the device file has another size']);
});
