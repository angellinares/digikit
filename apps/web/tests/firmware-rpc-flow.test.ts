// The RPC route of the Firmware page (apps/web/src/firmware/firmware-flow.ts `runRpcFlash`): the loop around the Rust state machine
// (patchkit.wasm), over a fake device on a virtual clock. The machine's own rules are tested in packages/patchkit/src/flash_rpc.rs; here:
// what the page shows, that nothing is flashed before the user says yes, and that leaving or cancelling sends End with commit 0.
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { test } from 'vitest';
import { decode8in7, encode8in7 } from '@digi/dk-session';
import { PatchKit, type PreparedFlash } from '../src/patch/patchkit-core.ts';
import type { Answer, FlashState } from '../src/firmware/firmware-core.ts';
import { runRpcFlash, type FlowLimits, type RpcOps } from '../src/firmware/firmware-flow.ts';

const wasm = fileURLToPath(new URL('../public/patchkit.wasm', import.meta.url));
const have = existsSync(wasm);
const kit = have ? await PatchKit.create(new Uint8Array(readFileSync(wasm)), '{"ports":[],"modules":[]}') : undefined;

const SIZE = 5000;
const CHUNK = 2048;
const prepared = (over: Partial<PreparedFlash> = {}): PreparedFlash => ({
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
  ends: [SIZE],
  image: Uint8Array.from({ length: SIZE }, (_, i) => (i * 31 + (i >> 3)) & 0xff),
  ...over,
});
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

const be32 = (n: number) => Uint8Array.of(n >>> 24, (n >>> 16) & 255, (n >>> 8) & 255, n & 255);
const cat = (...p: Uint8Array[]) => Uint8Array.from(p.flatMap((x) => Array.from(x)));
const ENVELOPE = Uint8Array.of(0xf0, 0x00, 0x20, 0x3c, 0x10, 0x00);

interface Opts {
  /** Answer 2 (damaged) to the Write at this offset this many times. */
  damage?: { at: number; times: number };
  /** Answer 3 with this message to the Write at this offset. */
  error?: { at: number; message: string };
  /** No reply to the Write at this offset. */
  silentAt?: number;
  /** Called after each Write was taken (to abort in the middle). */
  onWrite?: (n: number) => void;
}

/** A device on a virtual clock that speaks the RPC route of finding 75. */
function fake(opts: Opts = {}) {
  const t = { ms: 1000 };
  const log: string[] = [];
  const stream: number[] = [];
  const queue: Uint8Array[] = [];
  let damaged = 0;
  let writes = 0;
  let released = false;
  let endAt = 0;
  // the reply is `counter16 seq16 (type | 0x80) payload`: the device's own counter first, then the echoed seq
  const reply = (seq: number, ty: number, payload: Uint8Array) =>
    cat(
      ENVELOPE,
      encode8in7(cat(Uint8Array.of(0, 0x2a, seq >> 8, seq & 255, ty | 0x80), payload)),
      Uint8Array.of(0xf7),
    );
  const ops: RpcOps = {
    machine: async (image, chunk) => kit!.rpcFlash(image, chunk),
    send(m) {
      const body = decode8in7(m.subarray(6, m.length - 1));
      const seq = (body[0]! << 8) | body[1]!;
      const ty = body[4]!;
      const p = body.subarray(5);
      const dv = new DataView(p.buffer, p.byteOffset, p.byteLength);
      if (ty === 0x50) {
        log.push('start');
        queue.push(reply(seq, ty, Uint8Array.of(0, 0)));
      } else if (ty === 0x51) {
        const off = dv.getUint32(8);
        const data = p.subarray(12);
        if (opts.silentAt === off) return;
        if (opts.error?.at === off) {
          queue.push(
            reply(seq, ty, cat(be32(off), Uint8Array.of(3), new TextEncoder().encode(`${opts.error.message}\0`))),
          );
          return;
        }
        if (opts.damage?.at === off && damaged < opts.damage.times) {
          damaged++;
          log.push('damaged');
          queue.push(reply(seq, ty, cat(be32(off), Uint8Array.of(2), Uint8Array.of(0))));
          return;
        }
        stream.push(...data);
        writes++;
        log.push('write');
        const done = stream.length === SIZE;
        t.ms += done ? 1800 : 3; // the last Write checks the image
        queue.push(reply(seq, ty, cat(be32(stream.length), Uint8Array.of(done ? 1 : 0), Uint8Array.of(0))));
        opts.onWrite?.(writes);
      } else if (ty === 0x52) {
        log.push(`end ${p[0]}`);
        if (p[0] === 1) endAt = t.ms;
        queue.push(reply(seq, ty, Uint8Array.of(p[0] === 1 ? 0 : 2, 0)));
      }
    },
    async recv(ms) {
      const m = queue.shift();
      if (m) return m;
      t.ms += ms;
      return null;
    },
    now: () => t.ms,
    sleep: async (ms) => {
      t.ms += ms;
    },
    release: async () => {
      released = true;
    },
    port: 'Digitakt II',
    ports: async () => (released && t.ms - endAt <= 1500 ? [] : ['Digitakt II']),
    ask: async () => GOOD,
  };
  return { ops, log, stream, t };
}

const run = (
  f: ReturnType<typeof fake>,
  over: Partial<RpcOps> = {},
  signal = new AbortController().signal,
  p = prepared(),
) => {
  const states: FlashState[] = [];
  const ops = { ...f.ops, ...over };
  return runRpcFlash(ops, p, signal, (s) => states.push(s), SHORT, CHUNK).then((end) => ({ end, states }));
};
const message = (s: FlashState) => ('message' in s ? s.message : '');

test(
  'the image goes over in order, then End 1 follows at once and the restart is checked',
  { skip: !have },
  async () => {
    const f = fake();
    const p = prepared();
    const { end, states } = await run(f);
    assert.equal(end.phase, 'done');
    assert.deepEqual(f.log, ['start', 'write', 'write', 'write', 'end 1']);
    assert.deepEqual(Buffer.from(f.stream), Buffer.from(p.image));
    const phases = [...new Set(states.map((s) => (s.phase === 'rebooting' ? `rebooting:${s.stage}` : s.phase)))];
    assert.deepEqual(phases, ['sending', 'rebooting:leaving', 'rebooting:returning', 'rebooting:asking', 'done']);
    const last = states.filter((s) => s.phase === 'sending').at(-1);
    assert.deepEqual(last?.phase === 'sending' && [last.sent, last.total, last.unit], [SIZE, SIZE, 'bytes']);
    assert.ok(
      states.some((s) => s.phase === 'sending' && s.checking),
      'the page says the device checks the image',
    );
    assert.ok(
      states.some((s) => s.phase === 'sending' && s.flashing),
      'the page says the device accepted it and the flash follows',
    );
    assert.match(message(end), /runtime 0\.5\.2 · image id c5d8ccb27e139cbc matches/);
  },
);

test('Cancel while the device checks the image sends End 0 and says nothing was written', { skip: !have }, async () => {
  const ctl = new AbortController();
  const f = fake({ onWrite: (n) => n === 3 && ctl.abort() });
  const { end } = await run(f, {}, ctl.signal);
  assert.deepEqual(end.phase === 'stopped' && [end.reason, end.unit], ['cancelled', 'bytes']);
  assert.match(message(end), /Nothing was written/);
  assert.deepEqual(f.log.slice(-1), ['end 0']);
});

test('leaving or cancelling during the send sends End 0 and stops', { skip: !have }, async () => {
  const ctl = new AbortController();
  const f = fake({ onWrite: (n) => n === 1 && ctl.abort() });
  const { end } = await run(f, {}, ctl.signal);
  assert.equal(end.phase === 'stopped' && end.reason, 'cancelled');
  assert.deepEqual(f.log, ['start', 'write', 'end 0']);
  assert.equal(f.stream.length, CHUNK);
});

test('a damaged chunk is sent again; the run goes on', { skip: !have }, async () => {
  const f = fake({ damage: { at: CHUNK, times: 2 } });
  const { end } = await run(f);
  assert.equal(end.phase, 'done');
  assert.deepEqual(f.log.filter((l) => l === 'damaged').length, 2);
  assert.equal(f.stream.length, SIZE);
});

test("a device error is shown in the device's words, and End 0 follows", { skip: !have }, async () => {
  const f = fake({ error: { at: CHUNK, message: 'Checksum error!' } });
  const { end } = await run(f);
  assert.equal(end.phase === 'stopped' && end.reason, 'error');
  assert.match(message(end), /Checksum error\. Nothing/);
  assert.match(message(end), /Nothing was written/);
  assert.deepEqual(f.log.slice(-1), ['end 0']);
});

test(
  'a device that stops answering fails after the timeout, with End 0 and no repeated Write',
  { skip: !have },
  async () => {
    const f = fake({ silentAt: CHUNK });
    const { end } = await run(f);
    assert.equal(end.phase === 'stopped' && end.reason, 'error');
    assert.match(message(end), /stopped answering at byte 2048 of 5000/);
    assert.deepEqual(f.log, ['start', 'write', 'end 0']);
    assert.ok(f.t.ms >= 1000 + 2000, 'it waited for the write timeout');
  },
);

test("a port that refuses the message ends the run with the port's reason", { skip: !have }, async () => {
  const f = fake();
  const { end } = await run(f, {
    send: () => {
      throw new Error('The MIDI port is gone');
    },
  });
  assert.equal(end.phase === 'stopped' && end.reason, 'error');
  assert.match(message(end), /The MIDI port is gone/);
});
