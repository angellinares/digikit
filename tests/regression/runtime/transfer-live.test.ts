import fs from 'node:fs';
import path from 'node:path';
import { hexSha256 } from '@digi/dk-session';
import { LOG_KIND } from '@digi/dk-session/gen_proto';
import { expect } from 'vitest';
import type { PatchedSession } from '../support.ts';
import { DEBUG_MODULE, expectRuntimeReady, liveArtifacts, record, runtimeTests } from './live.ts';
import { cliInstall, transferCommand } from './transfer-cli.ts';

interface FolderResult {
  folder: string;
  continuous: boolean;
  files: { path: string; size: number; sha256: string }[];
}

/** CLI upload, persistence, folder mirroring and the selected-but-missing module boot. */
async function expectTransferLive({ session, dk }: PatchedSession): Promise<void> {
  const artifacts = liveArtifacts(session, 'transfer-live');
  const mirror = path.join(session.runDir, 'mirror');
  const modulePath = 'modules/sine.dkm';
  // the debug module is installed next to it: the log of the last boot is read at the end
  const debugPath = 'modules/debug.dkm';
  const mirrored = path.join(mirror, 'digikit', modulePath);
  const sha256 = async () => (fs.existsSync(mirrored) ? await hexSha256(fs.readFileSync(mirrored)) : null);

  await expectRuntimeReady(session);
  const epoch = (await session.status()).epoch;
  if (typeof epoch !== 'number') throw new Error('daemon status must report a numeric epoch');
  await cliInstall(session, 'test/sine', artifacts, 300_000, [DEBUG_MODULE]);
  expect((await session.status()).epoch, 'CLI RESTART boots another epoch').toBeGreaterThan(epoch);
  const entries = await dk.list();
  record(artifacts, 'installed-store', entries);
  expect(entries.map((entry) => entry.path).sort(), 'upload survives the soft reset').toEqual(
    [debugPath, modulePath].sort(),
  );
  const entry = entries.find((e) => e.path === modulePath)!;

  // The store follows the guest's card writes after its quiet period (about two seconds).
  await expect
    .poll(sha256, { timeout: 30_000, interval: 500, message: `mirror SHA-256: ${mirrored}` })
    .toBe(entry.sha256);
  const folder = await session.client.call<FolderResult>('card.folder');
  record(artifacts, 'installed-folder', folder);
  expect(folder.folder, 'card.folder reports the session-owned mirror').toBe(mirror);
  expect(folder.continuous, 'card.folder continuously mirrors the store').toBe(true);
  expect(
    folder.files.map(({ path, sha256 }) => ({ path, sha256 })).sort((x, y) => x.path.localeCompare(y.path)),
    'mirror metadata matches the card',
  ).toEqual([
    { path: debugPath, sha256: entries.find((e) => e.path === debugPath)!.sha256 },
    { path: modulePath, sha256: entry.sha256 },
  ]);

  const deleted = await transferCommand(session, ['delete', modulePath], path.join(artifacts, 'delete.log'), 300_000);
  expect(
    deleted.some((event) => event.event === 'delete'),
    'CLI reports deletion',
  ).toBe(true);
  const emptyStore = await dk.list();
  record(artifacts, 'deleted-store', emptyStore);
  expect(
    emptyStore.map((e) => e.path),
    'CLI delete removes the module and leaves the debug module',
  ).toEqual([debugPath]);
  const synced = await session.client.call<FolderResult>('card.sync');
  record(artifacts, 'deleted-folder', synced);
  expect(fs.existsSync(mirrored), 'card.sync removes the deleted module from the folder').toBe(false);
  expect(
    synced.files.map((f) => f.path),
    'card.sync reports the debug module only',
  ).toEqual([debugPath]);

  // DELETE keeps the selection. Exactly one more protocol restart preserves the original
  // boot-loop history: another early restart could enter safe mode instead of trying the path.
  await transferCommand(session, ['restart'], path.join(artifacts, 'restart.log'), 300_000);
  const events = await dk.readLog();
  record(artifacts, 'missing-module-boot', events);
  expect(
    events.some((event) => event.kind === LOG_KIND.MOD_LOAD && event.args[1] === 0xffffffff),
    'selected missing path logs MOD_LOAD E_NOT_FOUND; see missing-module-boot.json',
  ).toBe(true);
  expect(
    events.some((event) => event.kind === LOG_KIND.UI_FIRST),
    'boot reaches UI_FIRST after the missing module',
  ).toBe(true);
  const modules = await dk.modules();
  record(artifacts, 'missing-module-loaded', modules);
  expect(
    modules.map((module) => module.name),
    'boot skips the missing selected module',
  ).toEqual([DEBUG_MODULE]);
}

runtimeTests('CLI transfer survives restart, mirrors the store and skips a deleted selection', expectTransferLive, {
  mirror: true,
  timeout: 1_000_000,
});
