// The pure logic of Samples mode of the desktop Transfer page (apps/desktop/ui/src/samples-core.ts): folders and breadcrumbs (device and local),
// the rows with their upload states, the local rows and which files convert (the labels), and an upload run over a fake backend.
import assert from 'node:assert/strict';
import test from 'node:test';
import { fileGroups, nameInGroup } from '../../desktop/ui/src/card-core.ts';
import { sourceKind } from '@digi/dk-session/dksample';
import {
  applyUpload,
  confirmDeleteLabel,
  crumbs,
  deletePartLine,
  deleteRows,
  deletingLine,
  dropUploads,
  folderNameError,
  joinPath,
  baseName,
  kindTag,
  localCrumbs,
  localRowTags,
  localRows,
  parentOf,
  folderStatus,
  foldersFromOutside,
  noFoldersMessage,
  planUpload,
  sampleRows,
  selectedRows,
  uploadFiles,
  uploadCancelled,
  uploadFailure,
  uploadItems,
  uploadLines,
  uploadStatus,
} from '../../desktop/ui/src/samples-core.ts';

const file = (name, size = 10) => ({ name, kind: 'file', size });
const folder = (name) => ({ name, kind: 'folder', size: 0 });

test('folder paths and the breadcrumb', () => {
  assert.equal(joinPath('', 'kick'), 'kick');
  assert.equal(joinPath('drums/808', 'kick'), 'drums/808/kick');
  assert.equal(parentOf('drums/808'), 'drums');
  assert.equal(parentOf('drums'), '');
  assert.equal(parentOf(''), '');
  assert.deepEqual(crumbs(''), [{ name: 'Samples', path: '' }]);
  assert.deepEqual(crumbs('drums/808'), [
    { name: 'Samples', path: '' },
    { name: 'drums', path: 'drums' },
    { name: '808', path: 'drums/808' },
  ]);
});

test('a new folder name', () => {
  const here = [folder('drums'), file('kick')];
  assert.equal(folderNameError('loops', here), '');
  assert.match(folderNameError('', here), /name/);
  assert.match(folderNameError('  ', here), /name/);
  assert.match(folderNameError('a/b', here), /Not a folder name/);
  assert.match(folderNameError('..', here), /Not a folder name/);
  assert.match(folderNameError(' x', here), /Not a folder name/);
  assert.match(folderNameError('drums', here), /exists already/);
  assert.match(folderNameError('kick', here), /exists already/);
});

test('rows: folders first, then files by name; an upload that is not listed yet is a pending row', () => {
  const uploads = {
    'drums/new': {
      folder: 'drums',
      name: 'new',
      source: 'new.wav',
      kind: 'converted',
      state: 'uploading',
      sent: 5,
      bytes: 20,
      message: '',
    },
    'other/x': {
      folder: 'other',
      name: 'x',
      source: 'x',
      kind: 'unchanged',
      state: 'waiting',
      sent: 0,
      bytes: 1,
      message: '',
    },
    'drums/snare': {
      folder: 'drums',
      name: 'snare',
      source: 'snare.wav',
      kind: 'converted',
      state: 'ok',
      sent: 9,
      bytes: 9,
      message: 'device content hash matches\nthe firmware hash',
    },
  };
  const rows = sampleRows('drums', [file('snare', 9), folder('808'), file('kick')], uploads);
  assert.deepEqual(
    rows.map((r) => [r.name, r.path, r.kind, r.pending]),
    [
      ['808', 'drums/808', 'folder', false],
      ['kick', 'drums/kick', 'file', false],
      ['new', 'drums/new', 'file', true],
      ['snare', 'drums/snare', 'file', false],
    ],
  );
  assert.equal(rows[1].status, undefined);
  assert.deepEqual(rows[2].status, { kind: 'uploading', sent: 5, bytes: 20, message: '' });
  assert.deepEqual(rows[3].status, { kind: 'ok', message: 'device content hash matches\nthe firmware hash' });
});

test('the state of an upload: waiting, uploading, verifying, ok, failed', () => {
  const u = (state, sent = 0, bytes = 10, message = '') => ({
    folder: '',
    name: 'a',
    source: 'a',
    kind: 'native',
    state,
    sent,
    bytes,
    message,
  });
  assert.equal(uploadStatus(undefined), undefined);
  assert.equal(uploadStatus(u('waiting')).kind, 'waiting');
  assert.equal(uploadStatus(u('uploading', 3)).kind, 'uploading');
  assert.equal(uploadStatus(u('uploading', 10)).kind, 'verifying'); // all sent: the device is checking
  assert.equal(uploadStatus(u('verifying', 10)).kind, 'verifying');
  assert.equal(uploadStatus(u('ok', 10, 10, 'm')).message, 'm');
  assert.equal(uploadStatus(u('failed', 0, 10, 'no space')).kind, 'failed');
});

test('events build the upload state, and a file that could not be prepared fails alone', () => {
  let u = {};
  u = applyUpload(u, {
    event: 'prepared',
    folder: 'd',
    name: 'kick',
    source: 'kick.wav',
    kind: 'converted',
    bytes: 100,
  });
  u = applyUpload(u, { event: 'progress', folder: 'd', name: 'kick', sent: 40, bytes: 100 });
  assert.equal(u['d/kick'].state, 'uploading');
  u = applyUpload(u, { event: 'verifying', folder: 'd', name: 'kick' });
  assert.deepEqual([u['d/kick'].state, u['d/kick'].sent], ['verifying', 100]);
  u = applyUpload(u, { event: 'verified', folder: 'd', name: 'kick', ok: false, message: 'device hash differs' });
  assert.deepEqual([u['d/kick'].state, u['d/kick'].message], ['failed', 'device hash differs']);
  u = applyUpload(u, { event: 'failed', folder: 'd', name: 'bad.wav', source: 'bad.wav', message: 'not a usable WAV' });
  assert.deepEqual([u['d/bad.wav'].kind, u['d/bad.wav'].state, u['d/bad.wav'].source], ['none', 'failed', 'bad.wav']);
  assert.deepEqual(Object.keys(dropUploads(u, ['d/kick'])), ['d/bad.wav']);
  // a folder takes its files with it, and not a sibling that only starts with the same letters
  const nested = { 'Foo/a': u['d/kick'], 'Foo/sub/b': u['d/kick'], 'Foobar/x': u['d/kick'] };
  assert.deepEqual(Object.keys(dropUploads(nested, ['Foo'])), ['Foobar/x']);
});

test('which files convert: the label says what was done', () => {
  assert.equal(kindTag('converted').text, 'WAV→');
  assert.equal(kindTag('native').text, 'native');
  assert.equal(kindTag('unchanged').text, 'other');
  assert.equal(kindTag('unchanged').title, "Won't play as a sample");
  assert.equal(kindTag('none'), undefined);
});

test('delete: the selected rows that are on the device', () => {
  const rows = sampleRows('', [folder('a'), file('b'), file('c')], {
    p: { folder: '', name: 'p', source: 'p', kind: 'native', state: 'waiting', sent: 0, bytes: 1, message: '' },
  });
  assert.deepEqual(
    deleteRows(rows, new Set(['c', 'a', 'p', 'gone'])).map((r) => r.path),
    ['a', 'c'],
  );
});

/** A backend that keeps what it is given; `failPut` names a file whose put throws, `failVerify` one whose verify says no. */
const fakeBackend = ({ failPut = '', failVerify = '' } = {}) => {
  const calls = [];
  return {
    calls,
    async put(folderName, p, onProgress) {
      calls.push(`put ${folderName}/${p.name}`);
      if (p.name === failPut) throw new Error('{"error":"E_FULL: no space","kind":"device"}');
      onProgress(p.stored.length / 2, p.stored.length);
      onProgress(p.stored.length, p.stored.length);
    },
    async verify(folderName, p) {
      calls.push(`verify ${folderName}/${p.name}`);
      return {
        ok: p.name !== failVerify,
        message: p.name === failVerify ? 'device hash differs' : 'device hash matches',
        compared: 'hash of the stored file',
      };
    },
  };
};
const prepare = (name, data) => {
  if (name.endsWith('.bad.wav')) throw new Error('not a usable WAV: no fmt chunk');
  const wav = name.endsWith('.wav');
  return {
    name: wav ? name.replace(/\.wav$/, '') : name,
    bytes: data,
    stored: data,
    kind: wav ? 'converted' : 'unchanged',
  };
};
const run = async (backend, names) => {
  let u = {};
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
  assert.equal(u['drums/kick'].message, 'device hash matches\nhash of the stored file');
  assert.match(u['drums/x.bad.wav'].message, /not a usable WAV/);
  assert.equal(u['drums/snare'].message, 'device hash differs\nhash of the stored file');
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
  assert.match(u['drums/two'].message, /E_FULL: no space/);
  assert.equal(u['drums/three.wav'].message, 'not sent: the upload of two failed');
  assert.deepEqual(b.calls, ['put drums/one', 'verify drums/one', 'put drums/two']);
});

test('a run with a failed file has one error line with the first message and the counts, and one Log line per file', async () => {
  const b = fakeBackend({ failPut: 'two' });
  const u = await run(b, ['one.wav', 'two.wav', 'three.wav']);
  assert.equal(
    uploadFailure(u, {}),
    'Uploading failed: E_FULL: no space (1 of 3 sent, 2 not sent)'.replace(
      'E_FULL: no space',
      u['drums/two'].message.split('\n')[0],
    ),
  );
  assert.deepEqual(uploadLines(u, {}), [
    'one: verified',
    `two: failed: ${u['drums/two'].message}`,
    'three.wav: not sent: the upload of two failed',
  ]);
  const ok = await run(fakeBackend({}), ['one.wav']);
  assert.equal(uploadFailure(ok, {}), undefined);
  assert.equal(uploadFailure(ok, ok), undefined, 'files an earlier run left are not this run');
});

const cancelError = () => Object.assign(new Error('cancelled'), { kind: 'cancelled' });
const names = (u) => Object.values(u).map((x) => [x.name, x.state]);

test('Cancel between files: the file after the cancel and all later files are not sent, none fails, a verified file stays ok', async () => {
  let stop = false;
  const b = fakeBackend({});
  const verify = b.verify;
  b.verify = async (f, p) => {
    const v = await verify(f, p);
    if (p.name === 'one') stop = true; // the user presses Cancel while the first file ends
    return v;
  };
  let u = {};
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
  assert.equal(u['drums/two'].sent, 5, 'what went is kept on the row');
  assert.equal(u['drums/two'].message, 'cancelled while sending');
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
  assert.equal(u['drums/one'].message, 'sent, not verified: cancelled');
});

test('a real failure and a cancel in one run is still a failure', async () => {
  let u = {};
  await uploadFiles(
    fakeBackend({}),
    prepare,
    'drums',
    ['x.bad.wav', 'two.wav'].map((n) => ({ name: n, data: new Uint8Array(10) })),
    (e) => (u = applyUpload(u, e)),
    () => false,
  );
  u = applyUpload(u, { event: 'cancelled', folder: 'drums', name: 'two', message: 'cancelled while sending' });
  assert.match(uploadFailure(u, {}), /^Uploading failed: not a usable WAV/);
  assert.equal(uploadCancelled(u, {}), undefined);
});

test('the Card panel groups the store files by top folder', () => {
  const card = {
    folder: 'x',
    continuous: true,
    last_sync: null,
    error: null,
    files: [
      { path: 'samples/drums/kick', bytes: 300, hash: 'c' },
      { path: 'modules/pluck.dkm', bytes: 100, hash: 'a' },
      { path: 'samples/snare', bytes: 50, hash: 'b' },
      { path: 'top.bin', bytes: 1, hash: 'd' },
    ],
  };
  const g = fileGroups(card);
  assert.deepEqual(
    g.map((x) => [x.folder, x.files.length, x.bytes]),
    [
      ['', 1, 1],
      ['modules', 1, 100],
      ['samples', 2, 350],
    ],
  );
  assert.equal(nameInGroup('samples/drums/kick', 'samples'), 'drums/kick');
  assert.equal(nameInGroup('top.bin', ''), 'top.bin');
});

test('the local list: folders first, then files by name, then the added files; folders have no kind tag; the selection takes folders too', () => {
  const entries = [file('snare.wav', 40), folder('z'), file('kick', 30), folder('a')];
  const kinds = { 'drums/snare.wav': 'converted', 'drums/kick': 'native', '+x.txt': 'unchanged' };
  const rows = localRows(
    'drums',
    entries,
    [
      { name: 'x.txt', size: 5 },
      { name: 'loop.wav', size: 9 },
    ],
    kinds,
  );
  assert.deepEqual(
    rows.map((r) => [r.key, r.kind, r.added]),
    [
      ['drums/a', 'folder', false],
      ['drums/z', 'folder', false],
      ['drums/kick', 'file', false],
      ['drums/snare.wav', 'file', false],
      ['+x.txt', 'file', true],
      ['+loop.wav', 'file', true],
    ],
  );
  assert.deepEqual(
    rows.map((r) => localRowTags(r).map((t) => t.text)),
    [[], [], ['native'], ['WAV→'], ['other', 'added'], ['added']],
    'a file whose bytes were not read has no kind tag yet; an added file says so',
  );
  assert.deepEqual(
    selectedRows(rows, new Set(['drums/a', 'drums/snare.wav', '+loop.wav', 'gone'])).map((r) => r.key),
    ['drums/a', 'drums/snare.wav', '+loop.wav'],
    'folders are selectable like files; a selection of rows that are gone is dropped',
  );
  assert.deepEqual(rows[0].path, 'drums/a');
  assert.deepEqual(rows[4].path, '', 'an added file is not in the folder');
});

test('the local breadcrumb names the chosen folder; either path separator', () => {
  assert.equal(baseName('/Users/em/samples'), 'samples');
  assert.equal(baseName('/Users/em/samples/'), 'samples');
  assert.equal(baseName('C:\\Users\\em\\samples'), 'samples');
  assert.deepEqual(localCrumbs('/Users/em/samples', ''), [{ name: 'samples', path: '' }]);
  assert.deepEqual(localCrumbs('/Users/em/samples', 'drums/808'), [
    { name: 'samples', path: '' },
    { name: 'drums', path: 'drums' },
    { name: '808', path: 'drums/808' },
  ]);
});

test('the label of a local file comes from its first 64 bytes and its size, as prepareUpload decides', () => {
  const wav = new Uint8Array(64);
  wav.set(Buffer.from('RIFF'), 0);
  wav.set(Buffer.from('WAVE'), 8);
  const native = new Uint8Array(64);
  const v = new DataView(native.buffer);
  v.setUint32(4, 100); // PCM bytes
  v.setUint32(8, 48000);
  native[0x14] = 0x7f;
  assert.equal(sourceKind(wav, 5000), 'converted');
  assert.equal(sourceKind(native, 0x40 + 100), 'native');
  assert.equal(sourceKind(native, 0x40 + 100 + 16), 'native', 'with the trailer');
  assert.equal(sourceKind(native, 0x40 + 99), 'unchanged', 'the size must match the header');
  assert.equal(sourceKind(new Uint8Array(64), 64), 'unchanged');
  assert.equal(sourceKind(new Uint8Array(3), 3), 'unchanged');
});

// ---- the explorer: the plan of an upload with folders, the queue, the folder rows ----------------------------------------------------

/** A local tree for the plan: path -> entries of that level. */
const tree = {
  '': [folder('loops'), folder('empty'), file('a.wav', 5)],
  loops: [file('z.wav', 3), folder('breaks'), file('b.wav', 4)],
  'loops/breaks': [file('x.wav', 1), folder('deep')],
  'loops/breaks/deep': [file('y.wav', 2)],
  empty: [],
};
const lister =
  (log = []) =>
  async (rel) => {
    log.push(rel);
    return tree[rel] ?? [];
  };
const row = (kind, name, path = name, size = 0, added = false) => ({ kind, name, path, size, added });

test('the plan of an upload: a folder is made inside the open device folder, its contents follow, parents before children, structure kept', async () => {
  const log = [];
  const plan = await planUpload([row('folder', 'loops'), row('file', 'a.wav', 'a.wav', 5)], 'drums', lister(log));
  assert.deepEqual(plan.mkdirs, ['drums/loops', 'drums/loops/breaks', 'drums/loops/breaks/deep']);
  assert.deepEqual(
    plan.files.map((f) => [f.folder, f.name, f.rel]),
    [
      ['drums/loops/breaks/deep', 'y.wav', 'loops/breaks/deep/y.wav'],
      ['drums/loops/breaks', 'x.wav', 'loops/breaks/x.wav'],
      ['drums/loops', 'b.wav', 'loops/b.wav'],
      ['drums/loops', 'z.wav', 'loops/z.wav'],
      ['drums', 'a.wav', 'a.wav'],
    ],
    'depth first, as the list reads: in each folder, its folders come before its files, each by name',
  );
  assert.deepEqual(log, ['loops', 'loops/breaks', 'loops/breaks/deep']);
});

test('the plan: an empty folder is made, a file goes to the target, an added file keeps its flag, the top is the target', async () => {
  const plan = await planUpload([row('folder', 'empty'), row('file', 'f.wav', '', 7, true)], '', lister());
  assert.deepEqual(plan.mkdirs, ['empty']);
  assert.deepEqual(
    plan.files.map((f) => [f.folder, f.name, f.added, f.size]),
    [['', 'f.wav', true, 7]],
  );
  const none = await planUpload([], 'x', lister());
  assert.deepEqual(none, { mkdirs: [], files: [] });
});

test('the plan refuses a link loop and a huge folder with a clear message', async () => {
  const loop = async () => [folder('again'), file('f.wav')];
  await assert.rejects(planUpload([row('folder', 'a')], '', loop), /nest more than \d+ levels/);
  const many = async () => Array.from({ length: 21000 }, (_, i) => file(`f${i}.wav`));
  await assert.rejects(planUpload([row('folder', 'big')], '', many), /more than \d+ files/);
});

test('messages: no folders on the device, a folder dragged in from outside', () => {
  assert.match(noFoldersMessage('Digitone II'), /Digitone II storage has no folders/);
  assert.match(foldersFromOutside(1), /^A folder dragged in from outside/);
  assert.match(foldersFromOutside(3), /^3 folders dragged in/);
});

/** A backend that has folders: `list` answers from `have` (folder path -> names), `mkdir` is recorded. */
const folderBackend = (have = { '': [] }, failMkdir = '') => {
  const calls = [];
  const base = fakeBackend();
  return {
    calls,
    put: async (f, p, o) => {
      calls.push(`put ${f}/${p.name}`);
      return base.put(f, p, o);
    },
    verify: base.verify,
    async list(f) {
      calls.push(`list ${f}`);
      return (have[f] ?? []).map((name) => ({ name, kind: 'folder', size: 0 }));
    },
    async mkdir(f) {
      calls.push(`mkdir ${f}`);
      if (f === failMkdir) throw new Error('{"error":"E_FULL: no space","kind":"device"}');
    },
  };
};
const item = (folderPath, name, size = 10) => ({
  folder: folderPath,
  name,
  size,
  read: async () => new Uint8Array(size),
});
const runItems = async (backend, mkdirs, items) => {
  let u = {};
  const events = [];
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
  const state = (key) => [u[key].state, u[key].message];
  assert.deepEqual(state('loops/b').slice(0, 1), ['ok']);
  assert.equal(u['loops/breaks/deep/y.wav'].state, 'failed');
  assert.match(
    u['loops/breaks/deep/y.wav'].message,
    /not sent: the folder loops\/breaks could not be made: E_FULL: no space/,
  );
  assert.equal(u['loops/breaks/x.wav'].state, 'failed');
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
  assert.equal(u['d/gone.wav'].state, 'failed');
  assert.match(u['d/gone.wav'].message, /no such file/);
  assert.equal(u['d/ok'].state, 'ok');
});

test('events: a queued file takes the name and size it has on the device in place; queued bytes count until then', () => {
  let u = {};
  u = applyUpload(u, { event: 'queued', folder: 'd', name: 'a.wav', bytes: 100 });
  u = applyUpload(u, { event: 'queued', folder: 'd', name: 'b.wav', bytes: 50 });
  assert.deepEqual([u['d/a.wav'].state, u['d/a.wav'].bytes, u['d/a.wav'].kind], ['waiting', 100, 'none']);
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
  assert.deepEqual([u['d/a'].bytes, u['d/a'].kind], [90, 'converted']);
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

test('a folder row: pending while an upload fills it, with the state of the uploads inside it', () => {
  const up = (path, state, sent, bytes) => {
    const i = path.lastIndexOf('/');
    return {
      folder: path.slice(0, i),
      name: path.slice(i + 1),
      source: path,
      kind: 'converted',
      state,
      sent,
      bytes,
      message: '',
    };
  };
  const uploads = {
    'loops/a': up('loops/a', 'ok', 10, 10),
    'loops/breaks/x': up('loops/breaks/x', 'uploading', 5, 20),
    'loops/breaks/deep/y': up('loops/breaks/deep/y', 'waiting', 0, 10),
  };
  // not listed yet: a pending folder row (once, even with many files inside)
  const top = sampleRows('', [file('init')], uploads);
  assert.deepEqual(
    top.map((r) => [r.name, r.kind, r.pending]),
    [
      ['loops', 'folder', true],
      ['init', 'file', false],
    ],
  );
  assert.deepEqual(top[0].status, { kind: 'uploading', sent: 15, bytes: 40, message: '' });
  // inside it: the files of this level and the folders below, each with its own state
  const inside = sampleRows('loops', [folder('breaks'), file('a', 10)], uploads);
  assert.deepEqual(
    inside.map((r) => [r.name, r.kind, r.pending, r.status?.kind]),
    [
      ['breaks', 'folder', false, 'uploading'],
      ['a', 'file', false, 'ok'],
    ],
  );
  // an upload of another folder does not make rows here
  assert.deepEqual(sampleRows('other', [], uploads), []);
  assert.equal(folderStatus(uploads, 'nothing'), undefined);
  assert.equal(folderStatus({ 'f/a': up('f/a', 'waiting', 0, 5) }, 'f').kind, 'waiting');
  assert.equal(folderStatus({ 'f/a': up('f/a', 'ok', 5, 5) }, 'f').kind, 'ok');
  const mixed = folderStatus({ 'f/a': up('f/a', 'ok', 5, 5), 'f/b': up('f/b', 'failed', 0, 5) }, 'f');
  assert.deepEqual([mixed.kind, mixed.message], ['failed', '1 of 2 files failed']);
});

test('the delete texts: progress, what is left, and the armed button for a folder', () => {
  assert.equal(deletingLine(20, 357), 'Deleting 20 of 357');
  assert.equal(deletingLine(2, 9, 'Drums'), 'Deleting Drums: 2 of 9');
  assert.equal(deletePartLine(20, 357), '20 of 357 files deleted, 337 left');
  assert.equal(deletePartLine(0, 1), '0 of 1 file deleted, 1 left');
  assert.equal(confirmDeleteLabel([file('a')]), undefined);
  assert.equal(confirmDeleteLabel([folder('d')]), 'Delete this folder and everything in it?');
  assert.match(confirmDeleteLabel([folder('d'), file('a')]), /^Delete 2 items, folders with everything/);
});

// ---- skipping what the device holds already ----------------------------------------------------------------------------------------

/** A backend with a device: `have` is folder -> name -> bytes. `sameOnDevice` compares size and bytes as the real backends compare size and hash. */
const deviceBackend = (have = {}, { failList = null, cancelList = null } = {}) => {
  const calls = [];
  const base = fakeBackend();

  return {
    calls,
    put: async (f, p, o) => {
      calls.push(`put ${f}/${p.name}`);
      return base.put(f, p, o);
    },
    verify: async (f, p) => {
      calls.push(`verify ${f}/${p.name}`);
      return base.verify(f, p);
    },
    async list(f) {
      calls.push(`list ${f}`);
      if (f === failList) throw new Error('{"error":"E_BUSY: card busy","kind":"device"}');
      if (f === cancelList) throw cancelError();
      return Object.entries(have[f] ?? {}).map(([name, b]) => ({
        name,
        kind: 'file',
        size: b.length,
        hash: b.join(','),
      }));
    },
    sameOnDevice: (p, listing) =>
      listing.some((e) => e.name === p.name && e.size === p.stored.length && e.hash === p.stored.join(',')),
    async mkdir(f) {
      calls.push(`mkdir ${f}`);
    },
  };
};
const bytesOf = (n, fill) => new Uint8Array(n).fill(fill);
const filled = (folderPath, name, n, fill) => ({
  folder: folderPath,
  name,
  size: n,
  read: async () => bytesOf(n, fill),
});
const runAll = async (backend, mkdirs, items, stop) => {
  let u = {};
  const events = [];
  await uploadItems(
    backend,
    prepare,
    mkdirs,
    items,
    (e) => {
      events.push(e);
      u = applyUpload(u, e);
    },
    stop,
  );
  return { u, events };
};

test('a file that the device holds with the same bytes is skipped: no put, no verify, its own state, a Log line, counted as done', async () => {
  const b = deviceBackend({ d: { a: bytesOf(10, 1), b: bytesOf(10, 2) } });
  const { u } = await runAll(b, [], [filled('d', 'a', 10, 1), filled('d', 'b', 10, 9), filled('d', 'c', 10, 3)]);
  assert.deepEqual(names(u), [
    ['a', 'same'],
    ['b', 'ok'],
    ['c', 'ok'],
  ]);
  assert.deepEqual(b.calls, ['list d', 'put d/b', 'verify d/b', 'put d/c', 'verify d/c']);
  assert.equal(u['d/a'].sent, 10);
  assert.match(u['d/a'].message, /delete it and upload it again/);
  assert.deepEqual(uploadLines(u, {}), ['a: already on the device', 'b: verified', 'c: verified']);
  assert.equal(uploadFailure(u, {}), undefined);
  assert.equal(uploadCancelled(u, {}), undefined);
  assert.equal(uploadStatus(u['d/a']).kind, 'same');
});

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
