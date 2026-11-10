import fs from 'node:fs';
import { expect } from 'vitest';
import type { PatchedSession } from '../support.ts';
import { moduleArtifact } from './install.ts';
import { expectRuntimeReady } from './ready.ts';

/** The probe transport (with remote also selected): the SHARC probe effect sends one framed word per block through the module step word, and the ColdFire collector keeps it. */
export const PROBE = 'test/sharc-probe';
const COLLECTOR = 'digikit/probe';
/** Selected too: it takes service command slots (the collector got E_PROTO on a device when remote used all of them). */
const REMOTE = 'digikit/remote';
export const CMD_STATUS = 0x32;
const CMD_READ = 0x33;
export const CMD_RESET = 0x34;

export function status(body: Uint8Array): Record<string, number> {
  const keys = [
    'format',
    'blocks',
    'records',
    'drops',
    'repeats',
    'idle',
    'words',
    'overruns',
    'cap',
    'parsed',
    'frames_ok',
    'seq_errors',
    'cks_errors',
  ];
  const view = new DataView(body.buffer, body.byteOffset, body.byteLength);
  return Object.fromEntries(keys.map((key, i) => [key, view.getUint32(4 * i)]));
}

export async function installProbe({ session, dk }: PatchedSession, effect: string): Promise<void> {
  session.requireDsp('installProbe (a SHARC probe module)');
  await expectRuntimeReady(session);
  const paths: string[] = [];
  for (const id of [REMOTE, effect, COLLECTOR]) {
    const bytes = fs.readFileSync(moduleArtifact(session.options.device, id));
    const path = `modules/${id.split('/').at(-1)}.dkm`;
    await dk.put(path, bytes);
    expect(await dk.verifyFile(path, bytes), `${id} reads back from the card`).toBe(true);
    paths.push(path);
  }
  await dk.select(paths);
  await dk.restart();
  await dk.waitForBoot();
  await expect
    .poll(async () => (await dk.modules()).map((m) => m.name), { timeout: 30_000, interval: 500 })
    .toEqual(expect.arrayContaining([REMOTE, effect, COLLECTOR]));
}

export async function readRing(dk: PatchedSession['dk'], count: number): Promise<number[]> {
  const words: number[] = [];
  while (words.length < count) {
    const body = new Uint8Array(6);
    new DataView(body.buffer).setUint32(0, words.length);
    new DataView(body.buffer).setUint16(4, 200);
    const reply = await dk.request(CMD_READ, body);
    expect(reply.status, 'READ answers').toBe(0);
    const view = new DataView(reply.body.buffer, reply.body.byteOffset, reply.body.byteLength);
    const got = (reply.body.length - 8) / 4;
    if (got === 0) break;
    for (let k = 0; k < got; k++) words.push(view.getUint32(8 + 4 * k));
  }
  return words;
}
