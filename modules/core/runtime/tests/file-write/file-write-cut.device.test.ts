import fs from 'node:fs';
import path from 'node:path';
import { be32, type DkSession } from '@digi/dk-session';
import { describe, expect, test } from 'vitest';
import { blob, listing, pokeKnob, sha, type Files } from '@digi/regression/runtime/card-kit.ts';
import { cardRig, RT, type CardRig } from '@digi/regression/runtime/card-rig.ts';
import { advance, word } from '@digi/regression/runtime/fx-live.ts';
import { planSymbol } from '@digi/regression/runtime/runtime-plan.ts';
import { DEVICES, FIRMWARE } from '@digi/regression/runtime/devices.ts';
import { deviceListing } from '@digi/regression/runtime/upload-kit.ts';

/**
 * Power cuts during an SDK file write (tests/fixtures/modules/file-write), the way upload-faults.device.test.ts does for SysEx. The job writes
 * `data/sdk-cut.bin` (two 4096-byte chunks and a 777-byte tail: three data writes), then file_close commits (table, header B, header A). The first write is the repair of
 * header A that BEGIN does on a card built by plusdrive (`sync_headers`): seven card writes in all. `DKRT_UPLOAD_CUT` = N refuses every write after the Nth; `DKRT_UPLOAD_TEAR` = N tears the Nth (one sector) and refuses the rest. After
 * each fault the next boot reads a store that is the old one or the new one, never a broken one, and the store takes another SDK write and a
 * SysEx upload.
 */

const CMD = 0x24;
const WRITE = 1;
const SIZE = 2 * 4096 + 777;
const PATH = 'data/sdk-cut.bin';
const pattern = (n: number) => Buffer.from(Uint8Array.from({ length: n }, (_, i) => (i * 7 + (i >> 8)) & 255));
const cstr = (s: string) => Buffer.concat([Buffer.from(s), Buffer.from([0])]);

const cases = [
  ['a cut after the first write (the repair of header A)', { DKRT_UPLOAD_CUT: 1 }, false],
  ['a tear of the tail data sector', { DKRT_UPLOAD_TEAR: 4 }, false],
  ['a cut after the last data write (the table is refused)', { DKRT_UPLOAD_CUT: 4 }, false],
  ['a tear of header B', { DKRT_UPLOAD_TEAR: 6 }, false],
  ['a cut after header B (header A is refused)', { DKRT_UPLOAD_CUT: 6 }, true],
  ['no cut: header A is written', { DKRT_UPLOAD_CUT: 7 }, true],
] as const;

/** Start a write job of the fixture and run the emulator until it has ended; returns the job's status and the close result. */
async function runJob(rig: CardRig, dk: DkSession, size: number, file: string): Promise<{ job: number; close: number }> {
  const r = await dk.request(CMD, Buffer.concat([Buffer.from([WRITE]), be32(size), cstr(file)]));
  expect(r.status, 'the job is queued').toBe(0);
  for (let i = 0; i < 200; i++) {
    await advance(rig.session, 100);
    const b = Buffer.from((await dk.request(CMD + 1)).body);
    if (b[0] === 2) return { job: b.readInt32BE(13), close: b.readInt32BE(5) };
  }
  throw new Error('the job did not end');
}

for (const device of DEVICES) {
  describe(`${device.toUpperCase()} ${FIRMWARE[device]} SDK file write: power cuts`, () => {
    const rig = cardRig(device, 'file-write-cut', {
      dsp: 'zero',
      roots: RT,
      symbols: ['DKRT_UPLOAD_CUT', 'DKRT_UPLOAD_TEAR', 'DKRT_UPLOAD_WRITES'],
      project: true,
      uploads: true,
      relaxFs: true,
    });

    test('the store after a cut or tear is the old or the new state, and takes another SDK write and a SysEx upload', async () => {
      const old = blob(3000, 21);
      const oldFile = path.join(rig.artifacts, 'old.dkm');
      fs.writeFileSync(oldFile, old);
      const mod = rig.mod('test/file-write');
      const moduleBytes = fs.readFileSync(mod.file);
      const base = await rig.card({
        name: 'cut-base',
        modules: [mod, { name: 'modules/old.dkm', file: oldFile }],
        selection: [mod.name],
      });
      const oldFiles: Files = { 'modules/old.dkm': old, [mod.name]: moduleBytes };
      const data = pattern(SIZE);
      for (const [k, [label, knobs, committed]] of cases.entries()) {
        const dk = await rig.pausedBoot(base);
        for (const [name, value] of Object.entries(knobs)) await pokeKnob(rig.session, rig.plan, name, value);
        const done = await runJob(rig, dk, SIZE, PATH);
        const writes = await word(rig.session, planSymbol(rig.plan, 'DKRT_UPLOAD_WRITES'));
        const whole = 'DKRT_UPLOAD_CUT' in knobs && knobs.DKRT_UPLOAD_CUT >= 7;
        expect(done.job === 0, `${label}: the job ${whole ? 'ends ok' : 'is refused a write'} (status ${done.job}, ${writes} card writes counted)`).toBe(whole);
        const card = await rig.after(`sdkcut${k}`);

        const want: Files = committed ? { ...oldFiles, [PATH]: data } : oldFiles;
        const next = await rig.pausedBoot(card);
        expect(await deviceListing(next), `${label}: the next boot reads the store, whole`).toEqual(listing(want));
        const view = await rig.view(`sdkcut${k}-v`);
        expect(listing(view.files), `${label}: plusdrive reads the same from the card ${view.head}`).toEqual(listing(want));
        if (committed) expect(await next.verify(PATH), `${label}: VERIFY of the committed file`).toBe(sha(data));

        // the store still takes an SDK write (the path of a failed one is free again) and a SysEx upload
        const again = await runJob(rig, next, 5000, 'data/after.bin');
        expect([again.job, again.close], `${label}: the next SDK write works`).toEqual([0, 0]);
        const more = blob(700, 23);
        await next.put('modules/more.dkm', more);
        const files: Files = { ...want, 'data/after.bin': pattern(5000), 'modules/more.dkm': more };
        const after = await rig.view(`sdkcut${k}-w`);
        expect(listing(after.files), `${label}: both writes are on the card ${after.head}`).toEqual(listing(files));
      }
    }, 3_600_000);
  });
}
