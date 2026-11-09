// The pure logic of the desktop Remote page (apps/desktop/ui/src/remote-core.ts).
import assert from 'node:assert/strict';
import { test } from 'vitest';
import { DkError, type DkSession } from '@digi/dk-session';
import {
  INFO_EVERY_MS,
  deviceOfPort,
  infoDue,
  isRemoteMissing,
  leaveCancels,
  remoteNotice,
  remotePaused,
  remoteRuntime,
  remoteStateWanted,
} from '../src/remote-core.ts';

test('the notice: nothing when the module is on, where to check it, how to load the module', () => {
  assert.equal(remoteNotice({ enabled: true, format: 1, turnLimit: 30 }), '');
  assert.match(remoteNotice({ enabled: false, format: 1, turnLimit: 30 }), /SETTINGS > DIGIKIT > MODULES/);
  assert.match(remoteNotice(undefined), /Reading/);
  const noModule = new DkError({ error: 'remote state: E_PROTO', kind: 'device', status: 'E_PROTO' });
  assert.match(remoteNotice(undefined, noModule), /not loaded.*Transfer page/);
  const other = new DkError({ error: 'no reply from the device', kind: 'timeout' });
  assert.equal(remoteNotice(undefined, other), 'no reply from the device');
});

test('the device behind a session', () => {
  assert.equal(deviceOfPort('dt2'), 'dt2');
  assert.equal(deviceOfPort('dn2-1.12'), 'dn2');
  assert.equal(deviceOfPort('other'), undefined);
});

test('the runtime of the faceplate sends one request per key and turn', async () => {
  const calls: (string | number | boolean)[][] = [];
  const session = {
    remoteKey: async (code: number, down: boolean) => calls.push(['key', code, down]),
    remoteTurn: async (enc: number, d: number) => calls.push(['turn', enc, d]),
  };
  const rt = remoteRuntime(async () => session as unknown as DkSession);
  await rt.press(10);
  await rt.release(10);
  await rt.turn(3, -4);
  assert.deepEqual(calls, [
    ['key', 10, true],
    ['key', 10, false],
    ['turn', 3, -4],
  ]);
});

test('once the module is missing, REMOTE_STATE is asked again only when MODULES lists digikit/remote', () => {
  const noModule = new DkError({ error: 'remote state: E_PROTO', kind: 'device', status: 'E_PROTO' });
  assert.equal(isRemoteMissing(noModule), true);
  assert.equal(isRemoteMissing(new DkError({ error: 'no reply', kind: 'timeout' })), false);
  assert.equal(remoteStateWanted(false, undefined), true);
  assert.equal(remoteStateWanted(true, [{ name: 'test/crash' }]), false);
  assert.equal(remoteStateWanted(true, []), false);
  assert.equal(remoteStateWanted(true, [{ name: 'digikit/remote' }]), true);
  assert.equal(remoteStateWanted(true, undefined), true); // the list could not be read: ask as before
});

test('INFO only names the device: asked first, when the target changes, and every 30 s, not every poll', () => {
  assert.equal(infoDue(undefined, 1000, false), true);
  assert.equal(infoDue(1000, 3000, false), false);
  assert.equal(infoDue(1000, 3000, true), true);
  assert.equal(infoDue(1000, 1000 + INFO_EVERY_MS, false), true);
});

test('a Transfer job keeps the Remote page from cancelling its requests and from polling', () => {
  // leaving the page cancels what waits on the session only when no job writes to the device (a read is short and not worth keeping)
  assert.equal(leaveCancels(''), true);
  assert.equal(leaveCancels('Reading'), true);
  for (const job of ['Uploading', 'Installing', 'Deleting', 'Restarting']) {
    assert.equal(leaveCancels(job), false, job);
    assert.equal(remotePaused(job), true, job);
  }
  assert.equal(remotePaused(''), false);
  assert.equal(remotePaused('Reading'), false);
});
