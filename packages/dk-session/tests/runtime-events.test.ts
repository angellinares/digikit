// LogCursor and RuntimeEvents on a fake log with a ring limit and a fake clock: no real time passes.
import assert from 'node:assert/strict';
import { test } from 'vitest';
import { LogCursor, RuntimeEvents, type Clock, type DkLogEvent, type WaitRecord } from '@digi/dk-session';
import { LOG_KIND } from '@digi/dk-session/gen_proto';

const word = (s: string, i: number) => {
  const part = s.slice(i * 4, i * 4 + 4);
  let n = 0;
  for (let k = 0; k < 4; k++) n = n * 256 + (part.charCodeAt(k) || 0);
  return n;
};

class FakeLog {
  events: DkLogEvent[] = [];
  next = 1;
  pages: number[] = [];
  ring: number;
  pageSize = 4;
  constructor(ring = 1000) {
    this.ring = ring;
  }
  add(kind: keyof typeof LOG_KIND, args: number[] = [], time?: number): DkLogEvent {
    const e = {
      seq: this.next++,
      time: time ?? this.next,
      kind: LOG_KIND[kind],
      flags: 0,
      args: [...args, 0, 0, 0, 0, 0].slice(0, 5),
    };
    this.events.push(e);
    while (this.events.length > this.ring) this.events.shift();
    return e;
  }
  reset() {
    this.events = [];
    this.next = 1;
  }
  async logPage(from: number): Promise<[number, number, DkLogEvent[]]> {
    this.pages.push(from);
    const oldest = this.events[0]?.seq ?? 0;
    const newest = this.events[this.events.length - 1]?.seq ?? 0;
    return [oldest, newest, this.events.filter((e) => e.seq >= from).slice(0, this.pageSize)];
  }
  async logConfig() {
    return { level: 1, modules: [0, 0, 0, 0, 0, 0, 0, 0] };
  }
  async setLogConfig() {
    return { level: 1, modules: [], debug: true };
  }
  async modules() {
    return [];
  }
}

class FakeClock implements Clock {
  t = 0;
  async now() {
    return this.t;
  }
  async pause(ms: number) {
    this.t += ms;
  }
}

const make = (log: FakeLog, onWait?: (r: WaitRecord) => void) => {
  const clock = new FakeClock();
  const ev = new RuntimeEvents(log as never, clock, { onWait, tail: 3 });
  return { ev, clock };
};

test('an event after the cursor matches; events before listen() do not', async () => {
  const log = new FakeLog();
  log.add('SAFE');
  const { ev } = make(log);
  await ev.listen();
  await assert.rejects(ev.until('SAFE', undefined, { withinMs: 200 }), /not seen within 200 ms/);
  log.add('SAFE', [3]);
  const e = await ev.until('SAFE');
  assert.equal(e.seq, 2);
});

test('fromBoot sees the early events of the boot', async () => {
  const log = new FakeLog();
  log.add('BOOT_START');
  log.add('UI_FIRST', [0, 0, 2]);
  const { ev } = make(log);
  await ev.listen({ fromBoot: true });
  assert.equal((await ev.untilBoot()).seq, 1);
});

test('consumed events are not matched twice', async () => {
  const log = new FakeLog();
  const { ev } = make(log);
  await ev.listen({ fromBoot: true });
  log.add('SAFE');
  log.add('SAFE');
  assert.equal((await ev.until('SAFE')).seq, 1);
  assert.equal((await ev.until('SAFE')).seq, 2);
  await assert.rejects(ev.until('SAFE', undefined, { withinMs: 100 }));
});

test('a new boot is seen by a changed anchor, by BOOT_START after the anchor and by a shorter log', async () => {
  const log = new FakeLog();
  log.add('BOOT_START');
  log.add('SAFE');
  const cur = new LogCursor(log);
  await cur.fromNewest();
  // changed anchor: the log restarted and holds other events at the same numbers
  log.reset();
  log.add('BOOT_START', [], 500);
  log.add('MOD_DSP', [1], 501);
  let r = await cur.read();
  assert.equal(r.newBoot, true);
  assert.deepEqual(
    r.events.map((e) => e.seq),
    [1, 2],
  );
  assert.equal(cur.boots, 1);
  // BOOT_START after an anchor that is still there (the numbers kept going)
  log.add('SAFE');
  log.add('BOOT_START');
  r = await cur.read();
  assert.equal(r.newBoot, true);
  assert.equal(cur.boots, 2);
  // newest below the anchor
  log.add('SAFE');
  log.add('SAFE');
  await cur.read();
  log.reset();
  r = await cur.read();
  assert.equal(r.newBoot, true);
  assert.equal(cur.boots, 3);
  assert.deepEqual(r.events, []);
});

test('a ring that drops exactly the anchor loses nothing and is no new boot', async () => {
  const log = new FakeLog(2);
  log.add('SAFE');
  log.add('SAFE');
  const cur = new LogCursor(log);
  await cur.fromNewest(); // anchor #2
  log.add('SAFE');
  log.add('SAFE'); // the ring keeps #3, #4: the anchor is gone, nothing after it
  const r = await cur.read();
  assert.equal(r.newBoot, false);
  assert.equal(r.lost, 0);
  assert.deepEqual(
    r.events.map((e) => e.seq),
    [3, 4],
  );
  assert.equal(cur.boots, 0);

  const log2 = new FakeLog(2);
  log2.add('SAFE');
  log2.add('SAFE');
  const { ev } = make(log2);
  await ev.listen();
  log2.add('SAFE');
  log2.add('SAFE');
  assert.equal((await ev.until('SAFE')).seq, 3);
  assert.equal(ev.boots, 0);
});

test('a gap is reported as lost; until throws', async () => {
  const log = new FakeLog(5);
  log.add('SAFE');
  const cur = new LogCursor(log);
  await cur.fromNewest();
  for (let i = 0; i < 9; i++) log.add('SAFE');
  const r = await cur.read();
  assert.equal(r.newBoot, false);
  assert.equal(r.lost, 4); // seq 2..5 dropped, 6..10 kept
  assert.deepEqual(
    r.events.map((e) => e.seq),
    [6, 7, 8, 9, 10],
  );

  const log2 = new FakeLog(5);
  log2.add('SAFE');
  const { ev } = make(log2);
  await ev.listen();
  for (let i = 0; i < 9; i++) log2.add('SAFE');
  await assert.rejects(ev.until('BOOT_START'), /lost 4 events/);
});

test('the wait ends after withinMs with the tail of the events seen', async () => {
  const log = new FakeLog();
  const { ev, clock } = make(log);
  await ev.listen({ fromBoot: true });
  for (let i = 0; i < 5; i++) log.add('SAFE', [i]);
  await assert.rejects(ev.until('BOOT_START', undefined, { withinMs: 300 }), (e: Error) => {
    assert.match(e.message, /not seen within 300 ms/);
    assert.match(e.message, /#5 SAFE 4/);
    assert.match(e.message, /#3 SAFE 2/);
    assert.ok(!e.message.includes('#2 SAFE'), 'only the last 3 events');
    return true;
  });
  assert.ok(clock.t >= 300);
});

test('typed waits match on decoded fields', async () => {
  const log = new FakeLog();
  const { ev } = make(log);
  await ev.listen({ fromBoot: true });
  log.add('UI_FIRST', [0, 0, 1]);
  log.add('UI_FIRST', [0, 0, 4]);
  assert.equal((await ev.untilUiFirst({ loaded: 3 })).seq, 2);
  log.add('MOD_LOAD', [0, 0, word('abcd', 0), word('ef', 0)]);
  log.add('MOD_LOAD', [1, 0, word('usb-', 0), word('fast', 0)]);
  const m = await ev.untilModLoad('usb-fast');
  assert.equal(m.seq, 4);
  log.add('MOD_DSP', [2, 0]);
  log.add('MOD_DSP', [5, 0]);
  assert.equal((await ev.untilModDsp(5)).seq, 6);
  log.add('INPUT_TAKEN', [2, 7, 0xffffffff]);
  assert.equal((await ev.untilInputTaken(7, -1)).seq, 7);
});

test('untilFwReady matches the FW_READY of this boot, also when it was logged before the wait, and fails with the last events when it is missing', async () => {
  const log = new FakeLog();
  log.add('BOOT_START');
  log.add('UI_FIRST', [450, 0, 2]);
  log.add('FW_READY', [450]);
  const { ev } = make(log);
  await ev.listen({ fromBoot: true });
  const ready = await ev.untilFwReady();
  assert.equal(ready.seq, 3);
  assert.equal(ready.args[0], 450);
  // another boot without it
  log.reset();
  log.add('BOOT_START');
  await assert.rejects(ev.untilFwReady({ withinMs: 100 }), /FW_READY: not seen within 100 ms[\s\S]*BOOT_START/);
});

test('onWait reports success and failure', async () => {
  const log = new FakeLog();
  const records: WaitRecord[] = [];
  const { ev } = make(log, (r) => records.push(r));
  await ev.listen({ fromBoot: true });
  log.add('SAFE');
  await ev.until('SAFE');
  await assert.rejects(ev.until('SAFE', undefined, { withinMs: 100 }));
  assert.equal(records.length, 2);
  assert.deepEqual(
    records.map((r) => [r.kind, r.what, r.ok]),
    [
      ['event', 'SAFE', true],
      ['event', 'SAFE', false],
    ],
  );
  assert.ok(records[1]!.clockMs >= 100);
});
