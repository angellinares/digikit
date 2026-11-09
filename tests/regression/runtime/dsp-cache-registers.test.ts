import { createHash, randomUUID } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { repoRoot, resolveInputs } from '@digi/test-support';
import { describe, expect, test } from 'vitest';
import { withSession } from '../support.ts';

const binary = process.env.DIGI_DSP_PEEK_EMUD;
const enabled = Boolean(binary);
const root = repoRoot();
const hash = (file: string) => createHash('sha256').update(fs.readFileSync(file)).digest('hex');

async function bounded<T>(operation: Promise<T>, ms: number, label: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      operation,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error(`${label} exceeded ${ms} ms`)), ms);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

// Existing stock-ready inputs only. The caller provides the compatible daemon
// and may explicitly select a private ready directory with DIGI_PK_READY_DN2_112.
// Importing this file never builds a binary or prepares firmware/snapshots.
describe.skipIf(!enabled)('DN2 1.12 exact cache-register observation', () => {
  const runDir = path.join(root, 'out/runs/cache-registers-proof', randomUUID());
  if (enabled) {
    if (!path.isAbsolute(binary!) || !fs.existsSync(binary!))
      throw new Error('DIGI_DSP_PEEK_EMUD must name an existing absolute daemon path');
    fs.mkdirSync(runDir, { recursive: true });
  }
  const handle = withSession({ runDir, binary, device: 'dn2', firmware: '1.12', dsp: 'sharc', restart: false });

  test('reads exact CFG and CFG2 without disturbing stock progress', async () => {
    const session = handle.session;
    type Status = Awaited<ReturnType<typeof session.status>>;
    type Word = { addr: number; value: number | null };
    const evidence: Record<string, unknown> = {};
    let stopped = false;
    const save = (name: string, value: unknown) =>
      fs.writeFileSync(path.join(runDir, name), JSON.stringify(value, null, 2));
    const rpc = async <T>(method: string, params: object = {}): Promise<T> => {
      if (stopped) throw new Error('cache-register proof has stopped');
      const value = await bounded(session.client.call<T>(method, params), 5000, method);
      if (stopped) throw new Error('cache-register proof has stopped');
      return value;
    };
    const words = async (phase: string) => {
      const cfg = await rpc<Word>('dsp.peek_word', { addr: 0x3e000 });
      const cfg2 = await rpc<Word>('dsp.peek_word', { addr: 0x3e002 });
      const rr4start = await rpc<Word>('dsp.peek_word', { addr: 0x3e018 });
      const rr4end = await rpc<Word>('dsp.peek_word', { addr: 0x3e019 });
      evidence[`${phase}RawResponses`] = {
        cfg,
        cfg2,
        legacyMetadataRR4: {
          trusted: false,
          reason: 'Legacy packed images lack range-register names; these may be overlapping RAM fallback bytes.',
          start: rr4start,
          end: rr4end,
        },
      };
      // Controller metadata evolves between RPCs; compare only register fields.
      return {
        cfg: { addr: cfg.addr, value: cfg.value },
        cfg2: { addr: cfg2.addr, value: cfg2.value },
      };
    };
    const run = async () => {
      const inputs = resolveInputs('dn2', '1.12');
      evidence.identity = {
        binary: { path: binary, realpath: fs.realpathSync(binary!), sha256: hash(binary!) },
        inputs,
        hashes: { syx: hash(inputs.syx), dspImage: hash(inputs.dspImage), dspState: hash(inputs.dspState) },
        readyOverride: process.env.DIGI_PK_READY_DN2_112 ?? null,
        load: session.loadResult,
      };
      save('inputs.json', evidence.identity);
      expect(session.loadResult.syx_sha256).toBe(hash(inputs.syx));
      await rpc('run.mode', { mode: 'free' });
      const before = await rpc<Status>('status');
      evidence.beforeStatus = before;
      const beforeFrame = before.dsp_frames;
      if (typeof beforeFrame !== 'number' || !Number.isSafeInteger(beforeFrame))
        throw new Error('stock status must expose an integer DSP frame count');
      expect(before.cf.ready).toBe(true);
      const observed = await words('before');
      evidence.beforeWords = observed;
      save('before.json', { status: before, words: observed });
      // These are observations of this stock-ready fixture's completed startup:
      // CFG enable/invalidate bits and RR2/RR3 selection are retained.
      // RR4 correctness/unknown checks require fresh packed metadata and a new
      // cold state; legacy fallback bytes above are diagnostic evidence only.
      // These reads do not prove physical cache actions.
      expect(observed.cfg).toEqual({ addr: 0x3e000, value: 0x414141 });
      expect(observed.cfg2).toEqual({ addr: 0x3e002, value: 0xf0 });
      await expect(rpc('dsp.peek_word', { addr: 0x1_0000_0000 })).rejects.toMatchObject({ code: -32602 });
      evidence.invalidAddressRejected = true;
      await expect
        .poll(async () => (await rpc<Status>('status')).dsp_frames, { timeout: 10000, interval: 50 })
        .toBeGreaterThan(beforeFrame);
      const afterWords = await words('after');
      evidence.afterWords = afterWords;
      expect(afterWords).toEqual(observed);
      await bounded(session.expectNoFault('exact cache-register reads'), 5000, 'no-fault check');
      evidence.afterStatus = await rpc<Status>('status');
    };
    try {
      await bounded(run(), 30000, 'cache-register proof');
      save('result.json', { passed: true, ...evidence });
    } catch (error) {
      stopped = true;
      save('result.json', { passed: false, error: String(error), ...evidence });
      throw error;
    } finally {
      stopped = true;
      await bounded(session.close(), 10000, 'owned daemon shutdown');
    }
  }, 45000);
});
