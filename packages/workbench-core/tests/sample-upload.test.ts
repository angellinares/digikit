// The sample upload engine (packages/workbench-core/src/transfer/sample-upload.ts): the upload state built from events, and an upload run over a fake backend.
import assert from 'node:assert/strict';
import type { Prepared } from '@digi/dk-session/dksample';
import type { SampleEntry } from '@digi/dk-session/samples';
import { test } from 'vitest';
import {
  applyUpload,
  dropUploads,
  NOT_CONFIRMED,
  uploadCancelled,
  uploadFailure,
  uploadFiles,
  uploadItems,
  uploadLines,
  type UploadItem,
  type Uploads,
} from '../src/transfer/sample-upload.ts';
import {
  batchBackend,
  bytesOf,
  cancelError,
  deviceBackend,
  fakeBackend,
  filled,
  items3,
  names,
  prepare,
  runAll,
  type Backend,
} from './sample-fakes.ts';

test('events build the upload state, and a file that could not be prepared fails alone', () => {
  let u: Uploads = {};
  u = applyUpload(u, {
    event: 'prepared',
    folder: 'd',
    name: 'kick',
    source: 'kick.wav',
    kind: 'converted',
    bytes: 100,
  });
  u = applyUpload(u, { event: 'progress', folder: 'd', name: 'kick', sent: 40, bytes: 100 });
  assert.equal(u['d/kick']!.state, 'uploading');
  u = applyUpload(u, { event: 'verifying', folder: 'd', name: 'kick' });
  assert.deepEqual([u['d/kick']!.state, u['d/kick']!.sent], ['verifying', 100]);
  u = applyUpload(u, { event: 'verified', folder: 'd', name: 'kick', ok: false, message: 'device hash differs' });
  assert.deepEqual([u['d/kick']!.state, u['d/kick']!.message], ['failed', 'device hash differs']);
  u = applyUpload(u, { event: 'failed', folder: 'd', name: 'bad.wav', source: 'bad.wav', message: 'not a usable WAV' });
  assert.deepEqual(
    [u['d/bad.wav']!.kind, u['d/bad.wav']!.state, u['d/bad.wav']!.source],
    ['none', 'failed', 'bad.wav'],
  );
  assert.deepEqual(Object.keys(dropUploads(u, ['d/kick'])), ['d/bad.wav']);
  // a folder takes its files with it, and not a sibling that only starts with the same letters
  const nested = { 'Foo/a': u['d/kick'], 'Foo/sub/b': u['d/kick'], 'Foobar/x': u['d/kick'] };
  assert.deepEqual(Object.keys(dropUploads(nested, ['Foo'])), ['Foobar/x']);
});

const run = async (backend: Backend, names: string[]) => {
  let u: Uploads = {};
  await uploadFiles(
    backend,
    prepare,
    'drums',
    names.map((n) => ({ name: n, data: new Uint8Array(10) })),
    (e) => (u = applyUpload(u, e)),
  );
  return u;
};

test('an upload run: prepare, put with progress, verify; a broken WAV fails alone', async () => {
  const b = fakeBackend({ failVerify: 'snare' });
  const u = await run(b, ['kick.wav', 'x.bad.wav', 'snare.wav', 'notes.txt']);
  assert.deepEqual(
    Object.values(u).map((x) => [x.name, x.kind, x.state]),
    [
      ['kick', 'converted', 'ok'],
      ['x.bad.wav', 'none', 'failed'],
      ['snare', 'converted', 'failed'],
      ['notes.txt', 'unchanged', 'ok'],
    ],
  );
  assert.equal(u['drums/kick']!.message, 'device hash matches\nhash of the stored file');
  assert.match(u['drums/x.bad.wav']!.message, /not a usable WAV/);
  assert.equal(u['drums/snare']!.message, 'device hash differs\nhash of the stored file');
  assert.deepEqual(b.calls, [
    'put drums/kick',
    'verify drums/kick',
    'put drums/snare',
    'verify drums/snare',
    'put drums/notes.txt',
    'verify drums/notes.txt',
  ]);
});

test('a failed send stops the run: the files after it are marked not sent', async () => {
  const b = fakeBackend({ failPut: 'two' });
  const u = await run(b, ['one.wav', 'two.wav', 'three.wav']);
  assert.deepEqual(
    Object.values(u).map((x) => [x.name, x.state]),
    [
      ['one', 'ok'],
      ['two', 'failed'],
      ['three.wav', 'failed'],
    ],
  );
  assert.match(u['drums/two']!.message, /E_FULL: no space/);
  assert.equal(u['drums/three.wav']!.message, 'not sent: the upload of two failed');
  assert.deepEqual(b.calls, ['put drums/one', 'verify drums/one', 'put drums/two']);
});

test('a run with a failed file has one error line with the first message and the counts, and one Log line per file', async () => {
  const b = fakeBackend({ failPut: 'two' });
  const u = await run(b, ['one.wav', 'two.wav', 'three.wav']);
  assert.equal(
    uploadFailure(u, {}),
    'Uploading failed: E_FULL: no space (1 of 3 sent, 2 not sent)'.replace(
      'E_FULL: no space',
      u['drums/two']!.message.split('\n')[0],
    ),
  );
  assert.deepEqual(uploadLines(u, {}), [
    'one: verified',
    `two: failed: ${u['drums/two']!.message}`,
    'three.wav: not sent: the upload of two failed',
  ]);
  const ok = await run(fakeBackend({}), ['one.wav']);
  assert.equal(uploadFailure(ok, {}), undefined);
  assert.equal(uploadFailure(ok, ok), undefined, 'files an earlier run left are not this run');
});

test('Cancel between files: the file after the cancel and all later files are not sent, none fails, a verified file stays ok', async () => {
  let stop = false;
  const b = fakeBackend({});
  const verify = b.verify;
  b.verify = async (f, p) => {
    const v = await verify(f, p);
    if (p.name === 'one') stop = true; // the user presses Cancel while the first file ends
    return v;
  };
  let u: Uploads = {};
  await uploadFiles(
    b,
    prepare,
    'drums',
    ['one.wav', 'two.wav', 'three.wav'].map((n) => ({ name: n, data: new Uint8Array(10) })),
    (e) => (u = applyUpload(u, e)),
    () => stop,
  );
  assert.deepEqual(names(u), [
    ['one', 'ok'],
    ['two.wav', 'cancelled'],
    ['three.wav', 'cancelled'],
  ]);
  assert.deepEqual(b.calls, ['put drums/one', 'verify drums/one'], 'no later file starts');
  assert.equal(uploadFailure(u, {}), undefined);
  assert.equal(uploadCancelled(u, {}), '1 of 3 uploaded, 2 not sent');
  assert.deepEqual(uploadLines(u, {}), [
    'one: verified',
    'two.wav: not sent: cancelled',
    'three.wav: not sent: cancelled',
  ]);
});

test('Cancel in the middle of a file: its put fails as cancelled, it is cancelled (not failed), and the rest are not sent', async () => {
  const b = fakeBackend({});
  b.put = async (f, p, onProgress) => {
    b.calls.push(`put ${f}/${p.name}`);
    onProgress(5, 10);
    if (p.name === 'two') throw cancelError();
    onProgress(10, 10);
  };
  const u = await run(b, ['one.wav', 'two.wav', 'three.wav']);
  assert.deepEqual(names(u), [
    ['one', 'ok'],
    ['two', 'cancelled'],
    ['three.wav', 'cancelled'],
  ]);
  assert.equal(u['drums/two']!.sent, 5, 'what went is kept on the row');
  assert.equal(u['drums/two']!.message, 'cancelled while sending');
  assert.deepEqual(b.calls, ['put drums/one', 'verify drums/one', 'put drums/two']);
  assert.equal(uploadFailure(u, {}), undefined);
  assert.equal(uploadCancelled(u, {}), '1 of 3 uploaded, 2 not sent');
});

test('Cancel while a file is verified: it was sent, not verified, and cancelled', async () => {
  const b = fakeBackend({});
  b.verify = async () => {
    throw cancelError();
  };
  const u = await run(b, ['one.wav', 'two.wav']);
  assert.deepEqual(names(u), [
    ['one', 'cancelled'],
    ['two.wav', 'cancelled'],
  ]);
  assert.equal(u['drums/one']!.message, 'sent, not verified: cancelled');
});

test('a real failure and a cancel in one run is still a failure', async () => {
  let u: Uploads = {};
  await uploadFiles(
    fakeBackend({}),
    prepare,
    'drums',
    ['x.bad.wav', 'two.wav'].map((n) => ({ name: n, data: new Uint8Array(10) })),
    (e) => (u = applyUpload(u, e)),
    () => false,
  );
  u = applyUpload(u, { event: 'cancelled', folder: 'drums', name: 'two', message: 'cancelled while sending' });
  assert.match(uploadFailure(u, {}) ?? '', /^Uploading failed: not a usable WAV/);
  assert.equal(uploadCancelled(u, {}), undefined);
});

/** A backend that has folders: `list` answers from `have` (folder path -> names), `mkdir` is recorded. */
const folderBackend = (have: Record<string, string[]> = { '': [] }, failMkdir = '') => {
  const calls: string[] = [];
  const base = fakeBackend();
  return {
    calls,
    put: async (f: string, p: Prepared, o: (sent: number, total: number) => void) => {
      calls.push(`put ${f}/${p.name}`);
      return base.put(f, p, o);
    },
    verify: base.verify,
    async list(f: string): Promise<SampleEntry[]> {
      calls.push(`list ${f}`);
      return (have[f] ?? []).map((name): SampleEntry => ({ name, kind: 'folder', size: 0 }));
    },
    async mkdir(f: string) {
      calls.push(`mkdir ${f}`);
      if (f === failMkdir) throw new Error('{"error":"E_FULL: no space","kind":"device"}');
    },
  };
};
const item = (folderPath: string, name: string, size = 10): UploadItem => ({
  folder: folderPath,
  name,
  size,
  read: async () => new Uint8Array(size),
});
const runItems = async (backend: Backend, mkdirs: string[], items: UploadItem[]) => {
  let u: Uploads = {};
  const events: string[] = [];
  await uploadItems(backend, prepare, mkdirs, items, (e) => {
    events.push(e.event);
    u = applyUpload(u, e);
  });
  return { u, events };
};

test('an upload of folders: every file is queued first (the bar knows the whole run), folders are made in order, a folder that exists is not made again', async () => {
  const b = folderBackend({ '': ['loops'], loops: [] });
  const { u, events } = await runItems(
    b,
    ['loops', 'loops/breaks'],
    [item('loops/breaks', 'x.wav'), item('loops', 'b.wav', 20)],
  );
  assert.deepEqual(events.slice(0, 2), ['queued', 'queued']);
  assert.deepEqual(b.calls.slice(0, 4), ['list ', 'list loops', 'mkdir loops/breaks', 'put loops/breaks/x']);
  assert.ok(!b.calls.includes('mkdir loops'), 'loops is on the device already');
  assert.deepEqual(
    Object.values(u).map((x) => [x.folder, x.name, x.state, x.bytes]),
    [
      ['loops/breaks', 'x', 'ok', 10],
      ['loops', 'b', 'ok', 20],
    ],
    'a WAV was queued as x.wav and is x on the device, in the same place of the list',
  );
});

test('a folder that cannot be made fails the files inside it (and below) and nothing else', async () => {
  const b = folderBackend({ '': [] }, 'loops/breaks');
  const { u } = await runItems(
    b,
    ['loops', 'loops/breaks', 'loops/breaks/deep'],
    [item('loops/breaks/deep', 'y.wav'), item('loops', 'b.wav'), item('loops/breaks', 'x.wav')],
  );
  const state = (key: string) => [u[key]!.state, u[key]!.message];
  assert.deepEqual(state('loops/b').slice(0, 1), ['ok']);
  assert.equal(u['loops/breaks/deep/y.wav']!.state, 'failed');
  assert.match(
    u['loops/breaks/deep/y.wav']!.message,
    /not sent: the folder loops\/breaks could not be made: E_FULL: no space/,
  );
  assert.equal(u['loops/breaks/x.wav']!.state, 'failed');
  assert.ok(!b.calls.includes('mkdir loops/breaks/deep'), 'a folder inside a lost one is not tried');
  assert.ok(!b.calls.includes('put loops/breaks/x'));
});

test('a file that cannot be read fails alone and the run goes on', async () => {
  const b = folderBackend();
  const bad = {
    folder: 'd',
    name: 'gone.wav',
    size: 1,
    read: async () => Promise.reject(new Error('gone.wav: no such file')),
  };
  const { u } = await runItems(b, [], [bad, item('d', 'ok.wav')]);
  assert.equal(u['d/gone.wav']!.state, 'failed');
  assert.match(u['d/gone.wav']!.message, /no such file/);
  assert.equal(u['d/ok']!.state, 'ok');
});

test('events: a queued file takes the name and size it has on the device in place; queued bytes count until then', () => {
  let u: Uploads = {};
  u = applyUpload(u, { event: 'queued', folder: 'd', name: 'a.wav', bytes: 100 });
  u = applyUpload(u, { event: 'queued', folder: 'd', name: 'b.wav', bytes: 50 });
  assert.deepEqual([u['d/a.wav']!.state, u['d/a.wav']!.bytes, u['d/a.wav']!.kind], ['waiting', 100, 'none']);
  u = applyUpload(u, {
    event: 'prepared',
    folder: 'd',
    name: 'a',
    source: 'a.wav',
    kind: 'converted',
    bytes: 90,
    queued: 'a.wav',
  });
  assert.deepEqual(Object.keys(u), ['d/a', 'd/b.wav'], 'the entry keeps its place');
  assert.deepEqual([u['d/a']!.bytes, u['d/a']!.kind], [90, 'converted']);
  // a name that does not change (notes.txt) stays under its key
  u = applyUpload(u, {
    event: 'prepared',
    folder: 'd',
    name: 'b.wav',
    source: 'b.wav',
    kind: 'unchanged',
    bytes: 50,
    queued: 'b.wav',
  });
  assert.deepEqual(Object.keys(u), ['d/a', 'd/b.wav']);
});

// ---- skipping what the device holds already ----------------------------------------------------------------------------------------

test('the same name with another size, or other bytes of the same size, uploads and replaces', async () => {
  const b = deviceBackend({ d: { a: bytesOf(10, 1), b: bytesOf(10, 2) } });
  const { u } = await runAll(b, [], [filled('d', 'a', 11, 1), filled('d', 'b', 10, 3)]);
  assert.deepEqual(names(u), [
    ['a', 'ok'],
    ['b', 'ok'],
  ]);
  assert.deepEqual(b.calls, ['list d', 'put d/a', 'verify d/a', 'put d/b', 'verify d/b']);
});

test('a WAV is compared as the device holds it: the converted file, under the name without the extension', async () => {
  const b = deviceBackend({ d: { kick: bytesOf(10, 1) } });
  const { u } = await runAll(b, [], [filled('d', 'kick.wav', 10, 1)]);
  assert.deepEqual(names(u), [['kick', 'same']]);
});

test('each folder is listed once per run: 357 files in 3 folders are 3 listings', async () => {
  const b = deviceBackend({ f0: { x: bytesOf(4, 7) } });
  const items = [];
  for (let i = 0; i < 357; i++) items.push(filled(`f${i % 3}`, `s${i}`, 4, 1));
  const { u } = await runAll(b, [], items);
  assert.equal(b.calls.filter((c) => c.startsWith('list ')).length, 3);
  assert.equal(Object.values(u).filter((x) => x.state === 'ok').length, 357);
});

test('folders made by the run are not listed; a folder the device has is listed once, by the folder step and the compare together', async () => {
  const b = deviceBackend({ '': { z: bytesOf(1, 1) }, loops: { a: bytesOf(10, 1) } });
  b.list = ((list) => async (f) => {
    const r = await list(f);
    return f === '' ? [...r, { name: 'loops', kind: 'folder', size: 0 }] : r;
  })(b.list);
  const { u } = await runAll(
    b,
    ['loops', 'fresh'],
    [filled('loops', 'a', 10, 1), filled('fresh', 'n', 10, 1), filled('fresh', 'm', 10, 2)],
  );
  assert.deepEqual(names(u), [
    ['a', 'same'],
    ['n', 'ok'],
    ['m', 'ok'],
  ]);
  assert.deepEqual(
    b.calls.filter((c) => c.startsWith('list ')),
    ['list ', 'list loops'],
  );
  assert.deepEqual(
    b.calls.filter((c) => c.startsWith('mkdir')),
    ['mkdir fresh'],
  );
});

test('a folder the device does not have: nothing to skip, everything uploads', async () => {
  const b = deviceBackend({});
  const { u } = await runAll(b, [], [filled('new', 'a', 10, 1)]);
  assert.deepEqual(names(u), [['a', 'ok']]);
});

test('a listing that fails does not fail the run: the files upload, and one note says why', async () => {
  const b = deviceBackend({ d: { a: bytesOf(10, 1) } }, { failList: 'd' });
  const { u, events } = await runAll(b, [], [filled('d', 'a', 10, 1), filled('d', 'b', 10, 2)]);
  assert.deepEqual(names(u), [
    ['a', 'ok'],
    ['b', 'ok'],
  ]);
  const notes = events.filter((e) => e.event === 'note');
  assert.equal(notes.length, 1);
  assert.match(notes[0].message, /^d: could not list it, so every file goes: E_BUSY: card busy/);
  assert.equal(uploadFailure(u, {}), undefined);
  assert.equal(b.calls.filter((c) => c === 'list d').length, 1, 'a failed listing is not tried again for each file');
});

test('a backend without sameOnDevice or list uploads everything as before', async () => {
  const b = folderBackend();
  const { u } = await runAll(b, [], [filled('d', 'a', 10, 1)]);
  assert.deepEqual(names(u), [['a', 'ok']]);
  assert.ok(!b.calls.includes('list d'));
});

test('Cancel while a folder is listed: that file and the rest are cancelled, files skipped before stay skipped', async () => {
  const b = deviceBackend({ d: { a: bytesOf(10, 1) } }, { cancelList: 'e' });
  const { u } = await runAll(b, [], [filled('d', 'a', 10, 1), filled('e', 'b', 10, 1), filled('e', 'c', 10, 1)]);
  assert.deepEqual(names(u), [
    ['a', 'same'],
    ['b', 'cancelled'],
    ['c', 'cancelled'],
  ]);
  assert.equal(uploadFailure(u, {}), undefined);
  assert.equal(uploadCancelled(u, {}), '0 of 3 uploaded, 1 already on the device, 2 not sent');
  assert.ok(!b.calls.some((c) => c.startsWith('put')));
});

test('Cancel between skipped files stops the run', async () => {
  let stop = false;
  const b = deviceBackend({ d: { a: bytesOf(10, 1), b: bytesOf(10, 1), c: bytesOf(10, 1) } });
  const { u } = await runAll(
    b,
    [],
    [
      filled('d', 'a', 10, 1),
      { ...filled('d', 'b', 10, 1), read: async () => ((stop = true), bytesOf(10, 1)) },
      filled('d', 'c', 10, 1),
    ],
    () => stop,
  );
  assert.deepEqual(names(u), [
    ['a', 'same'],
    ['b', 'cancelled'],
    ['c', 'cancelled'],
  ]);
});

// ---- the batch commit ------------------------------------------------------------------------------------------------------------

test('files waiting become ok at once when a reply shows nothing pending (a limit committed them), the commit after the last file is skipped', async () => {
  const b = batchBackend({ pending: [1, 2, 0] });
  const { u } = await runAll(b, [], items3);
  assert.deepEqual(names(u), [
    ['a', 'ok'],
    ['b', 'ok'],
    ['c', 'ok'],
  ]);
  assert.deepEqual(b.calls, ['put d/a defer', 'put d/b defer', 'put d/c defer']);
});

test('a commit that fails marks the waiting files failed: not saved, send again', async () => {
  const b = batchBackend({ commitFails: '{"error":"E_WRITE: card write failed","kind":"device"}' });
  const { u } = await runAll(b, [], items3);
  assert.deepEqual(names(u), [
    ['a', 'failed'],
    ['b', 'failed'],
    ['c', 'failed'],
  ]);
  assert.ok(u['d/a']!.message.startsWith(NOT_CONFIRMED), u['d/a']!.message);
  assert.match(u['d/a']!.message, /E_WRITE/);
});

test('the link lost while files wait (the backend fell back): they fail as not saved, the file sent over the fallback is a normal one', async () => {
  const b = batchBackend({ fallbackAfter: 3 });
  const { u } = await runAll(b, [], items3);
  assert.deepEqual(names(u), [
    ['a', 'failed'],
    ['b', 'failed'],
    ['c', 'ok'],
  ]);
  assert.ok(u['d/a']!.message.startsWith(NOT_CONFIRMED));
  assert.match(u['d/a']!.message, /USB link was lost/);
  // the third put returned nothing pending on the fallback: its own verify ran, and there is no commit
  assert.deepEqual(b.calls.slice(-2), ['put d/c defer', 'verify d/c']);
  assert.ok(!b.calls.includes('commit'));
});

test('the rerun after a loss sends only the files the device does not hold (the existing same-on-device skip)', async () => {
  // a, b were lost: the device lists only c
  const b = deviceBackend({ d: { c: bytesOf(10, 3) } });
  const { u } = await runAll(b, [], items3);
  assert.deepEqual(names(u), [
    ['a', 'ok'],
    ['b', 'ok'],
    ['c', 'same'],
  ]);
});

test('a stop still commits the files already waiting', async () => {
  const b = batchBackend();
  let stopped = false;
  const put = b.put.bind(b);
  b.put = async (...args: Parameters<typeof put>) => {
    const r = await put(...args);
    stopped = true; // the user stops once a is sent
    return r;
  };
  const { u } = await runAll(b, [], items3, () => stopped);
  assert.ok(b.calls.includes('commit'), b.calls.join('|'));
  assert.equal(u['d/a']!.state, 'ok');
  assert.equal(u['d/c']!.state, 'cancelled');
});

test('a commit that answers files still pending is asked again; files still held back after that are not confirmed', async () => {
  const b = batchBackend({ commitLeft: [2, 0] });
  const { u } = await runAll(b, [], items3);
  assert.deepEqual(names(u), [
    ['a', 'ok'],
    ['b', 'ok'],
    ['c', 'ok'],
  ]);
  assert.equal(b.calls.filter((c) => c === 'commit').length, 2);
  const held = batchBackend({ commitLeft: [1] });
  const r = await runAll(held, [], items3);
  assert.deepEqual(names(r.u), [
    ['a', 'failed'],
    ['b', 'failed'],
    ['c', 'failed'],
  ]);
  assert.equal(held.calls.filter((c) => c === 'commit').length, 4, 'one commit and three more');
  assert.ok(r.u['d/a']!.message.startsWith(NOT_CONFIRMED));
});

test('a hung COMMIT is cut off by a stop that comes during it; the files are not confirmed', async () => {
  const b = batchBackend({ commitHangs: true });
  let stopNow = false;
  const put = b.put.bind(b);
  let puts = 0;
  b.put = async (...args: Parameters<typeof put>) => {
    const r = await put(...args);
    if (++puts === 3) setTimeout(() => (stopNow = true), 120); // the stop comes while the COMMIT hangs
    return r;
  };
  const { u } = await runAll(b, [], items3, () => stopNow);
  assert.deepEqual(names(u), [
    ['a', 'failed'],
    ['b', 'failed'],
    ['c', 'failed'],
  ]);
  assert.match(u['d/a']!.message, /not confirmed, run again: cancelled while saving/);
});

test('after the link was lost the run lists the folder again: files the device committed meanwhile are ok, the others not confirmed', async () => {
  // the device commits a and b by itself (its idle commit) while the run is on the fallback: they show in the fresh listing, c was sent over SysEx
  const lose = (committed: Record<string, Uint8Array>) => {
    const have: Record<string, Record<string, Uint8Array>> = {};
    const b = batchBackend({ fallbackAfter: 3, have });
    const put = b.put.bind(b);
    b.put = async (...args: Parameters<typeof put>) => {
      const r = await put(...args);
      if (args[1].name === 'c') have.d = committed;
      return r;
    };
    return b;
  };
  const both = await runAll(lose({ a: bytesOf(10, 1), b: bytesOf(10, 2) }), [], items3);
  assert.deepEqual(names(both.u), [
    ['a', 'ok'],
    ['b', 'ok'],
    ['c', 'ok'],
  ]);
  const one = await runAll(lose({ a: bytesOf(10, 1) }), [], items3);
  assert.deepEqual(names(one.u), [
    ['a', 'ok'],
    ['b', 'failed'],
    ['c', 'ok'],
  ]);
  assert.ok(one.u['d/b']!.message.startsWith(NOT_CONFIRMED));
});

// ---- reading and preparing the next file while one is sent ----------------------------------------------------------------------

/** A backend whose put waits a moment (so that work started ahead can run) and logs what the run does, in order. */
const slowBackend = (log: string[], onPut: (name: string) => void = () => {}) => {
  const base = fakeBackend();
  return {
    put: async (f: string, p: Prepared, o: (sent: number, total: number) => void) => {
      log.push(`put ${p.name}`);
      onPut(p.name);
      await new Promise((r) => setTimeout(r, 20));
      log.push(`put done ${p.name}`);
      return base.put(f, p, o);
    },
    verify: base.verify,
  };
};
const logged = (log: string[], name: string, size = 10, fill = 0): UploadItem => ({
  folder: 'd',
  name,
  size,
  read: async () => {
    log.push(`read ${name}`);
    return bytesOf(size, fill);
  },
});

test('the next file is read while one is sent, and its events keep their order: prepared comes after the verify of the file before', async () => {
  const log: string[] = [];
  const { events } = await runAll(slowBackend(log), [], [logged(log, 'a'), logged(log, 'b'), logged(log, 'c')]);
  assert.ok(log.indexOf('read b') < log.indexOf('put done a'), 'b is read while a is sent');
  assert.ok(log.indexOf('read c') < log.indexOf('put done b'), 'and c while b is sent');
  const order = events
    .filter((e) => e.event === 'prepared' || e.event === 'verified')
    .map((e) => `${e.event} ${e.name}`);
  assert.deepEqual(order, ['prepared a', 'verified a', 'prepared b', 'verified b', 'prepared c', 'verified c']);
});

test('a cancel during a prepared-ahead file: it is cancelled, never prepared, and its failing read leaves nothing unhandled', async () => {
  const log: string[] = [];
  let cancel = false;
  const b: UploadItem = {
    folder: 'd',
    name: 'b',
    size: 10,
    read: async () => {
      log.push('read b');
      throw new Error('b: read failed');
    },
  };
  const { u, events } = await runAll(
    slowBackend(log, (name) => {
      if (name === 'a') cancel = true; // the cancel comes while a is sent and b is read ahead (the fake send ignores the abort signal: a ends ok)
    }),
    [],
    [logged(log, 'a'), b, logged(log, 'c')],
    () => cancel,
  );
  assert.ok(log.includes('read b'), 'b was read ahead');
  assert.equal(u['d/a']!.state, 'ok');
  assert.equal(u['d/b']!.state, 'cancelled');
  assert.equal(u['d/c']!.state, 'cancelled');
  assert.ok(!events.some((e) => e.event === 'prepared' && e.name === 'b'));
  assert.ok(!log.includes('read c'), 'nothing after the cancel is read');
});

test('a file that cannot be prepared fails alone when its turn comes; the files around it are sent', async () => {
  const log: string[] = [];
  const { u, events } = await runAll(
    slowBackend(log),
    [],
    [logged(log, 'a'), logged(log, 'x.bad.wav'), logged(log, 'c')],
  );
  assert.deepEqual(names(u), [
    ['a', 'ok'],
    ['x.bad.wav', 'failed'],
    ['c', 'ok'],
  ]);
  assert.match(u['d/x.bad.wav']!.message, /not a usable WAV/);
  assert.ok(!events.some((e) => e.event === 'failed' && e.name !== 'x.bad.wav'));
  assert.ok(!log.includes('put x.bad'), 'nothing of it was sent');
  const order = events.filter((e) => e.event === 'verified' || e.event === 'failed').map((e) => `${e.event} ${e.name}`);
  assert.deepEqual(order, ['verified a', 'failed x.bad.wav', 'verified c']);
});
