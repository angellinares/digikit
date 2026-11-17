// Names in the Log pane (the app's log follower and name backends, apps/desktop/ui/src/log-follow.ts and log-names-backend.ts, with the shared
// log-names resolver from @digi/dk-session/log-names): a golden
// fault record with a small ship map, a module map and a load table read from a fake guest memory, through the real dkprof.wasm
// (`dkprof:wasm`, a dependency of this project's test-host). Unresolved addresses stay hex. The emulator and the device origin go through the same
// code (PEEK of the debug module on the follower's session); they differ in where the ship map comes from.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createRoot } from 'solid-js';
import { beforeAll, test } from 'vitest';
import { CAP, LOG_KIND } from '@digi/dk-session/gen_proto';
import { DkProf } from '@digi/dk-session/dkprof';
import { faultDetail, eventText, runtimeEvent, type DkLogEvent, type Fault } from '@digi/dk-session';
import type { FollowSession } from '../src/log-follow-core.ts';
import { createLogFollow } from '../src/log-follow.ts';
import type { LogInput } from '../src/log-core.ts';
import { createDkProfLoader } from '../src/dkprof-env.ts';
import { createDeviceNames, createEmulatorNames } from '../src/log-names-backend.ts';
import { makeResolver, readLoadTable } from '@digi/dk-session/log-names';
import { createFakeHost } from '@digi/workbench-core/host/fake-host.ts';

let prof: DkProf;
let wasm: Uint8Array<ArrayBuffer>;
beforeAll(async () => {
  wasm = new Uint8Array(readFileSync(new URL('../../../web/public/dkprof.wasm', import.meta.url)));
  prof = await DkProf.create(wasm);
});

const BASE = 0x47e10000;
const STATE = 0x40100000;
const entry = (addr: number, size: number, name: string, module?: string) => ({
  cpu: 'cf',
  addr,
  size,
  name,
  layer: 'our',
  ...(module ? { module } : {}),
});
const ship = {
  format: 'dk-addrmap',
  version: 1,
  kind: 'ship',
  key: { device: 'dt2', loader: { modules: 2, l2_byte_base: 0x20000000, l2_sw_base: 0xb80000, dsp: false } },
  entries: [entry(0x40101000, 0x80, 'dkrt::tick')],
};
const moduleMap = {
  format: 'dk-addrmap',
  version: 1,
  kind: 'module',
  key: { module: 'test/dyn-demo' },
  entries: [entry(0, 0x40, 'init', 'test/dyn-demo'), entry(0x40, 0x40, 'render', 'test/dyn-demo')],
};

/** The bytes of DKRT_STATE: ten header words, then 13 words per slot (used, name[32], version, base, size, fini); slot 0 holds test/dyn-demo. */
function stateBytes(): Buffer {
  const b = Buffer.alloc(40 + 52 * 2);
  b.writeUInt32BE(1, 40);
  b.write('test/dyn-demo', 44);
  b.writeUInt32BE(BASE, 40 + 4 + 32 + 4);
  b.writeUInt32BE(0x80, 40 + 4 + 32 + 8);
  return b;
}
/** PEEK of a fake guest: the memory holds DKRT_STATE only; anything else is the device's E_PEEK. */
const peek = async (addr: number, len: number): Promise<Uint8Array> => {
  const b = stateBytes();
  if (addr < STATE || addr + len > STATE + b.length) throw new Error('E_PEEK');
  return Uint8Array.from(b.subarray(addr - STATE, addr - STATE + len));
};

const word = (s: string) => {
  let n = 0;
  for (let i = 0; i < 4; i++) n = n * 256 + (s.charCodeAt(i) || 0);
  return n;
};
const ev = (seq: number, kind: keyof typeof LOG_KIND, args: number[]): DkLogEvent => ({
  seq,
  time: seq * 150,
  kind: LOG_KIND[kind],
  flags: 0,
  args: [...args, 0, 0, 0, 0, 0].slice(0, 5),
});
const cpuFault: Fault = { kind: 'cpu', site: 0, stage: 0, id: 0, step: 0, at: 0, runtime: 0, module: 'dyn-demo' };
/** The log of a boot that found a crash: pc, the module, a stack of three words. */
const crashLog = (pc: number, stack: number[]): DkLogEvent[] => [
  ev(1, 'CRASH_FOUND', [pc, 2, 0x2700, 0x40200000, 150]),
  ev(2, 'CRASH_MOD', [0, 1, word('test'), word('/dyn'), word('-dem')]),
  ev(3, 'CRASH_STACK', [0, ...stack]),
];

test('the load table is read from the symbol addresses of the image and names a module the table holds', async () => {
  const loads = await readLoadTable(prof, ship, { DKRT_STATE: STATE }, peek);
  assert.deepEqual(
    loads.modules.map((m) => [m.id, m.base]),
    [['test/dyn-demo', BASE]],
  );
  const name = makeResolver(prof, [ship, moduleMap], loads);
  assert.equal(name('cf', BASE + 4), 'test/dyn-demo: init+0x4');
  assert.equal(name('cf', 0x40101004), 'dkrt::tick+0x4');
  // a return address is looked up one instruction earlier: the call that ended the last instruction of init is in init
  assert.equal(name('cf', BASE + 0x40, true), 'test/dyn-demo: init+0x3e');
});

test('a fault record renders its pc and the stack words that point into code with names', async () => {
  const loads = await readLoadTable(prof, ship, { DKRT_STATE: STATE }, peek);
  const name = makeResolver(prof, [ship, moduleMap], loads);
  const events = crashLog(BASE + 0x44, [0x40200010, BASE + 0x52, 0x40101010, 0xdeadbeef]);
  const text = faultDetail(cpuFault, events, new Map(), name);
  assert.match(
    text,
    new RegExp(`pc ${(BASE + 0x44).toString(16)}  .*\\nin test/dyn-demo: render\\+0x4, unverified\\n`),
  );
  assert.match(text, /stack words that point into code \(return addresses\?\):/);
  assert.match(text, new RegExp(`\\+004  ${(BASE + 0x52).toString(16)}  test/dyn-demo: render\\+0x10, unverified`));
  assert.match(text, /\+008  40101010  dkrt::tick\+0xe/);
  // the plain words are not named
  assert.doesNotMatch(text, /deadbeef  \S/);
  assert.doesNotMatch(text, /\+000  40200010  /);
  // the crash line of the log carries the name after the pc
  const line = eventText(runtimeEvent(events[0]!), new Map(), name);
  assert.ok(line.includes(`pc=${(BASE + 0x44).toString(16)} (test/dyn-demo: render+0x4, unverified)`), line);
});

test('an address no map holds stays hex, and a module that is not loaded has no names', async () => {
  const loads = await readLoadTable(prof, ship, { DKRT_STATE: STATE }, peek);
  const name = makeResolver(prof, [ship, moduleMap], loads);
  assert.equal(name('cf', 0x60000000), null);
  const none = makeResolver(prof, [ship, moduleMap], { modules: [] });
  assert.equal(none('cf', BASE + 4), null);
  const text = faultDetail(cpuFault, crashLog(0x60000000, [0x60000010, 0x60000020]), new Map(), name);
  assert.match(text, /pc 60000000 /);
  assert.doesNotMatch(text, /return addresses|\nin (?!module)/);
  assert.ok(!eventText(runtimeEvent(crashLog(0x60000000, [])[0]!), new Map(), name).includes('('));
  // and without a resolver nothing changes
  assert.equal(faultDetail(cpuFault, crashLog(BASE + 4, []), new Map()).includes('test/dyn-demo: init'), false);
});

const CAPS = (1 << CAP.LOG) | (1 << CAP.SERVICE_CMDS) | (1 << CAP.PEEK);
const IMAGE = '0123456789abcdef';

/** The protocol session of an origin: its ring, INFO with the image id, and PEEK of the fake guest (counted). */
function origin(ring: DkLogEvent[], caps = CAPS) {
  const peeks: number[] = [];
  const session: FollowSession = {
    info: async () => ({ caps, image_id: IMAGE, port: 'dt2-1.17' }),
    peek: async (addr, len) => {
      peeks.push(addr);
      return peek(addr, len);
    },
    logPage: async (from) => [1, 3, ring.filter((e) => e.seq >= from)],
    logConfig: async () => ({ level: 3, modules: [], debug: true }) as never,
    setLogConfig: async () => ({}) as never,
    modules: async () => [{ index: 0, name: 'test/dyn-demo' }],
    fault: async () => null,
  };
  return { session, peeks };
}

async function follow(
  emulator: ReturnType<typeof origin>,
  device: ReturnType<typeof origin>,
  host: ReturnType<typeof createFakeHost>,
) {
  const prof = createDkProfLoader(host);
  const entries: LogInput[] = [];
  const wait = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));
  const dispose = createRoot((dispose) => {
    createLogFollow({
      follow: () => true,
      flashing: () => false,
      emulator: { running: () => true, session: async () => emulator.session, names: createEmulatorNames(host, prof) },
      device: {
        port: () => 'Digitakt',
        session: async () => device.session,
        present: async () => true,
        names: createDeviceNames(host, prof),
      },
      push: (inputs) => entries.push(...inputs),
      now: () => 0,
      sleep: () => wait(2),
    });
    return dispose;
  });
  await wait(80);
  dispose();
  return entries;
}

const hostFor = (ships: unknown[]) =>
  createFakeHost({
    'rpc.call': async (method: string) => {
      assert.equal(method, 'runtime.maps', 'the load table is read through PEEK, not through the emulator RPC');
      return { image_sha256: 'x', matches: true, ship: JSON.stringify(ship), symbols: { DKRT_STATE: STATE } };
    },
    'library.readMaps': { ships, modules: [{ id: 'test/dyn-demo', json: JSON.stringify(moduleMap) }] },
    'firmware.dkprofWasm': () => wasm.buffer.slice(wasm.byteOffset, wasm.byteOffset + wasm.byteLength),
  });
const shipRecord = { image_id: IMAGE, ship: JSON.stringify(ship), symbols: { DKRT_STATE: STATE } };
const crashEntry = (entries: LogInput[], source: string) =>
  entries.find((e) => e.source === source && e.rt?.event?.kind === 'CRASH_FOUND')!;

test('both origins name a crash through PEEK of the debug module: the emulator with the controller maps, the device with the kept ship map', async () => {
  const ring = crashLog(BASE + 0x44, [BASE + 0x52]);
  const emulator = origin(ring);
  const device = origin(ring);
  const host = hostFor([shipRecord]);
  const entries = await follow(emulator, device, host);
  const named = '(test/dyn-demo: render+0x4, unverified)';
  assert.ok(crashEntry(entries, 'emulator').text.includes(named), crashEntry(entries, 'emulator').text);
  assert.ok(crashEntry(entries, 'device').text.includes(named), crashEntry(entries, 'device').text);
  // the load table came from the symbol address, through the session of each origin
  assert.deepEqual([emulator.peeks[0], device.peeks[0]], [STATE, STATE]);
  // the maps were read for the crash, once per origin (not for every round)
  assert.equal(host.argsOf('rpc.call').length, 1);
  assert.equal(host.argsOf('firmware.dkprofWasm').length, 1);
});

test('a device whose image has no kept map, or no debug module, shows hex and says why once', async () => {
  const ring = crashLog(BASE + 0x44, [BASE + 0x52]);
  // another image id than the kept one: no map for it
  let entries = await follow(origin(ring), origin(ring), hostFor([{ ...shipRecord, image_id: 'ffffffffffffffff' }]));
  assert.ok(!crashEntry(entries, 'device').text.includes('('), crashEntry(entries, 'device').text);
  assert.ok(crashEntry(entries, 'emulator').text.includes('render+0x4'));
  const notes = entries.filter((e) => e.text.startsWith('device: names unavailable'));
  assert.equal(notes.length, 1);
  assert.match(notes[0]!.text, /no map for the image 0123456789abcdef/);
  // no PEEK capability: the debug module is not loaded
  entries = await follow(origin(ring), origin(ring, CAPS & ~(1 << CAP.PEEK)), hostFor([shipRecord]));
  assert.ok(!crashEntry(entries, 'device').text.includes('('));
  assert.match(entries.find((e) => e.text.startsWith('device: names unavailable'))!.text, /needs the debug module/);
});
