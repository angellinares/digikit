import { expect, test, vi } from 'vitest';
import { CMD, STATUS } from '../src/gen_proto.ts';
import type { DkSession } from '../src/dkupload-core.ts';
import {
  decodeFault,
  faultRows,
  faultSummary,
  readSettingsRecord,
  readSettingsRecordIfPresent,
  settingsValue,
} from '../src/fault.ts';

function record(entries: [string, Uint8Array][]): Uint8Array {
  const body = entries.flatMap(([key, value]) => [
    key.length,
    ...Buffer.from(key),
    value.length >> 8,
    value.length & 255,
    ...value,
  ]);
  const out = new Uint8Array(18 + body.length);
  out.set(Buffer.from('DKST'));
  new DataView(out.buffer).setUint32(10, out.length);
  out.set(body, 18);
  return out;
}

function faultValue(kind: number, site: number, stage: number, id: number, step: number, at: number, name: string) {
  const value = new Uint8Array(40);
  const view = new DataView(value.buffer);
  view.setUint32(0, 0x464c5431);
  view.setUint32(4, kind | (site << 8) | (stage << 16));
  view.setUint32(8, id);
  view.setUint32(12, step);
  view.setUint32(16, at);
  value.set(Buffer.from(name), 24);
  return value;
}

test('a value is found by its key among the entries of a settings record', () => {
  const rec = record([
    ['fx/route/test/gain', Uint8Array.of(1)],
    ['dk/fault', Uint8Array.of(1, 2, 3)],
  ]);
  expect([...settingsValue(rec, 'dk/fault')!]).toEqual([1, 2, 3]);
  expect(settingsValue(rec, 'dk/other')).toBeNull();
  expect(settingsValue(rec.subarray(0, 20), 'dk/fault')).toBeNull();
});

test('dk/fault decodes to the site, the steps and the module', () => {
  const f = decodeFault(faultValue(2, 3, 4, 96, 3, 0x600304, 'test/dyn-fx-hang'))!;
  expect(f).toMatchObject({ kind: 'dsp', site: 3, stage: 4, id: 96, step: 3, module: 'test/dyn-fx-hang' });
  expect(decodeFault(faultValue(1, 0, 0, 0, 0, 0x12a4f0, 'x'))).toMatchObject({ kind: 'cpu', at: 0x12a4f0 });
  expect(decodeFault(faultValue(3, 0, 0, 0, 0, 0, 'runtime'))).toMatchObject({ kind: 'panic', module: 'runtime' });
  expect(decodeFault(new Uint8Array(40))).toBeNull();
  expect(decodeFault(faultValue(7, 0, 0, 0, 0, 0, 'x'))).toBeNull();
});

const rowsOf = (kind: number, site: number, stage: number, step: number, at: number, name: string) =>
  faultRows(decodeFault(faultValue(kind, site, stage, 96, step, at, name))!);

// the same cases as tests/digikit/test_dklog.py, so the Log drawer and dklog show the rows of the device's menu
test('the rows are those of the runtime menu', () => {
  expect(rowsOf(2, 3, 4, 3, 0x30004, 'test/dyn-fx-hang')).toEqual([
    'FAULT: DSP STOP',
    'IN DYN-FX-HANG',
    'FX SEND STEP 3',
  ]);
  expect(rowsOf(2, 2, 4, 0, 0, 'fx/gain')[2]).toBe('FX MASTER CALL');
  expect(rowsOf(2, 0, 4, 0, 0, 'x')[2]).toBe('SITE UNKNOWN');
  expect(rowsOf(1, 0, 0, 0, 0x12a4f0, 'test/crash')).toEqual(['FAULT: CPU CRASH', 'IN CRASH', 'PC 0012A4F0']);
  expect(rowsOf(1, 0, 0, 0, 0, '')[1]).toBe('IN FIRMWARE');
  expect(rowsOf(3, 0, 0, 0, 0, 'runtime')).toEqual(['FAULT: CPU PANIC', 'IN RUNTIME']);
  expect(rowsOf(2, 1, 4, 0, 0, 'abcdefghijklmnop')[1]).toBe('IN ABCDEFGHIJKLMNO');
});

test('the summary joins the rows for one line', () => {
  const f = (...a: Parameters<typeof rowsOf>) =>
    faultSummary(decodeFault(faultValue(a[0], a[1], a[2], 96, a[3], a[4], a[5]))!);
  expect(f(2, 3, 4, 3, 0, 'test/dyn-fx-hang')).toBe('DSP STOP in DYN-FX-HANG, FX SEND STEP 3');
  expect(f(1, 0, 0, 0, 0x12a4f0, 'test/crash')).toBe('CPU CRASH in CRASH, PC 0012A4F0');
  expect(f(3, 0, 0, 0, 0, 'runtime')).toBe('CPU PANIC in RUNTIME');
});

function settingsReply(status: number, body = new Uint8Array()) {
  return { cmd: CMD.SETTINGS, seq: 0, status, body };
}
const settingsCode = (name: string) => STATUS.find(([, status]) => status === name)![0];

test('an absent settings record is explicit and the strict reader still rejects it', async () => {
  const request = vi.fn<DkSession['request']>().mockResolvedValue(settingsReply(settingsCode('E_SET_NOKEY')));
  await expect(readSettingsRecordIfPresent({ request })).resolves.toBeNull();
  await expect(readSettingsRecord({ request })).rejects.toMatchObject({ kind: 'device', status: 'E_SET_NOKEY' });
});

test('an optional settings read preserves a real record', async () => {
  const rec = record([['fx/route/test/gain', Uint8Array.of(1)]]);
  const body = new Uint8Array(10 + rec.length);
  new DataView(body.buffer).setUint32(6, rec.length);
  body.set(rec, 10);
  const request = vi.fn<DkSession['request']>().mockResolvedValue(settingsReply(0, body));
  await expect(readSettingsRecordIfPresent({ request })).resolves.toEqual(rec);
});

test('other device errors, malformed replies and transport errors never become absent settings', async () => {
  const request = vi.fn<DkSession['request']>().mockResolvedValue(settingsReply(settingsCode('E_SET_OFF')));
  await expect(readSettingsRecordIfPresent({ request })).rejects.toMatchObject({ kind: 'device', status: 'E_SET_OFF' });
  request.mockResolvedValueOnce(settingsReply(0));
  await expect(readSettingsRecordIfPresent({ request })).rejects.toThrow('short record reply');
  const error = new Error('transport lost E_SET_NOKEY');
  request.mockRejectedValueOnce(error);
  await expect(readSettingsRecordIfPresent({ request })).rejects.toBe(error);
});
