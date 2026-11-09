// The pure logic of the workbench Log drawer (apps/desktop/ui/src/log-core.ts).
import assert from 'node:assert/strict';
import { test } from 'vitest';
import {
  LOG_CAP,
  RUNTIME_CAP,
  addEntries,
  addEntry,
  autoOpens,
  clearEntries,
  copyText,
  emptyLog,
  flashEntry,
  filterEntries,
  formatTime,
  healthChanges,
  healthStart,
  reloadEntry,
  reloadFailureEntry,
  saveName,
  saveText,
  sourceMenu,
  transferEntry,
  unseenErrors,
  type LogInput,
  type LogState,
  overlayResetEntry,
} from '../src/log-core.ts';

const at = (h: number, m: number, s: number) => new Date(2026, 9, 28, h, m, s).getTime();
const info = (text: string, source: LogInput['source'] = 'reload'): LogInput => ({ source, level: 'info', text });
const bad = (text: string, source: LogInput['source'] = 'reload'): LogInput => ({ source, level: 'error', text });
const fill = (inputs: LogInput[]): LogState => inputs.reduce((s, i, n) => addEntry(s, i, at(14, 0, n)), emptyLog());

test('the log keeps the newest 500 entries', () => {
  let s = emptyLog();
  for (let n = 0; n < LOG_CAP + 25; n++) s = addEntry(s, info(`e${n}`), 0);
  assert.equal(s.entries.length, LOG_CAP);
  assert.equal(s.entries[0].text, 'e25');
  assert.equal(s.entries.at(-1)!.text, `e${LOG_CAP + 24}`);
});

const rtEntry = (
  origin: 'device' | 'emulator',
  module: string,
  text: string,
  level: 'info' | 'error' = 'info',
): LogInput => ({
  source: origin,
  level,
  text,
  rt: { origin, module },
});

test('each runtime log has its own cap of 2000, and the app keeps its 500', () => {
  let s = emptyLog();
  s = addEntries(
    s,
    Array.from({ length: RUNTIME_CAP + 30 }, (_, n) => rtEntry('device', '', `d${n}`)),
    0,
  );
  s = addEntries(
    s,
    Array.from({ length: 40 }, (_, n) => rtEntry('emulator', '', `e${n}`)),
    0,
  );
  s = addEntries(
    s,
    Array.from({ length: LOG_CAP + 10 }, (_, n) => info(`a${n}`)),
    0,
  );
  const count = (f: (e: (typeof s.entries)[number]) => boolean) => s.entries.filter(f).length;
  assert.equal(
    count((e) => e.source === 'device'),
    RUNTIME_CAP,
  );
  assert.equal(
    count((e) => e.source === 'emulator'),
    40,
  );
  assert.equal(
    count((e) => e.source === 'reload'),
    LOG_CAP,
  );
  assert.equal(s.entries.find((e) => e.source === 'device')!.text, 'd30');
  assert.equal(s.entries.find((e) => e.source === 'reload')!.text, 'a10');
});

test('ids keep growing after a clear', () => {
  const s = clearEntries(fill([info('a'), info('b')]));
  assert.equal(s.entries.length, 0);
  assert.equal(addEntry(s, info('c'), 0).entries[0].id, 3);
});

test('the filter takes one source and errors only', () => {
  const s = fill([info('a'), bad('b', 'build'), info('c', 'card'), bad('d', 'card')]);
  const text = (f: Parameters<typeof filterEntries>[1]) => filterEntries(s.entries, f).map((e) => e.text);
  assert.deepEqual(text({ source: 'all', errorsOnly: false }), ['a', 'b', 'c', 'd']);
  assert.deepEqual(text({ source: 'card', errorsOnly: false }), ['c', 'd']);
  assert.deepEqual(text({ source: 'all', errorsOnly: true }), ['b', 'd']);
  assert.deepEqual(text({ source: 'card', errorsOnly: true }), ['d']);
});

test('the Source menu picks an app source, an origin, its runtime or one of its modules', () => {
  const s = fill([
    info('a', 'reload'),
    info('health', 'emulator'),
    rtEntry('emulator', '', 'boot'),
    rtEntry('emulator', 'freqshift', 'emu mod'),
    rtEntry('device', '', 'dev boot'),
    rtEntry('device', 'freqshift', 'dev mod'),
    rtEntry('device', 'other', 'dev other', 'error'),
  ]);
  const text = (source: string, errorsOnly = false) =>
    filterEntries(s.entries, { source, errorsOnly }).map((e) => e.text);
  assert.deepEqual(text('reload'), ['a']);
  assert.deepEqual(text('emulator'), ['health']); // the app's entries about the emulator
  assert.deepEqual(text('emulator:all'), ['health', 'boot', 'emu mod']);
  assert.deepEqual(text('emulator:runtime'), ['boot']);
  assert.deepEqual(text('device:all'), ['dev boot', 'dev mod', 'dev other']);
  assert.deepEqual(text('device:runtime'), ['dev boot']);
  assert.deepEqual(text('device:mod:freqshift'), ['dev mod']);
  assert.deepEqual(text('device:all', true), ['dev other']);
});

test('the menu groups App, Emulator and the device; a group with nothing to follow has one disabled option', () => {
  const none = { live: false, logged: false, modules: [] };
  const [app, emu, dev] = sourceMenu({ emulator: none, device: { ...none } });
  assert.deepEqual(
    app!.options.map((o) => o.label),
    ['reload', 'build', 'firmware', 'card', 'transfer', 'reset'],
  );
  assert.deepEqual(emu!.options, [{ value: '', label: 'no runtime running', disabled: true }]);
  assert.deepEqual([dev!.label, dev!.options.map((o) => o.label)], ['Device', ['no MIDI port']]);
  const [, emu2, dev2] = sourceMenu({
    emulator: { live: true, logged: true, modules: ['freqshift'] },
    device: { live: true, logged: true, modules: ['a', 'b'], port: 'Elektron Digitakt II' },
  });
  assert.deepEqual(
    emu2!.options.map((o) => o.value),
    ['emulator:all', 'emulator', 'emulator:runtime', 'emulator:mod:freqshift'],
  );
  assert.equal(dev2!.label, 'Device: MIDI Elektron Digitakt II');
  assert.deepEqual(
    dev2!.options.map((o) => o.value),
    ['device:all', 'device:runtime', 'device:mod:a', 'device:mod:b'],
  );
});

test('the badge counts errors after the last one seen', () => {
  const s = fill([bad('a'), info('b'), bad('c'), bad('d')]);
  assert.equal(unseenErrors(s.entries, 0), 3);
  assert.equal(unseenErrors(s.entries, 3), 1);
  assert.equal(unseenErrors(s.entries, 4), 0);
});

test('the drawer opens for a failed reload, a failed build and an app error about the emulator', () => {
  assert.equal(autoOpens(bad('x', 'reload')), true);
  assert.equal(autoOpens(bad('x', 'build')), true);
  assert.equal(autoOpens(bad('fault: x', 'emulator')), true);
  assert.equal(autoOpens(info('x', 'reload')), false);
  assert.equal(autoOpens(bad('x', 'transfer')), false);
  assert.equal(autoOpens(bad('x', 'card')), false);
  assert.equal(autoOpens(bad('x', 'firmware')), false);
});

test('of a runtime log only the fault entry opens the drawer, not an error row', () => {
  for (const origin of ['device', 'emulator'] as const) {
    const rt = { origin, module: '' };
    const event = { kind: 'UPLOAD' } as unknown as NonNullable<LogInput['rt']>['event'];
    const row: LogInput = { source: origin, level: 'error', text: '!UPLOAD cmd=0x3e status=-60', rt: { ...rt, event } };
    const fault: LogInput = { source: origin, level: 'error', text: 'fault: DSP STOP in X', rt };
    assert.equal(autoOpens(row), false);
    assert.equal(autoOpens({ ...row, text: 'fault: looks like one', rt: { ...rt, event } }), false);
    assert.equal(autoOpens(fault), true);
    assert.equal(autoOpens({ ...fault, level: 'info' }), false);
  }
});

test('copy text has the time, the source, and the indented detail', () => {
  const s = addEntry(
    addEntry(emptyLog(), info('built a (248 ms)'), at(14, 2, 11)),
    { ...bad('build of a failed', 'build'), detail: 'line 1\nline 2\n' },
    at(14, 2, 40),
  );
  assert.equal(formatTime(at(9, 5, 3)), '09:05:03');
  assert.equal(
    copyText(s.entries),
    [
      '14:02:11  reload    built a (248 ms)',
      '14:02:40  build ✕   build of a failed',
      '          line 1',
      '          line 2',
    ].join('\n'),
  );
});

test('a reload that worked names the build and the swap', () => {
  const e = reloadEntry({
    path: 'swap',
    ms: 2300,
    ranges: 1,
    bytes: 1412,
    owners: ['test/ui-demo'],
    build: { built: [{ id: 'test/ui-demo', objects: [], ms: 248 }], skipped: [], ms: 248 },
  });
  assert.equal(e.text, 'built test/ui-demo (248 ms) · swapped 1412 B in test/ui-demo (2.3 s)');
  assert.equal(
    reloadEntry({ path: 'boot', seconds: 5.3, reason: 'core/runtime changed' }).text,
    'booted the new image, core/runtime changed (5.3 s)',
  );
});

test('a build error is a build entry with the compiler output as detail', () => {
  const e = reloadFailureEntry({
    status: 'Reload failed: build of test/ui-demo failed: ui_demo.c:117: error: invalid',
    full: 'Reload failed: build of test/ui-demo failed:\nui_demo.c:117: error: invalid',
  });
  assert.equal(e.source, 'build');
  assert.equal(e.level, 'error');
  assert.equal(e.text, 'build of test/ui-demo failed: ui_demo.c:117: error: invalid');
  assert.match(e.detail ?? '', /^build of test\/ui-demo failed:\nui_demo/);
  const other = reloadFailureEntry({ status: 'Reload failed: the running image has no patch record', full: '' });
  assert.deepEqual(other, { source: 'reload', level: 'error', text: 'failed: the running image has no patch record' });
});

test('a transfer run counts the files sent and skipped', () => {
  const lines = [
    'Installing a, b on dn2',
    'Uploading modules/a.dkm (2048 bytes)',
    'b: already on the card, not uploaded',
  ];
  const ok = transferEntry({ what: 'Installing', target: 'the emulator', message: 'On the card.', lines });
  assert.equal(ok.text, 'installed on the emulator · sent 1, skipped 1 · On the card.');
  assert.equal(ok.detail, lines.join('\n'));
  const failed = transferEntry({ what: 'Uploading', target: 'the emulator', error: 'no reply\nmore', lines: [] });
  assert.deepEqual([failed.level, failed.text], ['error', 'uploading failed: no reply']);
  // a sample upload run: counts come from the file lines, "verified" only when every file is
  const sent = transferEntry({
    what: 'Uploading',
    target: 'Emulator',
    lines: ['a: verified', 'b: verified', 'c: verified'],
  });
  assert.deepEqual([sent.level, sent.text], ['info', 'uploaded 3 files on Emulator, verified']);
  const none = transferEntry({
    what: 'Uploading',
    target: 'Emulator',
    lines: ['a: not sent: the folder x could not be made'],
  });
  assert.deepEqual([none.level, none.text], ['error', 'uploaded 0 files on Emulator']);
  // the error of a run that words it as "Uploading failed: ..." is not prefixed twice
  const own = transferEntry({
    what: 'Uploading',
    target: 'Emulator',
    error: 'Uploading failed: open /tst/a: the device answered status 0 (1 of 3 sent, 2 not sent)',
    lines: ['a: failed: open /tst/a: the device answered status 0'],
  });
  assert.equal(own.text, 'uploading failed: open /tst/a: the device answered status 0 (1 of 3 sent, 2 not sent)');
  // a cancelled run is an info entry with its counts and the line of each file
  const stopped = transferEntry({
    what: 'Uploading',
    target: 'Emulator',
    cancelled: true,
    counts: '1 of 3 uploaded, 2 not sent',
    lines: ['a: verified', 'b: cancelled while sending', 'c: not sent: cancelled'],
  });
  assert.deepEqual([stopped.level, stopped.text], ['info', 'uploading cancelled: 1 of 3 uploaded, 2 not sent']);
  assert.match(stopped.detail ?? '', /b: cancelled while sending/);
});

test('a fault, a stall and a DSP halt are logged once, when they start', () => {
  let h = healthStart();
  const step = (r: Parameters<typeof healthChanges>[1]) => {
    const out = healthChanges(h, r);
    h = out.health;
    return out.entries.map((e) => `${e.level} ${e.text}`);
  };
  assert.deepEqual(step({ error: null }), []);
  assert.deepEqual(step({ error: 'GUEST_FAULT at 0x4011' }), ['error fault: GUEST_FAULT at 0x4011']);
  assert.deepEqual(step({ error: 'GUEST_FAULT at 0x4011' }), []);
  assert.deepEqual(step({ stalled: true, stallMs: 1200 }), ['error stall: the main loop stopped for more than 1.2 s']);
  assert.deepEqual(step({ stalled: true }), []);
  assert.deepEqual(step({ stalled: false }), ['info stall ended']);
  assert.deepEqual(step({ halted: 'bad opcode' }), ['error DSP halt: bad opcode']);
  assert.deepEqual(step({ halted: 'bad opcode', error: 'GUEST_FAULT at 0x4011' }), []);
  // a reading that leaves a field out does not clear it
  assert.equal(h.error, 'GUEST_FAULT at 0x4011');
});

test('a flash of the Firmware page is logged as the firmware source, an error as an error', () => {
  assert.deepEqual(flashEntry({ level: 'info', text: 'flash dn2 OS 1.12 to Elektron Digitone II: 19290 messages' }), {
    source: 'firmware',
    level: 'info',
    text: 'flash dn2 OS 1.12 to Elektron Digitone II: 19290 messages',
  });
  const done = flashEntry({
    level: 'info',
    text: 'flash done: Restarted',
    detail: 'Runtime 0.5.2\nImage id c5d8ccb27e139cbc',
  });
  assert.equal(done.detail, 'Runtime 0.5.2\nImage id c5d8ccb27e139cbc');
  const failed = flashEntry({ level: 'error', text: 'flash failed: the image id differs' });
  assert.equal(failed.level, 'error');
  assert.equal(autoOpens(failed), false); // the Firmware page shows its own error; the Log does not open by itself
});

test('Save writes a # line for each entry and the event as JSON after the lines of runtime events', () => {
  const event = {
    seq: 7,
    blocks: 1050,
    t: 0.7,
    kind: 'CARD_CHECK',
    kind_id: 2,
    error: true,
    level: 'error',
    source: 0,
    args: [3, 100, 0, 0, 0],
    fields: { state: 'foreign' },
  };
  const s = fill([
    info('built a (248 ms)'),
    {
      ...rtEntry('device', '', 'CARD_CHECK  state=foreign', 'error'),
      rt: { origin: 'device', module: '', event, line: '     7      0.700s error  !CARD_CHECK    state=foreign' },
    },
    { ...rtEntry('device', '', 'fault: CPU PANIC in RUNTIME', 'error'), detail: 'FAULT: CPU PANIC\nIN RUNTIME' },
  ]);
  const lines = saveText(s.entries, at(14, 30, 0)).split('\n');
  assert.equal(lines[0], '# workbench log, saved 2026-10-28T14:30:00');
  assert.equal(lines[1], '# 14:00:00  reload  built a (248 ms)');
  assert.equal(lines[2], '#      7      0.700s error  !CARD_CHECK    state=foreign');
  const json = JSON.parse(lines[3]!);
  assert.equal(json.kind, 'CARD_CHECK');
  assert.match(json.host_time, /^2026-10-28T14:00:01\.\d{3}$/);
  assert.deepEqual(lines.slice(4, 7), [
    '# 14:00:02  device ✕  fault: CPU PANIC in RUNTIME',
    '#   FAULT: CPU PANIC',
    '#   IN RUNTIME',
  ]);
  // the JSON lines are what is left without the comments
  assert.equal(lines.filter((l) => l && !l.startsWith('#')).length, 1);
  assert.equal(saveName(at(9, 5, 0)), 'device-log-2026-10-28-0905.log');
});

test('the log says what the card reset set aside and where, or that there was nothing', () => {
  const moved = overlayResetEntry({ overlay: '/c/overlay.bin', set_aside: '/c/overlay.bin.bad-7' });
  assert.equal(moved.source, 'card');
  assert.match(moved.text, /\/c\/overlay\.bin as \/c\/overlay\.bin\.bad-7/);
  assert.match(overlayResetEntry({ overlay: '/c/overlay.bin', set_aside: null }).text, /nothing to reset/);
});
