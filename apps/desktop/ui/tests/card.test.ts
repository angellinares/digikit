import type { CardFolder } from '../src/card-core.ts';
// The pure logic of the desktop Card panel (apps/desktop/ui/src/card-core.ts).
import assert from 'node:assert/strict';
import { test } from 'vitest';
import {
  agoText,
  canUseDefault,
  fileRows,
  folderNotice,
  pickStart,
  shortFolder,
  statusText,
} from '../src/card-core.ts';

const card = (extra: Partial<CardFolder> = {}): CardFolder => ({
  folder: '/Users/em/src/digi/digitakt2/plus_drive/dt2',
  continuous: true,
  files: [
    { path: 'modules/sine.dkm', bytes: 932, hash: 'bb' },
    { path: 'modules/pluck.dkm', bytes: 2260, hash: 'aa' },
  ],
  last_sync: 1_000_000,
  behind: false,
  error: null,
  ...extra,
});

test('the time of the last sync', () => {
  assert.equal(agoText(null, 5000), 'never');
  assert.equal(agoText(1000, 1500), 'just now');
  assert.equal(agoText(0, 12_000), '12 s ago');
  assert.equal(agoText(0, 3 * 60_000 + 5000), '3 min ago');
  assert.equal(agoText(0, 2 * 3600_000 + 5000), '2 h ago');
  assert.equal(agoText(5000, 1000), 'just now'); // a clock step back is not a negative age
});

test('the status line: last sync, pending changes, an error, no mirror', () => {
  const now = 1_012_000;
  assert.equal(statusText(card(), now), 'Last sync 12 s ago');
  assert.equal(statusText(card({ behind: true }), now), 'Last sync 12 s ago; the card has changes not synced yet');
  assert.equal(statusText(card({ last_sync: null }), now), 'Last sync never');
  assert.equal(
    statusText(card({ error: 'module store: table checksum' }), now),
    'Sync failed: module store: table checksum',
  );
  assert.equal(statusText(card({ continuous: false }), now), 'The card is not mirrored to a folder.');
});

test('the folder and the files', () => {
  assert.equal(shortFolder('/Users/em/src/digi/digitakt2/plus_drive/dt2'), '…/plus_drive/dt2');
  assert.equal(shortFolder('plus_drive/dn2'), 'plus_drive/dn2');
  assert.equal(shortFolder(null), '');
  assert.deepEqual(
    fileRows(card()).map((f) => f.path),
    ['modules/pluck.dkm', 'modules/sine.dkm'],
  );
});

const DT2 = '/Users/em/src/digi/digitakt2/plus_drive/dt2';

test('a folder that waits for a reload says so; a missing choice says the default is used', () => {
  assert.equal(folderNotice(card({ device: 'dt2', reload_needed: false, next_folder: DT2 })), '');
  assert.equal(folderNotice(card({})), '', 'an older controller sends none of the fields');
  const waiting = card({ device: 'dt2', reload_needed: true, next_folder: '/Users/em/Music/Card' });
  assert.equal(
    folderNotice(waiting),
    'The card is built from its folder when it loads. A reload makes it use …/Music/Card; until then changes go to …/plus_drive/dt2.',
  );
  assert.equal(
    folderNotice(card({ chosen_missing: '/Volumes/Gone/card' })),
    'The folder you chose, …/Gone/card, is missing. The default folder is used.',
  );
});

test('"Use default" is for a saved choice; the dialog starts where the card goes next', () => {
  assert.equal(canUseDefault(card({ custom: false })), false);
  assert.equal(canUseDefault(card({ custom: true })), true);
  assert.equal(canUseDefault(card({ custom: false, chosen_missing: '/x/y' })), true);
  assert.equal(pickStart(card({ next_folder: '/a/b' })), '/a/b');
  assert.equal(pickStart(card()), DT2);
  assert.equal(pickStart(card({ folder: null })), null);
});
