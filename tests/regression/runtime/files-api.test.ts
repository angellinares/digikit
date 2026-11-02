import { execFile } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { promisify } from 'node:util';
import { RpcFiles } from '@digi/dk-session/rpcfiles';
import { repoRoot, resolveInputs } from '@digi/test-support';
import { describe, expect, test } from 'vitest';
import { withSession } from '../support.ts';
import { moduleArtifact } from './install.ts';
import { liveProtocol } from './protocol.ts';
import type { RuntimePlan } from './runtime-plan.ts';

const root = repoRoot();

/** `size_of::<Api>()`: every field of the `[api]` table in sdk/sdk.toml is one 4-byte word. */
function apiTableSize(): number {
  const toml = fs.readFileSync(path.join(root, 'sdk/sdk.toml'), 'utf8');
  const block = toml.slice(toml.indexOf('[api]\nfields = ['), toml.indexOf('\n]\n', toml.indexOf('[api]\nfields = [')));
  return 4 * (block.match(/^\s*\{ name = /gm) ?? []).length;
}
const exec = promisify(execFile);
const sha = (bytes: Uint8Array) => createHash('sha256').update(bytes).digest('hex');
const profiles = [
  {
    device: 'dt2',
    firmware: '1.17',
    binary: process.env.DIGI_FILES_EMUD_DT2,
    patched: process.env.DIGI_FILES_PATCH_DT2,
    plan: process.env.DIGI_FILES_PLAN_DT2,
    card: process.env.DIGI_FILES_CARD_DT2,
  },
  {
    device: 'dn2',
    firmware: '1.12',
    binary: process.env.DIGI_FILES_EMUD_DN2,
    patched: process.env.DIGI_FILES_PATCH_DN2,
    plan: process.env.DIGI_FILES_PLAN_DN2,
    card: undefined,
  },
] as const;
const be32 = (v: number) => {
  const b = Buffer.alloc(4);
  b.writeUInt32BE(v);
  return b;
};
function decode(bytes: Buffer) {
  expect(bytes.length).toBe(304);
  const end = bytes.indexOf(0, 48);
  expect(end).toBeGreaterThanOrEqual(48);
  return {
    kind: bytes.readUInt32BE(0),
    flags: bytes.readUInt32BE(4),
    size: bytes.readUInt32BE(8),
    hashKind: bytes.readUInt32BE(12),
    hash: bytes.subarray(16, 48).toString('hex'),
    path: bytes.toString('utf8', 48, end),
  };
}

for (const profile of profiles) {
  const enabled = Boolean(profile.binary);
  describe.skipIf(!enabled)(`${profile.device} public files API1.4`, () => {
    const runDir = path.join(root, 'out/runs/files-api', profile.device, randomUUID());
    let plan: RuntimePlan | undefined;
    let artifact: string | undefined;
    if (enabled) {
      for (const [label, value] of Object.entries({
        binary: profile.binary,
        patched: profile.patched,
        plan: profile.plan,
        patchkit: process.env.DIGI_PATCHKIT_BIN,
        ...(profile.device === 'dt2' ? { card: profile.card } : {}),
      })) {
        if (!value || !path.isAbsolute(value) || !fs.existsSync(value))
          throw new Error(`files API preflight requires prepared absolute ${label}: ${value}`);
      }
      plan = JSON.parse(fs.readFileSync(profile.plan!, 'utf8')) as RuntimePlan;
      if (sha(fs.readFileSync(profile.patched!)) !== plan.output?.syx_sha256)
        throw new Error('prepared listing patch/plan SHA mismatch');
      artifact = moduleArtifact(profile.device, 'test/files-proof');
      const dkm = fs.readFileSync(artifact);
      if (dkm.readUInt32BE(0x8c) !== 0) throw new Error('files-proof must use no slots; callbacks work without slot0');
      if (dkm[0x38] !== 1 || dkm[0x39] !== 4 || dkm.readUInt32BE(0x30) !== 0x504)
        throw new Error('prepare files-proof with API1.4/runtime0.5.4 patchkit');
      fs.mkdirSync(runDir, { recursive: true });
    }
    const handle = withSession({
      runDir,
      device: profile.device,
      firmware: profile.firmware,
      binary: profile.binary,
      dsp: 'sharc',
      restart: false,
    });
    test('lists both roots with typed metadata, owned rows and deferred cancellation without slot0', async () => {
      const session = handle.session;
      const save = (name: string, value: unknown) =>
        fs.writeFileSync(path.join(runDir, name), JSON.stringify(value, null, 2));
      let protocol: ReturnType<typeof liveProtocol> | undefined;
      let phase = 'prepared object preflight';
      try {
        // This proof applies no patch and builds nothing. Both SYX/plan and DKM
        // must already exist; also reject stale local runtime/dependency objects.
        const checked = await exec(
          process.env.DIGI_PATCHKIT_BIN!,
          [
            '--root',
            root,
            'build',
            'modules/core/runtime',
            'modules/core/expansion',
            'tests/fixtures/modules/files-proof',
            '--check',
          ],
          { timeout: 30000 },
        );
        fs.writeFileSync(path.join(runDir, 'object-check.log'), checked.stdout + checked.stderr);
        const inputs = resolveInputs(profile.device, profile.firmware);
        expect(plan!.base).toMatchObject({
          device: profile.device,
          version: profile.firmware,
          syx_sha256: sha(fs.readFileSync(inputs.syx)),
        });
        expect(plan!.modules.some((id) => id.split('@')[0] === 'core/runtime')).toBe(true);
        save('inputs.json', {
          profile,
          inputs,
          artifact,
          dkmSha: sha(fs.readFileSync(artifact!)),
          stockLoad: session.loadResult,
        });
        phase = 'patched startup';
        let timer: ReturnType<typeof setTimeout> | undefined;
        try {
          const load = await Promise.race([
            session.client.firmwareLoad({
              syx: profile.patched!,
              base_sha256: plan!.base.syx_sha256,
              start: 'auto',
              auto_no: true,
              ...(profile.card ? { card: profile.card } : {}),
            }),
            new Promise<never>((_, reject) => {
              timer = setTimeout(() => reject(new Error('files API startup exceeded120s')), 120000);
            }),
          ]);
          expect(load.syx_sha256).toBe(plan!.output?.syx_sha256);
          save('patched-load.json', load);
        } finally {
          clearTimeout(timer);
        }
        await session.client.call('run.mode', { mode: 'free' });
        await expect.poll(async () => (await session.status()).cf.ready, { timeout: 30000 }).toBe(true);
        protocol = liveProtocol(session);
        const fixture = fs.readFileSync(artifact!);
        await protocol.put('modules/files-proof.dkm', fixture);
        expect(await protocol.verifyFile('modules/files-proof.dkm', fixture)).toBe(true);
        await protocol.select(['modules/files-proof.dkm']);
        await protocol.restart();
        protocol.cancelPending();
        protocol = liveProtocol(session);
        await protocol.waitForBoot();
        await expect
          .poll(async () => (await protocol!.modules()).map((m) => m.name), { timeout: 30000 })
          .toContain('test/files-proof');
        const request = async (cmd: number, body = Buffer.alloc(0)) => {
          const reply = await protocol!.request(cmd, body);
          expect(reply.status, `public fixture command0x${cmd.toString(16)}`).toBe(0);
          return Buffer.from(reply.body);
        };
        const status = async () => {
          const reply = await protocol!.request(0x39);
          if (reply.status === -12) return { client: 0, ticket: 0, count: 0, terminal: 0, callbacks: 0, hint: 0 };
          expect(reply.status).toBe(0);
          const b = Buffer.from(reply.body);
          return {
            client: b.readUInt32BE(0),
            ticket: b.readUInt32BE(4),
            count: b.readUInt32BE(8),
            terminal: b.readInt32BE(12),
            callbacks: b.readUInt32BE(16),
            hint: b.readInt32BE(20),
          };
        };
        const begin = async (rootId: number, directory: string, mode = 1) =>
          (
            await request(0x38, Buffer.concat([Buffer.from([rootId, mode]), Buffer.from(`${directory}\0`)]))
          ).readUInt32BE();
        const poll = async (ticket: number, invalid = false) => {
          const body = await request(0x3c, Buffer.concat([be32(ticket), Buffer.from([invalid ? 1 : 0])]));
          return {
            rc: body.readInt32BE(0),
            untouched: body.readUInt32BE(4),
            entry: body.length > 8 ? decode(body.subarray(8)) : undefined,
          };
        };
        const rows = async (ticket: number) => {
          const observed = await status();
          const all = [];
          for (let index = 0; index < observed.count; ++index) {
            const raw = await request(0x3a, Buffer.from([index]));
            if (raw.readUInt32BE(0) === ticket)
              all.push({ index, raw: raw.toString('hex'), entry: decode(raw.subarray(4)) });
          }
          return all;
        };
        const collected = async (rootId: number, directory: string) => {
          const ticket = await begin(rootId, directory);
          await expect
            .poll(
              async () => {
                const s = await status();
                return s.ticket === ticket ? s.terminal : 0;
              },
              { timeout: 45000, interval: 50 },
            )
            .toBe(2);
          const entries = await rows(ticket);
          save(`listing-${rootId}-${directory.replaceAll('/', '_') || 'root'}-${ticket}.json`, {
            ticket,
            status: await status(),
            entries,
          });
          return { ticket, entries };
        };
        const caps = await request(0x3d);
        expect([caps.readUInt32BE(0), caps.readUInt32BE(4), caps.readUInt32BE(8)]).toEqual([
          apiTableSize(),
          0x504,
          304,
        ]);
        expect(caps.readInt32BE(12), 'client acquisition outside init fails').toBeLessThan(0);
        if (profile.device === 'dn2') {
          phase = 'empty virtual samples root';
          expect((await collected(1, '')).entries).toHaveLength(0);
        }
        phase = 'seed independent source metadata';
        const payload = Buffer.from(Array.from({ length: 257 }, (_, i) => (i * 73 + 19) & 255));
        await protocol.put('files-proof-store/item.bin', payload);
        if (profile.device === 'dn2') await protocol.put('samples/files-proof/item.bin', payload);
        else {
          const files = new RpcFiles(protocol, { chunk: 4096 });
          await files.mkdir('/files-proof');
          await files.write('/files-proof/item.bin', payload);
        }
        phase = 'root directory rows';
        for (const [rootId, childName] of [
          [1, 'files-proof'],
          [2, 'files-proof-store'],
        ] as const) {
          const listed = await collected(rootId, '');
          expect(listed.entries.some((row) => row.entry.path === '.' || row.entry.path === '..')).toBe(false);
          expect(listed.entries.find((row) => row.entry.path === childName)?.entry).toEqual({
            path: childName,
            kind: 2,
            flags: 0,
            size: 0,
            hashKind: 0,
            hash: '00'.repeat(32),
          });
        }
        phase = 'deferred samples listing';
        const samples = await collected(1, 'files-proof');
        expect(samples.entries).toHaveLength(1);
        const sample = samples.entries[0]!.entry;
        expect(sample).toMatchObject({
          path: 'files-proof/item.bin',
          kind: 1,
          flags: 3,
          size: profile.device === 'dt2' ? payload.length + 16 : payload.length,
          hashKind: profile.device === 'dt2' ? 2 : 1,
        });
        if (profile.device === 'dt2') {
          const stock = (await new RpcFiles(protocol).readDir('/files-proof')).find(
            (item) => item.name === 'item.bin',
          )!;
          expect(sample.hash).toBe(be32(stock.hash).toString('hex') + '00'.repeat(28));
          expect(sample.size).toBe(stock.size + 16);
        } else expect(sample.hash).toBe(sha(payload));
        phase = 'deferred store listing';
        const store = await collected(2, 'files-proof-store');
        expect(store.entries).toHaveLength(1);
        expect(store.entries[0]!.entry).toEqual({
          path: 'files-proof-store/item.bin',
          kind: 1,
          flags: 3,
          size: payload.length,
          hashKind: 1,
          hash: sha(payload),
        });
        expect(
          (await request(0x3a, Buffer.from([samples.entries[0]!.index]))).toString('hex'),
          'copied sample row survives later request',
        ).toBe(samples.entries[0]!.raw);
        phase = 'capacity validation preserves ready row';
        const manual = await begin(2, 'files-proof-store', 5); // notification hint, no auto-poll
        await expect
          .poll(
            async () => {
              const s = await status();
              return s.ticket === manual ? s.hint : 0;
            },
            { timeout: 45000 },
          )
          .toBe(1);
        expect(await poll(manual, true)).toMatchObject({ rc: -5, untouched: 1 });
        expect(await poll(manual)).toMatchObject({ rc: 1, entry: store.entries[0]!.entry });
        phase = 'supersede and cancel identities';
        const stale = await begin(2, 'files-proof-store', 0);
        const current = await begin(1, 'files-proof', 0);
        expect(await poll(stale)).toMatchObject({ rc: -1, untouched: 1 });
        expect((await request(0x3b, be32(stale))).readInt32BE()).toBe(-1);
        expect((await request(0x3b, be32(current))).readInt32BE()).toBe(0);
        expect(await poll(current)).toMatchObject({ rc: -1, untouched: 1 });
        phase = 'reentrant callback cancel';
        const canceled = await begin(2, 'files-proof-store', 3);
        await expect
          .poll(
            async () => {
              const s = await status();
              return s.ticket === canceled ? s.terminal : 0;
            },
            { timeout: 45000 },
          )
          .toBe(-1);
        expect(await poll(canceled)).toMatchObject({ rc: -1, untouched: 1 });
        expect((await rows(canceled)).map((row) => row.entry)).toEqual([store.entries[0]!.entry]);
        expect((await status()).callbacks).toBeGreaterThan(0);
        await session.expectNoFault('public files API');
        save('result.json', { passed: true, phase, status: await session.status(), fixture: await status() });
      } catch (error) {
        save('result.json', {
          passed: false,
          phase,
          error: String(error),
          status: await session.status().catch(String),
        });
        throw error;
      } finally {
        protocol?.cancelPending();
      }
    }, 300000);
  });
}
