// The pure store helpers (src/target-ops/store.ts) and the in-memory store (src/target-ops/fake-store.ts).
import assert from 'node:assert/strict';
import { test } from 'vitest';
import { fileHashHex, type DkEntry } from '@digi/dk-session';
import { fakeDkm, fakeStoreOps } from '../src/target-ops/fake-store.ts';
import {
  buildTree,
  categoryOf,
  hexDump,
  libraryMarks,
  orphanPaths,
  parseDkmHeader,
  usageOf,
  visibleRows,
} from '../src/target-ops/store.ts';

const file = (path: string, size: number): DkEntry => ({ path, size, hash: 'ab'.repeat(32) });
const dir = (path: string): DkEntry => ({ path, size: 0, hash: '0'.repeat(64) });

test('categoryOf: the store prefixes, anything else is other', () => {
  assert.equal(categoryOf('modules/a.dkm'), 'modules');
  assert.equal(categoryOf('services/a.dks'), 'services');
  assert.equal(categoryOf('data/x/y'), 'data');
  assert.equal(categoryOf('samples/k/a.wav'), 'samples');
  assert.equal(categoryOf('settings'), 'other');
  assert.equal(categoryOf('modulesx/a'), 'other');
});

test('usageOf: sums by category in order, overhead, null total, folder entries ignored', () => {
  const entries = [
    file('samples/a.wav', 300),
    file('modules/a.dkm', 100),
    file('modules/b.dkm', 50),
    dir('samples/new'),
  ];
  const u = usageOf(entries, { free: 400, total: 1000 });
  assert.deepEqual(
    u.used.map((x) => [x.key, x.bytes, x.files]),
    [
      ['modules', 150, 2],
      ['samples', 300, 1],
    ],
  );
  assert.equal(u.overhead, 150);
  assert.equal(usageOf(entries, { free: 400, total: null }).overhead, null);
  assert.equal(usageOf(entries, { free: 900, total: 1000 }).overhead, 0);
});

test('libraryMarks: other version, not in library; matches, other folders and folder entries get none', () => {
  const entries = [
    { path: 'modules/a.dkm', size: 1, hash: 'aa' },
    { path: 'modules/b.dkm', size: 1, hash: 'bb' },
    { path: 'services/c.dks', size: 1, hash: 'cc' },
    { path: 'samples/x.wav', size: 1, hash: 'dd' },
    dir('modules/sub'),
  ];
  const marks = libraryMarks(entries, [
    { path: 'modules/a.dkm', hash: 'aa' },
    { path: 'modules/b.dkm', hash: 'b2' },
  ]);
  assert.deepEqual(Array.from(marks), [
    ['modules/b.dkm', 'other version'],
    ['services/c.dks', 'not in library'],
  ]);
});

test('orphanPaths: owned files, slash ids, orphan files and folders, empty folders', () => {
  const entries = [
    file('data/em/pluck/a.bin', 1),
    file('data/test/sine/b.bin', 1),
    file('data/gone/x/c.bin', 1),
    file('data/gone/x/d.bin', 1),
    file('data/mixed/y.bin', 1),
    file('data/mixed/z/e.bin', 1),
    file('data/top.bin', 1),
    dir('data/empty/'),
    dir('data/em/pluck/sub/'),
    file('samples/a.wav', 1),
  ];
  const orphans = orphanPaths(entries, ['em/pluck', 'test/sine', 'mixed/z']);
  assert.deepEqual(
    Array.from(orphans).sort(),
    [
      'data/empty',
      'data/gone',
      'data/gone/x',
      'data/gone/x/c.bin',
      'data/gone/x/d.bin',
      'data/mixed/y.bin',
      'data/top.bin',
    ].sort(),
  );
});

test('buildTree: implied folders, empty folders with and without a slash, sums and order', () => {
  const root = buildTree([
    file('samples/kicks/b.wav', 20),
    file('samples/kicks/a.wav', 10),
    file('modules/z.dkm', 5),
    dir('samples/new'),
    dir('data/empty/'),
    file('top.bin', 1),
  ]);
  assert.deepEqual(
    root.children.map((c) => c.name),
    ['data', 'modules', 'samples', 'top.bin'],
  );
  const samples = root.children.find((c) => c.name === 'samples')!;
  assert.equal(samples.path, 'samples');
  assert.equal(samples.size, 30);
  assert.equal(samples.files, 2);
  assert.equal(samples.hash, null);
  assert.deepEqual(
    samples.children.map((c) => c.name),
    ['kicks', 'new'],
  );
  const kicks = samples.children[0]!;
  assert.deepEqual(
    kicks.children.map((c) => [c.name, c.path, c.files]),
    [
      ['a.wav', 'samples/kicks/a.wav', 1],
      ['b.wav', 'samples/kicks/b.wav', 1],
    ],
  );
  assert.equal(samples.children[1]!.dir, true);
  const data = root.children[0]!;
  assert.deepEqual(
    data.children.map((c) => [c.name, c.dir]),
    [['empty', true]],
  );
  assert.equal(root.size, 36);
  assert.equal(root.files, 4);
});

test('visibleRows: closed folders hide their children, open ones show them one level deeper', () => {
  const root = buildTree([file('a/b/c.bin', 1), file('a/d.bin', 2), file('e.bin', 3)]);
  assert.deepEqual(
    visibleRows(root, new Set()).map((r) => [r.node.path, r.depth]),
    [
      ['a', 0],
      ['e.bin', 0],
    ],
  );
  assert.deepEqual(
    visibleRows(root, new Set(['a', 'a/b'])).map((r) => [r.node.path, r.depth]),
    [
      ['a', 0],
      ['a/b', 1],
      ['a/b/c.bin', 2],
      ['a/d.bin', 1],
      ['e.bin', 0],
    ],
  );
});

test('parseDkmHeader: reads the fields, the trace bit and the service role; null for bad input', () => {
  const b = fakeDkm({
    name: 'em/pluck',
    version: 0x010203,
    runtime: 0x80001f00 | 2,
    size: 5000,
    imageSize: 1234,
    bssSize: 99,
  });
  const h = parseDkmHeader(b)!;
  assert.equal(h.formatVersion, 1);
  assert.equal(h.fileSize, 5000);
  assert.equal(h.headerSize, 0xa0);
  assert.equal(h.name, 'em/pluck');
  assert.equal(h.version, '1.2.3');
  assert.equal(h.runtime, '0.31.2');
  assert.equal(h.trace, true);
  assert.equal(h.role, 'module');
  assert.equal(h.imageSize, 1234);
  assert.equal(h.bssSize, 99);
  assert.equal(h.sha256.length, 64);
  assert.equal(parseDkmHeader(fakeDkm({ name: 'x/s', role: 1 }))!.role, 'service');
  assert.equal(parseDkmHeader(fakeDkm({ name: 'x/s' }))!.trace, false);
  const bad = fakeDkm({ name: 'x/m' });
  bad[0] = 0;
  assert.equal(parseDkmHeader(bad), null);
  assert.equal(parseDkmHeader(b.subarray(0, 0x9f)), null);
});

test('hexDump: 16 bytes per line with offset and ASCII column', () => {
  const lines = hexDump(
    Uint8Array.from({ length: 20 }, (_, i) => 0x41 + i),
    0x10,
  );
  assert.equal(lines.length, 2);
  assert.equal(lines[0], '00000010  41 42 43 44 45 46 47 48 49 4a 4b 4c 4d 4e 4f 50  |ABCDEFGHIJKLMNOP|');
  assert.equal(lines[1], `00000020  51 52 53 54${' '.repeat(36)}  |QRST|`);
  assert.equal(hexDump(Uint8Array.of(0, 0x7f))[0]!.endsWith('|..|'), true);
});

test('fakeStoreOps: snapshot with usage, readAll in chunks with progress', async () => {
  const data = Uint8Array.from({ length: 2000 }, (_, i) => i & 255);
  const ops = fakeStoreOps(
    [
      { path: 'modules/a.dkm', bytes: fakeDkm({ name: 'em/a', size: 1000 }) },
      { path: 'samples/k/a.wav', bytes: data },
      { path: 'samples/new/', bytes: new Uint8Array(0) },
    ],
    { free: 500, total: 4000 },
  );
  const snap = await ops.snapshot();
  assert.equal(snap.entries.length, 3);
  assert.equal(snap.entries.find((e) => e.path === 'samples/k/a.wav')!.hash, fileHashHex(data));
  assert.deepEqual(
    snap.usage.used.map((u) => [u.key, u.bytes]),
    [
      ['modules', 1000],
      ['samples', 2000],
    ],
  );
  assert.equal(snap.usage.overhead, 500);
  const calls: [number, number][] = [];
  const got = await ops.readAll('samples/k/a.wav', (done, size) => calls.push([done, size]));
  assert.deepEqual(got, data);
  assert.deepEqual(calls, [
    [768, 2000],
    [1536, 2000],
    [2000, 2000],
  ]);
  assert.equal((await ops.read('samples/k/a.wav', 0, 5000)).bytes.length, 768);
  assert.equal(await ops.verify('samples/k/a.wav'), fileHashHex(data));
  assert.equal((await ops.header('modules/a.dkm'))!.name, 'em/a');
});
