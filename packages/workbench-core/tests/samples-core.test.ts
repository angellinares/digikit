// The pure logic of Samples mode of the Transfer page (packages/workbench-core/src/transfer/samples-core.ts): folders and breadcrumbs (device and local),
// the rows with their upload states, the local rows and which files convert (the labels). The upload engine is tested in sample-upload.test.ts.
import assert from 'node:assert/strict';
import { sourceKind, type Prepared } from '@digi/dk-session/dksample';
import type { SampleEntry } from '@digi/dk-session/samples';
import { test } from 'vitest';
import {
  confirmDeleteLabel,
  crumbs,
  deletePartLine,
  deleteRows,
  deletingLine,
  folderNameError,
  baseName,
  kindTag,
  localCrumbs,
  localRowTags,
  localRows,
  folderStatus,
  foldersFromOutside,
  noFoldersMessage,
  planUpload,
  sampleRows,
  selectedRows,
  uploadStatus,
} from '../src/transfer/samples-core.ts';
import {
  applyUpload,
  joinPath,
  parentOf,
  SAVING_MESSAGE,
  uploadCancelled,
  uploadFailure,
  uploadLines,
  type Upload,
  type Uploads,
} from '../src/transfer/sample-upload.ts';
import { batchBackend, bytesOf, deviceBackend, filled, items3, names, runAll } from './sample-fakes.ts';

const file = (name: string, size = 10): SampleEntry => ({ name, kind: 'file', size });
const folder = (name: string): SampleEntry => ({ name, kind: 'folder', size: 0 });

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
  const uploads: Uploads = {
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
  assert.deepEqual(rows[2].status, { kind: 'uploading', sent: 5, bytes: 20, message: '', checking: '' });
  assert.deepEqual(rows[3].status, { kind: 'ok', message: 'device content hash matches\nthe firmware hash' });
});

test('the state of an upload: waiting, uploading, verifying, ok, failed', () => {
  const u = (state: Upload['state'], sent = 0, bytes = 10, message = ''): Upload => ({
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
  assert.equal(uploadStatus(u('waiting'))!.kind, 'waiting');
  assert.equal(uploadStatus(u('uploading', 3))!.kind, 'uploading');
  assert.equal(uploadStatus(u('uploading', 10))!.kind, 'uploading'); // all sent: still awaiting completion
  assert.equal(uploadStatus(u('verifying', 10))!.kind, 'verifying');
  assert.equal(uploadStatus(u('ok', 10, 10, 'm'))!.message, 'm');
  assert.equal(uploadStatus(u('failed', 0, 10, 'no space'))!.kind, 'failed');
});

test('which files convert: the label says what was done', () => {
  assert.equal(kindTag('converted')!.text, 'WAV→');
  assert.equal(kindTag('native')!.text, 'native');
  assert.equal(kindTag('unchanged')!.text, 'other');
  assert.equal(kindTag('unchanged')!.title, "Won't play as a sample");
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

test('the local list: folders first, then files by name, then the added files; folders have no kind tag; the selection takes folders too', () => {
  const entries = [file('snare.wav', 40), folder('z'), file('kick', 30), folder('a')];
  const kinds: Record<string, Prepared['kind']> = {
    'drums/snare.wav': 'converted',
    'drums/kick': 'native',
    '+x.txt': 'unchanged',
  };
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

// ---- the explorer: the plan of an upload with folders, the folder rows ---------------------------------------------------------------

/** A local tree for the plan: path -> entries of that level. */
const tree: Record<string, SampleEntry[]> = {
  '': [folder('loops'), folder('empty'), file('a.wav', 5)],
  loops: [file('z.wav', 3), folder('breaks'), file('b.wav', 4)],
  'loops/breaks': [file('x.wav', 1), folder('deep')],
  'loops/breaks/deep': [file('y.wav', 2)],
  empty: [],
};
const lister =
  (log: string[] = []) =>
  async (rel: string) => {
    log.push(rel);
    return tree[rel] ?? [];
  };
const row = (kind: 'file' | 'folder', name: string, path = name, size = 0, added = false) => ({
  kind,
  name,
  path,
  size,
  added,
});

test('the plan of an upload: a folder is made inside the open device folder, its contents follow, parents before children, structure kept', async () => {
  const log: string[] = [];
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

test('a folder row: pending while an upload fills it, with the state of the uploads inside it', () => {
  const up = (path: string, state: Upload['state'], sent: number, bytes: number): Upload => {
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
  assert.deepEqual(top[0].status, { kind: 'uploading', sent: 15, bytes: 40, message: '', checking: '' });
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
  assert.equal(folderStatus({ 'f/a': up('f/a', 'waiting', 0, 5) }, 'f')!.kind, 'waiting');
  assert.equal(folderStatus({ 'f/a': up('f/a', 'ok', 5, 5) }, 'f')!.kind, 'ok');
  const mixed = folderStatus({ 'f/a': up('f/a', 'ok', 5, 5), 'f/b': up('f/b', 'failed', 0, 5) }, 'f')!;
  assert.deepEqual([mixed.kind, mixed.message], ['failed', '1 of 2 files failed']);
});

test('the delete texts: progress, what is left, and the armed button for a folder', () => {
  assert.equal(deletingLine(20, 357), 'Deleting 20 of 357');
  assert.equal(deletingLine(2, 9, 'Drums'), 'Deleting Drums: 2 of 9');
  assert.equal(deletePartLine(20, 357), '20 of 357 files deleted, 337 left');
  assert.equal(deletePartLine(0, 1), '0 of 1 file deleted, 1 left');
  assert.equal(confirmDeleteLabel([file('a')]), undefined);
  assert.equal(confirmDeleteLabel([folder('d')]), 'Delete this folder and everything in it?');
  assert.match(confirmDeleteLabel([folder('d'), file('a')]) ?? '', /^Delete 2 items, folders with everything/);
});

// ---- upload runs whose result is read through the page logic (uploadStatus) -----------------------------------------------------------

test('a file that the device holds with the same bytes is skipped: no put, no verify, its own state, a Log line, counted as done', async () => {
  const b = deviceBackend({ d: { a: bytesOf(10, 1), b: bytesOf(10, 2) } });
  const { u } = await runAll(b, [], [filled('d', 'a', 10, 1), filled('d', 'b', 10, 9), filled('d', 'c', 10, 3)]);
  assert.deepEqual(names(u), [
    ['a', 'same'],
    ['b', 'ok'],
    ['c', 'ok'],
  ]);
  assert.deepEqual(b.calls, ['list d', 'put d/b', 'verify d/b', 'put d/c', 'verify d/c']);
  assert.equal(u['d/a']!.sent, 10);
  assert.match(u['d/a']!.message, /delete it and upload it again/);
  assert.deepEqual(uploadLines(u, {}), ['a: already on the device', 'b: verified', 'c: verified']);
  assert.equal(uploadFailure(u, {}), undefined);
  assert.equal(uploadCancelled(u, {}), undefined);
  assert.equal(uploadStatus(u['d/a']!)!.kind, 'same');
});

test('deferred files wait as "Saving…" and become ok when the commit after the last file succeeds; no SysEx verify', async () => {
  const b = batchBackend();
  const { u, events } = await runAll(b, [], items3);
  assert.deepEqual(names(u), [
    ['a', 'ok'],
    ['b', 'ok'],
    ['c', 'ok'],
  ]);
  assert.deepEqual(b.calls, ['put d/a defer', 'put d/b defer', 'put d/c defer', 'commit']);
  // each file was saving before the commit made them ok, none was verified earlier
  const order = events.filter((e) => e.event === 'saving' || e.event === 'verified').map((e) => e.event);
  assert.deepEqual(order, ['saving', 'saving', 'saving', 'verified', 'verified', 'verified']);
  assert.match(u['d/a']!.message, /saved it with the files of its batch/);
  let w = applyUpload({}, { event: 'prepared', folder: 'd', name: 'a', source: 'a', kind: 'unchanged', bytes: 10 });
  w = applyUpload(w, { event: 'saving', folder: 'd', name: 'a' });
  assert.deepEqual([w['d/a']!.state, w['d/a']!.message], ['verifying', SAVING_MESSAGE]);
  const status = uploadStatus(w['d/a']!)!;
  assert.deepEqual([status.kind, status.message], ['verifying', SAVING_MESSAGE]);
});
