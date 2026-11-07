import type { UploadEvent, Uploads } from '../src/samples-core.ts';
type WithoutFolder<T> = T extends unknown ? Omit<T, 'folder'> : never;
type UploadEventWithoutFolder = WithoutFolder<UploadEvent>;
// The pure logic behind the Transfer page's behaviour: select all, the speed and the time left, when the page reads by itself, when it says
// "Waiting for ...", the bottom bar of both modes (apps/web/src/transfer: select-core, rate-core, refresh-core, wait-core, activity, transfer-core,
// samples-core). The pictures of the page are checked with `node tools/devtools/ui_shots.mjs` (docs/TESTING.md).
import assert from 'node:assert/strict';
import { test } from 'vitest';
import { instrument } from '../../../web/src/transfer/activity.ts';
import { createRate, etaSeconds, fmtEta, fmtRate, statsLine } from '../../../web/src/transfer/rate-core.ts';
import { FOCUS_GAP_MS, canAutoRefresh, focusReadDue, listingChanged } from '../../../web/src/transfer/refresh-core.ts';
import { applyUpload, formatRate, samplesBar, speedTracker } from '../src/samples-core.ts';
import {
  MARQUEE_SLOP,
  ROWS_TYPE,
  clickSelect,
  dragKeys,
  dragKind,
  dropFolder,
  edgeScroll,
  keepVisible,
  keyCommand,
  marqueeKeys,
  marqueeSelection,
  modsOf,
  moveSelect,
  rangeKeys,
  selectEverything,
  selectState,
  tickSelect,
  toggleAll,
} from '../../../web/src/transfer/select-core.ts';
import { countLabel, applyEvent, modulesBar, startProgress } from '../../../web/src/transfer/transfer-core.ts';
import { beginJob, busy, endJob, error, setError } from '../../../web/src/transfer/transfer-state.ts';
import { WAIT_QUIET_MS, createActivity, waitShown } from '../../../web/src/transfer/wait-core.ts';

test('select all: none, some or all of the visible rows; a click ticks all, or clears them when all are ticked', () => {
  const keys = ['a', 'b', 'c'];
  assert.equal(selectState(keys, new Set()), 'none');
  assert.equal(selectState(keys, new Set(['b'])), 'some');
  assert.equal(selectState(keys, new Set(['a', 'b', 'c'])), 'all');
  assert.equal(selectState([], new Set(['x'])), 'none', 'no rows: nothing to select');
  // ticks of rows that are not visible do not count
  assert.equal(selectState(keys, new Set(['x', 'y'])), 'none');
  assert.equal(selectState(keys, new Set(['a', 'b', 'c', 'x'])), 'all');
  // a click: none and some tick everything visible; all clears the visible rows
  assert.deepEqual([...toggleAll(keys, new Set())].sort(), ['a', 'b', 'c']);
  assert.deepEqual([...toggleAll(keys, new Set(['b']))].sort(), ['a', 'b', 'c']);
  assert.deepEqual([...toggleAll(keys, new Set(['a', 'b', 'c']))], []);
  // rows of another folder (not visible) keep their ticks either way
  assert.deepEqual([...toggleAll(keys, new Set(['x']))].sort(), ['a', 'b', 'c', 'x']);
  assert.deepEqual([...toggleAll(keys, new Set(['a', 'b', 'c', 'x']))], ['x']);
  const ticked = new Set(['a']);
  toggleAll(keys, ticked);
  assert.deepEqual([...ticked], ['a'], 'the given set is not changed');
});

test('button counts: in parentheses, none when nothing is ticked', () => {
  assert.equal(countLabel('Upload', 0), 'Upload');
  assert.equal(countLabel('Upload', 1), 'Upload (1)');
  assert.equal(countLabel('Install', 12), 'Install (12)');
});

test('the rate is smoothed over the last 2 s and falls when the transfer stops', () => {
  const r = createRate();
  assert.equal(r.rate(0), undefined, 'nothing yet');
  r.add(0, 0);
  assert.equal(r.rate(100), undefined, 'one sample, or too short a span, says nothing');
  r.add(1000, 1_000_000);
  assert.equal(r.rate(1000), 1_000_000);
  // a burst at the end does not jump the number: it is the average over the window
  r.add(1100, 2_000_000);
  const mid = r.rate(1100)!;
  assert.ok(mid > 1_000_000 && mid < 2_000_000, String(mid));
  // samples older than the window are forgotten: the speed is the recent one (3 MB over the last ~2 s)
  r.add(2000, 2_100_000);
  r.add(3000, 2_200_000);
  r.add(3100, 2_300_000);
  const recent = r.rate(3100)!;
  assert.ok(recent > 100_000 && recent < 300_000, String(recent));
  // the transfer stopped: the number decays toward 0 instead of freezing
  assert.ok(r.rate(4000)! < recent);
  assert.equal(r.rate(9000), 0);
  // a new transfer (a smaller total) starts again
  r.add(10_000, 0);
  assert.equal(r.rate(10_000), undefined);
  r.add(11_000, 500_000);
  assert.equal(r.rate(11_000), 500_000);
  r.reset();
  assert.equal(r.rate(11_000), undefined);
});

test('the speed and time-left texts', () => {
  assert.equal(fmtRate(3_100_000), '3.1 MB/s');
  assert.equal(fmtRate(12_400_000), '12 MB/s');
  assert.equal(fmtRate(640_000), '640 KB/s');
  assert.equal(fmtRate(800), '800 B/s');
  assert.equal(fmtEta(12), '12 s left');
  assert.equal(fmtEta(0.2), '1 s left');
  assert.equal(fmtEta(65), '1 min 5 s left');
  assert.equal(fmtEta(7300), 'about 2 h left');
  assert.equal(etaSeconds(6_000_000, 2_000_000), 3);
  assert.equal(etaSeconds(6_000_000, 0), undefined);
  assert.equal(etaSeconds(6_000_000, undefined), undefined);
  assert.equal(statsLine(42, true, 3_000_000, 12_000_000), '42% · 3.0 MB/s · 4 s left');
  assert.equal(statsLine(42, true, undefined, 12_000_000), '42%', 'speed not known yet');
  assert.equal(statsLine(100, false, 3_000_000, 0), '100%', 'a finished job shows no speed');
  assert.equal(statsLine(99, true, 3_000_000, 0), '99% · 3.0 MB/s', 'nothing left to send: no time left');
});

test('when the page reads by itself', () => {
  assert.equal(canAutoRefresh(true, ''), true);
  assert.equal(canAutoRefresh(false, ''), false, 'a hidden window does not read');
  for (const busy of ['Reading', 'Uploading', 'Installing', 'Deleting', 'Restarting'])
    assert.equal(canAutoRefresh(true, busy), false, `not during ${busy}`);
  // focus and visibilitychange fire together: one read
  assert.equal(focusReadDue(undefined, 5000), true);
  assert.equal(focusReadDue(5000, 5000 + FOCUS_GAP_MS - 1), false);
  assert.equal(focusReadDue(5000, 5000 + FOCUS_GAP_MS), true);
  const e = (name: string, size = 1, kind = 'file') => ({ name, size, kind });
  assert.equal(listingChanged([e('a'), e('b')], [e('b'), e('a')]), false, 'order does not matter');
  assert.equal(listingChanged([e('a')], [e('a'), e('b')]), true, 'a file appeared');
  assert.equal(listingChanged([e('a'), e('b')], [e('a')]), true, 'a file went');
  assert.equal(listingChanged([e('a', 1)], [e('a', 2)]), true, 'a file changed size');
  assert.equal(listingChanged([e('a', 0, 'folder')], [e('a', 0, 'file')]), true);
  assert.equal(listingChanged(undefined, []), true, 'nothing shown yet');
  assert.equal(listingChanged([], []), false);
});

test('"Waiting for ..." shows only after silence, only while a request waits, never during progress', () => {
  const a = createActivity();
  assert.equal(a.quietFor(1000), undefined, 'no request waits');
  assert.equal(waitShown(true, a.quietFor(1000)), false);
  a.touch(1000); // a job starts
  a.recvBegin(); // a request waits for its reply
  assert.equal(a.quietFor(1200), 200);
  assert.equal(waitShown(true, a.quietFor(1000 + WAIT_QUIET_MS - 1)), false, 'a quick answer shows nothing');
  assert.equal(waitShown(true, a.quietFor(1000 + WAIT_QUIET_MS)), true, 'no reply for a while');
  assert.equal(waitShown(false, a.quietFor(9000)), false, 'no job: no notice');
  // an upload that makes progress: replies keep coming, so the silence never reaches the limit
  for (let t = 1000; t < 20_000; t += 400) {
    a.touch(t);
    assert.equal(waitShown(true, a.quietFor(t + 399)), false, `progress at ${t}`);
  }
  // CPU work between requests (a WAV converts) is not waiting for the device
  a.recvEnd();
  assert.equal(a.quietFor(60_000), undefined);
  assert.equal(waitShown(true, a.quietFor(60_000)), false);
  // two requests in flight: waiting until both are over
  a.recvBegin();
  a.recvBegin();
  a.recvEnd();
  assert.notEqual(a.quietFor(60_000), undefined);
  a.recvEnd();
  a.recvEnd(); // an unmatched end does not make the count negative
  a.recvBegin();
  assert.notEqual(a.quietFor(61_000), undefined);
});

test('a transport reports its traffic: a message is activity, a recv in flight is a waiting request', async () => {
  const sent = [];
  let reply: Uint8Array | null = null;
  let release!: () => void;
  const t = {
    extra: 'kept',
    send: async (m: Uint8Array) => void sent.push(m),
    recv: (_timeoutMs?: number) => new Promise<Uint8Array | null>((res) => (release = () => res(reply))),
  };
  const same = instrument(t);
  assert.equal(same, t, 'the transport is changed in place');
  assert.equal(t.extra, 'kept');
  await t.send(Uint8Array.of(1));
  assert.equal(sent.length, 1);
  const pending = t.recv(1000);
  // the module's tracker is shared: a request is now in flight
  const { quietMs } = await import('../../../web/src/transfer/activity.ts');
  assert.notEqual(quietMs(), undefined, 'a recv in flight is a waiting request');
  reply = Uint8Array.of(2);
  release();
  assert.deepEqual(await pending, Uint8Array.of(2));
  assert.equal(quietMs(), undefined, 'nothing waits after the reply');
});

test('the bottom bar of a module job: the bytes sent so far and in all, the message, the fraction', () => {
  let p = startProgress(['a', 'b'], false, { a: 1000, b: 3000 });
  assert.deepEqual([modulesBar(p).sent, modulesBar(p).total, modulesBar(p).message], [0, 4000, 'Starting…']);
  p = applyEvent(p, { event: 'upload', id: 'a', path: 'modules/a.dkm', bytes: 1000 });
  p = applyEvent(p, { event: 'progress', id: 'a', sent: 400, bytes: 1000 });
  assert.deepEqual([modulesBar(p).sent, modulesBar(p).total], [400, 4000]);
  p = applyEvent(p, { event: 'verify', id: 'a', path: 'modules/a.dkm', ok: true });
  assert.equal(modulesBar(p).sent, 1000, 'a verified module counts in full');
  assert.equal(modulesBar(p).failed, false);
  p = applyEvent(p, { event: 'error', message: 'E_FULL' });
  assert.equal(modulesBar(p).failed, false, 'the failed one is b: not yet active, so nothing failed');
  // a restart has no bytes: the bar has a message and a fraction only
  const r = modulesBar(startProgress([], true));
  assert.deepEqual([r.sent, r.total], [0, 0]);
});

test('the bottom bar of a samples upload: only what this run touched, one message for running and one for the end', () => {
  const ev = (u: Uploads, e: UploadEventWithoutFolder) => applyUpload(u, { folder: 'd', ...e });
  let u = {};
  // an earlier run left a verified file
  u = ev(u, { event: 'prepared', name: 'old', source: 'old.wav', kind: 'converted', bytes: 500 });
  u = ev(u, { event: 'verified', name: 'old', ok: true, message: '' });
  const base = u;
  assert.equal(samplesBar(u, base, false), undefined, 'nothing touched: no bar');
  u = ev(u, { event: 'prepared', name: 'k', source: 'k.wav', kind: 'converted', bytes: 1000 });
  u = ev(u, { event: 'prepared', name: 's', source: 's.wav', kind: 'converted', bytes: 3000 });
  let bar = samplesBar(u, base, true)!;
  assert.deepEqual([bar.sent, bar.total, bar.fraction, bar.message], [0, 4000, 0, 'Preparing…']);
  u = ev(u, { event: 'progress', name: 'k', sent: 500, bytes: 1000 });
  bar = samplesBar(u, base, true)!;
  assert.deepEqual([bar.sent, bar.fraction, bar.message], [500, 0.125, 'Uploading k (1 of 2)']);
  u = ev(u, { event: 'verifying', name: 'k' });
  assert.equal(samplesBar(u, base, true)!.message, 'Verifying k (1 of 2)');
  u = ev(u, { event: 'verified', name: 'k', ok: true, message: '' });
  u = ev(u, { event: 'progress', name: 's', sent: 1500, bytes: 3000 });
  bar = samplesBar(u, base, true)!;
  assert.deepEqual([bar.sent, bar.fraction, bar.message], [2500, 0.625, 'Uploading s (2 of 2)']);
  u = ev(u, { event: 'verified', name: 's', ok: true, message: '' });
  bar = samplesBar(u, base, false)!;
  assert.deepEqual([bar.fraction, bar.failed, bar.message], [1, false, 'Uploaded 2 files, verified.']);
  // a failure: the bar turns red and the end message counts
  u = ev(u, { event: 'prepared', name: 'x', source: 'x.wav', kind: 'converted', bytes: 100 });
  u = ev(u, { event: 'failed', name: 'x', message: 'not a usable WAV' });
  bar = samplesBar(u, base, false)!;
  assert.deepEqual([bar.failed, bar.message], [true, '2 of 3 uploaded, 1 failed.']);
  assert.equal(samplesBar(u, base, false)!.sent, 4000, 'the bytes of a failed file are not counted as sent');
  // files queued that never ran (the run stopped): not "Uploaded 0 files, verified."
  const queued = ev(base, { event: 'queued', name: 'q', bytes: 100 });
  bar = samplesBar(queued, base, false)!;
  assert.deepEqual([bar.failed, bar.message], [true, 'Nothing was uploaded.']);
});

test('the speed of an upload: a 5 s window, nothing in the first second, a stall pulls it down, the text', () => {
  let now = 10_000;
  const t = speedTracker(() => now);
  assert.equal(t.rate(), undefined, 'zero time: nothing');
  now += 500;
  t.sample(500_000);
  assert.equal(t.rate(), undefined, 'under a second of data: nothing');
  now += 500;
  t.sample(1_000_000);
  assert.equal(t.rate(), 1_000_000, '1 MB in 1 s');
  for (let i = 0; i < 10; i++) {
    now += 1000;
    t.sample(1_000_000 + (i + 1) * 100_000);
  }
  assert.equal(t.rate(), 100_000, 'the window forgets the fast start');
  // no new bytes (VERIFY, a stall): the number falls with the time, then reaches 0
  now += 2500;
  assert.equal(Math.round(t.rate()!), 54_545, 'the anchor is the last point before the window: 300 KB in 5.5 s');
  now += 5000;
  assert.equal(t.rate(), 0);
  assert.equal(t.average(2_000_000), 2_000_000 / 18.5, 'average over the whole run');
  assert.equal(t.average(0), undefined);
  assert.deepEqual(
    [formatRate(412_000), formatRate(999_400), formatRate(1_000_000), formatRate(1_240_000), formatRate(0)],
    ['412 KB/s', '999 KB/s', '1.00 MB/s', '1.24 MB/s', '0 KB/s'],
  );
  // in the bar: the speed while running (with a file), the average at the end
  const ev = (u: Uploads, e: UploadEventWithoutFolder) => applyUpload(u, { folder: 'd', ...e });
  let u = ev({}, { event: 'prepared', name: 'k', source: 'k.wav', kind: 'converted', bytes: 1000 });
  u = ev(u, { event: 'progress', name: 'k', sent: 500, bytes: 1000 });
  assert.equal(samplesBar(u, {}, true, { rate: 412_000 })!.message, 'Uploading k (1 of 1) · 412 KB/s');
  assert.equal(samplesBar(u, {}, true, {})!.message, 'Uploading k (1 of 1)');
  u = ev(u, { event: 'verified', name: 'k', ok: true, message: '' });
  assert.equal(
    samplesBar(u, {}, false, { average: 1_240_000 })!.message,
    'Uploaded 1 file, verified. · 1.24 MB/s average',
  );
  u = ev(u, { event: 'prepared', name: 'x', source: 'x.wav', kind: 'converted', bytes: 100 });
  u = ev(u, { event: 'failed', name: 'x', message: 'bad' });
  assert.equal(samplesBar(u, {}, false, { average: 8000 })!.message, '1 of 2 uploaded, 1 failed. · 8 KB/s average');
  assert.equal(samplesBar(u, {}, false, {})!.message, '1 of 2 uploaded, 1 failed.');
});

// ---- the selection of a list, like a file explorer (select-core.ts) ----------------------------------------------------------------------

const KEYS = ['a', 'b', 'c', 'd', 'e', 'f'];
const NONE = { shift: false, mod: false };
const sorted = (s: { selected: Set<string> }) => [...s.selected].sort();

test('click: one row; Cmd or Ctrl toggles; Shift takes the range from the anchor; Shift and Cmd add the range', () => {
  let s = clickSelect(KEYS, new Set(), undefined, 'b', NONE);
  assert.deepEqual([sorted(s), s.anchor], [['b'], 'b']);
  s = clickSelect(KEYS, s.selected, s.anchor, 'd', { shift: true, mod: false });
  assert.deepEqual([sorted(s), s.anchor], [['b', 'c', 'd'], 'b'], 'the anchor stays for the next Shift');
  s = clickSelect(KEYS, s.selected, s.anchor, 'f', { shift: true, mod: false });
  assert.deepEqual(sorted(s), ['b', 'c', 'd', 'e', 'f'], 'the range replaces the selection');
  s = clickSelect(KEYS, s.selected, s.anchor, 'a', { shift: true, mod: false });
  assert.deepEqual(sorted(s), ['a', 'b'], 'a range upwards');
  // Cmd toggles one row and moves the anchor
  s = clickSelect(KEYS, new Set(['a']), 'a', 'c', { shift: false, mod: true });
  assert.deepEqual([sorted(s), s.anchor], [['a', 'c'], 'c']);
  s = clickSelect(KEYS, s.selected, s.anchor, 'a', { shift: false, mod: true });
  assert.deepEqual(sorted(s), ['c']);
  // Shift and Cmd: the range is added to what is selected
  s = clickSelect(KEYS, new Set(['a']), 'c', 'e', { shift: true, mod: true });
  assert.deepEqual(sorted(s), ['a', 'c', 'd', 'e']);
  // a plain click on a selected row among many selects only it (Finder does too)
  s = clickSelect(KEYS, new Set(['a', 'b', 'c']), 'a', 'b', NONE);
  assert.deepEqual(sorted(s), ['b']);
  // Shift without an anchor (or one that is gone): just the clicked row
  s = clickSelect(KEYS, new Set(), undefined, 'c', { shift: true, mod: false });
  assert.deepEqual([sorted(s), s.anchor], [['c'], 'c']);
  s = clickSelect(KEYS, new Set(['x']), 'x', 'c', { shift: true, mod: false });
  assert.deepEqual(sorted(s), ['c']);
  assert.deepEqual(modsOf({ shiftKey: true, metaKey: false, ctrlKey: true }), { shift: true, mod: true });
  assert.deepEqual(modsOf({ shiftKey: false, metaKey: true, ctrlKey: false }), { shift: false, mod: true });
});

test('the tick of a row toggles that row only; select everything; ticks of rows that are gone are dropped', () => {
  assert.deepEqual(sorted(tickSelect(new Set(['a']), 'b')), ['a', 'b']);
  assert.deepEqual(sorted(tickSelect(new Set(['a', 'b']), 'a')), ['b']);
  assert.deepEqual([...selectEverything(KEYS)], KEYS, 'folders and files alike: the keys are all the rows');
  assert.deepEqual([...keepVisible(['a', 'b'], new Set(['b', 'x']))], ['b']);
  assert.deepEqual(rangeKeys(KEYS, 'd', 'b'), ['b', 'c', 'd']);
  assert.deepEqual(rangeKeys(KEYS, undefined, 'b'), ['b']);
  assert.deepEqual(rangeKeys(KEYS, 'x', 'b'), ['b']);
  assert.deepEqual(rangeKeys(KEYS, 'a', 'x'), []);
});

test('the marquee: the rows the band touches; Shift or Cmd keeps the selection from before; no band is a click that clears', () => {
  const rows = KEYS.map((key, i) => ({ key, top: i * 26, bottom: i * 26 + 25 }));
  assert.deepEqual(marqueeKeys(rows, 30, 60), ['b', 'c'], 'a band that touches the edge of a row takes it');
  assert.deepEqual(marqueeKeys(rows, 60, 30), ['b', 'c'], 'either direction');
  assert.deepEqual(marqueeKeys(rows, 300, 400), [], 'below the last row');
  assert.deepEqual(marqueeKeys(rows, -10, 5), ['a'], 'above the first row');
  assert.deepEqual(marqueeKeys(rows, 0, 1000), KEYS);
  assert.deepEqual([...marqueeSelection(new Set(['a']), ['c', 'd'], false)], ['c', 'd']);
  assert.deepEqual([...marqueeSelection(new Set(['a']), ['c', 'd'], true)], ['a', 'c', 'd']);
  assert.ok(MARQUEE_SLOP > 0, 'a press that hardly moves is a click');
});

test('the marquee scrolls the pane near its top and bottom edge, faster the closer the pointer is, not in the middle', () => {
  assert.equal(edgeScroll(300, 100, 500), 0);
  assert.ok(edgeScroll(110, 100, 500) < 0, 'near the top: up');
  assert.ok(edgeScroll(490, 100, 500) > 0, 'near the bottom: down');
  assert.ok(edgeScroll(100, 100, 500) < edgeScroll(120, 100, 500), 'closer to the edge: faster up');
  assert.ok(edgeScroll(500, 100, 500) > edgeScroll(480, 100, 500));
  assert.equal(edgeScroll(50, 100, 500), -24, 'beyond the edge: the top speed');
  assert.equal(edgeScroll(900, 100, 500), 24);
});

test('keys: Cmd or Ctrl+A all, Esc clear, Cmd/Ctrl+Up and Backspace up, Enter and Cmd/Ctrl+Down open, the arrows move', () => {
  const k = (key: string, o: Partial<Parameters<typeof keyCommand>[0]> = {}) =>
    keyCommand({ key, metaKey: false, ctrlKey: false, shiftKey: false, ...o });
  assert.deepEqual(k('a', { metaKey: true }), { type: 'all' });
  assert.deepEqual(k('a', { ctrlKey: true }), { type: 'all' });
  assert.equal(k('a'), undefined, 'a plain A is not a command');
  assert.deepEqual(k('Escape'), { type: 'clear' });
  assert.deepEqual(k('Backspace'), { type: 'up' });
  assert.deepEqual(k('ArrowUp', { metaKey: true }), { type: 'up' });
  assert.deepEqual(k('ArrowUp', { ctrlKey: true }), { type: 'up' });
  assert.deepEqual(k('Enter'), { type: 'open' });
  assert.deepEqual(k('ArrowDown', { metaKey: true }), { type: 'open' });
  assert.deepEqual(k('ArrowDown'), { type: 'move', delta: 1, extend: false });
  assert.deepEqual(k('ArrowUp', { shiftKey: true }), { type: 'move', delta: -1, extend: true });
  assert.equal(k('x'), undefined);
});

test('arrows move the selection by one row; Shift extends from the anchor; the ends hold', () => {
  const m = (anchor: string | undefined, cursor: string | undefined, delta: 1 | -1, extend: boolean) =>
    moveSelect(KEYS, anchor, cursor, delta, extend)!;
  let s = m(undefined, undefined, 1, false);
  assert.deepEqual([sorted(s), s.cursor], [['a'], 'a'], 'nothing selected: down starts at the top');
  s = m(undefined, undefined, -1, false);
  assert.deepEqual([sorted(s), s.cursor], [['f'], 'f'], 'up starts at the bottom');
  s = m('b', 'b', 1, false);
  assert.deepEqual([sorted(s), s.anchor, s.cursor], [['c'], 'c', 'c']);
  s = m('b', 'b', 1, true);
  s = m(s.anchor, s.cursor, 1, true);
  assert.deepEqual([sorted(s), s.anchor, s.cursor], [['b', 'c', 'd'], 'b', 'd']);
  s = m(s.anchor, s.cursor, -1, true);
  assert.deepEqual(sorted(s), ['b', 'c'], 'Shift back shrinks the range');
  assert.deepEqual(sorted(m('f', 'f', 1, false)), ['f'], 'the bottom holds');
  assert.deepEqual(sorted(m('a', 'a', -1, false)), ['a'], 'the top holds');
  assert.equal(moveSelect([], undefined, undefined, 1, false), undefined);
});

test('a drag carries the whole selection when it starts on a selected row, else just that row (which becomes the selection)', () => {
  const sel = new Set(['d', 'b']);
  assert.deepEqual(dragKeys(KEYS, sel, 'd'), { keys: ['b', 'd'], selected: new Set(['d', 'b']) }, 'list order');
  assert.deepEqual(dragKeys(KEYS, sel, 'e'), { keys: ['e'], selected: new Set(['e']) });
  assert.deepEqual(dragKeys(KEYS, new Set(), 'a'), { keys: ['a'], selected: new Set(['a']) });
});

test('drop targets: the folder row the drop is on, else the open folder; rows of our list or files from outside, nothing else', () => {
  assert.equal(dropFolder('drums/808', 'drums'), 'drums/808');
  assert.equal(dropFolder('', 'drums'), '', 'the Up row of a folder one level down is the top');
  assert.equal(dropFolder(undefined, 'drums'), 'drums');
  assert.equal(dragKind([ROWS_TYPE, 'text/plain']), 'rows');
  assert.equal(dragKind(['Files']), 'files');
  assert.equal(dragKind(['text/plain']), 'none');
  assert.equal(dragKind(undefined), 'none');
  assert.equal(dragKind(['Files', ROWS_TYPE]), 'rows', 'our own drag wins');
});

test('the busy action outlives the page: a page that comes back during an upload sees it, and its end clears it once', () => {
  assert.equal(beginJob('Uploading'), true);
  // a second page (a new mount) reads busy from the shared state: the bar stays running, the buttons stay off, no job starts
  assert.equal(busy(), 'Uploading');
  let u = applyUpload(
    {},
    { folder: 'd', event: 'prepared', name: 'k', source: 'k.wav', kind: 'converted', bytes: 1000 },
  );
  u = applyUpload(u, { folder: 'd', event: 'progress', name: 'k', sent: 500, bytes: 1000 });
  assert.equal(
    samplesBar(u, {}, busy() === 'Uploading')!.message,
    'Uploading k (1 of 1)',
    'still running, not "1 of 1 uploaded."',
  );
  assert.equal(beginJob('Uploading'), false, 'no second upload');
  assert.equal(beginJob('Deleting'), false);
  assert.equal(busy(), 'Uploading', 'a refused job does not change the running one');
  // a failure that arrives after the new page was made is on the error line it shows
  setError('Cancelled.');
  assert.equal(error(), 'Cancelled.');
  // a read asked for during the job is remembered and runs once at its end
  assert.equal(beginJob('Reading'), false);
  assert.equal(endJob(), true);
  assert.equal(busy(), '');
  assert.equal(beginJob('Reading'), true);
  assert.equal(endJob(), false, 'the read is asked for once');
  assert.equal(busy(), '');
  setError('');
});
