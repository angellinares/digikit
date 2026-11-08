import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { Sample, prepareUpload } from '@digi/dk-session/dksample';
import { repoRoot } from '@digi/test-support';
import type { PatchedSession } from '@digi/regression/support.ts';
import { timePhase } from '@digi/regression/timing.ts';
import { liveArtifacts, liveCheck as check } from '@digi/regression/runtime/artifacts.ts';
import { sampleWav } from '@digi/regression/runtime/fixtures.ts';
import { liveProtocol } from '@digi/regression/runtime/protocol.ts';
import { expectRuntimeReady, runtimeTests } from '@digi/regression/runtime/ready.ts';
import { deviceProfile } from '@digi/regression/runtime/device-data.ts';
import { fileHashHex } from '@digi/dk-session';
import { openSamples } from '@digi/dk-session/samples';
import { expectRemoveTree, expectSkipOnUpload } from './samples-tree.ts';

async function expectSamplesStore({ session }: PatchedSession): Promise<void> {
  await expectRuntimeReady(session);
  const out = liveArtifacts(session, 'samples-store-live');
  const wav = sampleWav(2);
  const storage = 'digikit';
  const profile = deviceProfile('dn2');
  check('the device profile selects the digikit samples backend', profile.storage?.samples === 'digikit');
  const totalKib = Math.floor((profile.card.store.header_b_sector - profile.card.store.first_sector) / 2);
  const sample = await Sample.create(
    Uint8Array.from(readFileSync(join(repoRoot(), 'apps/web/public/dksample.wasm'))),
  );
  let protocol = liveProtocol(session);
  try {
    let dev = openSamples(storage, { session: protocol, hashOf: (b) => sample.hash(b) });
    check(`the backend for [storage] samples = "${storage}"`, dev.kind === storage, dev.kind);

    const prep = prepareUpload(sample, 'test.wav', wav);
    check(
      'a WAV is converted to the native sample file (header, 4800 stereo frames, trailer)',
      prep.kind === 'converted' && prep.name === 'test' && prep.stored.length === 0x40 + 19200 + 16,
      `${prep.kind} ${prep.name} ${prep.stored.length}`,
    );
    writeFileSync(join(out, 'stored-test.bin'), prep.stored);

    const space0 = await dev.space();
    check(
      'free space and total from the device',
      space0.free > 0 && space0.total === totalKib * 1024,
      JSON.stringify(space0),
    );
    check('the folder is not there yet', !(await dev.list('')).some((e) => e.name === 'drums'));
    check('folders can be made', await dev.canMkdir());
    await dev.mkdir('drums');
    check(
      'MKDIR: the empty folder is listed',
      (await dev.list('')).some((e) => e.name === 'drums' && e.kind === 'folder'),
    );

    const progress: [number, number][] = [];
    await dev.put('drums', prep, (sent, total) => progress.push([sent, total]));
    check(
      'the upload took several chunks and ended at the file size',
      progress.length >= 3 && progress.at(-1)![0] === prep.stored.length && progress.at(-1)![1] === prep.stored.length,
      progress.map((p) => p[0]).join(' '),
    );
    const listing = await dev.list('drums');
    check(
      'the folder lists the file with the size sent',
      listing.length === 1 &&
        listing[0]!.name === 'test' &&
        listing[0]!.kind === 'file' &&
        listing[0]!.size === prep.stored.length,
      JSON.stringify(listing),
    );
    check(
      'the folder entry went when a file was stored in it, the folder is still listed',
      (await dev.list('')).some((e) => e.name === 'drums' && e.kind === 'folder'),
    );
    const space1 = await dev.space();
    check('the file took space', space1.free < space0.free, `${space0.free} -> ${space1.free}`);

    const ok = await dev.verify('drums', prep);
    check(
      'VERIFY: the device hash of the stored file equals the host hash of the bytes sent',
      ok.ok &&
        ok.device === ok.host &&
        ok.message === 'device hash matches' &&
        ok.host === fileHashHex(prep.stored),
      JSON.stringify(ok),
    );
    const wrong = { ...prep, stored: Uint8Array.from(prep.stored) };
    wrong.stored[0x40 + 100] = wrong.stored[0x40 + 100]! ^ 1;
    const bad = await dev.verify('drums', wrong);
    check(
      'a different content reports failed',
      !bad.ok && bad.device === ok.host && bad.host !== ok.host && bad.message === 'device hash differs',
      JSON.stringify(bad),
    );
    const none = await dev.verify('drums', { ...prep, name: 'nothing' });
    check('a path the device does not have reports failed', !none.ok && none.device === null, none.message);

    await dev.remove('drums', 'test');
    check('the delete removes the file', (await dev.list('drums')).length === 0);
    const space2 = await dev.space();
    check(
      'the delete freed the space',
      space2.free === space0.free && space2.free > space1.free,
      `${space1.free} -> ${space2.free}`,
    );
    await dev.put('drums', prep);
    check('the file goes in again and verifies', (await dev.verify('drums', prep)).ok);
    check('the space is the same as after the first upload', (await dev.space()).free === space1.free);

    // the store survives a restart: the guest's card writes are kept, the runtime finds its store again
    await timePhase('protocol.restart', () => protocol.restart());
    protocol.cancelPending();
    protocol = liveProtocol(session);
    await timePhase('protocol.waitForBoot', () => protocol.waitForBoot());
    dev = openSamples(storage, { session: protocol, hashOf: (b) => sample.hash(b) });
    let again: Awaited<ReturnType<typeof dev.list>> | undefined;
    for (let i = 0; i < 40 && !again; i++) {
      try {
        again = await dev.list('drums');
      } catch {
        await timePhase('settle.sample-list-retry', () => new Promise((r) => setTimeout(r, 1000)));
      }
    }
    check(
      'after the restart the file is still listed',
      again?.length === 1 && again[0]!.name === 'test' && again[0]!.size === prep.stored.length,
      JSON.stringify(again),
    );
    const after = await dev.verify('drums', prep);
    check('after the restart VERIFY still matches', after.ok && after.device === after.host, JSON.stringify(after));
    check('after the restart the space is the same', (await dev.space()).free === space1.free);

    writeFileSync(
      join(out, 'result.json'),
      JSON.stringify({
        path: 'samples/drums/test',
        hash: after.host,
        size: prep.stored.length,
        compared: after.compared,
      }),
    );

    const synced = await session.client.call('card.sync');
    const mirrored = join(session.runDir, 'mirror/digikit/samples/drums/test');
    check(
      'the mirror holds the file sent to samples/drums/test',
      readFileSync(mirrored).equals(Buffer.from(prep.stored)),
      JSON.stringify(synced),
    );

    // removeTree on the real store (a scratch folder only)
    await expectRemoveTree(dev, (name) => prepareUpload(sample, `${name}.wav`, wav));
    await expectSkipOnUpload(dev, wav, (name, w) => prepareUpload(sample, `${name}.wav`, w));
  } finally {
    protocol.cancelPending();
  }
}

runtimeTests('samples-store-live conversion, upload and persistent card bytes', expectSamplesStore, { dsp: 'sharc',
  devices: ['dn2'],
  mirror: true,
  timeout: 1_000_000,
});
