import fs from 'node:fs';
import { randomUUID } from 'node:crypto';
import path from 'node:path';
import { afterAll, afterEach, beforeAll, beforeEach, expect } from 'vitest';
import { DkSession, EmuUsbTransport } from '@digi/dk-session';
import {
  Session,
  withinRuntimeDeadline,
  type Device,
  type RuntimeBaseline,
  type SessionOptions,
} from '@digi/test-support';
import { fileTemp } from './file-temp.ts';
import { portFile } from './runtime/device-data.ts';
import { daemonBinary } from './runtime/devices.ts';
import type { RunDirClaim } from './runtime/run-claim.ts';
import { setTimingContext, timePhase, timingContext } from './timing.ts';
import { startNoProgressWatchdog, type Watchdog } from './watchdog.ts';

/** An AggregateError whose message names its causes (Vitest prints only the message). */
export function aggregate(errors: unknown[], what: string): AggregateError {
  const causes = errors.map((e) => (e instanceof Error ? e.message : String(e))).join(' | ');
  return new AggregateError(errors, `${what}: ${causes}`);
}

export interface SessionHandle {
  /** The session of this test file; valid inside tests and hooks. */
  readonly session: Session;
}

/** One daemon per test file: started in beforeAll, closed in afterAll. Unless `restart: false`, each test
 * starts from `session.restart()`. */
export function withSession(opts: SessionOptions & { restart?: boolean }): SessionHandle {
  let session: Session | undefined;
  beforeAll(async () => {
    // Without an explicit binary or DIGI_EMUD, take the daemon from DIGI_EMUD_<DEV> or dist/daemons.json.
    const binary = opts.binary ?? (process.env.DIGI_EMUD ? undefined : daemonBinary(opts.device));
    session = await Session.start({ ...opts, binary });
  });
  afterEach((context) => {
    if (context.task.result?.state === 'fail') session?.keepDiagnostics();
  });
  afterAll(async () => {
    await session?.close();
  });
  beforeEach(async () => {
    if (opts.restart !== false) await session!.restart();
  });
  return {
    get session() {
      if (!session) throw new Error('session not started');
      return session;
    },
  };
}

export interface PatchedSession {
  readonly session: Session;
  readonly dk: DkSession;
  close(): Promise<void>;
}

/** A fresh runtime daemon; protocol cancellation precedes daemon shutdown. No remote module is assumed installed. */
export async function startPatchedSession(
  opts: Omit<SessionOptions, 'runtime' | 'dsp'> & { patchSet?: string; dsp?: SessionOptions['dsp'] },
  scope = 'patched runtime',
): Promise<PatchedSession> {
  const context = { scope, device: opts.device, session: randomUUID() };
  const session = await timePhase(
    'session.startup',
    () =>
      Session.start({
        ...opts,
        dsp: opts.dsp ?? 'sharc',
        runtime: { patchSet: opts.patchSet },
      }),
    context,
  );
  setTimingContext(session, context);
  const watchdog = startNoProgressWatchdog(session);
  try {
    const dk = new DkSession(new EmuUsbTransport((method, params) => session.client.call(method, params)));
    let closing: Promise<void> | undefined;
    return {
      session,
      dk,
      close() {
        watchdog.stop();
        return (closing ??= timePhase(
          'session.shutdown',
          async () => {
            dk.cancelPending();
            await session.close();
          },
          context,
        ));
      },
    };
  } catch (err) {
    watchdog.stop();
    session.keepDiagnostics();
    await timePhase('session.shutdown', () => session.close(), context);
    throw err;
  }
}

/** Boot identity and health only: runtime patch sets do not install remote-control modules. */
export async function expectRuntimeBoot({ session, dk }: PatchedSession): Promise<void> {
  await timePhase(
    'boot.validation',
    async () => {
      const identity = session.loadResult.runtime;
      const prepared = session.preparedRuntimeImage;
      if (prepared) {
        expect(session.loadResult.syx_sha256).toBe(prepared.syxSha256);
        expect(prepared.modules.length).toBeGreaterThan(0);
      } else {
        expect(identity?.patched).toBe(true);
        if (!identity?.patched) throw new Error('stock input did not produce a runtime patch identity');
        expect(identity.set).toBe(session.options.runtime?.patchSet ?? 'emulator');
        expect(identity.base_sha256).toMatch(/^[a-f0-9]{64}$/);
        expect(identity.modules.length).toBeGreaterThan(0);
        expect(identity.cache_file).toBeTruthy();
      }
      expect(session.loadResult.start_used).toBeTruthy();
      await expect.poll(async () => (await session.status()).cf.ready, { timeout: 20_000 }).toBe(true);
      const status = await session.status();
      expect(status.mode).toBe('free');
      expect(status.cf.ready).toBe(true);
      if (prepared) {
        expect(status.firmware?.syx_sha256).toBe(prepared.syxSha256);
        expect(status.firmware?.base_sha256).toBe(prepared.baseSha256);
      } else expect(status.firmware?.runtime).toEqual(identity);
      expect(dk.rpcChunk).toBe(16384);
      await session.expectNoFault('runtime boot');
    },
    timingContext(session),
  );
}

/** No restart hook: every acceptance file owns a single fresh, free-running runtime. Vitest forbids `expect.poll` in
 * `beforeAll`, so each test calls `expectRuntimeBoot` itself. */
export function withPatchedSession(
  opts: Omit<SessionOptions, 'runtime' | 'dsp'> & { patchSet?: string; dsp?: SessionOptions['dsp'] },
  scope?: string,
): {
  readonly runtime: PatchedSession;
} {
  let runtime: PatchedSession | undefined;
  beforeAll(async () => {
    runtime = await startPatchedSession(opts, scope);
  });
  afterEach((context) => {
    if (context.task.result?.state === 'fail') runtime?.session.keepDiagnostics();
  });
  afterAll(async () => {
    await runtime?.close();
  });
  return {
    get runtime() {
      if (!runtime) throw new Error('runtime not started');
      return runtime;
    },
  };
}

export interface RuntimeCase extends PatchedSession {
  readonly epoch: number;
}

/** Opt-in fixture: independent cases reload pristine firmware/card state in one owned daemon. */
export function withReusablePatchedSession(
  opts: Omit<SessionOptions, 'runtime' | 'dsp'> & {
    patchSet?: string;
    dsp?: 'zero' | 'sharc' | 'cold';
    /** Called when the daemon starts: a persistent run dir for it (see `claimRunDir`), or undefined for a fresh temporary one. */
    runDirClaim?: () => RunDirClaim | undefined;
  },
  lifecycle: {
    prepareBaseline: (session: Session) => Promise<RuntimeBaseline>;
    /** Required scenario evidence (guard/fault/card/PCM) before state is discarded. Holds release in their own finally. */
    finishCase: (runtime: RuntimeCase) => Promise<void>;
    /** Validate process-lifetime artifacts only after the owned daemon has closed, even on a failed case. */
    afterClose?: (session: Session) => Promise<void>;
    /** Temporary directories of the fixture (artifacts): removed when the file ends clean, kept (and printed) when it failed. */
    tempDirs?: () => string[];
  },
  scope = 'reused runtime',
): { readonly runtime: RuntimeCase; reload(baseline: RuntimeBaseline): Promise<RuntimeCase> } {
  let session: Session | undefined;
  let watchdog: Watchdog | undefined;
  let baseline: RuntimeBaseline | undefined;
  let runtime: RuntimeCase | undefined;
  let invalidate: (() => void) | undefined;
  let transitioning = false;
  let claim: RunDirClaim | undefined;
  /** A failed file leaves a run dir that must not be reused: a snapshot of a boot that failed a check would pass the next run. */
  let failed = false;
  const releaseClaim = () => {
    claim?.release(failed);
    claim = undefined;
  };
  const disposeTemp = () => {
    for (const dir of lifecycle.tempDirs?.() ?? []) if (dir) fileTemp(dir, failed);
  };
  const loadCase = async (next: RuntimeBaseline): Promise<RuntimeCase> => {
    const context = { scope, device: opts.device, session: randomUUID() };
    setTimingContext(session!, context);
    const epoch = await timePhase('session.baseline-reload', () => session!.reloadRuntimeBaseline(next), context);
    let active = true;
    const dk = new DkSession(
      new EmuUsbTransport(async <T>(method: string, params?: Record<string, unknown>) => {
        if (!active) throw new Error('runtime case protocol is no longer active');
        const result = await session!.client.call<T>(method, params);
        if (!active) throw new Error('runtime case protocol is no longer active');
        return result;
      }),
    );
    const invalidateCase = () => {
      active = false;
      dk.cancelPending();
    };
    invalidate = invalidateCase;
    runtime = {
      session: session!,
      dk,
      epoch,
      close: async () => {
        if (!active) throw new Error('runtime case is no longer active');
        invalidateCase();
        await session!.close();
      },
    };
    return runtime;
  };
  const finishCurrentCase = async (): Promise<void> => {
    if (!runtime) return;
    const current = runtime;
    const cancel = invalidate;
    const deadline = Date.now() + 10_000;
    const errors: unknown[] = [];
    try {
      await withinRuntimeDeadline(deadline, () => lifecycle.finishCase(current));
    } catch (error) {
      errors.push(error);
    }
    try {
      // Without a DSP (zero or cold ColdFire-only) there is no audio capture to stop.
      if (opts.dsp !== 'zero' && opts.dsp !== 'cold')
        await withinRuntimeDeadline(deadline, () => session!.client.call('pcm.tap', { action: 'capture_stop' }));
      await withinRuntimeDeadline(deadline, () => session!.client.call('run.mode', { mode: 'paused' }));
    } catch (error) {
      errors.push(error);
    } finally {
      // No release-all RPC exists. Bounded taps/chords own release; the mandatory next load clears runtime/scheduled input.
      cancel?.();
      runtime = undefined;
    }
    if (errors.length) {
      failed = true;
      try {
        fs.writeFileSync(
          path.join(session!.runDir, 'runtime-case-error.json'),
          JSON.stringify(errors.map(String), null, 2),
        );
      } catch {
        /* Preserve evidence/cleanup errors if diagnostics cannot be written. */
      }
      await session!.close({ preserveDiagnostics: true });
      throw aggregate(errors, 'runtime case evidence or cleanup failed; owned daemon discarded');
    }
  };
  beforeAll(async () => {
    const deadline = Date.now() + 120_000;
    const { runDirClaim, ...sessionOptions } = opts;
    claim = runDirClaim?.();
    try {
      session = await Session.start({
        ...sessionOptions,
        ...(claim ? { runDir: claim.dir } : {}),
        dsp: opts.dsp ?? 'sharc',
        runtime: { patchSet: opts.patchSet, reuse: true },
      });
    } catch (error) {
      failed = true;
      releaseClaim();
      throw error;
    }
    watchdog = startNoProgressWatchdog(session);
    try {
      baseline = await withinRuntimeDeadline(deadline, () => lifecycle.prepareBaseline(session!));
    } catch (error) {
      watchdog.stop();
      failed = true;
      await session.close({ preserveDiagnostics: true });
      releaseClaim();
      throw error;
    }
  });
  beforeEach(async () => {
    await loadCase(baseline!);
  });
  afterEach(async (context) => {
    await finishCurrentCase();
    if (context.task.result?.state === 'fail') {
      failed = true;
      watchdog?.stop();
      await session?.close({ preserveDiagnostics: true });
    }
  });
  let checkedClose = false;
  afterAll(async () => {
    watchdog?.stop();
    invalidate?.();
    if (!session || checkedClose) {
      releaseClaim();
      return disposeTemp();
    }
    checkedClose = true;
    const errors: unknown[] = [];
    try {
      await session.close();
    } catch (error) {
      errors.push(error);
    }
    try {
      await lifecycle.afterClose?.(session);
    } catch (error) {
      errors.push(error);
    }
    if (errors.length) failed = true;
    releaseClaim();
    disposeTemp();
    if (errors.length) throw aggregate(errors, 'runtime shutdown or post-close evidence failed');
  });
  return {
    get runtime() {
      if (!runtime) throw new Error('runtime case not started');
      return runtime;
    },
    async reload(next) {
      if (!runtime || transitioning) throw new Error('runtime case not active or already transitioning');
      transitioning = true;
      try {
        await finishCurrentCase();
        return await loadCase(next);
      } finally {
        transitioning = false;
      }
    },
  };
}

export interface FreeRegion {
  name: string;
  lo: number;
  hi: number;
}

/** Free ColdFire regions of devices/ports/<device>-<firmware>.toml: every `[[region]]` with
 * `cpu = "cf"` (e.g. `cf.flash_cave`, `cf.expansion`), minus its `excludes` (`[lo, hi]` pairs).
 * `hi` is the file's `end`. */
export function freeRegions(device: Device, firmware: string): FreeRegion[] {
  const file = `${device}-${firmware}.toml`;
  const out: FreeRegion[] = [];
  for (const { name, cpu, start, end, excludes = [] } of portFile(device, firmware).region ?? []) {
    if (cpu !== 'cf') continue;
    if (name === undefined || start === undefined || end === undefined) {
      throw new Error(`${file}: malformed region block`);
    }
    let lo = start;
    for (const [xl, xh] of [...excludes].sort((a, b) => a[0] - b[0])) {
      if (xl > lo) out.push({ name, lo, hi: xl });
      lo = Math.max(lo, xh);
    }
    if (lo < end) out.push({ name, lo, hi: end });
  }
  if (out.length === 0) throw new Error(`${file}: no cf regions found`);
  return out;
}

/** Arm the guard on the free regions of the device and firmware; no PC is allowed (stock firmware). */
export async function armFreeRegionGuard(session: Session): Promise<void> {
  const { device, firmware } = session.options;
  const protect = freeRegions(device, firmware).map((r) => ({ lo: r.lo, hi: r.hi, label: r.name }));
  await session.client.guardSet({ protect, allow_pc: [] });
}

/** Fails when the guard saw a write into a free region; lists the first hits. */
export async function expectNoGuardHits(session: Session): Promise<void> {
  const report = await session.client.guardReport();
  if (report.total === 0) return;
  const hex = (n: number) => `0x${n.toString(16)}`;
  const lines = report.hits
    .slice(0, 8)
    .map((h) => `${h.label} pc=${hex(h.pc)} addr=${hex(h.addr)} size=${h.size} icount=${h.icount}`);
  throw new Error(`guard: ${report.total} write(s) into free regions:\n${lines.join('\n')}`);
}
