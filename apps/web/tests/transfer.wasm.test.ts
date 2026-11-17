import type {
  Group,
  RowTag,
  TransferEvent,
  TransferRow,
  TransferView,
} from '@digi/workbench-core/transfer/transfer-core.ts';
import { useFitWasm } from './fit-wasm.ts';

await useFitWasm();
// The pure logic of the Transfer page (packages/workbench-core/src/transfer/transfer-core.ts).
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
  installServices,
  tickCountText,
  installResultLines,
  deleteResultLines,
  neededServiceText,
  serviceRefusalText,
  serviceTags,
  modulesBar,
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
  tickedLoad,
  MODES,
  parseMode,
  storageLine,
  tickedFit,
  warningText,
} from '@digi/workbench-core/transfer/transfer-core.ts';

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
  assert.deepEqual(
    texts(localTags({ ...sine, fits: true })),
    [],
    'fitting is the usual state: only a module that does not fit says so',
  );
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
  assert.deepEqual(texts(deviceTags({ ...sine, on_card: 'yes', not_loaded: 'sine not loaded: arena full' })), [
    'not loaded',
  ]);
  assert.deepEqual(
    texts(deviceTags({ ...sine, on_card: 'yes', not_loaded: 'sine refused: no room', load_state: 'REFUSED' })),
    ['refused'],
  );
  assert.deepEqual(
    texts(deviceTags({ ...sine, on_card: 'yes', not_loaded: 'sine skipped: safe mode', load_state: 'SKIPPED' })),
    ['skipped'],
  );
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
  // the load after the next boot, shown beside a set that fits; none for a set that does not
  assert.match(tickedLoad(v, new Set(['a', 'c']))!, /^DSP \d+%/);
  assert.equal(tickedLoad(v, new Set(['a', 'b'])), null);
  assert.equal(tickedLoad(v, new Set()), null);
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

const fx = { stem: 'fx', label: 'fx service', version: 0x010200, state: 'not on device', size: 38400 } as const;
const machines = {
  stem: 'machines',
  label: 'machines service',
  version: 0x010200,
  state: 'on device',
  size: 62464,
} as const;
const pluck = row('a/pluck', 'Machines', { label: 'pluck', services: [fx, machines] });
const shot = row('a/shot', 'Machines', { label: 'shot', services: [machines] });
const sv = { ...view, modules: [pluck, shot] };

test('services: the with line, sub row tags, the fit line and the services to upload', () => {
  assert.deepEqual(tickCountText(0, 12), { lead: '', rest: '12 items' });
  assert.deepEqual(tickCountText(0, 1), { lead: '', rest: '1 item' });
  assert.deepEqual(tickCountText(0, 0), { lead: '', rest: '' });
  assert.deepEqual(tickCountText(3, 12), { lead: '3', rest: ' of 12 selected' });
  assert.deepEqual(
    serviceTags(fx).map((t) => [t.text, t.tone]),
    [['not on device', 'warn']],
  );
  assert.deepEqual(
    serviceTags(machines).map((t) => [t.text, t.tone]),
    [['on device', 'ok']],
  );
  assert.equal(serviceTags({ ...fx, state: 'differs' })[0].text, 'differs');
  // each service once, skipping the one on the device
  assert.deepEqual(installServices(sv, new Set(['a/pluck', 'a/shot'])), [
    { id: 'fx', path: 'services/fx.dks', bytes: 38400 },
  ]);
  assert.deepEqual(installModules(sv, new Set(['a/pluck'])), ['a/pluck']);
});

test('services: tickedFit counts the services once and enforces the provider limit', () => {
  const caps = {
    MODULES: { limit: 8, used: 0, largest: 0 },
    SELECTION: { limit: 8, used: 0, largest: 0 },
    ARENA: { limit: 524288, used: 0, largest: 524288 },
    STAGE: { limit: 65536, used: 0, largest: 65536 },
    DSP_AREA: { limit: 524288, used: 0, largest: 524288 },
  };
  const needs = { file_size: 900, ARENA: 64, DSP_AREA: 8, DSP_ALIGN: 8 };
  const v = {
    ...sv,
    capacity: caps,
    modules: [
      { ...pluck, needs },
      { ...shot, needs },
    ],
  };
  assert.deepEqual(tickedFit(v, new Set(['a/pluck'])), {
    text: 'The ticked module and its 2 services fit the device.',
    tone: 'ok',
  });
  assert.equal(
    tickedFit(v, new Set(['a/pluck', 'a/shot']))!.text,
    'The 2 ticked modules and their 2 services fit the device together.',
  );
  const many = Array.from({ length: 7 }, (_, i) => ({ ...fx, stem: `s${i}`, label: `s${i} service` }));
  const big = { ...v, modules: [{ ...pluck, needs, services: many }] };
  assert.deepEqual(tickedFit(big, new Set(['a/pluck'])), {
    text: 'The selection needs more than 6 services.',
    tone: 'warn',
  });
  assert.equal(tickedLoad(big, new Set(['a/pluck'])), null);
  // the memory of a service counts: a module that fits alone does not fit with a service that needs more DSP area than is left
  const heavy = { ...fx, needs: { file_size: 900, ARENA: 64, DSP_AREA: 600000, DSP_ALIGN: 8 } };
  const withHeavy = { ...v, modules: [{ ...pluck, needs, services: [heavy] }] };
  const noFit = tickedFit(withHeavy, new Set(['a/pluck']))!;
  assert.equal(noFit.tone, 'warn');
  assert.match(noFit.text, /^The services of the ticked modules do not fit the device with them: /);
  assert.equal(tickedLoad(withHeavy, new Set(['a/pluck'])), null);
});

test('services: refusal texts, the service mismatch tag, the needed-by tag', () => {
  const r = { module: 'pluck', service: 'machines' } as const;
  assert.equal(
    serviceRefusalText({ ...r, kind: 'fingerprint', runtime: '0.5.2', builtFor: '0.5.0' }),
    'pluck not installed. It needs the machines service, and this workbench has no machines service for the runtime on your device (runtime 0.5.2, service built for 0.5.0). Nothing was uploaded for pluck. Update the workbench, or flash a matching runtime from the Firmware page.',
  );
  assert.equal(
    serviceRefusalText({ module: 'pluck', service: 'fx', kind: 'missing' }),
    'pluck not installed. It needs the fx service, which this workbench does not have.',
  );
  assert.equal(
    serviceRefusalText({ module: 'pluck', service: 'fx', kind: 'unknown-hash' }),
    'pluck not installed. It needs the fx service, which this workbench does not have.',
  );
  assert.equal(
    serviceRefusalText({ module: 'pluck', service: 'fx', kind: 'version', need: '1.3', have: '1.2' }),
    'pluck not installed. It needs fx service 1.3, the runtime has 1.2.',
  );
  assert.equal(
    serviceRefusalText({ module: 'pluck', service: 'fx', kind: 'too-many' }),
    'pluck not installed. The selection needs more than 6 services.',
  );
  const refusal = { ...r, kind: 'fingerprint', runtime: '0.5.2', builtFor: '0.5.0' } as const;
  const [tag] = localTags({ ...pluck, service_refusal: refusal }).filter((t) => t.text === 'service mismatch');
  assert.equal(tag.tone, 'warn');
  assert.equal(tag.title, 'machines service built for 0.5.0, device runtime is 0.5.2');
  const dks = { ...row(null, 'Tools'), path: 'services/machines.dks', needed_by: ['shot'] };
  assert.deepEqual(
    deviceTags(dks).map((t) => [t.text, t.tone]),
    [['needed by shot', 'dim']],
  );
  assert.equal(
    neededServiceText('machines', ['shot']),
    'machines service is needed by shot. Delete that module first (its services go with it).',
  );
});

test('services: the install progress runs the service phase first and names refused modules', () => {
  let p = startProgress(['a/pluck', 'a/freqshift'], false, { 'a/pluck': 1000, 'a/freqshift': 1000 }, [
    { id: 'fx', path: 'services/fx.dks', bytes: 4000 },
    { id: 'machines', path: 'services/machines.dks', bytes: 4000 },
  ]);
  assert.deepEqual(
    p.modules.map((m) => m.kind),
    ['service', 'service', undefined, undefined],
  );
  const ev = (e: TransferEvent) => (p = applyEvent(p, e));
  ev({ event: 'upload', kind: 'service', id: 'fx', path: 'services/fx.dks', bytes: 4000 });
  ev({ event: 'progress', kind: 'service', id: 'fx', sent: 1600, bytes: 4000 });
  assert.equal(p.message, 'Uploading services/fx.dks: 40%');
  ev({ event: 'verify', kind: 'service', id: 'fx', path: 'services/fx.dks', ok: true });
  assert.equal(p.message, 'Verified services/fx.dks on the card');
  ev({ event: 'skip', kind: 'service', id: 'machines', path: 'services/machines.dks' });
  assert.equal(p.message, 'services/machines.dks: already on the card, not uploaded');
  assert.equal(p.modules[0].state, 'ok');
  assert.equal(p.modules[2].state, 'wait', 'a module with the same id as no service is not touched');
  assert.equal(rowStatus(p, {}, 'services/fx.dks')!.kind, 'ok');
  // a refused module leaves the steps; the other one installs
  const refusal = { module: 'a/pluck', service: 'machines', kind: 'missing' } as const;
  ev({ event: 'refuse', refusal });
  assert.deepEqual(
    p.modules.map((m) => m.id),
    ['fx', 'machines', 'a/freqshift'],
  );
  assert.equal(p.message, 'a/pluck not installed. It needs the machines service, which this workbench does not have.');
  assert.deepEqual(installResultLines(p, ['a/pluck', 'a/freqshift']), [
    'a/pluck not installed. It needs the machines service, which this workbench does not have.',
    'freqshift was installed.',
  ]);
  assert.equal(modulesBar(p).message, p.message);
  assert.equal(progressFailed(p), false);
  // every ticked module refused: the bar fails and says no files went up
  let q = startProgress(['a/pluck'], false, {}, []);
  q = applyEvent(q, { event: 'refuse', refusal });
  assert.equal(progressFailed(q), true);
  assert.equal(modulesBar(q).message, 'Install failed: no files uploaded');
  assert.deepEqual(installResultLines(startProgress(['a/x'], false), ['a/x']), []);
});

test('services: delete texts', () => {
  assert.equal(eventText({ event: 'delete', path: 'modules/pluck.dkm' }), 'Deleting modules/pluck.dkm');
  assert.equal(
    eventText({ event: 'delete', path: 'services/fx.dks', why: 'unused-service' }),
    'Deleted services/fx.dks (no module on the card needs it)',
  );
  assert.equal(
    eventText({ event: 'keep', path: 'services/machines.dks', by: ['shot'] }),
    'Kept services/machines.dks (shot needs it)',
  );
  assert.deepEqual(
    deleteResultLines([
      { event: 'delete', path: 'modules/pluck.dkm' },
      { event: 'delete', path: 'services/fx.dks', why: 'unused-service' },
      { event: 'keep', path: 'services/machines.dks', by: ['shot'] },
    ]),
    [
      'Deleted modules/pluck.dkm',
      'Deleted services/fx.dks (no module on the card needs it)',
      'Kept services/machines.dks (shot needs it)',
      'Restart the device to apply.',
    ],
  );
  assert.deepEqual(deleteResultLines([]), []);
});
