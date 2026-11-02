import { execFile } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { promisify } from 'node:util';
import { repoRoot, type Session } from '@digi/test-support';
import { expect } from 'vitest';
import { Sample, prepareUpload } from '@digi/dk-session/dksample';
import { RpcFiles, putVerified, verifyWritten } from '@digi/dk-session/rpcfiles';
import type { PatchedSession } from '@digi/regression/support.ts';
import { liveArtifacts, liveCheck as check } from '@digi/regression/runtime/artifacts.ts';
import { sampleWav } from '@digi/regression/runtime/fixtures.ts';
import { liveProtocol } from '@digi/regression/runtime/protocol.ts';
import { expectRuntimeReady, runtimeTests } from '@digi/regression/runtime/ready.ts';
import { cardAfter } from '@digi/regression/runtime/write-safety.ts';
import { overlayArgs, plusdriveBinary } from '@digi/regression/runtime/card-kit.ts';

const exec = promisify(execFile);

/** Independent +Drive reader/builder oracle over the guest's card writes. No implicit builds. */
async function expectSampleCard(session: Session, out: string, hash: number): Promise<void> {
  const plusdrive = plusdriveBinary();
  if (!existsSync(plusdrive)) throw new Error('plusdrive CLI missing: run mise run build:plusdrive');
  const base = session.cardPath;
  if (!base) throw new Error('sample card oracle requires the loaded base card image');
  const overlay = join(out, 'samples.ov.bin');
  await session.client.call('card.overlay', { path: overlay });
  const after = cardAfter(base, overlay, join(out, 'samples-after'));
  const run = (args: string[]) =>
    exec(plusdrive, args, { cwd: repoRoot(), timeout: 30_000, encoding: 'buffer', maxBuffer: 4 * 1024 * 1024 });
  const got = (await run(['fs', 'cat', after, ...overlayArgs(after), '/live/test'])).stdout;
  const sent = readFileSync(join(out, 'sent-test.bin'));
  expect(got, 'the card holds sent bytes plus a 16-byte zero trailer').toEqual(Buffer.concat([sent, Buffer.alloc(16)]));
  const folder = join(out, 'built/plus_drive/dt2');
  mkdirSync(folder, { recursive: true });
  copyFileSync(join(out, 'test.wav'), join(folder, 'test.wav'));
  const built = join(out, 'built.img');
  await run(['build', folder, '-o', built, '--device', 'dt2', '--no-project']);
  const want = (await run(['fs', 'cat', built, '/test'])).stdout;
  expect(want.length).toBeGreaterThan(0x50);
  expect(got, 'the card equals the +Drive builder file for the same WAV').toEqual(want);
  const listing = (await run(['fs', 'ls', after, ...overlayArgs(after)])).stdout.toString();
  writeFileSync(join(out, 'card-listing.txt'), listing);
  const row = listing
    .split('\n')
    .find((line) => line.startsWith('live/test\t'))
    ?.split('\t');
  expect(row, 'card reader lists the uploaded sample').toHaveLength(3);
  expect(parseInt(row![2]!, 16), 'record hash|1 matches the firmware hash').toBe(hash);
  expect(Number(row![1]), 'record size includes the trailer').toBe(sent.length + 16);
  expect(
    listing.split('\n').some((line) => line.startsWith('live/notes.txt') || line.startsWith('live/renamed.txt')),
    'card keeps no deleted text file',
  ).toBe(false);
}

async function expectSamplesLive({ session }: PatchedSession): Promise<void> {
  await expectRuntimeReady(session);
  const out = liveArtifacts(session, 'samples-live');
  const wav = sampleWav(1);
  const same = (a: Uint8Array, b: Uint8Array) => Buffer.from(a).equals(Buffer.from(b));
  writeFileSync(join(out, 'test.wav'), wav);
  const sample = await Sample.create(
    Uint8Array.from(readFileSync(join(repoRoot(), 'apps/web/public/dksample.wasm'))),
  );
  const protocol = liveProtocol(session);
  try {
    const files = new RpcFiles(protocol, { chunk: 4096 });
    const hashOf = (b: Uint8Array) => sample.hash(b);

    const space = await files.space();
    check(
      'StorageSpace of the +Drive: free and total',
      space.free > 0n && space.total >= space.free,
      `${space.free} of ${space.total}`,
    );
    check('the folder is not there yet', !(await files.readDir('/')).some((e) => e.name === 'live'));

    // the conversion
    const prep = prepareUpload(sample, 'test.wav', wav);
    check(
      'a WAV is converted to a native sample named without .wav',
      prep.kind === 'converted' && prep.name === 'test',
      `${prep.kind} ${prep.name}`,
    );
    check(
      '44.1 kHz mono: 4410 frames become 4800, the file is header + PCM',
      prep.info?.srcFrames === 4410 && prep.info?.frames === 4800 && prep.bytes.length === 0x40 + 9600,
      JSON.stringify(prep.info),
    );
    const converted = sample.convert(wav);
    check(
      "what goes over MidiRpc is the card builder's file without its trailer",
      same(prep.bytes, converted.file.subarray(0, converted.file.length - 16)) &&
        converted.file.length === prep.bytes.length + 16,
    );
    writeFileSync(join(out, 'sent-test.bin'), prep.bytes);

    // the upload: three chunks, close with the byte count, the folder listing, the hash
    const progress: number[] = [];
    const { closed, verified } = await putVerified(files, 'live/test', prep.bytes, hashOf, (sent) =>
      progress.push(sent),
    );
    check(
      'the upload took three chunks',
      progress.length === 3 && progress[2] === prep.bytes.length,
      progress.join(' '),
    );
    check('the close answers the size without the trailer', closed.size === prep.bytes.length, JSON.stringify(closed));
    const listing = await files.readDir('/live');
    const entry = listing.find((e) => e.name === 'test');
    check(
      'ReadDir lists it as a file with the size sent',
      entry?.kind === 'F' && entry!.size === prep.bytes.length,
      JSON.stringify(entry),
    );
    check(
      'the firmware hash|1 equals the host hash over the bytes sent and a zero trailer',
      verified.ok && verified.firmware === verified.host && verified.firmware === entry?.hash,
      `${verified.firmware} ${verified.host}`,
    );
    writeFileSync(
      join(out, 'result.json'),
      JSON.stringify({ hash: verified.firmware, size: verified.size, path: '/live/test', compared: verified.compared }),
    );
    const info = await files.info('/live/test');
    check(
      'GetFileInfoV2 agrees with ReadDir',
      info.found && info.size === entry!.size && info.hash === entry!.hash,
      JSON.stringify(info),
    );

    // the host sees a mismatch: one byte of what it believes it sent is different
    const wrong = Uint8Array.from(prep.bytes);
    wrong[0x40 + 100] = wrong[0x40 + 100]! ^ 1;
    const bad = await verifyWritten(files, '/live/test', wrong, hashOf);
    check(
      'a different content hashes differently: verify reports failed',
      !bad.ok && bad.firmware !== bad.host && bad.size === wrong.length,
      `${bad.firmware} ${bad.host}`,
    );
    const missing = await verifyWritten(files, '/live/nothing', wrong, hashOf);
    check('a path the device does not have reports failed', !missing.ok && missing.firmware === null);

    // a native file passes through (its trailer is dropped: the firmware adds its own), any other file is unchanged
    const native = prepareUpload(sample, 'again', converted.file);
    check(
      'a native file passes through without its trailer',
      native.kind === 'native' && same(native.bytes, prep.bytes),
    );
    const text = new TextEncoder().encode('digikit notes\n');
    const plain = prepareUpload(sample, 'notes.txt', text);
    check(
      'a text file is not converted',
      plain.kind === 'unchanged' && plain.bytes === text && plain.name === 'notes.txt',
    );
    const second = await putVerified(files, 'live/notes.txt', plain.bytes, hashOf);
    check(
      'the text file is uploaded and its hash verifies',
      second.verified.ok && second.closed.size === text.length,
      JSON.stringify(second.verified),
    );

    // remove and rename
    await files.rename('/live/notes.txt', '/live/renamed.txt');
    check(
      'RenameFile moves it',
      (await files.readDir('/live')).some((e) => e.name === 'renamed.txt'),
    );
    await files.remove('/live/renamed.txt');
    check('DeleteFile removes it', !(await files.readDir('/live')).some((e) => e.name === 'renamed.txt'));
    const after = await files.space();
    check('StorageSpace shrank by the file', after.free < space.free, `${space.free} -> ${after.free}`);

    await expectSampleCard(session, out, verified.firmware!);
  } finally {
    protocol.cancelPending();
  }
}

runtimeTests('samples-live conversion, upload and persistent card bytes', expectSamplesLive, {
  devices: ['dt2'],
  mirror: true,
  timeout: 1_000_000,
});
