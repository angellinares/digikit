import type { Group, RowTag, TransferEvent, TransferRow, TransferView } from '../src/transfer/transfer-core.ts';
import { useFitWasm } from './fit-wasm.ts';

await useFitWasm();
// The pure logic of the Transfer page (apps/web/src/transfer/transfer-core.ts).
import assert from 'node:assert/strict';
import { test } from 'vitest';
import {
  DONE_ON_CARD,
  applyEvent,
  applyVerified,
  deleteDoneText,
  deleteLabel,
  deletePaths,
  available,
  eventText,
  installModules,
  onDevice,
  onDeviceWithPending,
  progressFailed,
  rowStatus,
  progressFraction,
  startProgress,
  deviceTags,
  freeText,
  freeLine,
  localTags,
  MODES,
  parseMode,
  storageLine,
  tickedFit,
  warningText,
} from '../src/transfer/transfer-core.ts';

const row = (id: string | null, group: Group, extra: Partial<TransferRow> = {}): TransferRow => ({
  id,
  path: `modules/${(id ?? 'x').split('/').pop()}.dkm`,
  name: id ?? 'x',
  group,
  test: !!id && id.startsWith('test/'),
  version: '0.1.0',
  file_size: 100,
  on_card: 'no',
  loaded: false,
  ...extra,
});
const view: TransferView = {
  device: 'dt2',
  runtime: '0.2.4',
  free_bytes: 1000,
  loaded: ['someone/machine', 'someone/effect'],
  modules: [
    row('someone/machine', 'Machines', { loaded: true, on_card: 'yes' }),
    row('test/sine', 'Machines'),
    row('test/dyn-demo', 'Tools', { warnings: ['arena: needs 5, largest free 1'], fits: false }),
  ],
  card_only: [
    { ...row(null, 'Effects', { loaded: true, on_card: 'yes', name: 'someone/effect' }), path: 'modules/effect.dkm' },
    { ...row(null, 'Tools', { name: 'junk' }), path: 'modules/junk.dkm' },
  ],
};

test('the lists: on device is every card file, available is the repo in three groups', () => {
  assert.deepEqual(
    onDevice(view).map((r) => r.path),
    ['modules/effect.dkm', 'modules/junk.dkm', 'modules/machine.dkm'],
  );
  const a = available(view, false);
  assert.deepEqual(
    a.map((g) => g.group),
    ['Machines', 'Effects', 'Tools'],
  );
  assert.deepEqual(
    a[0].rows.map((r) => r.id),
    ['someone/machine'],
  );
  assert.equal(a[1].rows.length, 0); // Effects: no run-time effects yet
  assert.equal(a[2].rows.length, 0); // the test module is hidden
  assert.equal(available(view, true)[2].rows.length, 1);
  assert.equal(available({ ...view, modules: [] }, true)[1].rows.length, 0);
});

test('ticks mean install: only ticked repo modules, in list order, none to start with', () => {
  assert.deepEqual(installModules(view, new Set()), []);
  assert.deepEqual(installModules(view, new Set(['test/sine', 'someone/machine'])), ['someone/machine', 'test/sine']);
  // a card-only path is never an install
  assert.deepEqual(installModules(view, new Set(['modules/effect.dkm'])), []);
});

test('texts', () => {
  assert.equal(eventText({ event: 'build', id: 'a/b' }), 'Building a/b');
  assert.equal(eventText({ event: 'skip', id: 'a/b' }), 'a/b: already on the card, not uploaded');
  assert.equal(eventText({ event: 'verify', path: 'modules/x.dkm', ok: true }), 'Verified modules/x.dkm on the card');
  assert.equal(eventText({ event: 'done', restarted: false, selected: [] }), DONE_ON_CARD);
  assert.equal(DONE_ON_CARD, 'On the card. Select it in MODULES on the device, then apply and restart.');
  assert.equal(eventText({ event: 'done', restarted: true, loaded: ['x', 'y'] }), 'Restarted. Loaded: x, y');
  assert.equal(eventText({ event: 'done', restarted: true, loaded: [] }), 'Restarted. No module is loaded.');
  assert.equal(eventText({ event: 'error', message: 'paused' }), 'Error: paused');
  assert.equal(warningText(view.modules[2]), 'arena: needs 5, largest free 1');
  assert.equal(warningText(view.modules[0]), '');
});

test('the install progress: bytes by upload, small fixed shares, no select, no result step', () => {
  let p = startProgress(['a/x', 'a/y'], false, { 'a/x': 10000, 'a/y': 10000 });
  assert.equal(p.restart, 'none');
  assert.equal('select' in p, false);
  assert.equal(progressFraction(p), 0);
  const ev = (e: TransferEvent) => (p = applyEvent(p, e));
  ev({ event: 'upload', id: 'a/x', path: 'modules/x.dkm', bytes: 10000 });
  // the build share of x is over; its bytes have not started
  const base = progressFraction(p);
  assert.ok(base > 0 && base < 0.1, String(base));
  ev({ event: 'progress', id: 'a/x', sent: 5000, bytes: 10000 });
  assert.equal(p.message, 'Uploading modules/x.dkm: 50%');
  assert.ok(progressFraction(p) > base);
  ev({ event: 'progress', id: 'a/x', sent: 10000, bytes: 10000 });
  const full = progressFraction(p);
  ev({ event: 'verify', id: 'a/x', path: 'modules/x.dkm', ok: true });
  assert.ok(progressFraction(p) > full);
  assert.deepEqual(
    p.modules.map((m) => m.state),
    ['ok', 'wait'],
  );
  // a skipped upload jumps straight to the verify: all its bytes count at once
  ev({ event: 'upload', id: 'a/y', path: 'modules/y.dkm', bytes: 10000 });
  const before = progressFraction(p);
  ev({ event: 'skip', id: 'a/y' });
  assert.ok(progressFraction(p) > before);
  assert.equal(p.modules[1].sent, 10000);
  ev({ event: 'verify', id: 'a/y', path: 'modules/y.dkm', ok: true });
  assert.equal(progressFraction(p), 1);
  assert.equal(progressFailed(p), false);
  // the done text is the last text; the `result` line does not replace it
  ev({ event: 'done', restarted: false, selected: [] });
  assert.equal(p.message, DONE_ON_CARD);
  ev({ event: 'result', built: [], uploaded: [], selected: [], restarted: false });
  assert.equal(p.message, DONE_ON_CARD);
  // a failed verify is red and not counted
  p = startProgress(['a/x'], false, { 'a/x': 100 });
  ev({ event: 'upload', id: 'a/x', path: 'p', bytes: 100 });
  ev({ event: 'verify', id: 'a/x', path: 'p', ok: false });
  assert.equal(p.modules[0].state, 'failed');
  assert.ok(progressFraction(p) < 1);
  assert.equal(progressFailed(p), true);
  // an error mid-upload marks the module failed
  p = startProgress(['a/x'], false);
  ev({ event: 'upload', id: 'a/x', path: 'p', bytes: 100 });
  ev({ event: 'error', message: 'full' });
  assert.equal(p.modules[0].state, 'failed');
});

test('a file row: upload bar, verifying, then verified or failed; a pending row while it uploads', () => {
  let p = startProgress(['test/sine'], false, { 'test/sine': 700 });
  assert.equal(rowStatus(p, {}, 'modules/sine.dkm'), undefined);
  p = applyEvent(p, { event: 'upload', id: 'test/sine', path: 'modules/sine.dkm', bytes: 700 });
  assert.deepEqual(rowStatus(p, {}, 'modules/sine.dkm'), { kind: 'uploading', sent: 0, bytes: 700 });
  // the file is not on the card yet: the On device list gets a pending row for it
  const rows = onDeviceWithPending(view, p);
  const pending = rows.find((r) => r.path === 'modules/sine.dkm');
  assert.ok(pending);
  assert.equal(pending.id, 'test/sine');
  assert.equal(pending.file_size, 700);
  assert.equal(rows.filter((r) => r.path === 'modules/machine.dkm').length, 1); // no double rows
  p = applyEvent(p, { event: 'progress', id: 'test/sine', sent: 350, bytes: 700 });
  assert.deepEqual(rowStatus(p, {}, 'modules/sine.dkm'), { kind: 'uploading', sent: 350, bytes: 700 });
  p = applyEvent(p, { event: 'progress', id: 'test/sine', sent: 700, bytes: 700 });
  assert.deepEqual(rowStatus(p, {}, 'modules/sine.dkm'), { kind: 'verifying' });
  p = applyEvent(p, { event: 'verify', id: 'test/sine', path: 'modules/sine.dkm', ok: true });
  assert.deepEqual(rowStatus(p, {}, 'modules/sine.dkm'), { kind: 'ok' });
  // the file that is already on the card gets its bar in its own row (no pending row)
  p = startProgress(['someone/machine'], false);
  p = applyEvent(p, { event: 'upload', id: 'someone/machine', path: 'modules/machine.dkm', bytes: 100 });
  assert.equal(onDeviceWithPending(view, p).length, onDevice(view).length);
  assert.equal(rowStatus(p, {}, 'modules/machine.dkm')!.kind, 'uploading');
  // without an install: the last VERIFY
  assert.deepEqual(rowStatus(undefined, { 'modules/x.dkm': 'failed' }, 'modules/x.dkm'), { kind: 'failed' });
});

test('the restart progress: one step, green after the boot, red on an error', () => {
  let p = startProgress([], true);
  const ev = (e: TransferEvent) => (p = applyEvent(p, e));
  assert.equal(progressFraction(p), 0);
  ev({ event: 'restart' });
  assert.equal(p.restart, 'build');
  ev({ event: 'done', restarted: true, loaded: [], missing: [] });
  assert.equal(p.restart, 'ok'); // nothing was expected: no error for missing modules
  assert.equal(progressFraction(p), 1);
  p = startProgress([], true);
  ev({ event: 'restart' });
  ev({ event: 'error', message: 'no new boot' });
  assert.equal(p.restart, 'failed');
  assert.equal(progressFailed(p), true);
});

test('verify results are kept by path', () => {
  let v = {};
  v = applyVerified(v, { event: 'verify', path: 'modules/x.dkm', ok: true });
  v = applyVerified(v, { event: 'verify', path: 'modules/y.dkm', ok: false });
  v = applyVerified(v, { event: 'upload', path: 'modules/z.dkm' });
  assert.deepEqual(v, { 'modules/x.dkm': 'ok', 'modules/y.dkm': 'failed' });
});

test('delete: the ticked card files, the button label, the done text', () => {
  // repo modules that are on the card and card-only files are both rows of the On device pane, keyed by path
  assert.deepEqual(deletePaths(view, new Set()), []);
  assert.deepEqual(deletePaths(view, new Set(['modules/machine.dkm', 'modules/junk.dkm'])), [
    'modules/junk.dkm',
    'modules/machine.dkm',
  ]);
  // a file that is not on the card (an Available row) or gone since is dropped
  assert.deepEqual(deletePaths(view, new Set(['modules/sine.dkm', 'modules/gone.dkm'])), []);
  assert.equal(deleteLabel(0, false), 'Delete');
  assert.equal(deleteLabel(2, false), 'Delete (2)');
  assert.equal(deleteLabel(2, true), 'Confirm delete (2)');
  assert.equal(deleteLabel(0, true), 'Delete', 'nothing ticked: no count and nothing to confirm');
  assert.equal(deleteDoneText(view, ['modules/junk.dkm']), 'Deleted 1 file.');
  // machine is loaded: it stays loaded until the restart
  assert.equal(
    deleteDoneText(view, ['modules/machine.dkm', 'modules/junk.dkm']),
    'Deleted 2 files. someone/machine: deleted; unloads at restart.',
  );
});

test('the delete progress: one step per path, active then done, text from the events', () => {
  let p = startProgress(['modules/a.dkm', 'modules/b.dkm'], false);
  const ev = (e: TransferEvent) => (p = applyEvent(p, e));
  ev({ event: 'start', paths: ['modules/a.dkm', 'modules/b.dkm'] });
  ev({ event: 'delete', path: 'modules/a.dkm' });
  assert.equal(p.message, 'Deleting modules/a.dkm');
  const half = progressFraction(p);
  assert.ok(half > 0 && half < 0.5, String(half));
  ev({ event: 'delete', path: 'modules/b.dkm' });
  assert.deepEqual(
    p.modules.map((m) => m.state),
    ['ok', 'verify'],
  );
  ev({ event: 'done', deleted: ['modules/a.dkm', 'modules/b.dkm'] });
  assert.equal(progressFraction(p), 1);
  assert.equal(p.message, 'Deleted 2 files');
  // an error marks the active path failed
  p = startProgress(['modules/a.dkm'], false);
  ev({ event: 'delete', path: 'modules/a.dkm' });
  ev({ event: 'error', message: 'E_FULL' });
  assert.equal(p.modules[0].state, 'failed');
  assert.equal(progressFailed(p), true);
});

test('the tags: only what says something; the usual state has none', () => {
  const [machine, sine0] = view.modules;
  const sine = { ...sine0, test: false };
  const texts = (tags: RowTag[]) => tags.map((t) => t.text);
  // On your computer: what the device holds of a module, a test build, a fit warning
  assert.deepEqual(texts(localTags({ ...sine, on_card: 'no' })), [], 'not on the device is the usual state');
  assert.deepEqual(texts(localTags({ ...sine, on_card: 'yes' })), ['on device']);
  assert.deepEqual(texts(localTags({ ...sine, on_card: 'other version' })), ['differs']);
  assert.match(localTags({ ...sine, on_card: 'other version' })[0].title!, /Install replaces it/);
  assert.deepEqual(texts(localTags({ ...sine0, test: true, on_card: 'yes' })), ['test', 'on device']);
  const fit = localTags({ ...sine, warnings: ['arena 2 MiB is more than the free 1 MiB'] });
  assert.deepEqual(
    [fit[0].text, fit[0].tone, fit[0].title],
    ["won't fit", 'warn', 'arena 2 MiB is more than the free 1 MiB'],
  );
  assert.deepEqual(texts(localTags({ ...sine, fits: true })), ['fits'], 'a module that fits says so');
  assert.deepEqual(
    texts(localTags({ ...sine, fits: true, loaded: true, on_card: 'yes' })),
    ['on device'],
    'a loaded one says nothing of fit',
  );
  // On device: loaded first, then how the file stands against the library
  assert.deepEqual(texts(deviceTags(machine)), ['loaded'], 'the same as the library is not said');
  assert.deepEqual(texts(deviceTags({ ...sine, on_card: 'yes' })), []);
  assert.deepEqual(texts(deviceTags({ ...sine, on_card: 'other version' })), ['differs']);
  assert.deepEqual(texts(deviceTags(view.card_only[1])), ['not in library']);
  assert.deepEqual(texts(deviceTags(view.card_only[0])), ['loaded', 'not in library']);
  assert.deepEqual(texts(deviceTags(sine)), [], 'a file that is still going there has no tag yet');
  // no tag says "card"
  for (const r of [...view.modules, ...view.card_only])
    for (const on_card of ['yes', 'no', 'other version'] as const)
      for (const t of [...localTags({ ...r, on_card }), ...deviceTags({ ...r, on_card })])
        assert.doesNotMatch(t.text + (t.title ?? ''), /on card/);
});

test('the mode of the page: Modules and Samples, Modules for anything unknown', () => {
  assert.deepEqual(
    MODES.map((m) => m.label),
    ['Modules', 'Samples'],
  );
  assert.equal(parseMode('samples'), 'samples');
  assert.equal(parseMode('modules'), 'modules');
  assert.equal(parseMode(null), 'modules');
  assert.equal(parseMode('card'), 'modules');
});

test('the device line: the free space in GiB from 1 GiB up, the same line for both modes', () => {
  assert.equal(freeText(512), '512 B');
  assert.equal(freeText(5 * 1024 * 1024), '5.00 MiB');
  assert.equal(freeText(2 ** 30), '1.00 GiB');
  assert.equal(freeText(19508 * 1024 * 1024), '19.05 GiB');
  assert.equal(freeText(2 ** 32 - 1 + 1), '4.00 GiB');
  assert.equal(freeLine(19508 * 1024 * 1024), '19.05 GiB free');
  assert.equal(freeLine(undefined), '');
});

test('the ticked modules together: the sum rule against the device limits, in list order', () => {
  const caps = {
    MODULES: { limit: 8, used: 3, largest: 0 }, // what is loaded now does not count: the next boot starts empty
    SELECTION: { limit: 8, used: 3, largest: 0 },
    ARENA: { limit: 524288, used: 9000, largest: 100 },
    STAGE: { limit: 65536, used: 0, largest: 65536 },
    DSP_AREA: { limit: 524288, used: 400000, largest: 100 },
  };
  const m = (id: string, dsp: number) =>
    row(id, 'Machines', { label: id, needs: { file_size: 900, ARENA: 64, DSP_AREA: dsp, DSP_ALIGN: 8 } });
  const v = {
    device: 'dt2',
    runtime: '0.3.0',
    free_bytes: 0,
    loaded: [],
    capacity: caps,
    card_only: [],
    modules: [m('a', 300000), m('b', 300000), m('c', 1000)],
  };
  assert.equal(tickedFit({ ...v, capacity: null }, new Set(['a'])), null, 'no CAPACITY, no verdict');
  assert.equal(tickedFit(v, new Set()), null, 'nothing ticked');
  assert.deepEqual(tickedFit(v, new Set(['a'])), { text: 'The ticked module fits the device.', tone: 'ok' });
  assert.deepEqual(tickedFit(v, new Set(['a', 'c'])), {
    text: 'The 2 ticked modules fit the device together.',
    tone: 'ok',
  });
  const two = tickedFit(v, new Set(['a', 'b']))!;
  assert.equal(two.tone, 'warn');
  assert.match(two.text, /^b does not fit with the ticked modules before it: DSP memory 293 KiB needed, 219 KiB free$/);
  const alone = tickedFit({ ...v, modules: [m('big', 600000)] }, new Set(['big']))!;
  assert.match(alone.text, /^big does not fit the device: DSP memory 586 KiB needed, 511 KiB free$/);
});

test('the storage line of Modules mode: files on the card, their size and the free space; no memory wording', () => {
  // three files of 100 bytes on the card (machine, effect, junk), 1000 bytes free
  assert.equal(storageLine(view), 'Modules: 3 files, 300 B · 1000 B free');
  assert.equal(
    storageLine({ ...view, modules: [], card_only: [view.card_only[0]], free_bytes: 3.5 * 2 ** 30 }),
    'Modules: 1 file, 100 B · 3.50 GiB free',
  );
  assert.equal(
    storageLine({ ...view, modules: [], card_only: [], free_bytes: 2 ** 30 }),
    'Modules: 0 files, 0 B · 1.00 GiB free',
  );
  assert.doesNotMatch(storageLine(view), /arena|memory|DSP|RAM/i);
  // free space is in the storage line, not in the device line, in Modules mode
  assert.equal(freeLine(undefined), '');
});
