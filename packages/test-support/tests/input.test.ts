import assert from 'node:assert/strict';
import { test } from 'vitest';
import type { Clock } from '../src/emulated-clock.ts';
import { panelInput, probeInput, remoteInput } from '../src/input.ts';
import { button, encoder } from '../src/keys.ts';
import { MS } from '../src/session.ts';

function manualClock(): Clock & { at: number } {
  const clock = {
    at: 0,
    async now() {
      return clock.at;
    },
    async pause(ms: number) {
      clock.at += ms;
    },
  };
  return clock;
}

function fakePanel(clock: { at: number }, failTap = false) {
  const calls: unknown[][] = [];
  const session = {
    status: async () => ({ elapsed: clock.at * MS }),
    client: {
      inputButton: async (code: number, down: boolean) => void calls.push(['button', code, down]),
      inputTurn: async (enc: number, detents: number) => void calls.push(['turn', enc, detents]),
      inputTap: async (code: number, hold: number) => {
        calls.push(['tap', code, hold]);
        if (failTap) throw new Error('rejected');
        return { release_at: (clock.at + 300) * MS };
      },
    },
  };
  return { session, calls };
}

test('panelInput.tap returns after the release in emulated time', async () => {
  const clock = manualClock();
  const { session, calls } = fakePanel(clock);
  await panelInput(session as never, clock).tap('YES', 300);
  assert.deepEqual(calls, [['tap', button('YES'), 300 * MS]]);
  assert.ok(clock.at >= 305, `clock at ${clock.at}`);
  assert.ok(clock.at < 330);
});

test('panelInput.tap releases the key when the press fails', async () => {
  const clock = manualClock();
  const { session, calls } = fakePanel(clock, true);
  await assert.rejects(panelInput(session as never, clock).tap('YES', 300), /rejected/);
  assert.deepEqual(calls.at(-1), ['button', button('YES'), false]);
});

test('panelInput maps button and turn to the daemon', async () => {
  const clock = manualClock();
  const { session, calls } = fakePanel(clock);
  const input = panelInput(session as never, clock);
  await input.button('UP', true);
  await input.turn('ENC A', -2);
  assert.deepEqual(calls, [
    ['button', button('UP'), true],
    ['turn', encoder('ENC A'), -2],
  ]);
});

test('probeInput sends request 0x38, checks the status and does not turn', async () => {
  const clock = manualClock();
  const sent: [number, number[]][] = [];
  let status = 0;
  const input = probeInput(
    {
      request: async (cmd, body) => {
        sent.push([cmd, [...body]]);
        return { status };
      },
    },
    clock,
  );
  await input.tap('YES', 100);
  assert.deepEqual(sent, [
    [0x38, [button('YES'), 1]],
    [0x38, [button('YES'), 0]],
  ]);
  assert.ok(clock.at >= 100);
  status = 3;
  await assert.rejects(input.button('YES', true), /status 3/);
  await assert.rejects(input.turn('ENC A', 1), /not supported/);
});

test('remoteInput maps calls and releases after a failed hold', async () => {
  const clock = manualClock();
  const calls: unknown[][] = [];
  const input = remoteInput(
    {
      remoteKey: async (code, down) => void calls.push(['key', code, down]),
      remoteTurnOnce: async (enc, delta) => {
        calls.push(['turn', enc, delta]);
        return delta;
      },
    },
    clock,
  );
  await input.tap('NO', 60);
  await input.turn('ENC B', 3);
  assert.deepEqual(calls, [
    ['key', button('NO'), true],
    ['key', button('NO'), false],
    ['turn', encoder('ENC B'), 3],
  ]);
  clock.pause = async () => {
    throw new Error('stuck');
  };
  calls.length = 0;
  await assert.rejects(input.tap('NO', 60), /stuck/);
  assert.deepEqual(calls.at(-1), ['key', button('NO'), false]);
});
