// A flash from the first message to the verdict (apps/web/src/firmware/firmware-flow.ts), over a fake device on a virtual clock. The pacing is
// the Rust sender's (patchkit.wasm); the tests skip when the wasm is not built.
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { test } from 'vitest';
import { PatchKit, type PreparedFlash } from '../src/patch/patchkit-core.ts';
import type { Answer, FlashState } from '../src/firmware/firmware-core.ts';
import { runFlash, type FlowLimits, type FlowOps } from '../src/firmware/firmware-flow.ts';

const wasm = fileURLToPath(new URL('../public/patchkit.wasm', import.meta.url));
const have = existsSync(wasm);
const kit = have ? await PatchKit.create(new Uint8Array(readFileSync(wasm)), '{"ports":[],"modules":[]}') : undefined;

const NAME = 'Digitakt II';
const TOTAL = 40;
function prepared(over: Partial<PreparedFlash> = {}): PreparedFlash {
  const image = Uint8Array.from({ length: TOTAL * 8 }, (_, i) => i & 0xff);
  return {
    ok: true,
    restore: false,
    device: 'dn2',
    os: '1.12',
    shaStock: 'a'.repeat(64),
    shaImage: 'b'.repeat(64),
    imageId: 'c5d8ccb27e139cbc',
    runtime: '0.5.2',
    paceMs: 2,
    checks: [],
    ends: Array.from({ length: TOTAL }, (_, i) => (i + 1) * 8),
    image,
    ...over,
  };
}

const SHORT: FlowLimits = {
  leaveMs: 3000,
  returnMs: 3000,
  askMs: 6000,
  pollMs: 100,
  askEveryMs: 500,
  releaseMs: 30,
  portsMs: 30,
  askTryMs: 30,
};
const GOOD: Answer = { os: '1.12', runtime: '0.5.2', imageId: 'c5d8ccb27e139cbc' };

/** A device on a virtual clock: its port goes away `goneAfter` ms after the last message and is back `backAfter` ms later. */
function fake(opts: {
  goneAfter?: number | null;
  backAfter?: number | null;
  answers?: (n: number) => Answer | Error;
  failAt?: number;
}) {
  const t = { ms: 1000 };
  const sent: { at: number; msg: Uint8Array }[] = [];
  let lastSend = 0;
  let released = false;
  let asks = 0;
  const present = async (): Promise<boolean> => {
    if (!released) return true;
    const gone = opts.goneAfter === undefined ? 500 : opts.goneAfter;
    const back = opts.backAfter === undefined ? 1500 : opts.backAfter;
    if (gone === null) return true;
    const since = t.ms - lastSend;
    if (since < gone) return true;
    if (back === null) return false;
    return since >= gone + back;
  };
  const ops: FlowOps = {
    sender: async (total) => kit!.sender(total),
    send(m) {
      if (opts.failAt !== undefined && sent.length === opts.failAt) throw new Error('The MIDI port is gone');
      sent.push({ at: t.ms, msg: m });
      lastSend = t.ms;
    },
    now: () => t.ms,
    sleep: async (ms) => {
      t.ms += ms;
    },
    release: async () => {
      released = true;
    },
    port: NAME,
    ports: async () => ((await present()) ? [NAME] : []),
    async ask() {
      const a = (opts.answers ?? (() => GOOD))(asks++);
      if (a instanceof Error) throw a;
      return a;
    },
  };
  return { ops, sent, t, wasReleased: () => released, asks: () => asks };
}

const run = (
  f: ReturnType<typeof fake>,
  p: PreparedFlash,
  signal = new AbortController().signal,
  states: FlashState[] = [],
) => runFlash(f.ops, p, signal, (s) => states.push(s), SHORT);
const code = (s: FlashState) => (s.phase === 'failed' ? s.code : s.phase);
const message = (s: FlashState) => ('message' in s ? s.message : '');

test(
  'a patch is sent in order, 2 ms apart, then the restart and the runtime are checked',
  { skip: !have },
  async () => {
    const p = prepared();
    const f = fake({});
    const states: FlashState[] = [];
    const end = await run(f, p, undefined, states);
    assert.equal(end.phase, 'done');
    assert.match(message(end), /runtime 0\.5\.2 · image id c5d8ccb27e139cbc matches/);
    assert.equal(f.sent.length, TOTAL);
    // the messages are the image's, in order
    assert.deepEqual(Buffer.concat(f.sent.map((s) => s.msg)), Buffer.from(p.image));
    for (let i = 1; i < f.sent.length; i++)
      assert.ok(f.sent[i]!.at - f.sent[i - 1]!.at >= 2, `message ${i} comes 2 ms after the one before`);
    assert.ok(f.wasReleased());
    // the states go sending, rebooting (leaving, returning, asking), done
    const phases = [...new Set(states.map((s) => (s.phase === 'rebooting' ? `rebooting:${s.stage}` : s.phase)))];
    assert.deepEqual(phases, ['sending', 'rebooting:leaving', 'rebooting:returning', 'rebooting:asking', 'done']);
    const last = states.filter((s) => s.phase === 'sending').at(-1);
    assert.deepEqual(last?.phase === 'sending' && [last.sent, last.total], [TOTAL, TOTAL]);
  },
);

test(
  'Cancel stops the sending at once; the device keeps its image and the port is not released',
  { skip: !have },
  async () => {
    const f = fake({});
    const ctl = new AbortController();
    const origSend = f.ops.send;
    f.ops.send = (m) => {
      void origSend(m);
      if (f.sent.length === 7) ctl.abort();
    };
    const end = await run(f, prepared(), ctl.signal);
    assert.deepEqual(end.phase === 'stopped' && [end.reason, end.sent, end.total], ['cancelled', 7, TOTAL]);
    assert.match(message(end), /Press NO on the device to cancel, or send again from the start/);
    assert.equal(f.sent.length, 7);
    assert.equal(f.wasReleased(), false);
  },
);

test('a port that fails mid-send stops the run, and the sender does not go on', { skip: !have }, async () => {
  const f = fake({ failAt: 12 });
  const end = await run(f, prepared());
  assert.deepEqual(end.phase === 'stopped' && [end.reason, end.sent], ['error', 12]);
  assert.match(message(end), /Message 13 of 40 failed: The MIDI port is gone\. The device has changed nothing/);
  assert.equal(f.sent.length, 12);
  assert.equal(f.wasReleased(), false);
});

test(
  'a device that does not come back is a clear failure; one that never leaves is asked after leaveMs',
  { skip: !have },
  async () => {
    const gone = await run(fake({ backAfter: null }), prepared());
    assert.equal(code(gone), 'no-return');
    assert.match(message(gone), /did not answer on any port named Digitakt II\./);
    // the restart was not seen (faster than a poll): the device is asked, and its answer is the verdict
    const never = fake({ goneAfter: null });
    assert.equal((await run(never, prepared())).phase, 'done');
    assert.ok(never.asks() >= 1);
  },
);

test('the runtime or the image id of another image is a failure', { skip: !have }, async () => {
  assert.equal(code(await run(fake({ answers: () => ({ ...GOOD, runtime: '0.5.1' }) }), prepared())), 'runtime');
  assert.equal(
    code(await run(fake({ answers: () => ({ ...GOOD, imageId: 'ffffffffffffffff' }) }), prepared())),
    'image-id',
  );
});

test('a stale answer of the old image is not the verdict while time remains', { skip: !have }, async () => {
  const OLD = { ...GOOD, imageId: 'a6b3d7b4a110ccb5' };
  // the port never seemed to leave and the old image answers first
  const stale = fake({ goneAfter: null, answers: (n) => (n < 2 ? OLD : GOOD) });
  assert.equal((await run(stale, prepared())).phase, 'done');
  assert.equal(stale.asks(), 3);
  // a real mismatch is judged on the last answer, after askMs
  const wrong = fake({ goneAfter: null, answers: () => OLD });
  const end = await run(wrong, prepared());
  assert.equal(code(end), 'image-id');
  assert.ok(wrong.t.ms - 1000 >= SHORT.leaveMs + SHORT.askMs - SHORT.askEveryMs);
});

test('a device that answers late is waited for; one that never does is a failure', { skip: !have }, async () => {
  const late = fake({ answers: (n) => (n < 3 ? new Error('no reply') : GOOD) });
  assert.equal((await run(late, prepared())).phase, 'done');
  assert.equal(late.asks(), 4);
  // the stock part answers, the runtime does not: wait for it, then say so
  assert.equal(code(await run(fake({ answers: () => ({ os: '1.12' }) }), prepared())), 'runtime');
  assert.equal(code(await run(fake({ answers: () => new Error('no reply') }), prepared())), 'no-answer');
});

test('a restore ends when the stock firmware answers twice without a runtime', { skip: !have }, async () => {
  const p = prepared({ restore: true, runtime: null, imageId: null });
  const f = fake({ answers: () => ({ os: '1.12' }) });
  const end = await run(f, p);
  assert.equal(end.phase, 'done');
  assert.equal(f.asks(), 2);
  assert.match(message(end), /stock OS 1\.12, no runtime/);
  // a runtime that is still there is a failure at once
  assert.equal(code(await run(fake({ answers: () => GOOD }), p)), 'still-runtime');
});

test('the user can stop waiting for the restart', { skip: !have }, async () => {
  const ctl = new AbortController();
  const f = fake({ goneAfter: null });
  const origSleep = f.ops.sleep;
  f.ops.sleep = async (ms) => {
    await origSleep(ms);
    if (f.wasReleased()) ctl.abort();
  };
  const end = await run(f, prepared(), ctl.signal);
  assert.equal(code(end), 'no-restart');
  assert.match(message(end), /You stopped waiting/);
});

// ---- faults of the port itself (a fake sender: these do not need the wasm) -------------------------------------------------------------

/** A device whose port list is `ports(ms since the last message)`; each fault replaces one operation. */
function faulty(
  ports: (since: number) => string[],
  over: Partial<FlowOps> = {},
  signal: AbortSignal = new AbortController().signal,
): { run: () => Promise<FlashState>; t: { ms: number }; moved: string[]; log: string[]; states: FlashState[] } {
  const t = { ms: 1000 };
  const moved: string[] = [];
  const log: string[] = [];
  const states: FlashState[] = [];
  const ops: FlowOps = {
    sender: async () => ({ next: () => ({ kind: 'done' }), sent: () => 1, fail() {}, free() {} }) as never,
    send() {},
    now: () => t.ms,
    sleep: async (ms) => {
      t.ms += ms;
    },
    release: async () => {},
    port: NAME,
    ports: async () => ports(t.ms - 1000),
    moved: (name) => void moved.push(name),
    log: (line) => void log.push(line),
    ask: async () => GOOD,
    ...over,
  };
  const p = prepared({ ends: [8], image: new Uint8Array(8) });
  const run = () => runFlash(ops, p, signal, (s) => states.push(s), SHORT);
  return { run, t, moved, log, states };
}
const within15s = (t: { ms: number }) => assert.ok(t.ms - 1000 <= 15_000, `took ${t.ms - 1000} ms of virtual time`);

test('the normal restart: the port goes, comes back, the device answers; the log has one line per stage', async () => {
  const f = faulty((s) => (s < 2000 || s >= 4000 ? [NAME] : []));
  assert.equal((await f.run()).phase, 'done');
  within15s(f.t);
  assert.deepEqual(f.moved, []);
  assert.deepEqual(f.log, [
    'rebooting: leaving · ports: Digitakt II',
    'rebooting: returning · ports: none',
    'rebooting: asking · ports: Digitakt II',
  ]);
});

test('a restart no poll saw (the port is never missing from the list) goes on to asking after leaveMs', async () => {
  const f = faulty(() => [NAME]);
  assert.equal((await f.run()).phase, 'done');
  within15s(f.t);
  assert.ok(f.log.some((l) => l.startsWith('rebooting: asking')));
  assert.ok(!f.log.some((l) => l.startsWith('rebooting: returning')));
});

test('a device that comes back as "Digitakt II 2" is used on that port', async () => {
  const f = faulty((s) => (s < 2000 ? [NAME] : s < 4000 ? [] : ['Digitakt II 2', 'Other 2']));
  const end = await f.run();
  assert.equal(end.phase, 'done');
  within15s(f.t);
  assert.deepEqual(f.moved, ['Digitakt II 2']);
  assert.ok(f.log.some((l) => l.includes('came back as Digitakt II 2')));
});

test('a port of another name does not count as the device', async () => {
  const f = faulty((s) => (s < 2000 ? [NAME] : ['Digitakt II Pro', 'Digitakt', 'Digitakt II 2x']));
  const end = await f.run();
  assert.equal(code(end), 'no-return');
  assert.deepEqual(f.moved, []);
});

test('release() that hangs does not stop the run', async () => {
  const f = faulty((s) => (s < 2000 || s >= 4000 ? [NAME] : []), { release: () => new Promise(() => {}) });
  assert.equal((await f.run()).phase, 'done');
  within15s(f.t);
});

test('ports() that hangs is "unknown": the run polls again', async () => {
  let calls = 0;
  // calls 1-2 (log, first poll): present; call 3 hangs; then the port is gone (calls 4-8), then back
  const f = faulty(() => [], {
    ports: () => (++calls === 3 ? new Promise(() => {}) : Promise.resolve(calls < 4 || calls > 8 ? [NAME] : [])),
  });
  assert.equal((await f.run()).phase, 'done');
  assert.ok(calls > 8, 'the polling went on after the hung call');
  within15s(f.t);
});

test('an ask() that hangs is tried again; the run ends in a clear failure when none answers', async () => {
  let n = 0;
  const late = faulty((s) => (s < 2000 || s >= 4000 ? [NAME] : []), {
    ask: () => (++n < 3 ? new Promise(() => {}) : Promise.resolve(GOOD)),
  });
  assert.equal((await late.run()).phase, 'done');
  const never = faulty((s) => (s < 2000 || s >= 4000 ? [NAME] : []), { ask: () => new Promise(() => {}) });
  assert.equal(code(await never.run()), 'no-answer');
  within15s(never.t);
});

test('stopping the run ends a hung release()', async () => {
  const ctl = new AbortController();
  const f = faulty(() => [NAME], { release: () => new Promise(() => {}) }, ctl.signal);
  setTimeout(() => ctl.abort(), 5);
  const end = await f.run();
  assert.match(message(end), /You stopped waiting/);
});
