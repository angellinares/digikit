// The words for the runtime's log events (src/runtime-log.ts); the cases are those of tests/digikit/test_dklog.py, so the Log drawer and dklog agree.
import { expect, test } from 'vitest';
import type { DkLogEvent } from '../src/dkupload-core.ts';
import { LOG_KIND } from '../src/gen_proto.ts';
import { decodeFault } from '../src/fault.ts';
import { asciiOf, eventLine, eventText, faultDetail, moduleNames, runtimeEvent } from '../src/runtime-log.ts';

const word = (s: string) => {
  let n = 0;
  for (let i = 0; i < 4; i++) n = n * 256 + (s.charCodeAt(i) || 0);
  return n;
};
const ev = (seq: number, kind: keyof typeof LOG_KIND, args: number[] = [0, 0, 0, 0, 0], flags = 0): DkLogEvent => ({
  seq,
  time: seq * 150,
  kind: LOG_KIND[kind],
  flags,
  args: [...args, 0, 0, 0, 0, 0].slice(0, 5),
});
const lv = (level: number, src = 0, error = false) => (level << 1) | (src << 8) | (error ? 1 : 0);

test('the guard events decode', () => {
  const d = runtimeEvent(ev(1, 'MACHINE_FAULT', [18 | (2 << 16), 1 | (9 << 8), 0x10004000, 0x00002000, 0x01007f00], 1));
  expect(d.error).toBe(true);
  expect(d.fields).toMatchObject({ type: 18, voice: 2, track: 2, kind: 'nonfinite', faults: 9 });
  expect(d.fields.params).toEqual(['4000', '1000', '2000', '0000', '7f00', '0100']);
  expect(runtimeEvent(ev(2, 'MACHINE_FAULT', [18, 2 | (3 << 8), 0, 0, 0])).fields.kind).toBe('overlevel');
  expect(runtimeEvent(ev(3, 'MACHINE_DISABLED', [18, 1, 17], 1)).fields).toEqual({ type: 18, disabled: 1, faults: 17 });
});

test('events decode to readable fields', () => {
  const d = runtimeEvent(ev(7, 'CARD_CHECK', [3, 100, 0x0f498b0e], 1));
  expect(d).toMatchObject({ error: true, kind: 'CARD_CHECK', t: 0.7 });
  expect(d.fields).toMatchObject({ state: 'foreign', first_nonblank: 100, hash: '0f498b0e' });
  expect(runtimeEvent(ev(1, 'CARD_CHECK', [2, 0xffffffff])).fields.first_nonblank).toBeNull();
  const load = runtimeEvent(ev(2, 'MOD_LOAD', [0, 0xffffffd3, word('dyn-'), word('plu'), 0])).fields;
  expect(load).toMatchObject({ name: 'dyn-plu', result: -45 });
  expect(runtimeEvent(ev(3, 'UPLOAD', [0x12, 0xffffffc7, 5])).fields).toMatchObject({ status: -57, cmd: '0x12' });
  const health = runtimeEvent(ev(4, 'HEALTH', [((2 | 4) << 16) | 371, 120, 450, 100000, 0xffffffff])).fields;
  expect(health).toMatchObject({ cpu_pct: 37.1, dsp_max_pct: 45, heap_free_kb: null });
  const log = runtimeEvent(ev(5, 'MOD_LOG', [word('cras'), word('h-pc'), 0, 0, 0x47e8b610]));
  expect(log.fields).toEqual({ msg: 'crash-pc', value: 0x47e8b610 });
  // a kind that a newer runtime has
  expect(runtimeEvent({ seq: 9, time: 1, kind: 999, flags: 0, args: [1, 2, 3, 4, 5] }).kind).toBe('KIND999');
  expect(asciiOf(word('ab\x01c'))).toBe('ab?c');
});

test('level, source and the error flag are decoded', () => {
  expect(runtimeEvent(ev(1, 'MOD_LOG', [], lv(4, 3)))).toMatchObject({ level: 'trace', source: 3, error: false });
  expect(runtimeEvent(ev(2, 'SAFE', [], lv(0, 0, true)))).toMatchObject({ level: 'error', source: 0, error: true });
  expect(runtimeEvent(ev(3, 'MENU', [], 7 << 1)).level).toBe('7');
});

test('a line names the kind, the module and the arguments', () => {
  const safe = runtimeEvent(ev(10, 'SAFE', [1, 2, 3], 1));
  expect(eventLine(safe)).toContain('!SAFE');
  expect(eventLine(safe)).toContain('reason=counter');
  expect(eventText(safe, new Map())).toBe('SAFE  reason=counter  boot_counter=2  record_seq=3');
  const mod = runtimeEvent(ev(5, 'MOD_LOG', [word('shif'), word('t in'), word('it'), 0, 300], lv(2, 2)));
  expect(eventLine(mod)).toContain('mod#1');
  expect(eventLine(mod, new Map([[2, 'c-demo']]))).toContain('c-demo');
  expect(eventText(mod, new Map([[2, 'freqshift']]))).toBe('freqshift  "shift init"  value=0x0000012c');
});

test('module names come from MOD_LIST events: record index + 1', () => {
  const names = moduleNames([ev(1, 'MOD_LIST', [1, 0, word('freq'), word('shif'), word('t')]), ev(2, 'MENU')]);
  expect([...names]).toEqual([[2, 'freqshift']]);
});

test('the detail of a CPU crash holds the vector, the registers, the stack and the events before it', () => {
  const crash = decodeFault(
    (() => {
      const v = new Uint8Array(40);
      const view = new DataView(v.buffer);
      view.setUint32(0, 0x464c5431);
      view.setUint32(4, 1);
      view.setUint32(16, 0x12a4f0);
      v.set(Buffer.from('test/crash'), 24);
      return v;
    })(),
  )!;
  const events = [
    ev(1, 'BOOT_START'),
    ev(2, 'CRASH_FOUND', [0x12a4f0, 4 | (2 << 8), 0x2700, 0x47e8b000, 3000], 1),
    ev(3, 'CRASH_MOD', [0, 3, word('test'), word('/cra'), word('sh')], 1),
    ev(4, 'CRASH_REG', [0, 1, 2, 3, 4]),
    ev(5, 'CRASH_STACK', [0, 0xdead, 0xbeef, 0, 0]),
    // one event before the crash: seq 90, time 4500, MENU (flags 0), args 1 0 0 0 0
    ev(6, 'CRASH_TAIL', [90, 4500, (LOG_KIND.MENU << 16) | 0, 1, 0]),
    ev(7, 'CRASH_TAIL', [90, 0, 0, 0, 1]),
  ];
  const text = faultDetail(crash, events);
  expect(text.split('\n').slice(0, 3)).toEqual(['FAULT: CPU CRASH', 'IN CRASH', 'PC 0012A4F0']);
  expect(text).toContain('kept on the card as dk/fault (survives power cycles)');
  expect(text).toContain('vector 4 (illegal instruction), format 2, sr 2700');
  expect(text).toContain("in module 'test/crash' (index 0)");
  expect(text).toContain('d0 00000001  d1 00000002  d2 00000003  d3 00000004');
  expect(text).toContain('+000  0000dead 0000beef');
  expect(text).toContain('events before the crash (oldest first):');
  expect(text).toMatch(/ 90 +3\.000s .*MENU/);
  // without the events of the boot only the record is shown
  expect(faultDetail(crash, events.slice(0, 1))).not.toContain('vector');
});

test('a fault detail names the crashed module from CRASH_MOD when no module list is known, and keeps a known name', () => {
  const stop = decodeFault(
    (() => {
      const v = new Uint8Array(40);
      const view = new DataView(v.buffer);
      view.setUint32(0, 0x464c5431);
      view.setUint32(4, 2 | (3 << 8) | (4 << 16));
      v.set(Buffer.from('test/dyn-fx-hang'), 24);
      return v;
    })(),
  )!;
  const events = [
    ev(1, 'FAULT_FOUND', [2 | (3 << 8) | (4 << 16), 96, 3, 0x600304, 0], 1),
    ev(2, 'CRASH_MOD', [0, 0, word('dyn-'), word('fx-h'), word('ang')], 1),
    // one event before the stop, written by the module with record index 0 (source 1)
    ev(3, 'CRASH_TAIL', [90, 4500, (LOG_KIND.MOD_LOG << 16) | lv(2, 1), word('tick'), 0]),
    ev(4, 'CRASH_TAIL', [90, 0, 0, 0, 1]),
  ];
  expect(faultDetail(stop, events)).toMatch(/ 90 +3\.000s info +dyn-fx-hang +MOD_LOG/);
  expect(faultDetail(stop, events, new Map([[1, 'listed']]))).toMatch(/ 90 +3\.000s info +listed +MOD_LOG/);
});

test('safe reason 6 reads as a selection read failure', () => {
  const safe = runtimeEvent(ev(1, 'SAFE', [6, 0, 10], 1));
  expect(safe.fields.reason).toBe('read');
  expect(eventText(safe, new Map())).toBe('SAFE  reason=read  boot_counter=0  record_seq=10');
});
