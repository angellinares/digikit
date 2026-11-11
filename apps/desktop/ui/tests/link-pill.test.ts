// The link pill of the Target row and the progress suffix (apps/web/src/transfer/link-core.ts), and the bar message that carries the suffix.
import assert from 'node:assert/strict';
import { test } from 'vitest';
import { linkTag, viaSuffix } from '../../../web/src/transfer/link-core.ts';
import { applyUpload, samplesBar, type Uploads } from '../src/samples-core.ts';

test('no pill and no suffix while the link is unknown', () => {
  assert.equal(linkTag(undefined), undefined);
  assert.equal(viaSuffix(undefined), '');
});

test('bulk is a green FAST USB pill, sysex a neutral SYSEX pill', () => {
  const fast = linkTag({ state: 'bulk' });
  assert.equal(fast?.text, 'FAST USB');
  assert.equal(fast?.tone, 'ok');
  assert.match(fast?.title ?? '', /USB bulk interface of the usb-fast module/);
  const sysex = linkTag({ state: 'sysex' });
  assert.equal(sysex?.text, 'SYSEX');
  assert.equal(sysex?.tone, 'dim');
  assert.match(sysex?.title ?? '', /install the usb-fast module/i);
  assert.doesNotMatch(sysex?.title ?? '', /USB CONFIG/);
  const taken = linkTag({ state: 'bulk', takenOver: true });
  assert.equal(taken?.text, 'FAST USB (USB audio paused)');
  assert.equal(taken?.tone, 'ok');
  assert.match(taken?.title ?? '', /USB audio is paused/);
});

test('a SysEx link with a usb-fast load failure names the module it needs', () => {
  const t = linkTag({ state: 'sysex', failure: { name: 'usb-fast.dkm', result: -7, needs: 'usb-bulk' } });
  assert.equal(t?.text, 'SYSEX: usb-fast not loaded (needs usb-bulk)');
  assert.equal(t?.tone, 'warn');
  assert.equal(
    t?.title,
    'usb-fast not loaded: this firmware has no core/usb-bulk. Flash firmware from the Firmware page.',
  );
});

test('a fallback is an amber pill whose tooltip names the logged reason', () => {
  const t = linkTag({ state: 'fallback', reason: 'the device did not answer' });
  assert.equal(t?.text, 'SYSEX: FALLBACK');
  assert.equal(t?.tone, 'warn');
  assert.match(
    t?.title ?? '',
    /stopped during this run \(the device did not answer\)\. The rest of the run went over SysEx\./,
  );
});

test('the suffix says Fast USB for bulk and SysEx for sysex and fallback', () => {
  assert.equal(viaSuffix({ state: 'bulk' }), ' · via Fast USB');
  assert.equal(viaSuffix({ state: 'sysex' }), ' · via SysEx');
  assert.equal(viaSuffix({ state: 'fallback', reason: 'x' }), ' · via SysEx');
});

test('the progress line of a running upload carries the suffix, and a finished run does not', () => {
  let u: Uploads = {};
  u = applyUpload(u, { event: 'queued', folder: '', name: 'kick.wav', bytes: 1000 });
  u = applyUpload(u, { event: 'progress', folder: '', name: 'kick.wav', sent: 10, bytes: 1000 });
  const run = (link?: Parameters<typeof samplesBar>[4]) => samplesBar(u, {}, true, undefined, link)?.message;
  assert.match(run({ state: 'bulk' }) ?? '', /^Uploading kick\.wav \(1 of 1\) · via Fast USB$/);
  assert.match(run({ state: 'fallback', reason: 'x' }) ?? '', / · via SysEx$/);
  assert.doesNotMatch(run() ?? '', /via/);
  assert.doesNotMatch(samplesBar(u, {}, false, undefined, { state: 'bulk' })?.message ?? '', /via/);
});
