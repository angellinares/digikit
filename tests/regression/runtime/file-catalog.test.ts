import { execFile } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { promisify } from 'node:util';
import { RpcFiles } from '@digi/dk-session/rpcfiles';
import { repoRoot, resolveInputs } from '@digi/test-support';
import { describe, expect, test } from 'vitest';
import { withSession } from '../support.ts';
import { liveProtocol } from './protocol.ts';
import { planSymbol, type RuntimePlan } from './runtime-plan.ts';

const root = repoRoot();
const binary = process.env.DIGI_CATALOG_EMUD;
const patchkit = process.env.DIGI_PATCHKIT_BIN;
const enabled = Boolean(binary && patchkit);
const card = process.env.DIGI_CATALOG_CARD;
const exec = promisify(execFile);
const guard = 0xa55a3cc3;
const hash = (bytes: Buffer) => createHash('sha256').update(bytes).digest('hex');

// Emulator only; explicit prebuilt binaries, stock DSP counts, no runtime module.
describe.skipIf(!enabled)('DT2 1.17 private UI-task file catalogue', () => {
  const runDir = path.join(root, 'out/runs/file-catalog', randomUUID());
  if (enabled) {
    if (!card) throw new Error('DIGI_CATALOG_CARD must name a prebuilt DT2 card image; this proof never prepares one');
    for (const input of [binary!, patchkit!, card]) {
      if (!path.isAbsolute(input) || !fs.existsSync(input))
        throw new Error(`catalogue preflight input missing: ${input}`);
    }
    fs.mkdirSync(runDir, { recursive: true });
  }
  const handle = withSession({
    runDir,
    binary,
    device: 'dt2',
    firmware: '1.17',
    dsp: 'sharc',
    restart: false,
  });
  test('matches stock ReadDir and checks errors, EOF and private canaries', async () => {
    const session = handle.session;
    const save = (name: string, data: unknown) =>
      fs.writeFileSync(path.join(runDir, name), JSON.stringify(data, null, 2));
    const input = resolveInputs('dt2', '1.17');
    const stockSha = hash(fs.readFileSync(input.syx));
    const output = path.join(runDir, 'catalogue.syx');
    const planFile = path.join(runDir, 'catalogue.plan.json');
    let protocol: ReturnType<typeof liveProtocol> | undefined;
    let phase = 'patch';
    let diagnostics: (() => Promise<unknown>) | undefined;
    const operations: unknown[] = [];
    try {
      const checked = await exec(
        patchkit!,
        ['--root', root, 'build', 'modules/core/adapters/file-catalog-proof', 'modules/core/expansion', '--check'],
        { timeout: 30000 },
      );
      fs.writeFileSync(path.join(runDir, 'object-check.log'), checked.stdout + checked.stderr);
      const patched = await exec(
        patchkit!,
        [
          '--root',
          root,
          'apply',
          '--firmware',
          input.syx,
          '--modules',
          'core/file-catalog-proof',
          '-o',
          output,
          '--plan',
          planFile,
        ],
        { timeout: 30000 },
      );
      fs.writeFileSync(path.join(runDir, 'patch.log'), patched.stdout + patched.stderr);
      const plan = JSON.parse(fs.readFileSync(planFile, 'utf8')) as RuntimePlan;
      expect(plan.base).toMatchObject({ device: 'dt2', version: '1.17', syx_sha256: stockSha });
      expect(plan.modules.map((id) => id.split('@')[0]).sort()).toEqual(['core/expansion', 'core/file-catalog-proof']);
      save('inputs.json', {
        input,
        binary,
        patchkit,
        card,
        cardBytes: fs.statSync(card!).size,
        stockSha,
        stockLoad: session.loadResult,
      });
      phase = 'patched startup';
      let timer: ReturnType<typeof setTimeout> | undefined;
      let load;
      try {
        load = await Promise.race([
          session.client.firmwareLoad({
            syx: output,
            base_sha256: stockSha,
            start: 'auto',
            auto_no: true,
            card: card!,
          }),
          new Promise<never>((_, reject) => {
            timer = setTimeout(() => reject(new Error('catalogue patched startup exceeded120s')), 120000);
          }),
        ]);
      } finally {
        clearTimeout(timer);
      }
      expect(load.syx_sha256).toBe(plan.output?.syx_sha256);
      await session.client.call('run.mode', { mode: 'free' });
      await expect.poll(async () => (await session.status()).cf.ready, { timeout: 30000 }).toBe(true);
      await session.expectNoFault(phase);
      save('patched-load.json', load);
      const mail = planSymbol(plan, 'catalog_mail');
      const state = planSymbol(plan, 'catalog_state');
      const out = planSymbol(plan, 'catalog_output');
      const pathname = planSymbol(plan, 'catalog_path');
      const word = async (addr: number) => {
        const value = await session.client.memPeek(addr);
        expect(value, `known CF word0x${addr.toString(16)}`).not.toBeNull();
        return value!;
      };
      const read = async (addr: number, length: number) => {
        const data = Buffer.alloc(length);
        for (let off = 0; off < length; off += 4) data.writeUInt32BE(await word(addr + off), off);
        return data;
      };
      const canaries = async () => {
        const values = await Promise.all([state, state + 20, out, out + 0x118].map(word));
        expect(values).toEqual([guard, guard, guard, guard]);
        return values;
      };
      diagnostics = async () => ({
        mail: await read(mail, 32),
        state: await read(state, 24),
        output: await read(out, 0x11c),
      });
      let previousCalls = 0;
      const request = async (command: number, name?: string) => {
        if (name !== undefined) {
          const data = Buffer.alloc(256);
          expect(Buffer.byteLength(name)).toBeLessThan(256);
          data.write(name);
          await session.client.call('mem.cf.write', { addr: pathname, hex: data.toString('hex') });
        }
        await session.client.call('mem.cf.poke', { addr: mail + 4, value: 0 });
        await session.client.call('mem.cf.poke', { addr: mail, value: command }); // command last
        await expect.poll(() => word(mail + 4), { timeout: 20000, interval: 20 }).toBe(2);
        const values = await read(mail, 32);
        const result = {
          command,
          path: name,
          rc: values.readInt32BE(8),
          error: values.readUInt32BE(12),
          opened: values.readUInt32BE(16),
          eof: values.readUInt32BE(20),
          calls: values.readUInt32BE(24),
          size: values.readUInt32BE(28),
          guards: await canaries(),
        };
        expect(result.calls).toBeGreaterThanOrEqual(previousCalls);
        expect(result.calls - previousCalls, 'at most one diagnostic firmware call per request').toBeLessThanOrEqual(1);
        previousCalls = result.calls;
        operations.push(result);
        save('operations.json', operations);
        return result;
      };
      protocol = liveProtocol(session);
      const files = new RpcFiles(protocol, { chunk: 4096 });
      phase = 'deterministic stock fixture';
      const payload = Uint8Array.from({ length: 257 }, (_, n) => (n * 73 + 19) & 255);
      const child = '/catalog-proof';
      await files.mkdir(child);
      const closed = await files.write(`${child}/item.bin`, payload);
      expect(closed.size).toBe(payload.length);
      const enumerate = async (dir: string) => {
        phase = `enumerate ${dir}`;
        const stock = await files.readDir(dir);
        expect(stock.length, 'bounded proof fixture directory').toBeLessThan(128);
        expect(await request(1, dir)).toMatchObject({ rc: 0, error: 0, opened: 1 });
        type SizeResult = Awaited<ReturnType<typeof request>>;
        const entries: {
          name: string;
          type: number;
          id: number;
          size?: number;
          sizeResult?: SizeResult;
          raw: string;
        }[] = [];
        let eofCalls = 0;
        for (let i = 0; i < 130; ++i) {
          const next = await request(2);
          expect(next.error).toBe(0);
          if (next.rc === 0) {
            eofCalls = next.calls;
            break;
          }
          expect(next.rc).toBe(1);
          const raw = await read(out + 4, 0x114);
          const end = raw.indexOf(0, 20);
          expect(end).toBeGreaterThanOrEqual(20);
          const entry = {
            name: raw.toString('utf8', 20, end),
            type: raw.readUInt16BE(4),
            id: raw.readUInt32BE(0),
            size: undefined as number | undefined,
            sizeResult: undefined as SizeResult | undefined,
            raw: raw.toString('hex'),
          };
          entries.push(entry);
          save(`raw-${dir === '/' ? 'root' : 'child'}.json`, entries);
          // Collect every valid row through EOF before consulting the stock oracle.
          // SIZE errors are observations too; defer their assertions until collection ends.
          if (!['.', '..'].includes(entry.name) && entry.type !== 1) {
            entry.sizeResult = await request(3);
            entry.size = entry.sizeResult.size;
            save(`raw-${dir === '/' ? 'root' : 'child'}.json`, entries);
          }
        }
        save(`listing-${dir === '/' ? 'root' : 'child'}.json`, { stock, entries, trailerBytes: 16 });
        expect(eofCalls, 'iterator reached EOF within bound').toBeGreaterThan(0);
        expect(await request(2)).toMatchObject({ rc: 0, eof: 1, calls: eofCalls });
        for (const entry of entries.filter((item) => !['.', '..'].includes(item.name))) {
          const oracle = stock.find((item) => item.name === entry.name);
          expect(oracle, `stock ReadDir contains ${entry.name}`).toBeDefined();
          if (oracle!.kind === 'D') expect(entry.type).toBe(1);
          else {
            expect(entry.type).not.toBe(1);
            expect(entry.sizeResult, `SIZE succeeded for ${entry.name}`).toMatchObject({ rc: 0, error: 0 });
            // On-card size includes the native trailer that ReadDir subtracts.
            expect(entry.size, `physical size vs stock size for ${entry.name}`).toBe(oracle!.size + 16);
          }
        }
        expect(
          entries
            .filter((item) => !['.', '..'].includes(item.name))
            .map((item) => item.name)
            .sort(),
        ).toEqual(stock.map((item) => item.name).sort());
      };
      expect(await request(2)).toMatchObject({ rc: -1, error: 3, calls: 0 });
      await enumerate('/');
      await enumerate(child);
      phase = 'unterminated path';
      await session.client.call('mem.cf.write', { addr: pathname, hex: Buffer.alloc(256, 0x78).toString('hex') });
      const invalid = await request(1);
      expect(invalid).toMatchObject({ rc: -1, error: 1, opened: 0 });
      expect(await request(2)).toMatchObject({ rc: -1, error: 3, calls: invalid.calls });
      phase = 'invalid directory paths';
      for (const name of ['/catalog-proof-missing', `${child}/item.bin`]) {
        expect(await request(1, name)).toMatchObject({ rc: -1, error: 0, opened: 0 });
        expect(await request(2)).toMatchObject({ rc: -1, error: 3, opened: 0 });
      }
      save('guards.json', { state: await read(state, 24), output: await read(out, 0x11c), canaries: await canaries() });
      await session.expectNoFault('file catalogue');
      save('result.json', { passed: true, phase, status: await session.status() });
    } catch (error) {
      save('result.json', {
        passed: false,
        phase,
        error: String(error),
        status: await session.status().catch(String),
        memory: await diagnostics?.().catch(String),
      });
      throw error;
    } finally {
      protocol?.cancelPending();
    }
  }, 240000);
});
