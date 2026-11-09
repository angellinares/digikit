// digikit/remote and the screenshot in the pure TypeScript client (`DkSession`, dkupload-core.ts) against a fake device that frames its replies byte for byte.
import assert from 'node:assert/strict';
import { test } from 'vitest';
import { DkSession, decode8in7, decodeScreen, encode8in7, frame, screenPixel } from '@digi/dk-session';
import { SERVICE_CMD } from '@digi/dk-session/gen_proto';

const concat = (...p: Uint8Array[]) => Uint8Array.from(Buffer.concat(p.map((x) => Buffer.from(x))));
const PREFIX = [0xf0, 0x7d, 0x64, 0x6b, 1];
const reply = (cmd: number, seq: number, status: number, body: number[] = []) =>
  concat(
    Uint8Array.from(PREFIX),
    Uint8Array.of(cmd | 0x40, seq),
    encode8in7(Uint8Array.of(status & 255, ...body)),
    Uint8Array.of(0xf7),
  );

/** The device: digikit/remote loaded, a firmware that clamps a TURN to `limit`, and the stock screenshot. */
class FakeRemote {
  limit: number;
  seen: string[] = [];
  queue: Uint8Array[] = [];
  held = new Set<number>();
  screen: Uint8Array;
  constructor({ limit = 30 } = {}) {
    this.limit = limit;
    this.seen = [];
    this.queue = [];
    this.held = new Set();
    this.screen = new Uint8Array(1024);
    this.screen[7] = 1; // pixel (0, 0)
  }
  send(msg: Uint8Array) {
    if (msg[1] === 0x00) {
      // a stock MidiRpc request (F0 00 20 3C 10 00, seq16 00 00 type): the Screenshot is answered with type 0x84
      assert.equal(decode8in7(msg.subarray(6, msg.length - 1))[4], 0x04);
      this.seen.push('SCREEN');
      const payload = concat(Uint8Array.of(1, 0, 0x80, 0, 0x40, 0, 0, 4, 0), this.screen);
      const body = concat(Uint8Array.of(0, 1, 0, 0, 0x84), payload);
      this.queue.push(concat(Uint8Array.of(0xf0, 0x00, 0x20, 0x3c, 0x10, 0x00), encode8in7(body), Uint8Array.of(0xf7)));
      return;
    }
    const cmd = msg[5];
    const seq = msg[6];
    const body = decode8in7(msg.subarray(7, msg.length - 1));
    const names = Object.fromEntries(Object.entries(SERVICE_CMD).map(([k, v]) => [v, k]));
    this.seen.push(names[cmd]!);
    if (cmd === SERVICE_CMD.REMOTE_STATE) return void this.queue.push(reply(cmd, seq, 0, [1, 1, this.limit]));
    if (cmd === SERVICE_CMD.REMOTE_KEY) {
      if (body[1]) this.held.add(body[0]);
      else this.held.delete(body[0]);
      return void this.queue.push(reply(cmd, seq, 0));
    }
    if (cmd === SERVICE_CMD.REMOTE_TURN) {
      const d = body[1] > 127 ? body[1] - 256 : body[1];
      const took = Math.max(-this.limit, Math.min(this.limit, d));
      return void this.queue.push(reply(cmd, seq, 0, [took & 255]));
    }
    if (cmd === SERVICE_CMD.REMOTE_RELEASE) {
      this.held.clear();
      return void this.queue.push(reply(cmd, seq, 0));
    }
    this.queue.push(reply(cmd, seq, -60));
  }
  async recv() {
    return this.queue.shift() ?? null;
  }
}

test('the service numbers are the generated ones', () => {
  const remote = Object.entries(SERVICE_CMD).filter(([name]) => name.startsWith('REMOTE_'));
  assert.deepEqual(Object.fromEntries(remote), {
    REMOTE_KEY: 0x38,
    REMOTE_TURN: 0x39,
    REMOTE_RELEASE: 0x3a,
    REMOTE_STATE: 0x3e,
  });
});

test('a request of the client has the framing the module reads', () => {
  const m = frame(SERVICE_CMD.REMOTE_KEY, 5, Uint8Array.of(2, 1));
  assert.equal(m[5], 0x38);
  assert.equal(m[6], 5);
});

test('state, key, a paced turn and release', async () => {
  const dev = new FakeRemote();
  const s = new DkSession(dev, { timeoutMs: 200 });
  assert.deepEqual(await s.remoteState(), { enabled: true, format: 1, turnLimit: 30 });
  await s.remoteKey(2, true);
  assert.deepEqual([...dev.held], [2]);
  await s.remoteTurn(1, 100, 0);
  assert.deepEqual(dev.seen.filter((x) => x === 'REMOTE_TURN').length, 4); // 30 + 30 + 30 + 10
  await s.remoteRelease();
  assert.equal(dev.held.size, 0);
});

test('the screenshot is decoded and pixel (x, y) reads the buffer', async () => {
  const dev = new FakeRemote();
  const s = new DkSession(dev, { timeoutMs: 200 });
  const shot = await s.screen();
  assert.equal(shot.length, 1024);
  assert.equal(screenPixel(shot, 0, 0), true);
  assert.equal(screenPixel(shot, 1, 0), false);
  assert.equal(decodeScreen(new Uint8Array(10)), null);
});
