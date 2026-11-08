import { SERVICE_CMD } from '@digi/dk-session/gen_proto';
import { expect } from 'vitest';
import type { PatchedSession } from '@digi/regression/support.ts';
import { timePhase } from '@digi/regression/timing.ts';
import { liveArtifacts } from '@digi/regression/runtime/artifacts.ts';
import { runtimeTests, expectRuntimeReady } from '@digi/regression/runtime/ready.ts';
import { waitEmulated } from '@digi/regression/runtime/waits.ts';
import {
  captureRemoteOled,
  expectRemoteEnabled,
  installRemote,
  prepareRemote,
  remoteTap,
  withRemoteRelease,
} from '@digi/regression/runtime/remote.ts';
import { openDigikitMenu, openRuntimeAbout } from '@digi/regression/runtime/runtime-menu.ts';

/** KEY navigation opens semantic ABOUT content; TURN reports accepted signed deltas. */
async function expectRemoteKeys(runtime: PatchedSession): Promise<void> {
  const { session, dk } = runtime;
  await withRemoteRelease(dk, async () => {
    const plan = await prepareRemote(runtime);
    let shots = 0;
    const shot = (name: string) => captureRemoteOled(session, `${String(++shots).padStart(2, '0')}-${name}`);
    const tap = async (name: string, settleMs = 1200) => {
      await remoteTap(session, dk, name, settleMs);
      await shot(name.toLowerCase());
    };
    await shot('start');
    await openDigikitMenu(tap);
    await openRuntimeAbout(tap, session, dk, plan, liveArtifacts(session, 'remote-input'));
    for (let i = 0; i < 4; i++) await tap('NO', 2000);
    try {
      expect(await dk.remoteTurnOnce(1, 20)).toBe(20);
      expect(await dk.remoteTurnOnce(1, -20)).toBe(-20);
    } finally {
      await shot('turn-signed');
    }
  });
  await session.expectNoFault('remote keys');
}

/** The remote commands answer as soon as digikit/remote is installed: checks, bounds and paced TURN. */
async function expectRemoteControl(runtime: PatchedSession): Promise<void> {
  const { session, dk } = runtime;
  await expectRuntimeReady(session);
  expect((await dk.request(SERVICE_CMD.REMOTE_STATE)).status, 'remote service is absent before upload').toBe(-60);
  await installRemote(runtime);
  await expectRemoteEnabled(session, dk);
  const cmd = (command: number, body: number[] = []) => dk.request(command, Uint8Array.from(body));
  const state = async () => {
    const reply = await cmd(SERVICE_CMD.REMOTE_STATE);
    return [reply.status, ...reply.body];
  };
  expect(await state()).toEqual([0, 1, 1, 30]);
  await withRemoteRelease(dk, async () => {
    expect([
      (await cmd(SERVICE_CMD.REMOTE_KEY, [2, 1])).status,
      (await cmd(SERVICE_CMD.REMOTE_KEY, [2, 0])).status,
      (await cmd(SERVICE_CMD.REMOTE_RELEASE)).status,
    ]).toEqual([0, 0, 0]);
    expect([
      (await cmd(SERVICE_CMD.REMOTE_KEY, [1])).status,
      (await cmd(SERVICE_CMD.REMOTE_KEY, [2, 2])).status,
      (await cmd(SERVICE_CMD.REMOTE_TURN, [0, 1])).status,
    ]).toEqual([-50, -50, -50]);
    expect((await cmd(SERVICE_CMD.REMOTE_KEY, [127, 1])).status).toBe(-1);
    const first = await cmd(SERVICE_CMD.REMOTE_TURN, [1, 100]);
    const immediate = await cmd(SERVICE_CMD.REMOTE_TURN, [1, 100]);
    expect([first.status, [...first.body]]).toEqual([0, [30]]);
    expect(immediate.status).toBe(0);
    expect(immediate.body.length).toBe(1);
    // Adjacent host requests may span a firmware refill; exact timing is covered by the C pacing unit.
    expect([0, 30]).toContain(immediate.body[0]);
    await timePhase('settle.remote-refill', () => waitEmulated(session, 3000));
    const back = await cmd(SERVICE_CMD.REMOTE_TURN, [1, 156]);
    expect(back.status).toBe(0);
    expect(back.body.length).toBe(1);
    expect(256 - back.body[0]!).toBeGreaterThanOrEqual(1);
    expect(256 - back.body[0]!).toBeLessThanOrEqual(30);
  });
  await session.expectNoFault('remote control');
}

runtimeTests('remote keys open ABOUT and TURN accepts signed deltas', expectRemoteKeys, { dsp: 'sharc' });
runtimeTests('remote commands answer once the module is installed and TURN is paced', expectRemoteControl, { dsp: 'sharc' });
