import fs from 'node:fs';
import path from 'node:path';
import { expect } from 'vitest';
import { parse } from 'smol-toml';
import { openSamples } from '@digi/dk-session/samples';
import { installModule, moduleArtifact } from '@digi/regression/runtime/install.ts';
import { deviceProfile } from '@digi/regression/runtime/device-data.ts';
import { expectRuntimeReady, runtimeTests } from '@digi/regression/runtime/ready.ts';
import { expectPollEmulated } from '@digi/regression/runtime/waits.ts';
import type { PatchedSession } from '@digi/regression/support.ts';

/**
 * The Storage contract (`contract.toml`) on the real adapter of each device. The host side is the `FakeStorage` test of the runtime crate
 * (`the_contract_table_holds_for_the_fake`). The only host-reachable call into `Storage` is the listing that `test/files-proof` starts through the files
 * service (commands 0x38 to 0x3a), so the rows with `device = "list"` run here; the others say why not. The tree of the table is uploaded first: the store
 * root by upload, the samples root by the device's own backend (the firmware file system on the DT2, the store on the DN2).
 */
const HERE = path.dirname(new URL(import.meta.url).pathname);
const ROOT_ID = { samples: 1, store: 2, other: 9 } as const; // DK_FILES_SAMPLES, DK_FILES_STORE, and one the core does not know
const STORE_PATH_MAX = 159; // dkrt_sdk::store::PATH_MAX
const DIR = 2;

type Root = keyof typeof ROOT_ID;
interface FileRow {
  root: Root;
  path: string;
  size: number;
  device?: string;
}
interface CaseRow {
  id: string;
  call: string;
  root: Root;
  path?: string;
  path_len?: string;
  expect: string;
  entries?: string;
  includes?: string;
  device?: string;
  devices?: string;
  device_pending?: string;
}
const contract = parse(fs.readFileSync(path.join(HERE, 'contract.toml'), 'utf8')) as unknown as {
  dt2_samples_trailer: number;
  file: FileRow[];
  case: CaseRow[];
};

/** The ramp every file of the table holds. */
const ramp = (n: number) => Uint8Array.from({ length: n }, (_, i) => (i * 31 + 7) & 255);

/** The error names of the table by the status the runtime gives. */
const ERRORS: Record<number, string> = { [-1]: 'E_NOT_FOUND', [-3]: 'E_READ', [-5]: 'E_FORMAT', [-13]: 'E_TOO_BIG', [-40]: 'E_SEL_PATH' };
const errorName = (status: number) => ERRORS[status] ?? `error ${status}`;

async function expectStorageContract({ session, dk }: PatchedSession): Promise<void> {
  const device = session.options.device;
  expect(['zero', 'none'], 'the listing runs on the ColdFire; no DSP is needed').toContain((await session.status()).dsp);
  await expectRuntimeReady(session);
  await installModule(dk, 'test/files-proof', moduleArtifact(device, 'test/files-proof'), { device });
  const profile = deviceProfile(device);
  const samplesMax = profile.digikit.sample_path_max;
  const dt2 = device === 'dt2';

  // the tree
  const samples = openSamples(dt2 ? 'stock-rpc' : 'digikit', { session: dk, hashOf: () => 0 });
  const made = new Set<string>();
  for (const f of contract.file.filter((row) => row.device !== 'no')) {
    const bytes = ramp(f.size);
    if (f.root === 'store') {
      await dk.put(f.path, bytes);
      continue;
    }
    const at = f.path.lastIndexOf('/');
    const folder = f.path.slice(0, at);
    const parts = folder.split('/');
    if (dt2)
      for (let n = 1; n <= parts.length; ++n) {
        const part = parts.slice(0, n).join('/');
        if (!made.has(part)) await samples.mkdir(part);
        made.add(part);
      }
    await samples.put(folder, { name: f.path.slice(at + 1), bytes, stored: bytes, kind: 'unchanged' });
  }

  // the listing through test/files-proof
  const request = async (cmd: number, body: Buffer) => {
    const reply = await dk.request(cmd, body);
    return { status: reply.status, body: Buffer.from(reply.body) };
  };
  const status = async () => {
    const reply = await request(0x39, Buffer.alloc(0));
    if (reply.status !== 0) return { ticket: 0, count: 0, terminal: 0 }; // busy while a callback runs
    return { ticket: reply.body.readUInt32BE(4), count: reply.body.readUInt32BE(8), terminal: reply.body.readInt32BE(12) };
  };
  /** The entries of a listing as `name:file:size` and `name:dir`, or the error name. */
  const listing = async (c: CaseRow, dir: string): Promise<string[] | string> => {
    const begin = await request(0x38, Buffer.concat([Buffer.from([ROOT_ID[c.root], 1]), Buffer.from(`${dir}\0`)]));
    if (begin.status !== 0) return errorName(begin.status);
    const ticket = begin.body.readUInt32BE(0);
    const end = await expectPollEmulated(
      session,
      async () => {
        const now = await status();
        return now.ticket === ticket ? now.terminal : 0;
      },
      (value) => expect(value, `${c.id}: the listing ends`).not.toBe(0),
      { emulatedMs: 45_000, intervalMs: 50, message: c.id },
    );
    if (end < 0) return errorName(end);
    const names: string[] = [];
    for (let index = 0, n = (await status()).count; index < n; ++index) {
      const raw = (await request(0x3a, Buffer.from([index]))).body;
      if (raw.readUInt32BE(0) !== ticket) continue;
      const row = raw.subarray(4);
      const kind = row.readUInt32BE(0);
      const size = row.readUInt32BE(8);
      const full = row.toString('utf8', 48, row.indexOf(0, 48));
      const name = full.slice(dir === '' ? 0 : dir.length + 1);
      names.push(kind === DIR ? `${name}:dir` : `${name}:file:${size}`);
    }
    return names;
  };
  /** The size that a listing of `root` reports for a file of `size` bytes. */
  const reported = (root: Root, size: number) => size + (dt2 && root === 'samples' ? contract.dt2_samples_trailer : 0);

  const ran: string[] = [];
  for (const c of contract.case) {
    expect(c.device !== undefined || c.device_pending !== undefined, `${c.id}: a device check or a reason`).toBe(true);
    if (c.device === undefined || (c.devices !== undefined && c.devices !== device)) continue;
    const limit = c.root === 'store' ? STORE_PATH_MAX : samplesMax;
    const dir =
      c.path_len === 'max+1' ? 'a'.repeat(limit + 1) : c.path_len === 'max' ? 'a'.repeat(limit) : (c.path ?? '');
    const got = await listing(c, dir);
    if (c.expect === 'list') {
      expect(Array.isArray(got), `${c.id}: a listing, not ${String(got)}`).toBe(true);
      const have = [...(got as string[])].sort();
      if (c.entries !== undefined) {
        const want = c.entries
          .split(',')
          .map((e) => {
            const [name, kind, size] = e.split(':');
            return kind === 'dir' ? e : `${name}:file:${reported(c.root, Number(size))}`;
          })
          .sort();
        expect(have, c.id).toEqual(want);
      } else for (const e of c.includes!.split(',')) expect(have, `${c.id}: ${e}`).toContain(e);
    } else expect(got, c.id).toBe(c.expect);
    ran.push(c.id);
  }
  expect(ran.length, 'the listing rows ran').toBeGreaterThanOrEqual(10);
  await session.expectNoFault('storage contract');
}

runtimeTests('storage contract: the listing rows run on the real adapter', expectStorageContract, { dsp: 'zero' });
