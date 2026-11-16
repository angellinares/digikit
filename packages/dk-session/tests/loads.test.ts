// The LOADS reply in words (src/loads.ts): both versions of the record, one state each, and the text of every refusal.
import { expect, test } from 'vitest';
import { LOAD_STATE, LOAD_STATUS } from '../src/gen_proto.ts';
import { moduleReason, moduleStateText, parseLoads, type ModuleState } from '../src/loads.ts';

const fixed = (s: string, n: number) => Uint8Array.from({ length: n }, (_, i) => s.charCodeAt(i) || 0);
const be32 = (n: number) => Uint8Array.of((n >>> 24) & 255, (n >>> 16) & 255, (n >>> 8) & 255, n & 255);
const cat = (...p: Uint8Array[]) => Uint8Array.from(p.flatMap((x) => [...x]));

const v2 = (m: { name: string; result: number; state: number; detail?: [number, number]; needs?: string }) =>
  cat(
    be32(m.result >>> 0),
    Uint8Array.of(m.state, 0, 0, 0),
    be32(m.detail?.[0] ?? 0),
    be32(m.detail?.[1] ?? 0),
    fixed(m.needs ?? '', 12),
    fixed(m.name, 28),
  );
const v1 = (m: { name: string; result: number; needs?: string }) =>
  cat(be32(m.result >>> 0), fixed(m.needs ?? '', 12), fixed(m.name, 28));
const state = (name: string) => LOAD_STATE.find(([, n]) => n === name)![0];
const code = (name: string) => LOAD_STATUS.find(([, n]) => n === name)![0];

test('a version 2 reply gives every module its state, code, detail and needs', () => {
  const body = cat(
    Uint8Array.of(2, 4),
    v2({ name: 'pluck.dkm', result: 0, state: state('ACTIVE') }),
    v2({ name: 'big.dkm', result: code('E_PLAN_FIT'), state: state('REFUSED'), detail: [6, 0] }),
    v2({ name: 'usb-fast.dkm', result: code('E_IMPORT'), state: state('FAILED'), needs: 'usb-bulk' }),
    v2({ name: 'fx.dkm', result: 1, state: state('DSP_LOADING') }),
  );
  expect(parseLoads(body)).toEqual([
    { index: 0, name: 'pluck.dkm', state: 'ACTIVE', result: 0, detail: [0, 0] },
    { index: 1, name: 'big.dkm', state: 'REFUSED', result: -78, detail: [6, 0] },
    { index: 2, name: 'usb-fast.dkm', state: 'FAILED', result: -7, detail: [0, 0], needs: 'usb-bulk' },
    { index: 3, name: 'fx.dkm', state: 'DSP_LOADING', result: 1, detail: [0, 0] },
  ]);
});

test('a version 1 reply gets the state its code implies', () => {
  const body = cat(
    Uint8Array.of(1, 5),
    v1({ name: 'a.dkm', result: 0 }),
    v1({ name: 'b.dkm', result: 1 }),
    v1({ name: 'c.dkm', result: code('E_SAFE_MODE') }),
    v1({ name: 'd.dkm', result: code('E_PLAN_KEY') }),
    v1({ name: 'e.dkm', result: code('E_INIT') }),
  );
  expect(parseLoads(body).map((m) => m.state)).toEqual(['ACTIVE', 'DSP_LOADING', 'SKIPPED', 'REFUSED', 'FAILED']);
});

test('an unknown version, an unknown state and a short reply are errors', () => {
  expect(() => parseLoads(Uint8Array.of(3, 0))).toThrow(/version 3/);
  expect(() => parseLoads(cat(Uint8Array.of(2, 1), v2({ name: 'x', result: 0, state: 99 })))).toThrow(/state 99/);
  expect(() => parseLoads(Uint8Array.of(2, 1, 0))).toThrow(/short/);
});

const mod = (over: Partial<ModuleState>): ModuleState => ({
  index: 0,
  name: 'm.dkm',
  state: 'REFUSED',
  result: 0,
  detail: [0, 0],
  ...over,
});

test('each refusal of the boot plan says what to change, in words about the module', () => {
  expect(moduleStateText(mod({ result: code('E_PLAN_FIT'), detail: [6, 0] }))).toBe(
    'm refused: no room: the machine rows that the earlier modules of the selection left is not enough',
  );
  expect(moduleReason(mod({ result: code('E_PLAN_FIT'), detail: [15, 0] }))).toContain('DSP load');
  expect(moduleReason(mod({ result: code('E_PLAN_KIND'), detail: [90, 0] }))).toBe(
    'its records of kind 90 have no owner: no service or selected module takes that kind',
  );
  expect(moduleReason(mod({ result: code('E_PLAN_OWNER'), detail: [90, 0] }))).toBe(
    'another module already owns 0x5a (a record kind, slot or hook site it takes)',
  );
  expect(moduleReason(mod({ result: code('E_PLAN_KEY'), detail: [90, 0x1f] }))).toBe(
    'another selected module has the same key 0x1f for record kind 90',
  );
  expect(moduleReason(mod({ result: code('E_MISSING') }))).toContain('a service it needs');
  expect(moduleReason(mod({ result: code('E_MISSING'), detail: [0xabcd, 0] }))).toContain('0xabcd');
  expect(moduleReason(mod({ result: code('E_CONTRACT') }))).toBe('it needs a newer runtime than this one');
});

test('failures keep the loader text, a missing module names core/<module>, and active modules have no reason', () => {
  expect(moduleStateText(mod({ state: 'FAILED', result: code('E_INIT'), name: 'b.dkm' }))).toBe(
    'b not loaded: init returned non-zero',
  );
  expect(
    moduleStateText(mod({ state: 'FAILED', result: code('E_IMPORT'), needs: 'usb-bulk', name: 'usb-fast.dkm' })),
  ).toBe('usb-fast not loaded: this firmware has no core/usb-bulk. Flash firmware from the Firmware page.');
  expect(moduleStateText(mod({ state: 'SKIPPED', result: code('E_SAFE_MODE') }))).toBe(
    'm skipped: this boot is in safe mode, so no module is loaded',
  );
  expect(moduleReason(mod({ state: 'ACTIVE' }))).toBeUndefined();
  expect(moduleStateText(mod({ state: 'DSP_LOADING', result: 1 }))).toBeUndefined();
  expect(moduleReason(mod({ state: 'INIT' }))).toBe('loading has not finished');
});
