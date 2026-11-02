import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { promisify } from 'node:util';
import { bytesOfHex, concat, be32, hexOf, type DkEntry, type DkSession } from '@digi/dk-session';
import { repoRoot, resolveInputs, type Device, type Session } from '@digi/test-support';
import { parse } from 'smol-toml';
import { describe, expect, test } from 'vitest';
import { CMD as GCMD } from '../../../packages/dk-session/src/gen_proto.ts';
import { listing, plusdriveBinary, storeList, type Files, type Listing } from './card-kit.ts';
import { cardRig, type CardRig } from './card-rig.ts';
import { bounded, rpc } from './fx-live.ts';
import { DEVICES, FIRMWARE, FULL_TIER } from './live.ts';
import { overlayRecords } from './write-safety.ts';

/**
 * Helpers of the module-upload device tests (tools/patchkit/pk_digikit_upload.py): the upload protocol bodies, the device's LIST against the card as the
 * plusdrive reader sees it, raw USB MIDI, and the upload rules of the write safety (`common_checks_upload`). The card fixture is card-rig.ts.
 */

const exec = promisify(execFile);
export { GCMD };

export const shaBytes = (b: Uint8Array): Uint8Array => createHash('sha256').update(b).digest();
const text = new TextEncoder();
/** A NUL-terminated path. */
export const cstr = (p: string): Uint8Array => concat(text.encode(p), Uint8Array.of(0));
export const beginBody = (p: string, size: number, hash: Uint8Array = new Uint8Array(32)): Uint8Array =>
  concat(cstr(p), be32(size), hash);
export const dataBody = (offset: number, part: Uint8Array): Uint8Array => concat(be32(offset), part);

/** The runtime version of sdk/sdk.toml, packed major << 16 | minor << 8 | patch (INFO's `runtime`). */
export function runtimeVersion(): number {
  const toml = parse(fs.readFileSync(path.join(repoRoot(), 'sdk/sdk.toml'), 'utf8')) as {
    runtime: { version: string };
  };
  const [major, minor, patch] = toml.runtime.version.split('.').map(Number);
  return (major! << 16) | (minor! << 8) | patch!;
}

// ---- files, the device's LIST and the card ------------------------------------------------------------------------------------------

export const deviceListing = async (dk: DkSession, prefix?: string): Promise<Listing> =>
  Object.fromEntries((await dk.list(prefix)).map((e: DkEntry) => [e.path, [e.size, e.sha256]]));
export const clearFiles = (files: Files): void => {
  for (const p of Object.keys(files)) delete files[p];
};

/** BEGIN, DATA and END with the given hash in BEGIN; the first status that is not 0. */
export async function putWithHash(rig: CardRig, storePath: string, data: Buffer, hash: Uint8Array): Promise<number> {
  let r = await rig.request(GCMD.BEGIN, beginBody(storePath, data.length, hash));
  for (let off = 0; r.status === 0 && off < data.length; off += 32768)
    r = await rig.request(GCMD.DATA, dataBody(off, data.subarray(off, off + 32768)));
  return r.status === 0 ? (await rig.request(GCMD.END)).status : r.status;
}

async function run(file: string, args: string[]) {
  try {
    const r = await exec(file, args, { cwd: repoRoot(), maxBuffer: 64 << 20 });
    return { code: 0, stdout: r.stdout, stderr: r.stderr };
  } catch (e) {
    const x = e as { code?: number; stdout?: string; stderr?: string };
    return { code: typeof x.code === 'number' ? x.code : 1, stdout: x.stdout ?? '', stderr: x.stderr ?? String(e) };
  }
}

const walk = (dir: string, base = dir): Files =>
  Object.assign(
    {},
    ...fs
      .readdirSync(dir, { withFileTypes: true })
      .map((e) =>
        e.isDirectory()
          ? walk(path.join(dir, e.name), base)
          : { [path.relative(base, path.join(dir, e.name))]: fs.readFileSync(path.join(dir, e.name)) },
      ),
  );

// ---- raw USB MIDI: the messages that are not ours ------------------------------------------------------------------------------------

/** Hand one message to the emulated USB input (high-speed delivery) and run 16 M ColdFire instructions (Python: 40 steps of 400,000). */
export async function usbSend(session: Session, msg: Uint8Array): Promise<void> {
  await rpc(session, 'midi.usb.in', { bytes: hexOf(msg), chunk: 384, period: 16_500 });
  await bounded(() => session.client.step(16_000_000, { timeout_wall_s: 120 }));
}
/** Everything the firmware sent since the last take. */
export async function usbTake(session: Session): Promise<Uint8Array> {
  const { hex } = await rpc<{ hex?: string }>(session, 'midi.usb.out');
  return bytesOfHex(hex ?? '');
}

// ---- the tests ----------------------------------------------------------------------------------------------------------------------

/**
 * One test per device on its own card rig (ColdFire-only daemon unless `sharc`; cards seeded with the stock project; uploads allowed). `symbols`:
 * runtime variables the test reads or sets (`DKRT_BOOT` is always planned). The writes of every case are limited to the module store and the
 * firmware's own project sectors (`common_checks_upload`).
 */
export function uploadTests(
  name: string,
  body: (rig: CardRig) => Promise<void>,
  opts: {
    devices?: readonly Device[];
    full?: boolean;
    symbols?: string[];
    sharc?: boolean;
    timeout?: number;
    stallMs?: number;
    /** Skip with this reason (the Python scenario still covers it); see docs/plans/python-test-triage.md. */
    skip?: string;
  } = {},
): void {
  for (const device of opts.devices ?? DEVICES) {
    describe.skipIf(opts.skip !== undefined || (opts.full === true && !FULL_TIER))(
      `${device.toUpperCase()} ${FIRMWARE[device]}`,
      () => {
        const rig = cardRig(device, name, {
          symbols: opts.symbols,
          dsp: opts.sharc ? 'sharc' : 'zero',
          project: true,
          uploads: true,
          stallMs: opts.stallMs,
          onClose: (r, report) => {
            expect(report.emulators).toBeGreaterThan(0);
            const { first, end, firmwareFirst } = r.store;
            const written = new Set(
              r.overlays.flatMap((f) => Array.from(overlayRecords(fs.readFileSync(f)), (rec) => rec.sector)),
            );
            const outside = [...written]
              .filter((s) => !(first <= s && s < end) && s < firmwareFirst)
              .sort((a, b) => a - b);
            expect(
              outside.map((s) => `0x${s.toString(16)}`),
              'card writes: the module store range and the firmware project sectors',
            ).toEqual([]);
          },
        });
        test(
          name,
          async () => {
            await body(rig);
          },
          opts.timeout ?? 1_800_000,
        );
      },
    );
  }
}

// ---- the +Drive round trip ----------------------------------------------------------------------------------------------------------

/** `plusdrive export` of the card with the guest's writes writes the store's files to `digikit/` of a folder; `plusdrive build` of that folder gives a card whose store holds the same files. */
export async function roundtrip(rig: CardRig, files: Files): Promise<void> {
  const overlay = path.join(rig.artifacts, 'roundtrip.overlay.bin');
  await rpc(rig.session, 'card.overlay', { path: overlay });
  const top = path.join(rig.artifacts, 'roundtrip');
  const folder = path.join(top, 'plus_drive', rig.device);
  fs.rmSync(top, { recursive: true, force: true });
  fs.mkdirSync(folder, { recursive: true });
  const exportArgs = [
    'export',
    rig.cardImage,
    '--overlay',
    overlay,
    '-o',
    folder,
    '--out',
    path.join(top, 'out'),
    '--device',
    rig.device,
  ];
  const first = await run(plusdriveBinary(), exportArgs);
  const digikit = path.join(folder, 'digikit');
  const got = fs.existsSync(digikit) ? walk(digikit) : {};
  delete got['selection.txt'];
  expect(first.code, `+Drive round trip: the export: ${first.stderr.slice(-200)}`).toBe(0);
  expect(listing(got), '+Drive round trip: the export wrote the store files to digikit/').toEqual(listing(files));
  const second = await run(plusdriveBinary(), exportArgs);
  expect(second.code, second.stderr.slice(-200)).toBe(0);
  expect(second.stdout, '+Drive round trip: a second export adds, replaces and keeps nothing').toContain(
    'added 0 replaced 0',
  );
  expect(second.stdout).toContain('kept 0');
  const built = path.join(top, 'card.img');
  const syx = resolveInputs(rig.device, FIRMWARE[rig.device]).syx;
  const build = await run(plusdriveBinary(), ['build', folder, '-o', built, '--device', rig.device, '--syx', syx]);
  expect(build.code, build.stderr.slice(-200)).toBe(0);
  const names = (await storeList(built, rig.device)).entries.map((e) => e.path).sort();
  expect(names, '+Drive round trip: the next card build holds the uploaded files').toEqual(Object.keys(files).sort());
}
