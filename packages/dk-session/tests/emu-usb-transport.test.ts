import assert from 'node:assert/strict';
import { afterEach, test, vi } from 'vitest';
import { EmuUsbTransport, EMU_RPC_CHUNK, type EmuRpcCall } from '../src/index.ts';

afterEach(() => {
  vi.useRealTimers();
});

function adapter(replies: { hex?: string }[] = []) {
  const calls: { method: string; params?: Record<string, unknown> }[] = [];
  const call: EmuRpcCall = async <T>(method: string, params?: Record<string, unknown>) => {
    calls.push({ method, params });
    return (replies.shift() ?? {}) as T;
  };
  return { transport: new EmuUsbTransport(call), calls };
}

test('send preserves USB pacing, hex payload and large RPC request limit', async () => {
  const { transport, calls } = adapter();
  await transport.send(Uint8Array.of(0xf0, 0x00, 0x0a, 0xff, 0xf7));
  assert.equal(transport.rpcChunk, EMU_RPC_CHUNK);
  assert.equal(EMU_RPC_CHUNK, 16384);
  assert.deepEqual(calls, [
    {
      method: 'midi.usb.in',
      params: { bytes: 'f0000afff7', chunk: 384, period: 16500 },
    },
  ]);
});

test('recv joins fragments and queues complete messages without another RPC', async () => {
  const { transport, calls } = adapter([{ hex: 'f001' }, { hex: '02f7f003f7f004' }, { hex: '05f7' }]);
  assert.deepEqual(await transport.recv(100), Uint8Array.of(0xf0, 1, 2, 0xf7));
  assert.equal(calls.length, 2);
  assert.deepEqual(await transport.recv(100), Uint8Array.of(0xf0, 3, 0xf7));
  assert.equal(calls.length, 2);
  assert.deepEqual(await transport.recv(100), Uint8Array.of(0xf0, 4, 5, 0xf7));
  assert.deepEqual(
    calls,
    Array.from({ length: 3 }, () => ({ method: 'midi.usb.out', params: undefined })),
  );
});

test('timeout retains incomplete bytes for the next receive', async () => {
  const replies = [{ hex: 'f001' }, {}, { hex: '02f7' }];
  const linked = adapter(replies);
  assert.equal(await linked.transport.recv(0), null);
  assert.deepEqual(await linked.transport.recv(100), Uint8Array.of(0xf0, 1, 2, 0xf7));
});

test('empty polls wait on the wall clock and eventually time out', async () => {
  const { transport, calls } = adapter();
  vi.useFakeTimers();
  const start = Date.now();
  const pending = transport.recv(15);
  await vi.advanceTimersByTimeAsync(100);
  assert.equal(await pending, null);
  assert.ok(Date.now() - start >= 15);
  assert.ok(calls.length >= 2);
  assert.ok(calls.every(({ method }) => method === 'midi.usb.out'));
});

test('an aborted receive avoids polling; RPC failures propagate', async () => {
  const { transport, calls } = adapter();
  assert.equal(await transport.recv(100, AbortSignal.abort()), null);
  assert.deepEqual(calls, []);
  const failure = new Error('RPC disconnected');
  const failed = new EmuUsbTransport(async () => {
    throw failure;
  });
  await assert.rejects(failed.send(Uint8Array.of(0xf0, 0xf7)), failure);
  await assert.rejects(failed.recv(100), failure);
});

test('a paused daemon is stepped on an empty poll instead of waited for', async () => {
  const calls: { method: string; params?: Record<string, unknown> }[] = [];
  const replies = [{}, {}, { hex: 'f007f7' }];
  const call: EmuRpcCall = async <T>(method: string, params?: Record<string, unknown>) => {
    calls.push({ method, params });
    return (method === 'midi.usb.out' ? (replies.shift() ?? {}) : {}) as T;
  };
  const transport = new EmuUsbTransport(call, { stepInstructions: 400_000, pollMs: 60_000 });
  // fake timers and no advance: a poll that waited pollMs would never finish
  vi.useFakeTimers();
  assert.deepEqual(await transport.recv(5_000), Uint8Array.of(0xf0, 7, 0xf7));
  assert.deepEqual(
    calls.map((c) => c.method),
    ['midi.usb.out', 'step', 'midi.usb.out', 'step', 'midi.usb.out'],
  );
  assert.deepEqual(calls[1].params, { instructions: 400_000 });
});
