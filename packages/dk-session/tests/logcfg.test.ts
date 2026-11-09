// LOGCFG in the pure TypeScript client against a fake debug module that keeps the levels as modules/tools/debug `cmd_logcfg` does (set needs exactly 10 body bytes).
import assert from 'node:assert/strict';
import { test } from 'vitest';
import { DkError, DkSession, decode8in7, encode8in7 } from '@digi/dk-session';
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

class FakeRuntime {
  level = 2;
  modules = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
  debug = true;
  bodies: number[][] = [];
  queue: Uint8Array[] = [];
  send(msg: Uint8Array) {
    const cmd = msg[5]!;
    const seq = msg[6]!;
    const body = decode8in7(msg.subarray(7, msg.length - 1));
    assert.equal(cmd, SERVICE_CMD.LOGCFG);
    this.bodies.push([...body]);
    if (body[0] === 1) {
      if (body.length !== 10) return void this.queue.push(reply(cmd, seq, -2));
      this.level = body[1]!;
      this.modules = [...body.subarray(2, 10)];
    }
    if (body[0]! > 1) return void this.queue.push(reply(cmd, seq, -60)); // op 2, the old debug switch, is gone
    this.queue.push(reply(cmd, seq, 0, [this.level, ...this.modules, this.debug ? 1 : 0]));
  }
  async recv() {
    return this.queue.shift() ?? null;
  }
}

test('logConfig reads and setLogConfig sends op 1 with exactly 10 bytes and returns the new state', async () => {
  const dev = new FakeRuntime();
  const s = new DkSession(dev, { timeoutMs: 200 });
  assert.deepEqual(await s.logConfig(), { level: 2, modules: Array(8).fill(0xff), debug: true });
  const set = await s.setLogConfig(4, [0xff, 0, 3, 0xff, 0xff, 0xff, 0xff, 1]);
  assert.deepEqual(dev.bodies.at(-1), [1, 4, 0xff, 0, 3, 0xff, 0xff, 0xff, 0xff, 1]);
  assert.deepEqual(set, { level: 4, modules: [0xff, 0, 3, 0xff, 0xff, 0xff, 0xff, 1], debug: true });
  assert.deepEqual(await s.logConfig(), set);
});

test('setLogConfig refuses a level or a module count the runtime would refuse, without sending', async () => {
  const dev = new FakeRuntime();
  const s = new DkSession(dev, { timeoutMs: 200 });
  const eight = Array(8).fill(0xff);
  await assert.rejects(s.setLogConfig(5, eight), DkError);
  await assert.rejects(s.setLogConfig(0xff, eight), DkError);
  await assert.rejects(s.setLogConfig(2, [...eight.slice(0, 7), 7]), DkError);
  await assert.rejects(s.setLogConfig(2, eight.slice(0, 7)), DkError);
  assert.equal(dev.bodies.length, 0);
});
