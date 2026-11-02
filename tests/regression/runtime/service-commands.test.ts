import { hasCap } from '@digi/dk-session';
import { RANGES } from '@digi/dk-session/gen_proto';
import { expect } from 'vitest';
import type { PatchedSession } from '../support.ts';
import { expectRuntimeReady, installModule, moduleArtifact, runtimeTests } from './live.ts';

/** Port of live-svc: runtime command registration, dispatch and refusal semantics. */
async function expectServiceCommands({ session, dk }: PatchedSession): Promise<void> {
  await expectRuntimeReady(session);
  await installModule(dk, 'test/svc-echo', moduleArtifact(session.options.device, 'test/svc-echo'));
  expect(hasCap(await dk.info(), 'SERVICE_CMDS'), 'INFO advertises service commands').toBe(true);
  expect(RANGES.remote_state, 'reserved remote state command range').toEqual([0x3e, 0x3f]);

  const echo = 0x3e;
  for (const body of [
    Uint8Array.from([...'hello '].map((char) => char.charCodeAt(0)).concat([0x80, 0xff])),
    new Uint8Array(),
    Uint8Array.from({ length: 150 }, (_, index) => index),
    Uint8Array.from({ length: 250 }, (_, index) => index),
  ]) {
    const reply = await dk.request(echo, body);
    expect(reply.status, `${body.length}-byte echo status`).toBe(0);
    expect(reply.body, `${body.length}-byte echo body`).toEqual(body);
  }

  const ownStatus = await dk.request(echo, Uint8Array.of(0xee));
  expect(ownStatus.status, 'module-defined status').toBe(-77);
  expect(ownStatus.body).toEqual(new Uint8Array());
  expect((await dk.request(echo, new Uint8Array(300))).status, 'oversized request is E_MSG').toBe(-50);

  const refusals = await dk.request(0x3f);
  expect(refusals.status).toBe(0);
  // Init attempted registration outside a reserved range, over an existing one, and for an empty range.
  expect([...refusals.body], 'three E_CMD_RANGE refusals').toEqual([0xc0, 0xc0, 0xc0]);
  expect((await dk.request(0x3d)).status, 'unregistered command is E_PROTO').toBe(-60);
  await session.expectNoFault('service commands');
}

runtimeTests('service commands echo bodies and preserve registration and dispatch refusals', expectServiceCommands, {
  timeout: 360_000,
});
