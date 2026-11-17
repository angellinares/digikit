// The link pill of the Target row and the progress suffix (packages/workbench-core/src/transfer/link-core.ts), and the bar message that carries the suffix.
import assert from 'node:assert/strict';
import { test } from 'vitest';
import { linkHint, linkTag, viaSuffix } from '@digi/workbench-core/transfer/link-core.ts';
import { applyUpload, type Uploads } from '@digi/workbench-core/transfer/sample-upload.ts';
import { samplesBar } from '@digi/workbench-core/transfer/samples-core.ts';

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
  assert.doesNotMatch(fast?.title ?? '', /paused/);
});

test('a SysEx link in USB AUDIO/MIDI with no bulk route says to set USB CONFIG', () => {
  const t = linkTag({ state: 'sysex', wrongMode: true });
  assert.equal(t?.text, 'SYSEX');
  assert.equal(t?.tone, 'warn');
  assert.equal(t?.title, linkHint({ state: 'sysex', wrongMode: true }));
  assert.match(t?.title ?? '', /no Fast USB interface in USB AUDIO\/MIDI.*set USB CONFIG to USB MIDI/);
});

test('a SysEx link with a reason is an amber pill whose tooltip is the hint', () => {
  const link = { state: 'sysex', reason: 'USB device x: boom' } as const;
  const t = linkTag(link);
  assert.equal(t?.text, 'SYSEX');
  assert.equal(t?.tone, 'warn');
  assert.equal(t?.title, 'Fast USB not used: USB device x: boom');
  assert.equal(t?.title, linkHint(link));
});

test('linkHint gives one sentence for each way samples miss Fast USB', () => {
  assert.equal(linkHint(undefined), null);
  assert.equal(linkHint({ state: 'bulk' }), null);
  assert.equal(
    linkHint({ state: 'fallback', reason: 'timeout' }),
    'Fast USB stopped during this run (timeout); the rest went over SysEx.',
  );
  assert.match(linkHint({ state: 'fallback' }) ?? '', /\(no reason logged\)/);
  const failure = { name: 'usb-fast.dkm', result: -7, needs: 'usb-bulk' };
  assert.equal(linkHint({ state: 'sysex', failure }), linkTag({ state: 'sysex', failure })?.title);
  assert.match(linkHint({ state: 'sysex', wrongMode: true }) ?? '', /^This firmware has no Fast USB interface/);
  assert.equal(linkHint({ state: 'sysex', reason: 'r' }), 'Fast USB not used: r');
  assert.equal(linkHint({ state: 'sysex' }), 'Install the usb-fast module for Fast USB.');
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
  assert.match(run({ state: 'bulk' }) ?? '', /^Transferring 1 of 1 · kick\.wav · via Fast USB$/);
  assert.match(run({ state: 'fallback', reason: 'x' }) ?? '', / · via SysEx$/);
  assert.doesNotMatch(run() ?? '', /via/);
  assert.doesNotMatch(samplesBar(u, {}, false, undefined, { state: 'bulk' })?.message ?? '', /via/);
});
