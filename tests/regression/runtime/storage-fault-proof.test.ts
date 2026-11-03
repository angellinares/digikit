import { execFile } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { promisify } from 'node:util';
import { repoRoot, resolveInputs, Session } from '@digi/test-support';
import { describe, expect, test } from 'vitest';
import { planSymbol, type RuntimePlan } from './runtime-plan.ts';
import { validateStorageWatch, type StorageWatchEvent as Event } from './storage-fault-watch.ts';

const binary = process.env.DIGI_STORAGE_FAULT_EMUD;
const patchkit = process.env.DIGI_PATCHKIT_BIN;
const root = repoRoot();
const exec = promisify(execFile);
const mask = 0x00310000;
const mounted = 0x44f2bd68;
const hash = (file: string) => createHash('sha256').update(fs.readFileSync(file)).digest('hex');
type Code = [number, number];
type Plan = RuntimePlan & { writes: { owner: string; kind: string; addr: number; len: number; code?: Code[] }[] };

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

// Explicit immutable binaries and existing DT2 1.17 ready/cold inputs only.
// Applies current prepared objects; never builds a daemon/module/core or injects MMIO faults.
describe.skipIf(!binary || !patchkit)('private DT2 boot-scoped raw storage observer', () => {
  test('observes loader initialization, sticky host seed and a second true cold boot', async () => {
    const runDir = path.join(root, 'out/runs/storage-fault-proof/native', randomUUID());
    fs.mkdirSync(runDir, { recursive: true });
    const evidence: Record<string, unknown> = {};
    const save = (name: string, value: unknown) =>
      fs.writeFileSync(path.join(runDir, name), JSON.stringify(value, null, 2));
    let session: Session | undefined;
    let plan: Plan | undefined;
    let latch = 0;
    let phase = 'preflight';
    let stopped = false;
    let failure: unknown;
    const errors: string[] = [];
    const rpc = async <T>(method: string, params: object = {}, ms = 5000): Promise<T> => {
      if (stopped || !session) throw new Error('storage observer proof has stopped/not started');
      const reply = await bounded(session.client.call<T>(method, params), ms, method);
      if (stopped) throw new Error('storage observer proof has stopped');
      return reply;
    };
    const readLatch = async () => {
      const { value } = await rpc<{ value: number | null }>('mem.cf.peek', { addr: latch });
      expect(value).not.toBeNull();
      expect(Number.isInteger(value)).toBe(true);
      expect((value! & ~mask) >>> 0).toBe(0);
      return value!;
    };
    const run = async () => {
      for (const file of [binary!, patchkit!]) {
        if (!path.isAbsolute(file) || !fs.statSync(file).isFile())
          throw new Error(`storage observer requires an existing absolute binary: ${file}`);
      }
      const inputs = resolveInputs('dt2', '1.17');
      const coldState = path.join(inputs.readyDir, 'cold.state');
      for (const file of [inputs.syx, inputs.cfSnapshot, inputs.dspImage, inputs.dspState, coldState])
        if (!fs.statSync(file).isFile()) throw new Error(`missing prepared input ${file}`);
      const stockSha = hash(inputs.syx);
      evidence.identity = {
        inputs,
        readyOverride: process.env.DIGI_PK_READY_DT2_117 ?? null,
        binary: { path: binary, realpath: fs.realpathSync(binary!), sha256: hash(binary!) },
        patchkit: { path: patchkit, realpath: fs.realpathSync(patchkit!), sha256: hash(patchkit!) },
        hashes: Object.fromEntries(
          [inputs.syx, inputs.cfSnapshot, inputs.dspImage, inputs.dspState, coldState].map((file) => [
            file,
            hash(file),
          ]),
        ),
      };
      save('inputs.json', evidence.identity);
      const checkArgs = ['--root', root, 'build', 'core/storage-fault-proof', 'core/expansion', '--check'];
      const checked = await exec(patchkit!, checkArgs, { cwd: root, timeout: 30000 });
      fs.writeFileSync(path.join(runDir, 'object-check.log'), checked.stdout + checked.stderr);
      const output = path.join(runDir, 'observer.syx');
      const planFile = path.join(runDir, 'observer.plan.json');
      const applyArgs = [
        '--root',
        root,
        'apply',
        '--firmware',
        inputs.syx,
        '--modules',
        'core/storage-fault-proof',
        '-o',
        output,
        '--plan',
        planFile,
      ];
      const applied = await exec(patchkit!, applyArgs, { cwd: root, timeout: 30000 });
      fs.writeFileSync(path.join(runDir, 'apply.log'), applied.stdout + applied.stderr);
      plan = JSON.parse(fs.readFileSync(planFile, 'utf8')) as Plan;
      expect(plan.base).toMatchObject({ device: 'dt2', version: '1.17', syx_sha256: stockSha });
      expect(plan.modules.map((id) => id.split('@')[0]).sort()).toEqual(['core/expansion', 'core/storage-fault-proof']);
      const patchedSha = hash(output);
      expect(plan.output?.syx_sha256).toBe(patchedSha);
      latch = planSymbol(plan, 'storage_fault_bits');
      for (const symbol of ['pk_loader', 'storage_fault_hook_a', 'storage_fault_hook_b', 'storage_fault_hook_c'])
        planSymbol(plan, symbol);
      evidence.patch = { planFile, output, sha256: patchedSha, latch, plan };
      const watchEnv: Record<string, string | undefined> = {
        DIGI_WRITE_WATCH: `0x${latch.toString(16)}+4,0x${mounted.toString(16)}+4`,
        DIGI_WRITE_WATCH_OUT: path.join(runDir, 'writewatch.jsonl'),
        DIGI_WRITE_WATCH_SEQ: '1',
        DIGI_WRITE_WATCH_LIMIT: '100000',
        DIGI_WRITE_WATCH_AFTER: undefined,
        DIGI_WRITE_WATCH_ALLOW_PC: undefined,
        DIGI_STACK_WATCH: undefined,
        DIGI_STACK_WATCH_EPOCH: undefined,
        DIGI_FLUSH_CHECK: undefined,
      };
      const previous = Object.fromEntries(Object.keys(watchEnv).map((key) => [key, process.env[key]]));
      evidence.commands = {
        check: { binary: patchkit, argv: checkArgs },
        apply: { binary: patchkit, argv: applyArgs },
        watchEnv,
        clearedEnvironment: Object.keys(watchEnv).filter((key) => watchEnv[key] === undefined),
      };
      save('commands.json', evidence.commands);
      phase = 'session-start';
      try {
        for (const [key, value] of Object.entries(watchEnv)) {
          if (value === undefined) delete process.env[key];
          else process.env[key] = value;
        }
        // The latch is resolved before spawning; every later emulator inherits this watch.
        session = await Session.start({ runDir, binary, device: 'dt2', firmware: '1.17', dsp: 'sharc', autoNo: true });
      } finally {
        for (const [key, value] of Object.entries(previous)) {
          if (value === undefined) delete process.env[key];
          else process.env[key] = value;
        }
      }
      if (stopped) {
        await session.close();
        throw new Error('late session startup after proof timeout');
      }
      evidence.stockLoad = session.loadResult;
      expect(session.loadResult.syx_sha256).toBe(stockSha);
      const boots = [];
      for (const boot of [1, 2]) {
        phase = `cold-${boot}`;
        const loaded = await rpc<Record<string, unknown>>(
          'firmware.load',
          { syx: output, base_sha256: stockSha, start: 'cold', force: true, auto_no: true },
          180000,
        );
        expect(loaded).toMatchObject({
          syx_sha256: patchedSha,
          start_used: 'cold',
          cache_hit: false,
          // Only the first load replaces stock MAIN; the second reloads the same image.
          main_changed: boot === 1,
        });
        expect(loaded.error).toBeUndefined();
        await rpc('run.mode', { mode: 'paused' });
        const status = await session.status();
        expect(status.cf.ready).toBe(true);
        await bounded(session.expectNoFault(phase), 5000, 'no-fault check');
        // Raw handled startup errors may exist: record, validate mask, never assume zero.
        const readyLatch = await readLatch();
        boots.push({ boot, loaded, status, readyLatch });
        evidence.boots = boots;
        save(`cold-${boot}.json`, boots.at(-1));
        if (boot === 1) {
          phase = 'host-seed';
          await rpc('mem.cf.poke', { addr: latch, value: mask });
          expect(await readLatch()).toBe(mask);
          const before = await session.status();
          const stepped = await rpc('step', { instructions: 1_000_000, timeout_wall_s: 20 }, 25000);
          const after = await session.status();
          if (typeof before.elapsed !== 'number' || typeof after.elapsed !== 'number')
            throw new Error('CF progress requires numeric status.elapsed');
          expect(after.elapsed).toBeGreaterThan(before.elapsed);
          const retained = await readLatch();
          expect(retained).toBe(mask);
          evidence.hostSeed = {
            value: mask,
            before,
            stepped,
            after,
            retained,
            note: 'Host SDRAM seed, not injected hardware error; CF progress alone does not prove an after-seed IRQ.',
          };
          await bounded(session.expectNoFault(phase), 5000, 'post-seed no-fault check');
        }
      }
      expect(hash(binary!)).toBe((evidence.identity as { binary: { sha256: string } }).binary.sha256);
      expect(hash(inputs.syx)).toBe(stockSha);
    };
    try {
      await bounded(run(), 450000, 'private storage observer proof');
    } catch (error) {
      failure = error;
      evidence.failurePhase = phase;
      errors.push(String(error));
      if (session)
        evidence.failureDiagnostics = await bounded(
          Promise.allSettled([session.status(), session.faults()]),
          5000,
          'failure diagnostics',
        ).catch(String);
    } finally {
      stopped = true;
      if (session) {
        try {
          await bounded(session.close(), 15000, 'owned daemon shutdown');
        } catch (error) {
          failure ??= error;
          errors.push(String(error));
        }
      }
    }
    // Closing the owned daemon flushes every emulator's sequenced watch and summary.
    try {
      if (!plan) throw new Error('patch plan unavailable for watch validation');
      phase = 'watch-validation';
      const codeFor = (module: string) =>
        plan!.writes.filter((w) => w.kind === 'place' && w.owner.startsWith(`${module}@`)).flatMap((w) => w.code ?? []);
      const loader = codeFor('core/expansion');
      const observer = codeFor('core/storage-fault-proof');
      expect(loader).toHaveLength(1);
      expect(observer).toHaveLength(1);
      const hooks = ['a', 'b', 'c'].map((name) => planSymbol(plan!, `storage_fault_hook_${name}`));
      const hookRanges = hooks.map((lo, i): Code => [lo, hooks[i + 1] ?? observer[0]![1]]);
      const inside = (pc: number, ranges: Code[]) => ranges.some(([lo, hi]) => lo <= pc && pc < hi);
      const files = fs
        .readdirSync(runDir)
        .filter((name) => /^writewatch-\d+\.jsonl$/.test(name))
        .sort((a, b) => Number(a.match(/-(\d+)/)![1]) - Number(b.match(/-(\d+)/)![1]));
      const summaries = [];
      for (const file of files) {
        const rows = fs
          .readFileSync(path.join(runDir, file), 'utf8')
          .split('\n')
          .filter(Boolean)
          .map((line) => JSON.parse(line) as Event);
        const summary = JSON.parse(
          fs.readFileSync(path.join(runDir, file.replace(/\.jsonl$/, '.summary.json')), 'utf8'),
        ) as { events: number; logged: number };
        const dataRows = validateStorageWatch(rows, summary, latch, mask, [...loader, ...hookRanges]);
        const latchRows = dataRows.filter((r) => r.address < latch + 4 && r.address + r.size! > latch);
        const zeros = latchRows.filter((r) => inside(Number(r.pc), loader) && Number(r.value) === 0);
        if (!zeros.length) {
          summaries.push({ file, summary, coldBootTrace: false });
          continue;
        }
        const observerRows = latchRows.filter((r) => inside(Number(r.pc), hookRanges));
        for (const row of latchRows.filter((r) => inside(Number(r.pc), loader))) {
          expect(Number(row.value)).toBe(0);
          expect(row.ic).toBeLessThan(observerRows[0]?.ic ?? -1);
        }
        const fsInit = dataRows.find((r) => r.t === 'w' && Number(r.pc) === 0x4015a722 && r.address === mounted);
        expect(observerRows.length).toBeGreaterThan(0);
        expect(fsInit, 'audited FS-init mounted-flag store').toBeDefined();
        expect(zeros[0]!.ic).toBeLessThan(observerRows[0]!.ic);
        expect(observerRows[0]!.ic).toBeLessThan(fsInit!.ic);
        let previous = 0;
        for (const row of observerRows) {
          const value = Number(row.value);
          expect((value | previous) >>> 0).toBe(value);
          previous = value;
        }
        summaries.push({
          file,
          summary,
          coldBootTrace: true,
          zero: zeros[0],
          firstObserver: observerRows[0],
          fsInit,
          observerWrites: observerRows.length,
          finalObservedBits: previous,
          hostSeedNote:
            'First cold boot may jump to the host-seeded full mask; no hardware fault or after-seed IRQ is inferred without its own watch row.',
        });
      }
      evidence.watch = { loader, hookRanges, summaries };
      expect(summaries.filter((s) => s.coldBootTrace)).toHaveLength(2);
      save('watch-summary.json', evidence.watch);
    } catch (error) {
      failure ??= error;
      errors.push(String(error));
    }
    save('result.json', { passed: failure === undefined, phase, errors, ...evidence });
    if (failure) throw failure;
  }, 480000);
});
