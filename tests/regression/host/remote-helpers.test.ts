import fs from 'node:fs';
import { createHash } from 'node:crypto';
import { beforeEach, expect, test, vi } from 'vitest';
import type { DkSession } from '@digi/dk-session';
import { emulatorClock, type Session } from '@digi/test-support';
import { withWaitApi, type PatchedSession } from '../support.ts';

const fake = vi.hoisted(() => ({
  exec: vi.fn(),
  read: vi.fn(),
  exists: vi.fn(() => true),
  mkdir: vi.fn(),
  mkdtemp: vi.fn(),
  write: vi.fn(),
}));
vi.mock('node:timers/promises', () => ({
  setTimeout: (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms)),
}));
vi.mock('../runtime/patchkit-exec.ts', () => ({ patchkit: fake.exec, patchkitCovered: fake.exec }));
vi.mock('node:fs', async (original) => ({
  default: {
    ...(await original<{ default: typeof import('node:fs') }>()).default,
    readFileSync: fake.read,
    existsSync: fake.exists,
    mkdirSync: fake.mkdir,
    mkdtempSync: fake.mkdtemp,
    writeFileSync: fake.write,
  },
}));
vi.mock('../../../packages/test-support/src/inputs.ts', async (original) => ({
  ...(await original<object>()),
  resolveInputs: () => ({ syx: '/stock.syx' }),
}));
import { expectRuntimeUi } from '../runtime/ready.ts';
import { bindBootClock, pollEmulated, waitForBootEmulated, withEmulatedProbe } from '../runtime/waits.ts';
import { expectRemoteEnabled, installRemote, prepareRemote, withRemoteRelease } from '../runtime/remote.ts';
import { prepareRuntimePlanModulesOffline } from '../runtime/runtime-plan-modules.ts';
import {
  planSymbol,
  prepareRuntimePlanOffline,
  validateRuntimePlan,
  type RuntimePlan,
} from '../runtime/runtime-plan.ts';
import { fakeClock } from './fake-status.ts';
import { readAboutRows, expectRuntimeAbout, providerFailureLine } from '../runtime/runtime-menu.ts';
import { openRuntimeAbout } from '../runtime/flows/menu.ts';

const patchedSha256 = 'c'.repeat(64);
const plan: RuntimePlan = {
  output: { syx_sha256: patchedSha256 },
  base: { device: 'dt2', version: '1.17', syx_sha256: 'a'.repeat(64) },
  modules: ['core/runtime@0.1.0', 'core/digikit-menu@0.1.0'],
  symbols: {
    'core/digikit-menu@0.1.0:DK_ABOUT': 4096,
    'core/digikit-menu@0.1.0:DK_ABOUT_N': 8192,
    'core/runtime@0.1.0:DKRT_BOOT': 12288,
  },
};
const session = {
  options: { device: 'dt2', firmware: '1.17' },
  dsp: 'sharc',
  runDir: '/run/own-daemon',
  loadResult: {
    syx_sha256: patchedSha256,
    runtime: {
      patched: true,
      set: 'emulator',
      base_sha256: plan.base.syx_sha256,
      cache_file: '/cache/runtime.syx',
      modules: plan.modules,
    },
  },
} as unknown as Session;

/** Mutable daemon evidence for boot transitions; omitted words remain unmapped. */
function readyDaemon() {
  const status = {
    mode: 'free',
    epoch: 4,
    /** Each status call advances the emulated clock by 100 ms (132 000 instructions a millisecond). */
    elapsed: 0,
    cf: { ready: true, main_ui_reached: true, filesystem_verified: null as boolean | null },
  };
  const words: Record<number, number | null> = {
    0: 0x444b4254,
    4: 1,
    8: 0,
    12: 0,
    16: 0,
    28: 0,
    36: 1,
  };
  const memPeek = vi.fn(async (address: number) => words[address - 12288] ?? null);
  const daemon = {
    ...session,
    status: vi.fn(async () => ((status.elapsed += status.mode === 'paused' ? 0 : 13_200_000), status)),
    client: { memPeek },
  } as unknown as Session;
  return { daemon, status, words, memPeek };
}

/** A fake protocol session that records the order of the native install calls. */
function fakeDk() {
  const { daemon, status } = readyDaemon();
  const calls: string[] = [];
  const record = (name: string, result?: unknown) =>
    vi.fn(async (..._args: unknown[]) => {
      calls.push(name);
      return result;
    });
  const dk = {
    put: record('put'),
    verifyFile: record('verifyFile', true),
    select: record('select'),
    restart: record('restart'),
    waitForBoot: record('waitForBoot'),
    modules: record('modules', [{ name: 'digikit/remote' }]),
    remoteRelease: vi.fn().mockResolvedValue(undefined),
    remoteState: vi.fn().mockResolvedValue({ enabled: true }),
  };
  dk.restart.mockImplementation(async () => {
    calls.push('restart');
    status.epoch++;
  });
  return { calls, dk, runtime: { session: daemon, dk } as unknown as PatchedSession };
}

/**
 * Every read returns `text`, except dist/bytes/images.sha256, which covers the DT2 1.17 emulator set (runtime-plan.ts assertCovered), and a packed `.dkm`, a stand-in
 * with no header table (it needs no service).
 */
const readFiles = (text: string) =>
  fake.read.mockImplementation((file: string) =>
    String(file).endsWith('images.sha256')
      ? 'dt2-1.17 emulator image x plan y addrmap z\n'
      : String(file).endsWith('.dkm')
        ? Buffer.alloc(16)
        : text,
  );

beforeEach(() => {
  vi.resetAllMocks();
  readFiles(JSON.stringify(plan));
  fake.exists.mockReturnValue(true);
  fake.mkdtemp.mockReturnValue('/artifacts/remote-input/dt2/run-1');
  fake.exec.mockResolvedValue({ stdout: '' });
});

test('plan identity requires the device, version, base hash and expanded module membership', () => {
  expect(() => validateRuntimePlan(plan, session)).not.toThrow();
  for (const base of [
    { ...plan.base, device: 'dn2' },
    { ...plan.base, version: '1.16' },
    { ...plan.base, syx_sha256: 'b'.repeat(64) },
  ])
    expect(() => validateRuntimePlan({ ...plan, base }, session)).toThrow('does not match');
  for (const modules of [['emulator'], [...plan.modules, 'extra']]) {
    expect(() => validateRuntimePlan({ ...plan, modules }, session)).toThrow('does not match');
  }
});

test('symbol lookup accepts aliases of one address, including mangled Rust names, but rejects ambiguity or absence', () => {
  const symbols = { 'a:DK_ABOUT': 1, 'b:DK_ABOUT': 1, _8DK_ABOUT: 1, 'a:DK_ABOUT_N': 2 };
  expect(planSymbol({ ...plan, symbols }, 'DK_ABOUT')).toBe(1);
  expect(planSymbol({ ...plan, symbols: { _8DK_ABOUT: 1 } }, 'DK_ABOUT')).toBe(1);
  expect(() => planSymbol({ ...plan, symbols: { ...symbols, 'c:DK_ABOUT': 3 } }, 'DK_ABOUT')).toThrow('2 addresses');
  expect(() => planSymbol({ ...plan, symbols: {} }, 'DK_ABOUT')).toThrow('0 addresses');
});

test('setup patches once with a bounded tool deadline, uses its own daemon, then installs natively', async () => {
  const { calls, runtime } = fakeDk();
  expect(await prepareRemote(runtime)).toEqual(plan);
  expect(fake.exec.mock.calls.map((call) => call[1].timeout)).toEqual([30_000]);
  expect(fs.readFileSync).toHaveBeenCalledWith('/run/own-daemon/runtime-dt2.plan.json', 'utf8');
  expect(calls).toEqual(['put', 'verifyFile', 'select', 'restart', 'waitForBoot', 'modules']);
});

for (const stage of ['patch', 'install', 'plan'] as const) {
  test(`setup ${stage} failure cannot continue to navigation`, async () => {
    const { dk, runtime } = fakeDk();
    if (stage === 'patch') fake.exec.mockRejectedValueOnce(new Error('patch failed'));
    if (stage === 'install') dk.put.mockRejectedValueOnce(new Error('install failed'));
    if (stage === 'plan') readFiles(JSON.stringify({ ...plan, modules: ['wrong'] }));
    await expect(prepareRemote(runtime)).rejects.toThrow();
    expect(fake.exec).toHaveBeenCalledTimes(1);
    expect(dk.put).toHaveBeenCalledTimes(stage === 'install' ? 1 : 0);
  });
}

/** A paused fake daemon whose emulated clock advances 100 ms on every read. */
const releaseClock = () => {
  const { status, step } = fakeClock({ msPerRead: 100 });
  return emulatorClock({ status, client: { step } } as unknown as Pick<Session, 'status' | 'client'>);
};

test('navigation failure releases remote inputs; a hung release has a bounded deadline', async () => {
  const dk = { remoteRelease: vi.fn().mockResolvedValue(undefined) } as unknown as DkSession;
  await expect(
    withRemoteRelease(dk, releaseClock(), async () => {
      throw new Error('ABOUT missing');
    }),
  ).rejects.toThrow('ABOUT missing');
  const actionError = new Error('ABOUT missing');
  const releaseError = new Error('release failed');
  vi.mocked(dk.remoteRelease).mockRejectedValueOnce(releaseError);
  await expect(
    withRemoteRelease(dk, releaseClock(), async () => {
      throw actionError;
    }),
  ).rejects.toMatchObject({
    name: 'AggregateError',
    errors: [actionError, releaseError],
  });
  vi.mocked(dk.remoteRelease).mockImplementation(() => new Promise(() => {}));
  await expect(withRemoteRelease(dk, releaseClock(), async () => {})).rejects.toThrow('remote release timed out');
});

test('pollEmulated fails after the emulated time, counts a lowered clock as a restart and can retry a failing read', async () => {
  // 10 emulated ms per status read (132 000 instructions a millisecond), and a reboot that sets the clock back once.
  let elapsed = 0;
  let reads = 0;
  const clock = {
    status: vi.fn(async () => ({ elapsed: (elapsed = reads++ === 3 ? 0 : elapsed + 1_320_000) })),
  } as unknown as Session;
  await expect(
    pollEmulated(clock, async () => false, Boolean, { emulatedMs: 50, intervalMs: 1, message: 'never' }),
  ).rejects.toThrow('never: not seen within 50 ms of emulated time');
  // the clock reads 10, 20, 30 ms, then 0 after the reset (adds nothing), then 10, 20, 30 ms again: 50 ms are counted after 7 reads
  expect(clock.status).toHaveBeenCalledTimes(7);
  let failures = 2;
  const flaky = async () => {
    if (failures-- > 0) throw new Error('restarting');
    return true;
  };
  await expect(
    pollEmulated(clock, flaky, Boolean, { emulatedMs: 1000, intervalMs: 1, message: 'm', retry: true }),
  ).resolves.toBe(true);
  await expect(
    pollEmulated(
      clock,
      async () => {
        throw new Error('boom');
      },
      Boolean,
      { emulatedMs: 1000, intervalMs: 1, message: 'm' },
    ),
  ).rejects.toThrow('boom');
});

test('pollEmulated counts boot progress as movement only while a job runs', async () => {
  // The emulated clock stands still. A restart's cold boot moves job.icount: no trip. A running ColdFire without a job (a halted DSP) trips.
  let icount = 0;
  const booting = {
    status: vi.fn(async () => ({
      audio: { frames: 1000 },
      job: { kind: 'boot', phase: 'booting', icount: (icount += 1000), progress: 0.5, wall_seconds: 1 },
    })),
  } as unknown as Session;
  let reads = 0;
  const bootDone = async () => ++reads > 30;
  await expect(
    pollEmulated(booting, bootDone, Boolean, { emulatedMs: 1000, intervalMs: 5, hangMs: 40, message: 'm' }),
  ).resolves.toBe(true);
  // Everything stands still and no job runs (paused or dead): trip.
  let cf = 0;
  const frozen = {
    status: vi.fn(async () => ({
      mode: 'paused',
      audio: { frames: 1000 },
      elapsed: 5000,
      cf: { icount: (cf += 1000) },
    })),
  } as unknown as Session;
  await expect(
    pollEmulated(frozen, async () => false, Boolean, { emulatedMs: 1000, intervalMs: 5, hangMs: 40, message: 'm' }),
  ).rejects.toThrow('emulated clock stuck');
});

test('a halted DSP does not stop the emulated clock: the ColdFire instruction count advances it', async () => {
  // Frames frozen, 1 320 000 ColdFire instructions (10 emulated ms) per status read, no job: no stuck trip, the wait ends at emulatedMs.
  const make = () => {
    let elapsed = 0;
    return {
      status: vi.fn(async () => ({ mode: 'free', audio: { frames: 1000 }, elapsed: (elapsed += 1_320_000) })),
    } as unknown as Session;
  };
  const halted = make();
  await expect(
    pollEmulated(halted, async () => false, Boolean, { emulatedMs: 50, intervalMs: 1, hangMs: 40, message: 'm' }),
  ).rejects.toThrow('m: not seen within 50 ms of emulated time');
  let waits = 0;
  const sleeps: number[] = [];
  const probeOpts: unknown[] = [];
  const dk = {
    waitForBoot: async (sleep: (ms: number) => Promise<void>, _t?: number, opts?: { probe?: unknown }) => {
      probeOpts.push(typeof opts?.probe);
      for (; waits < 3; waits++) await sleep(1).then(() => sleeps.push(waits));
    },
  };
  bindBootClock(dk, make());
  await expect(waitForBootEmulated(dk)).resolves.toBeUndefined();
  expect(sleeps).toEqual([0, 1, 2]);
  // every probe runs under an emulated-time bound (see waitForBootEmulated)
  expect(probeOpts).toEqual(['function']);
  const late = {
    waitForBoot: async (sleep: (ms: number) => Promise<void>) => {
      for (;;) await sleep(1);
    },
  };
  bindBootClock(late, make());
  await expect(waitForBootEmulated(late, { emulatedMs: 50 })).rejects.toThrow('no new boot: not seen within 50 ms');
});

test('installer failure still releases remote inputs without reaching scenario actions', async () => {
  const { dk, runtime } = fakeDk();
  dk.put.mockRejectedValueOnce(new Error('install failed'));
  const action = vi.fn();
  await expect(
    withRemoteRelease(dk as unknown as DkSession, releaseClock(), async () => {
      await prepareRemote(runtime);
      action();
    }),
  ).rejects.toThrow('install failed');
  expect(action).not.toHaveBeenCalled();
  expect(dk.remoteRelease).toHaveBeenCalledTimes(1);
});

test('tagged dependency order and reordered untagged cache-hit reports identify the same loaded image', () => {
  const identity = session.loadResult.runtime;
  if (!identity?.patched) throw new Error('test requires a patched identity');
  for (const modules of [plan.modules, ['core/digikit-menu', 'core/runtime']]) {
    expect(() =>
      validateRuntimePlan(plan, {
        options: session.options,
        loadResult: { ...session.loadResult, runtime: { ...identity, modules } },
      }),
    ).not.toThrow();
  }
  expect(fake.read).not.toHaveBeenCalled();
  expect(() =>
    validateRuntimePlan(plan, {
      options: session.options,
      loadResult: { ...session.loadResult, runtime: { ...identity, modules: ['core/digikit-menu', 'different'] } },
    }),
  ).toThrow('module membership');
});

test('plan and actual loaded image require present matching SHA-256 digests', () => {
  for (const output of [null, { syx_sha256: '' }, { syx_sha256: 'b'.repeat(64) }]) {
    expect(() => validateRuntimePlan({ ...plan, output }, session)).toThrow('output hash');
  }
  for (const syx_sha256 of [undefined, '', 'b'.repeat(64)]) {
    expect(() =>
      validateRuntimePlan(plan, {
        options: session.options,
        loadResult: { ...session.loadResult, syx_sha256 },
      }),
    ).toThrow('output hash');
  }
});

test('negative setup installs natively on its own device-set daemon without patching or reading a symbol plan', async () => {
  const { calls, dk, runtime } = fakeDk();
  const negative = {
    ...runtime,
    session: { ...session, options: { ...session.options, runtime: { patchSet: 'device' } } },
  } as PatchedSession;
  await installRemote(negative);
  expect(fake.exec).not.toHaveBeenCalled();
  // the module is read to upload it and to find the services it needs
  expect(fake.read).toHaveBeenCalledWith(expect.stringMatching(/dt2\/remote\.dkm$/));
  expect(calls).toEqual(['put', 'verifyFile', 'select', 'restart', 'waitForBoot', 'modules']);
  expect(dk.put.mock.calls[0]?.[0]).toBe('modules/remote.dkm');
  expect(dk.verifyFile.mock.calls[0]?.[0]).toBe('modules/remote.dkm');
  expect(dk.select).toHaveBeenCalledExactlyOnceWith(['modules/remote.dkm']);
});

test('ABOUT reads are bounded and assert the live protocol runtime identity without a Python formatter', async () => {
  const memPeek = vi.fn().mockResolvedValue(25);
  const menuSession = { client: { memPeek } } as unknown as Session;
  await expect(readAboutRows(menuSession, plan)).rejects.toThrow('invalid ABOUT row count');
  expect(memPeek.mock.calls).toEqual([[8192]]);
  const rows = ['DIGIKIT 0.1.0', 'RUNTIME 0.2.5', 'BOOT: 1 OF 1 OK', 'RAM KEPT: NO'];
  const bytes = Buffer.from(rows.map((row) => row.padEnd(24, '\0')).join(''), 'latin1');
  memPeek.mockImplementation(async (address: number) =>
    address === 8192 ? rows.length : bytes.readUInt32BE(address - 4096),
  );
  const info = vi.fn().mockResolvedValue({ runtime: 0x205 });
  await expect(expectRuntimeAbout(menuSession, { info } as unknown as DkSession, plan, '/artifacts')).resolves.toEqual(
    rows,
  );
  expect(info).toHaveBeenCalledWith();
  expect(fake.exec).not.toHaveBeenCalled();
});

test('UI readiness rejects stale epochs and records the last daemon and boot evidence on timeout', async () => {
  const { daemon, status } = readyDaemon();
  await expect(
    expectRuntimeUi(daemon, plan, '/artifacts', { afterEpoch: status.epoch, emulatedMs: 20 }),
  ).rejects.toThrow();
  const saved = fake.write.mock.calls.find(([file]) => file.endsWith('readiness-post-install.json'));
  expect(JSON.parse(saved![1])).toMatchObject({ afterEpoch: 4, status: { epoch: 4 }, boot: { ui_reached: 1 } });
  status.epoch++;
  await expect(expectRuntimeUi(daemon, plan, '/artifacts', { afterEpoch: 4, emulatedMs: 20 })).resolves.toBe(5);
});

test('UI readiness requires a valid normal boot and both daemon and actual runtime UI hooks', async () => {
  const { daemon, status, words } = readyDaemon();
  for (const [offset, invalid] of [
    [0, 0],
    [4, 2],
    [8, 1],
    [12, 1],
    [36, 0],
    [0, null],
  ] as const) {
    const previous = words[offset]!;
    words[offset] = invalid;
    await expect(expectRuntimeUi(daemon, plan, '/artifacts', { emulatedMs: 20 })).rejects.toThrow();
    words[offset] = previous;
  }
  for (const field of ['ready', 'main_ui_reached'] as const) {
    status.cf[field] = false;
    await expect(expectRuntimeUi(daemon, plan, '/artifacts', { emulatedMs: 20 })).rejects.toThrow();
    status.cf[field] = true;
  }
  // A paused daemon's clock stands still: the stuck-clock guard fails the wait, naming the mode.
  status.mode = 'paused';
  await expect(expectRuntimeUi(daemon, plan, '/artifacts', { emulatedMs: 20, hangMs: 50 })).rejects.toThrow(
    /emulated clock stuck at .* \(mode paused\)/,
  );
  const stuck = fake.write.mock.calls.filter(([file]) => file.endsWith('readiness-post-install.json')).at(-1);
  expect(JSON.parse(stuck![1])).toMatchObject({ expired: false, stopped: expect.stringContaining('stuck') });
  status.mode = 'free';
  await expect(expectRuntimeUi(daemon, plan, '/artifacts', { emulatedMs: 20 })).resolves.toBe(4);
}, 20_000);

test('fresh empty boot is an explicit prerequisite for skipping the pre-install stability wait', async () => {
  const { daemon, words } = readyDaemon();
  for (const offset of [16, 28]) {
    words[offset] = 1;
    await expect(expectRuntimeUi(daemon, plan, '/artifacts', { fresh: true, emulatedMs: 20 })).rejects.toThrow(
      'fresh pre-install',
    );
    words[offset] = 0;
  }
  await expect(expectRuntimeUi(daemon, plan, '/artifacts', { fresh: true, emulatedMs: 20 })).resolves.toBe(4);
});

/** The wait API over a fake session: the clock steps with `client.step`, inputs are drained at once. */
function fakeRuntime(client: Record<string, unknown>, dk: Record<string, unknown>) {
  const { status, step } = fakeClock();
  const daemon = { ...session, status, client: { step, inputTap: vi.fn().mockResolvedValue({}), ...client } };
  return withWaitApi({ session: daemon, dk, close: async () => {} } as unknown as PatchedSession);
}

test('ABOUT refuses stale content before pressing YES', async () => {
  const memPeek = vi.fn().mockResolvedValue(4);
  const rt = fakeRuntime({ memPeek }, {});
  await expect(openRuntimeAbout(rt, plan, '/artifacts')).rejects.toThrow('ABOUT buffer is empty');
  expect(rt.session.client.inputTap).not.toHaveBeenCalled();
  const saved = fake.write.mock.calls.find(([file]) => file.endsWith('about-before-open.json'));
  expect(JSON.parse(saved![1])).toMatchObject({ initialCount: 4 });
});

test('ABOUT returns only the complete snapshot after the new open', async () => {
  const rows = ['DIGIKIT 0.1.0', 'RUNTIME 0.2.5', 'BOOT: 1 OF 1 OK', 'RAM KEPT: YES'];
  const bytes = Buffer.from(rows.map((row) => row.padEnd(24, '\0')).join(''), 'latin1');
  let opened = false;
  let counts = 0;
  const memPeek = vi.fn(async (address: number) => {
    // Pre-open is empty; after the open the buffer fills: identity only, then BOOT, then the final RAM row (each poll reads the count twice).
    if (address === 8192) return opened ? [2, 3, 4][Math.min(Math.floor(counts++ / 2), 2)]! : 0;
    return bytes.readUInt32BE(address - 4096);
  });
  const rt = fakeRuntime(
    { memPeek, inputTap: vi.fn(async () => ((opened = true), {})) },
    { info: vi.fn().mockResolvedValue({ runtime: 0x205 }) },
  );
  await expect(openRuntimeAbout(rt, plan, '/artifacts')).resolves.toEqual(rows);
  expect(rt.session.client.inputTap).toHaveBeenCalledTimes(1);
  expect(counts).toBeGreaterThanOrEqual(6);
});

/** A request nobody answers: the emulated probe (as `emulatorDk` bounds every real one) ends it after `ms` emulated ms. */
const lostRequest = <T>(session: Session, ms = 30) =>
  withEmulatedProbe(
    session,
    ({ signal }) =>
      new Promise<T>((_resolve, reject) =>
        signal.addEventListener('abort', () => reject(new Error('request lost')), { once: true }),
      ),
    ms,
  );

test('a never-answering remote state request obeys the readiness deadline', async () => {
  const { status, step } = fakeClock({ msPerRead: 100, mode: 'free' });
  const clockSession = { ...session, status, client: { step } } as unknown as Session;
  const dk = { remoteState: vi.fn(() => lostRequest(clockSession)) } as unknown as DkSession;
  await expect(expectRemoteEnabled(clockSession, dk, 30)).rejects.toThrow('request lost');
  const saved = fake.write.mock.calls.find(([file]) => file.endsWith('remote-readiness.json'));
  expect(JSON.parse(saved![1])).toMatchObject({ state: null, expired: true });
}, 30_000);

test('post-restart UI readiness allows an unobserved filesystem worker only with a newer epoch', async () => {
  const { daemon, status } = readyDaemon();
  status.cf.ready = false;
  await expect(expectRuntimeUi(daemon, plan, '/artifacts', { afterEpoch: 3, emulatedMs: 20 })).resolves.toBe(4);
  await expect(expectRuntimeUi(daemon, plan, '/artifacts', { emulatedMs: 20 })).rejects.toThrow();
  await expect(expectRuntimeUi(daemon, plan, '/artifacts', { afterEpoch: 4, emulatedMs: 20 })).rejects.toThrow();
  status.cf.filesystem_verified = false;
  await expect(expectRuntimeUi(daemon, plan, '/artifacts', { afterEpoch: 3, emulatedMs: 20 })).rejects.toThrow();
  status.cf.ready = true;
  await expect(expectRuntimeUi(daemon, plan, '/artifacts', { afterEpoch: 3, emulatedMs: 20 })).rejects.toThrow();
});

test('offline preparation returns a symbol plan before any loaded-image identity exists', async () => {
  const prepared = await prepareRuntimePlanOffline({ device: 'dt2', firmware: '1.17' }, '/offline', '/artifacts', [
    'DKRT_BOOT',
  ]);
  expect(prepared.plan).toEqual(plan);
  expect(prepared.planFile).toBe('/offline/runtime-dt2.plan.json');
  // Offline availability never substitutes for the explicit loaded-image hash validation.
  expect(() =>
    validateRuntimePlan(prepared.plan, {
      ...session,
      loadResult: { ...session.loadResult, syx_sha256: 'd'.repeat(64) },
    }),
  ).toThrow('output hash');
});

test('prepared plan checks exact closure and artifact without requiring or synthesizing runtime identity', () => {
  const preparedSession = {
    ...session,
    loadResult: { syx_sha256: patchedSha256 },
    preparedRuntimeImage: {
      syx: '/prepared.syx',
      baseSha256: plan.base.syx_sha256,
      syxSha256: patchedSha256,
      modules: ['core/runtime', 'core/digikit-menu'],
    },
  };
  expect(() => validateRuntimePlan(plan, preparedSession)).not.toThrow();
  expect(() => validateRuntimePlan({ ...plan, modules: ['core/runtime'] }, preparedSession)).toThrow(
    'module membership',
  );
  expect(() => validateRuntimePlan({ ...plan, output: { syx_sha256: 'b'.repeat(64) } }, preparedSession)).toThrow(
    'output hash',
  );
});

for (const invalid of [null, 'root', 'artifact'] as const) {
  test(`explicit offline preparation pins expanded membership and exact output (${invalid ?? 'valid'})`, async () => {
    const digest = (text: string) => createHash('sha256').update(text).digest('hex');
    const explicitPlan = {
      ...plan,
      base: { ...plan.base, syx_sha256: digest('stock') },
      output: { syx_sha256: digest('prepared') },
    };
    fake.read.mockImplementation((file: string) => {
      if (file.endsWith('.plan.json')) return JSON.stringify(explicitPlan);
      if (file === '/stock.syx') return Buffer.from('stock');
      return Buffer.from(invalid === 'artifact' ? 'changed' : 'prepared');
    });
    const preparing = prepareRuntimePlanModulesOffline(
      { device: 'dt2', firmware: '1.17' },
      '/offline',
      '/artifacts',
      ['DKRT_BOOT'],
      { modules: [invalid === 'root' ? 'core/missing' : 'core/digikit-menu'] },
    );
    if (invalid) await expect(preparing).rejects.toThrow();
    else {
      const prepared = await preparing;
      expect(prepared.preparedImage).toEqual({
        syx: '/offline/runtime-dt2.syx',
        baseSha256: digest('stock'),
        syxSha256: digest('prepared'),
        modules: ['core/runtime', 'core/digikit-menu'],
      });
    }
  });
}

test('an emulated probe ends after its emulated bound, however slow the wall clock is', async () => {
  // a fake session: every status read is 10 emulated ms (1 320 000 ColdFire instructions) on a clock that ignores the wall
  let elapsed = 0;
  const session = {
    status: vi.fn(async () => ({ mode: 'free', elapsed: (elapsed += 1_320_000) })),
  } as unknown as Session;
  // a probe that is never answered: only the signal ends it, after 200 emulated ms (about 20 reads)
  const lost = await withEmulatedProbe(
    session,
    ({ signal }) =>
      new Promise<string>((done) => signal.addEventListener('abort', () => done('ended'), { once: true })),
    200,
  );
  expect(lost).toBe('ended');
  expect(session.status).toHaveBeenCalled();
  // a probe that answers (wall-slow, emulated-fast) is not cut: the bound is emulated time, not the reply's wall time
  const slow = await withEmulatedProbe(session, async () => 'answer', 60_000);
  expect(slow).toBe('answer');
});

test('a provider refused by the declare-check names the item that differs', () => {
  const mach = 0x6d616368;
  const index = 0xffff_fffe;
  const e = (res: number, hash: number, base: number) => providerFailureLine([index, res >>> 0, mach, hash, base]);
  expect(e(-79, 0x946ae751, 7 | 0x100)).toBe('provider mach res -79 E_DECL_KIND (EXPORTS 946ae751 not declared)');
  expect(e(-79, 0x00000006, 4)).toBe('provider mach res -79 E_DECL_KIND (KINDS 00000006 not in the header)');
  expect(e(-79, 0x12345678, 0)).toBe('provider mach res -79 E_DECL_KIND');
  expect(e(-7, 0x12345678, 0)).toBe('provider mach res -7 E_IMPORT');
});
