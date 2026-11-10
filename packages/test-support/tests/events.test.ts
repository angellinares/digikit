import assert from 'node:assert/strict';
import { test } from 'vitest';
import { Events, type LogEvent, type LogReader } from '../src/events.ts';

const BOOT = 1;
const TAKEN = 41;
const MACHINE = 26;

/** A device log: sequence numbers from 1, a ring of `ring` events, paging as LOG does (from the oldest kept, 27 per reply). */
class FakeLog implements LogReader {
  events: LogEvent[] = [];
  seq = 0;
  levels = { level: 2, modules: [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff] };
  loaded = [
    { index: 0, name: 'digikit/debug' },
    { index: 2, name: 'digikit/remote' },
  ];
  readonly ring: number;
  constructor(ring = 512) {
    this.ring = ring;
  }
  add(kind: number, ...args: number[]): void {
    this.events.push({ seq: ++this.seq, time: 0, kind, flags: 0, args: [...args, 0, 0, 0, 0, 0].slice(0, 5) });
    if (this.events.length > this.ring) this.events.shift();
  }
  reboot(): void {
    this.events = [];
    this.seq = 0;
    this.levels.modules.fill(0xff);
    this.add(BOOT, 7);
  }
  async logPage(from: number): Promise<[number, number, LogEvent[]]> {
    const oldest = this.events[0]?.seq ?? 0;
    const start = Math.max(from, oldest);
    return [oldest, this.seq, this.events.filter((e) => e.seq >= start).slice(0, 27)];
  }
  async logConfig() {
    return { ...this.levels, modules: [...this.levels.modules] };
  }
  async setLogConfig(level: number, modules: number[]) {
    this.levels = { level, modules: [...modules] };
  }
  async modules() {
    return this.loaded;
  }
}

/** The clock moves 10 ms of emulated time at every read. */
const clock = () => {
  let n = 0;
  return { status: async () => ({ elapsed: (n += 10 * 132_000) }) } as never;
};
const make = (log: FakeLog) => new Events(log, clock(), { bootStart: BOOT, pollMs: 0, name: (k) => `K${k}` });

test('listen sets the debug level of the named modules only and skips older events', async () => {
  const log = new FakeLog();
  log.reboot();
  log.add(TAKEN, 2, 4, 1);
  const events = make(log);
  await events.listen();
  assert.deepEqual(log.levels.modules, [3, 0xff, 3, 0xff, 0xff, 0xff, 0xff, 0xff]);
  assert.equal(log.levels.level, 2);
  await assert.rejects(
    events.until(TAKEN, () => true, { emulatedMs: 50 }),
    /not seen within 50 ms/,
  );
});

test('an event matches once: the cursor moves past it', async () => {
  const log = new FakeLog();
  log.reboot();
  const events = make(log);
  await events.listen();
  log.add(TAKEN, 2, 4, 1);
  log.add(MACHINE, 0);
  log.add(TAKEN, 2, 4, -1);
  const first = await events.until(TAKEN, () => true, { emulatedMs: 100 });
  assert.equal(first.seq, 2);
  const second = await events.until(TAKEN, (e) => e.args[2] === -1 >>> 0 || e.args[2] === -1, { emulatedMs: 100 });
  assert.equal(second.seq, 4);
  await assert.rejects(
    events.until(TAKEN, () => true, { emulatedMs: 30 }),
    /not seen/,
  );
});

test('a miss reports the last events with their names', async () => {
  const log = new FakeLog();
  log.reboot();
  const events = make(log);
  await events.listen();
  log.add(MACHINE, 3);
  await assert.rejects(
    events.until(TAKEN, () => true, { emulatedMs: 30 }),
    /Last events:\n#2 K26 3 0 0 0 0/,
  );
});

test('a new boot restarts the cursor, also when its log is longer than the old cursor', async () => {
  const log = new FakeLog();
  log.reboot();
  for (let i = 0; i < 3; i++) log.add(MACHINE, i);
  const events = make(log);
  await events.listen();
  log.reboot();
  for (let i = 0; i < 8; i++) log.add(MACHINE, 9);
  log.add(TAKEN, 1, 2, 3);
  const hit = await events.until(TAKEN, () => true, { emulatedMs: 100 });
  assert.equal(hit.seq, 10);
  assert.equal(events.boots, 1);
  assert.deepEqual(log.levels.modules.slice(0, 3), [3, 0xff, 3], 'the levels are set again after the boot');
});

test('a boot that finds the cursor beyond the new newest event is detected too', async () => {
  const log = new FakeLog();
  log.reboot();
  for (let i = 0; i < 10; i++) log.add(MACHINE, i);
  const events = make(log);
  await events.listen();
  log.reboot();
  log.add(TAKEN, 0, 0, 0);
  assert.equal((await events.until(TAKEN, () => true, { emulatedMs: 100 })).seq, 2);
});

test('a sequence gap fails loudly', async () => {
  const log = new FakeLog(4);
  log.reboot();
  const events = make(log);
  await events.listen();
  for (let i = 0; i < 9; i++) log.add(MACHINE, i);
  await assert.rejects(
    events.until(TAKEN, () => true, { emulatedMs: 100 }),
    /lost events \(oldest kept #7, the cursor is at #2\)/,
  );
});
