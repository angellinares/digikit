import { execFile } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { promisify } from 'node:util';
import { repoRoot, resolveInputs } from '@digi/test-support';
import { describe, expect, test } from 'vitest';
import { withSession } from '../support.ts';
import type { RuntimePlan } from './runtime-plan.ts';

const binary = process.env.DIGI_PATCHED_STARTUP_EMUD;
const patchkit = process.env.DIGI_PATCHKIT_BIN;
const enabled = Boolean(binary && patchkit);
const root = repoRoot();
const exec = promisify(execFile);
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

// Opt in with immutable prebuilt daemon/patchkit binaries and existing stock
// ready inputs (DIGI_PK_READY_DT2_117 may select a private ready directory).
// This checks prepared objects only: no daemon, module or core builds here.
// Frame progress and register retention make no PCM, hardware or performance claim.
describe.skipIf(!enabled)('DT2 1.17 changed-DSP startup and cached restore', () => {
  const runDir = path.join(root, 'out/runs/patched-dsp-startup', randomUUID());
  if (enabled) {
    for (const file of [binary!, patchkit!]) {
      if (!path.isAbsolute(file) || !fs.statSync(file).isFile())
        throw new Error(`startup proof requires an existing absolute binary: ${file}`);
    }
    const inputs = resolveInputs('dt2', '1.17');
    if (!fs.statSync(path.join(inputs.readyDir, 'cold.state')).isFile())
      throw new Error('prepare raw cold.state before running the changed-DSP startup proof');
    fs.mkdirSync(runDir, { recursive: true });
  }
  const handle = withSession({ runDir, binary, device: 'dt2', firmware: '1.17', dsp: 'sharc', restart: false });

  test('boots the patched DSP and restores its initialized ready pair', async () => {
    const session = handle.session;
    type Status = Awaited<ReturnType<typeof session.status>>;
    type Word = { addr: number; value: number | null };
    const evidence: Record<string, unknown> = {};
    let phase = 'preflight';
    let stopped = false;
    const save = (name: string, value: unknown) =>
      fs.writeFileSync(path.join(runDir, name), JSON.stringify(value, null, 2));
    const rpc = async <T>(method: string, params: object = {}, ms = 5000): Promise<T> => {
      if (stopped) throw new Error('startup proof has stopped');
      const value = await bounded(session.client.call<T>(method, params), ms, method);
      if (stopped) throw new Error('startup proof has stopped');
      return value;
    };
    const words = async () => {
      const cfg = await rpc<Word>('dsp.peek_word', { addr: 0x3e000 });
      const cfg2 = await rpc<Word>('dsp.peek_word', { addr: 0x3e002 });
      evidence[`${phase}RawWords`] = { cfg, cfg2 };
      expect(cfg.addr).toBe(0x3e000);
      expect(cfg2.addr).toBe(0x3e002);
      expect(cfg.value).not.toBeNull();
      expect(cfg2.value).not.toBeNull();
      return { cfg: cfg.value, cfg2: cfg2.value };
    };
    const progress = async () => {
      await rpc('run.mode', { mode: 'free' });
      const before = await rpc<Status>('status');
      evidence[`${phase}BeforeStatus`] = before;
      expect(before.cf.ready).toBe(true);
      expect(before.audio?.dsp_core).toMatch(/^dt2-/);
      const frame = before.dsp_frames;
      if (typeof frame !== 'number' || !Number.isSafeInteger(frame))
        throw new Error('startup status must expose an integer DSP frame count');
      await expect
        .poll(async () => (await rpc<Status>('status')).dsp_frames, { timeout: 10000, interval: 50 })
        .toBeGreaterThan(frame);
      await bounded(session.expectNoFault(phase), 5000, 'no-fault check');
      evidence[`${phase}AfterStatus`] = await rpc<Status>('status');
    };
    const run = async () => {
      const inputs = resolveInputs('dt2', '1.17');
      const coldState = path.join(inputs.readyDir, 'cold.state');
      const stockSha = hash(inputs.syx);
      evidence.identity = {
        inputs,
        readyOverride: process.env.DIGI_PK_READY_DT2_117 ?? null,
        binary: { path: binary, realpath: fs.realpathSync(binary!), sha256: hash(binary!) },
        patchkit: { path: patchkit, realpath: fs.realpathSync(patchkit!), sha256: hash(patchkit!) },
        hashes: {
          syx: stockSha,
          cfSnapshot: hash(inputs.cfSnapshot),
          dspImage: hash(inputs.dspImage),
          dspState: hash(inputs.dspState),
          coldState: hash(coldState),
        },
        stockLoad: session.loadResult,
      };
      save('inputs.json', evidence.identity);
      expect(session.loadResult.syx_sha256).toBe(stockSha);
      phase = 'stock';
      await progress();
      evidence.stockWords = await words();

      phase = 'patch';
      const checked = await exec(
        patchkit!,
        [
          '--root',
          root,
          'build',
          'tests/fixtures/modules/patched-pluck',
          'modules/core/adapters/dsp-voice',
          'modules/core/adapters/machines',
          'modules/core/adapters/params',
          'modules/core/expansion',
          '--check',
        ],
        { cwd: root, timeout: 30000 },
      );
      fs.writeFileSync(path.join(runDir, 'object-check.log'), checked.stdout + checked.stderr);
      const output = path.join(runDir, 'patched-pluck.syx');
      const planFile = path.join(runDir, 'patched-pluck.plan.json');
      const patched = await exec(
        patchkit!,
        [
          '--root',
          root,
          'apply',
          '--firmware',
          inputs.syx,
          '--modules',
          'test/patched-pluck',
          '-o',
          output,
          '--plan',
          planFile,
        ],
        { cwd: root, timeout: 30000 },
      );
      fs.writeFileSync(path.join(runDir, 'patch.log'), patched.stdout + patched.stderr);
      const plan = JSON.parse(fs.readFileSync(planFile, 'utf8')) as RuntimePlan;
      expect(plan.base).toMatchObject({ device: 'dt2', version: '1.17', syx_sha256: stockSha });
      expect(plan.modules.map((id) => id.split('@')[0]).sort()).toEqual([
        'core/dsp-voice',
        'core/expansion',
        'core/machines',
        'core/params',
        'test/patched-pluck',
      ]);
      const outputSha = hash(output);
      expect(plan.output?.syx_sha256).toBe(outputSha);
      evidence.patchIdentity = { planFile, output, sha256: outputSha, plan };
      save('patch-identity.json', evidence.patchIdentity);

      phase = 'cold';
      const cold = await rpc<Record<string, unknown>>(
        'firmware.load',
        {
          syx: output,
          base_sha256: stockSha,
          start: 'cold',
          auto_no: true,
        },
        180000,
      );
      evidence.coldLoad = cold;
      save('cold-load.json', cold);
      expect(cold).toMatchObject({
        syx_sha256: outputSha,
        main_changed: true,
        start_used: 'cold',
        cache_hit: false,
        audio: true,
        dsp: { applied: true, at: 'cold boot' },
      });
      expect(cold.error).toBeUndefined();
      await progress();
      const coldWords = await words();
      evidence.coldWords = coldWords;

      phase = 'stockRestore';
      const stock = await rpc<Record<string, unknown>>(
        'firmware.load',
        {
          syx: inputs.syx,
          start: 'snapshot',
          auto_no: true,
        },
        30000,
      );
      evidence.stockRestore = stock;
      expect(stock.syx_sha256).toBe(stockSha);

      phase = 'cached';
      const cached = await rpc<Record<string, unknown>>(
        'firmware.load',
        {
          syx: output,
          base_sha256: stockSha,
          start: 'snapshot',
          auto_no: true,
        },
        30000,
      );
      evidence.cachedLoad = cached;
      save('cached-load.json', cached);
      expect(cached).toMatchObject({
        syx_sha256: outputSha,
        cache_hit: true,
        start_used: 'snapshot-cache',
        snapshot_ok: true,
        audio: true,
        dsp: { applied: true, at: 'cold boot' },
      });
      expect(cached.cache_dir).toBe(cold.cache_dir);
      expect(cached.error).toBeUndefined();
      expect(await words()).toEqual(coldWords);
      await progress();
      expect(await words()).toEqual(coldWords);
      expect(hash(binary!)).toBe((evidence.identity as { binary: { sha256: string } }).binary.sha256);
      expect(hash(coldState)).toBe((evidence.identity as { hashes: { coldState: string } }).hashes.coldState);
    };
    try {
      await bounded(run(), 300000, 'changed-DSP startup proof');
      save('result.json', { passed: true, phase, ...evidence });
    } catch (error) {
      stopped = true;
      save('result.json', { passed: false, phase, error: String(error), ...evidence });
      const diagnostics = await bounded(
        Promise.allSettled([session.status(), session.faults()]),
        5000,
        'failure diagnostics',
      ).catch((failure) => ({ error: String(failure) }));
      save('failure-diagnostics.json', diagnostics);
      throw error;
    } finally {
      stopped = true;
      await bounded(session.close(), 10000, 'owned daemon shutdown');
    }
  }, 330000);
});
