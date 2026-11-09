// Following a runtime log (apps/desktop/ui/src/log-follow-core.ts): what a poll turns into log entries, with a fake ring in place of a device.
import assert from 'node:assert/strict';
import { createRoot, createSignal } from 'solid-js';
import { test } from 'vitest';
import { CAP, LOG_KIND } from '@digi/dk-session/gen_proto';
import type { DkLogEvent, Fault } from '@digi/dk-session';
import { autoOpens, type LogInput } from '../src/log-core.ts';
import {
  FROZEN_MS,
  IDLE_MS,
  LogFollower,
  POLL_MS,
  pace,
  type FollowSession,
  type OriginStatus,
} from '../src/log-follow-core.ts';
import { createLogFollow } from '../src/log-follow.ts';

const word = (s: string) => {
  let n = 0;
  for (let i = 0; i < 4; i++) n = n * 256 + (s.charCodeAt(i) || 0);
  return n;
};
const ev = (seq: number, kind: keyof typeof LOG_KIND, args: number[] = [], flags = 0): DkLogEvent => ({
  seq,
  time: seq * 150,
  kind: LOG_KIND[kind],
  flags,
  args: [...args, 0, 0, 0, 0, 0].slice(0, 5),
});
const lv = (level: number, src = 0, error = false) => (level << 1) | (src << 8) | (error ? 1 : 0);

/** A device: a ring of events, the answers to INFO, LOGCFG, MODULES and the fault record, and a way to not answer. */
class Device implements FollowSession {
  ring: DkLogEvent[] = [];
  caps: number | null = 1 << CAP.LOG;
  debug = { level: 3, modules: Array<number>(8).fill(0xff), debug: true };
  modulesLoaded: { index: number; name: string }[] = [];
  faultRecord: Fault | null = null;
  failWith: Error | null = null;
  async info() {
    if (this.failWith) throw this.failWith;
    return { caps: this.caps };
  }
  async logPage(from: number): Promise<[number, number, DkLogEvent[]]> {
    if (this.failWith) throw this.failWith;
    const events = this.ring.filter((e) => e.seq >= from).slice(0, 6);
    return [this.ring[0]?.seq ?? 0, this.ring.at(-1)?.seq ?? 0, events];
  }
  async logConfig() {
    return this.debug;
  }
  async setLogConfig(level: number, modules: number[]) {
    this.debug = { ...this.debug, level, modules };
    return this.debug;
  }
  async modules() {
    return this.modulesLoaded;
  }
  async fault() {
    return this.faultRecord;
  }
}

function setup(device: Device, origin: 'device' | 'emulator' = 'device') {
  const entries: LogInput[] = [];
  const statuses: OriginStatus[] = [];
  const moduleLists: string[][] = [];
  let now = 0;
  let present = true;
  const follower = new LogFollower({
    origin,
    session: async () => device,
    present: async () => present,
    now: () => now,
    out: {
      push: (inputs) => entries.push(...inputs),
      status: (s) => statuses.push(s),
      modules: (names) => moduleLists.push(names),
    },
  });
  return {
    follower,
    entries,
    statuses,
    moduleLists,
    texts: () => entries.map((e) => e.text),
    advance: (ms: number) => (now += ms),
    setPresent: (p: boolean) => (present = p),
  };
}

const fault = (over: Partial<Fault>): Fault => ({
  kind: 'dsp',
  site: 3,
  stage: 4,
  id: 96,
  step: 3,
  at: 0,
  runtime: 0,
  module: 'test/dyn-fx-hang',
  ...over,
});

test('a poll shows the events of the ring with the module that wrote them, and reads the log configuration once', async () => {
  const d = new Device();
  d.modulesLoaded = [{ index: 1, name: 'freqshift' }];
  d.ring = [
    ev(1, 'BOOT_START'),
    ev(2, 'MOD_LIST', [1, 0, word('freq'), word('shif'), word('t')]),
    // 8 events in the ring: more than one page
    ...[3, 4, 5, 6, 7].map((n) => ev(n, 'MENU', [1])),
    ev(8, 'MOD_LOG', [word('shif'), word('t in'), word('it'), 0, 300], lv(2, 2)),
  ];
  const t = setup(d);
  await t.follower.tick();
  await t.follower.tick();
  const rows = t.entries.filter((e) => e.rt?.event);
  assert.equal(rows.length, 8);
  assert.equal(rows.at(-1)!.text, 'freqshift  "shift init"  value=0x0000012c');
  assert.equal(rows.at(-1)!.rt!.module, 'freqshift');
  assert.equal(rows.at(-1)!.source, 'device');
  assert.deepEqual(t.moduleLists.at(-1), ['freqshift']);
  assert.equal(t.texts().filter((x) => x.startsWith('device: debug module')).length, 1);
  assert.ok(t.texts().includes('device: debug module loaded, level debug'));
});

test('without the debug module only the fault record is read, and the log starts when the module is there', async () => {
  const d = new Device();
  d.caps = 1 << CAP.SERVICE_CMDS; // a runtime without the LOG capability
  d.faultRecord = fault({});
  let pages = 0;
  const page = d.logPage.bind(d);
  d.logPage = async (from) => {
    pages++;
    return page(from);
  };
  const t = setup(d);
  assert.equal(await t.follower.tick(), IDLE_MS);
  assert.deepEqual(t.statuses, ['no-debug']);
  assert.equal(pages, 0);
  assert.deepEqual(
    t.entries.filter((e) => e.text.startsWith('fault:')).map((e) => e.level),
    ['error'],
  );
  assert.equal(await t.follower.tick(), IDLE_MS);
  assert.equal(t.entries.filter((e) => e.text.startsWith('fault:')).length, 1); // the same record is not shown twice
  // the device restarts with the module loaded: the log is followed
  d.caps = (1 << CAP.SERVICE_CMDS) | (1 << CAP.LOG);
  d.ring = [ev(1, 'BOOT_START')];
  await t.follower.tick();
  assert.equal(t.statuses.at(-1), 'following');
  assert.equal(t.entries.filter((e) => e.rt?.event).length, 1);
});

test('a DSP stop of the boot before is one error entry, with the crash events folded into its detail', async () => {
  const d = new Device();
  d.faultRecord = fault({});
  d.ring = [
    ev(1, 'BOOT_START'),
    ev(2, 'FAULT_FOUND', [2 | (3 << 8) | (4 << 16), 96, 3, 0x600304, 0], 1),
    ev(3, 'CRASH_MOD', [0, 0, word('test'), word('/dyn'), word('-fx\0')], 1),
    ev(4, 'CRASH_TAIL', [90, 4500, LOG_KIND.MENU << 16, 1, 0]),
    ev(5, 'CRASH_TAIL', [90, 0, 0, 0, 1]),
  ];
  const t = setup(d);
  await t.follower.tick();
  const kinds = t.entries.filter((e) => e.rt?.event).map((e) => e.rt!.event!.kind);
  assert.deepEqual(kinds, ['BOOT_START', 'FAULT_FOUND', 'CRASH_MOD']); // CRASH_TAIL is not a row
  const entry = t.entries.find((e) => e.text.startsWith('fault:'))!;
  assert.equal(entry.text, 'fault: DSP STOP in DYN-FX-HANG, FX SEND STEP 3');
  assert.equal(entry.level, 'error');
  assert.match(entry.detail!, /kept on the card as dk\/fault/);
  assert.match(entry.detail!, /events before the stop \(oldest first\):\n {2} +90 +3\.000s .*MENU/);
  // the drawer opens for the fault entry, not for the error row FAULT_FOUND, and not again for the same record on the next start
  assert.deepEqual(
    t.entries.filter(autoOpens).map((e) => e.text),
    [entry.text],
  );
  await t.follower.tick();
  assert.equal(t.entries.filter((e) => e.text.startsWith('fault:')).length, 1);
  assert.equal(t.entries.filter(autoOpens).length, 1);
});

test('events the ring overwrote are counted, a restart is noted and the fault record is read again', async () => {
  const d = new Device();
  d.ring = [ev(1, 'MENU'), ev(2, 'MENU')];
  const t = setup(d);
  await t.follower.tick();
  d.ring = [ev(6, 'MENU'), ev(7, 'MENU')]; // 3, 4 and 5 are gone
  await t.follower.tick();
  assert.ok(t.texts().includes('3 events lost (the ring held from 6)'));
  d.ring = [ev(1, 'BOOT_START')]; // the device booted again
  d.faultRecord = fault({ kind: 'cpu', site: 0, stage: 0, step: 0, at: 0x12a4f0, module: 'test/crash' });
  await t.follower.tick();
  assert.ok(t.texts().includes('the device restarted: the log starts again (was at 7, now 1)'));
  assert.ok(t.texts().includes('fault: CPU CRASH in CRASH, PC 0012A4F0'));
  assert.equal(t.entries.filter((e) => e.rt?.event?.kind === 'BOOT_START').length, 1);
});

test('the emulator says "the emulator restarted"', async () => {
  const d = new Device();
  d.ring = [ev(1, 'MENU'), ev(2, 'MENU')];
  const t = setup(d, 'emulator');
  await t.follower.tick();
  d.ring = [ev(1, 'BOOT_START')];
  await t.follower.tick();
  assert.ok(t.texts().some((x) => x.startsWith('the emulator restarted: the log starts again')));
});

test('a device that does not answer for 5 s is noted once, polled every 5 s, and noted again when it answers', async () => {
  const d = new Device();
  d.ring = [ev(1, 'MENU')];
  const t = setup(d);
  await t.follower.tick();
  d.failWith = Object.assign(new Error('no reply from the device'), { kind: 'timeout' });
  for (let i = 0; i < 4; i++) {
    t.advance(1000);
    assert.equal(await t.follower.tick(), 1000);
  }
  assert.ok(!t.texts().some((x) => x.startsWith('no answer')));
  t.advance(1000);
  assert.equal(await t.follower.tick(), FROZEN_MS);
  t.advance(FROZEN_MS);
  assert.equal(await t.follower.tick(), FROZEN_MS);
  assert.equal(t.texts().filter((x) => x.startsWith('no answer to LOG for 5 s')).length, 1);
  assert.ok(t.texts().includes('no answer to LOG for 5 s: the device may be frozen (still asking)'));
  d.failWith = null;
  assert.equal(await t.follower.tick(), 1000);
  assert.equal(t.texts().at(-1), 'the device answers again');
  assert.equal(t.statuses.at(-1), 'following');
});

test('a cancelled request makes no entry; a missing port is not a frozen device', async () => {
  const d = new Device();
  d.ring = [ev(1, 'MENU')];
  const t = setup(d);
  await t.follower.tick();
  const before = t.entries.length;
  d.failWith = Object.assign(new Error('cancelled'), { kind: 'cancelled' });
  t.advance(10_000);
  await t.follower.tick();
  assert.equal(t.entries.length, before);
  d.failWith = Object.assign(new Error('no MIDI input port'), { kind: 'timeout' });
  t.setPresent(false);
  t.advance(10_000);
  await t.follower.tick();
  assert.equal(t.entries.length, before);
  assert.equal(t.statuses.at(-1), 'absent');
});

test('an origin without the LOG capability is not followed', async () => {
  const d = new Device();
  d.caps = 1 << CAP.PEEK;
  d.ring = [ev(1, 'MENU')];
  const t = setup(d);
  assert.equal(await t.follower.tick(), FROZEN_MS);
  assert.equal(t.entries.length, 0);
  assert.equal(t.statuses.at(-1), 'no-runtime');
  d.caps = null;
  await t.follower.tick();
  assert.equal(t.entries.length, 0);
});

test('a firmware flash pauses the follower once, and the resume starts again from INFO', async () => {
  const d = new Device();
  d.ring = [ev(1, 'MENU')];
  const t = setup(d);
  await t.follower.tick();
  t.follower.pause();
  t.follower.pause();
  assert.equal(t.texts().filter((x) => x === 'paused during the firmware flash').length, 1);
  assert.equal(t.follower.paused, true);
  t.follower.resume();
  assert.equal(t.follower.paused, false);
  d.ring.push(ev(2, 'MENU'));
  await t.follower.tick();
  assert.equal(t.entries.filter((e) => e.rt?.event).length, 2);
});

test('flash progress does not restart the loop: one pause note, and polling goes on after the flash', async () => {
  const d = new Device();
  d.ring = [ev(1, 'MENU')];
  let infos = 0;
  const info = d.info.bind(d);
  d.info = async () => (infos++, info());
  const [progress, setProgress] = createSignal(0);
  const [flashing, setFlashing] = createSignal(true);
  const entries: LogInput[] = [];
  const wait = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));
  const dispose = createRoot((dispose) => {
    createLogFollow({
      follow: () => true,
      flashing: () => (progress(), flashing()),
      emulator: { running: () => false, session: async () => d },
      device: { port: () => 'Digitakt', session: async () => d, present: async () => true },
      push: (inputs) => entries.push(...inputs),
      now: () => 0,
      sleep: () => wait(2),
    });
    return dispose;
  });
  await wait(10);
  for (let i = 1; i <= 20; i++) {
    setProgress(i);
    await wait(1);
  }
  await wait(10);
  assert.equal(entries.filter((e) => e.text === 'paused during the firmware flash').length, 1);
  assert.equal(infos, 0);
  setFlashing(false);
  await wait(20);
  assert.ok(infos > 0);
  dispose();
});

test('pace: a hidden window asks nothing; a closed drawer reads the device slowly, the emulator keeps its pace', () => {
  assert.equal(pace(true, false, true, POLL_MS).ask, false);
  assert.deepEqual(pace(true, true, true, POLL_MS), { ask: true, wait: POLL_MS });
  assert.deepEqual(pace(true, true, false, POLL_MS), { ask: true, wait: IDLE_MS });
  assert.deepEqual(pace(false, true, false, POLL_MS), { ask: true, wait: POLL_MS });
  assert.equal(pace(true, true, false, 30_000).wait, 30_000);
});
