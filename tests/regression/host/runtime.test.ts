import fs from 'node:fs';
import { createHash } from 'node:crypto';
import path from 'node:path';
import { beforeEach, expect, test, vi } from 'vitest';

const fake = vi.hoisted(() => {
  const client = {
    eventsSubscribe: vi.fn(),
    cardLoad: vi.fn(),
    firmwareLoad: vi.fn(),
    watchdogConfig: vi.fn(),
    status: vi.fn(),
    drainEvents: vi.fn(),
    guardSet: vi.fn(),
    call: vi.fn(),
  };
  return { client, close: vi.fn(), spawn: vi.fn(), coupledEnv: vi.fn() };
});
vi.mock('@digi/control-client', async (original) => ({
  ...(await original<object>()),
  spawnDaemon: fake.spawn,
}));
vi.mock('../../../packages/test-support/src/inputs.ts', async (original) => ({
  ...(await original<object>()),
  validateRuntimeInputs: vi.fn(),
  coupledEnv: fake.coupledEnv,
  resolveInputs: () => ({ syx: '/stock.syx', cfSnapshot: '/ready.snap', dspImage: '/dsp.bin', dspState: '/dsp.snap' }),
}));
import { PRE_SOCKET_HANG_MS, Session } from '@digi/test-support';
import { startPatchedSession } from '../support.ts';
import { startNoProgressWatchdog } from '../watchdog.ts';

const identity = {
  patched: true as const,
  set: 'emulator',
  cache_hit: false,
  modules: ['digikit'],
  base_sha256: 'a'.repeat(64),
  cache_file: '/cache.syx',
  seconds: 0.1,
};
beforeEach(() => {
  vi.resetAllMocks();
  fake.coupledEnv.mockReturnValue({ SHARC_FAST_REGIONS: 'fixture-regions' });
  fake.spawn.mockResolvedValue({ client: fake.client, close: fake.close });
  fake.client.cardLoad.mockResolvedValue({ card: '/card.img' });
  fake.client.firmwareLoad.mockResolvedValue({ runtime: identity, start_used: 'snapshot' });
  fake.client.call.mockResolvedValue({ hex: '' });
  fake.client.status.mockResolvedValue({ epoch: 2, mode: 'free', cf: { ready: true } });
  fake.client.drainEvents.mockReturnValue([]);
});

test('stock defaults retain snapshot load and stepped lifecycle', async () => {
  const session = await Session.start({ device: 'dt2', firmware: '1.17', dsp: 'zero' });
  try {
    expect(fake.client.firmwareLoad).toHaveBeenCalledWith({
      syx: '/stock.syx',
      start: 'snapshot',
      force: true,
      auto_no: true,
    });
    expect(fake.client.call).not.toHaveBeenCalled();
    expect(fake.coupledEnv).not.toHaveBeenCalled();
    expect(fake.spawn.mock.calls[0]?.[0].env).toBeUndefined();
    expect(fake.spawn.mock.calls[0]?.[0].args).toEqual(['--dsp', 'zero', '--cf-snapshot', '/ready.snap']);
  } finally {
    await session.close();
  }
});

test('zero-DSP runtime startup selects the firmware before applying the runtime patch', async () => {
  const session = await Session.start({ device: 'dt2', firmware: '1.17', dsp: 'zero', runtime: {} });
  try {
    expect(fake.spawn.mock.calls[0]?.[0].args).toEqual(['/stock.syx', '--dsp', 'zero', '--cf-snapshot', '/ready.snap']);
    expect(fake.client.firmwareLoad).toHaveBeenCalledWith({
      syx: '/stock.syx',
      start: 'auto',
      force: true,
      auto_no: true,
      runtime: true,
      patch_set: undefined,
    });
    expect(fake.client.call).toHaveBeenCalledWith('run.mode', { mode: 'free' });
  } finally {
    await session.close();
  }
});

test('patched fixture preserves zero-DSP startup while loading the runtime firmware', async () => {
  const runtime = await startPatchedSession({ device: 'dn2', firmware: '1.12', dsp: 'zero' });
  try {
    expect(fake.coupledEnv).not.toHaveBeenCalled();
    expect(fake.client.firmwareLoad).toHaveBeenCalledWith(
      expect.objectContaining({ runtime: true, syx: '/stock.syx' }),
    );
  } finally {
    await runtime.close();
  }
});

test('a mirrored runtime owns its mirror inside the temporary run directory', async () => {
  const runtime = await startPatchedSession({
    device: 'dn2',
    firmware: '1.12',
    dsp: 'sharc',
    mirror: true,
    autoNo: false,
  });
  const runDir = runtime.session.runDir;
  try {
    expect(fs.existsSync(runDir)).toBe(true);
    expect(fake.coupledEnv).toHaveBeenCalledWith('/stock.syx');
    expect(fake.spawn.mock.calls[0]?.[0].env).toEqual({ SHARC_FAST_REGIONS: 'fixture-regions' });
    expect(fake.client.firmwareLoad).toHaveBeenCalledWith(expect.objectContaining({ auto_no: false }));
    expect(fake.spawn.mock.calls[0]?.[0].args.slice(-2)).toEqual(['--mirror-dir', `${runDir}/mirror`]);
    fs.mkdirSync(`${runDir}/mirror/digikit/modules`, { recursive: true });
    fs.writeFileSync(`${runDir}/mirror/digikit/modules/pluck.dkm`, 'owned test data');
  } finally {
    await runtime.close();
  }
  expect(fs.existsSync(runDir), 'close removes the owned mirror along with its run directory').toBe(false);
});

test('patched fixture loads the card, requests runtime auto boot and free mode, and wires USB RPC', async () => {
  const runtime = await startPatchedSession({
    device: 'dt2',
    firmware: '1.17',
    dsp: 'sharc',
    card: '/samples',
    patchSet: 'emulator',
  });
  try {
    expect(fake.client.firmwareLoad).toHaveBeenCalledWith({
      syx: '/stock.syx',
      start: 'auto',
      force: true,
      auto_no: true,
      runtime: true,
      patch_set: 'emulator',
      card: '/card.img',
    });
    expect(fake.spawn.mock.calls[0]?.[0].args).toEqual([
      '/stock.syx',
      '--auto-coupled',
      '--audio-profile-syx',
      '/stock.syx',
      '--dsp-image',
      '/dsp.bin',
      '--dsp-state',
      '/dsp.snap',
      '--cf-snapshot',
      '/ready.snap',
      '--no-audio',
    ]);
    expect(fake.client.call).toHaveBeenCalledWith('run.mode', { mode: 'free' });
    expect(runtime.session.loadResult.runtime).toEqual(identity);
    await expect(runtime.session.restart()).rejects.toThrow('fresh daemon');
    await expect(runtime.session.wait(1)).rejects.toThrow('run freely');
    const pending = runtime.dk.request(1);
    const rejected = expect(pending).rejects.toMatchObject({ kind: 'cancelled' });
    await vi.waitFor(() => expect(fake.client.call).toHaveBeenCalledWith('midi.usb.out', undefined));
    expect(fake.client.call.mock.calls.some(([method]) => method === 'midi.usb.in')).toBe(true);
    await runtime.close();
    await rejected;
    await runtime.close();
    expect(fake.close).toHaveBeenCalledTimes(1);
  } finally {
    await runtime.close();
  }
});

for (const failure of ['firmwareLoad', 'watchdogConfig', 'call'] as const) {
  test(`startup ${failure} failure closes its daemon`, async () => {
    fake.client[failure].mockRejectedValueOnce(new Error('startup failed'));
    await expect(startPatchedSession({ device: 'dn2', firmware: '1.12' })).rejects.toThrow('startup failed');
    expect(fake.close).toHaveBeenCalledTimes(1);
  });
}

test('daemon spawn failure keeps the owned temporary run directory for diagnostics', async () => {
  fake.spawn.mockRejectedValueOnce(new Error('spawn failed'));
  await expect(startPatchedSession({ device: 'dn2', firmware: '1.12' })).rejects.toThrow('spawn failed');
  const runDir = fake.spawn.mock.calls[0]?.[0].runDir;
  expect(runDir).toBeTruthy();
  expect(fs.existsSync(runDir)).toBe(true);
  fs.rmSync(runDir, { recursive: true, force: true });
});

test('a watchdog trip during the startup load fails the load and closes the owned daemon', async () => {
  let failLoad!: (error: Error) => void;
  fake.client.firmwareLoad.mockImplementation(
    () =>
      new Promise((_resolve, reject) => {
        failLoad = reject;
      }),
  );
  // The watchdog closes the client socket, which fails the pending RPC (as the real client does).
  (fake.client as { close?: () => void }).close = () => failLoad(new Error('connection closed'));
  let watchdog: ReturnType<typeof startNoProgressWatchdog> | undefined;
  const started = Session.start({
    device: 'dn2',
    firmware: '1.12',
    dsp: 'zero',
    onSession: (session) => {
      watchdog = startNoProgressWatchdog(session, { intervalMs: 5, limitMs: 30, log: () => {} });
    },
  });
  await expect(started).rejects.toThrow('connection closed');
  expect(watchdog?.tripped).toContain('no emulated progress');
  expect(fake.close).toHaveBeenCalled();
  const runDir = fake.spawn.mock.calls[0]?.[0].runDir;
  fs.rmSync(runDir, { recursive: true, force: true });
});

test('environment preparation that never finishes fails at the hang guard and spawns no daemon', async () => {
  vi.useFakeTimers();
  let directory = '';
  try {
    const started = Session.start({
      device: 'dn2',
      firmware: '1.12',
      dsp: 'sharc',
      runtime: { reuse: true },
      env: (runDir) => {
        directory = runDir;
        return new Promise(() => {});
      },
    });
    const failed = expect(started).rejects.toThrow('environment preparation hung');
    await vi.advanceTimersByTimeAsync(PRE_SOCKET_HANG_MS);
    await failed;
    expect(fake.spawn).not.toHaveBeenCalled();
    fs.rmSync(directory, { recursive: true, force: true });
  } finally {
    vi.useRealTimers();
  }
});

test('concurrent fixture and Session closes await the same daemon cleanup', async () => {
  let finishClose!: () => void;
  fake.close.mockImplementation(
    () =>
      new Promise<void>((resolve) => {
        finishClose = resolve;
      }),
  );
  const runtime = await startPatchedSession({ device: 'dn2', firmware: '1.12' });
  const first = runtime.close();
  const second = runtime.close();
  expect(second).toBe(first);
  const sessionClose = runtime.session.close();
  expect(runtime.session.close()).toBe(sessionClose);
  expect(fake.close).toHaveBeenCalledTimes(1);
  let closed = false;
  void second.then(() => {
    closed = true;
  });
  await Promise.resolve();
  expect(closed).toBe(false);
  finishClose();
  await Promise.all([first, second, sessionClose]);
  expect(closed).toBe(true);
});

async function reusableSession() {
  const session = await Session.start({ device: 'dn2', firmware: '1.12', dsp: 'sharc', runtime: { reuse: true } });
  const directory = path.join(session.runDir, 'pristine');
  fs.mkdirSync(directory);
  const cardImage = path.join(directory, 'card.img');
  fs.writeFileSync(cardImage, 'host fixture card');
  const baseline = { cardImage, guard: { protect: [{ lo: 16, hi: 32, label: 'probe' }], allow_pc: [] } };
  return { session, baseline };
}

test('reusable runtime rejects mirroring before starting a daemon', async () => {
  await expect(
    Session.start({ device: 'dn2', firmware: '1.12', dsp: 'sharc', mirror: true, runtime: { reuse: true } }),
  ).rejects.toThrow('cannot mirror');
  expect(fake.spawn).not.toHaveBeenCalled();
});

test('baseline reload replaces Session identity/card/events and reapplies actor and guard policy', async () => {
  const { session, baseline } = await reusableSession();
  try {
    fake.client.drainEvents.mockReturnValueOnce([{ kind: 'fault', message: 'old case' }]);
    expect(await session.faults()).toHaveLength(1);
    const loaded = { runtime: { ...identity, cache_file: '/reloaded.syx' }, start_used: 'cold' };
    fake.client.firmwareLoad.mockResolvedValueOnce(loaded);
    fake.client.status
      .mockResolvedValueOnce({ epoch: 2, job: null })
      .mockResolvedValueOnce({ epoch: 3, mode: 'free', cf: { ready: true } });
    expect(await session.reloadRuntimeBaseline(baseline)).toBe(3);
    expect(session.cardPath).toBe(fs.realpathSync(baseline.cardImage));
    expect(session.loadResult).toBe(loaded);
    expect(await session.faults()).toEqual([]);
    expect(fake.client.firmwareLoad).toHaveBeenLastCalledWith(
      expect.objectContaining({ card: fs.realpathSync(baseline.cardImage), replace: true, runtime: true, force: true }),
    );
    expect(fake.client.call).toHaveBeenCalledWith('cpu.exceptions', { deliver: false });
    expect(fake.client.guardSet).toHaveBeenLastCalledWith(baseline.guard);
    expect(fake.spawn).toHaveBeenCalledTimes(1);
  } finally {
    await session.close();
  }
});

for (const invalid of ['outside', 'symlink', 'same-epoch', 'busy', 'load-error'] as const) {
  test(`unusable ${invalid} baseline discards only the owned daemon and invalidates Session metadata`, async () => {
    const { session, baseline } = await reusableSession();
    if (invalid === 'outside') baseline.cardImage = '/private/tmp/card.img';
    if (invalid === 'symlink') {
      fs.unlinkSync(baseline.cardImage);
      fs.symlinkSync('/etc/hosts', baseline.cardImage);
    }
    if (invalid === 'busy') fake.client.firmwareLoad.mockRejectedValueOnce(new Error('patch job busy'));
    if (invalid === 'load-error') fake.client.firmwareLoad.mockRejectedValueOnce(new Error('load failed'));
    await expect(session.reloadRuntimeBaseline(baseline)).rejects.toThrow();
    expect(fake.close).toHaveBeenCalledTimes(1);
    expect(() => session.loadResult).toThrow('not loaded');
    expect(session.cardPath).toBeUndefined();
    await expect(session.reloadRuntimeBaseline(baseline)).rejects.toThrow('closed');
    expect(fs.existsSync(path.join(session.runDir, 'runtime-baseline-error.json'))).toBe(true);
    fs.rmSync(session.runDir, { recursive: true, force: true });
  });
}

test('reuse starts subscribed with no SDK card or patched load until the explicit baseline', async () => {
  const session = await Session.start({ device: 'dn2', firmware: '1.12', dsp: 'sharc', runtime: { reuse: true } });
  try {
    expect(fake.client.eventsSubscribe).toHaveBeenCalledOnce();
    expect(fake.client.cardLoad).not.toHaveBeenCalled();
    expect(fake.client.firmwareLoad).not.toHaveBeenCalled();
    expect(fake.client.watchdogConfig).not.toHaveBeenCalled();
    expect(fake.client.call).not.toHaveBeenCalled();
    expect(() => session.loadResult).toThrow('not loaded');
  } finally {
    await session.close();
  }
});

test('startup prepares instrumentation before spawn and preserves coupled defaults', async () => {
  const env = vi.fn(async (runDir: string) => {
    expect(fake.spawn).not.toHaveBeenCalled();
    expect(fs.existsSync(runDir)).toBe(true);
    return { DIGI_WRITE_WATCH_OUT: path.join(runDir, 'watch.jsonl') };
  });
  const session = await Session.start({ device: 'dn2', firmware: '1.12', dsp: 'sharc', runtime: { reuse: true }, env });
  try {
    expect(env).toHaveBeenCalledExactlyOnceWith(session.runDir);
    expect(fake.spawn.mock.calls[0]![0].env).toMatchObject({
      SHARC_FAST_REGIONS: 'fixture-regions',
      DIGI_WRITE_WATCH_OUT: path.join(session.runDir, 'watch.jsonl'),
    });
  } finally {
    await session.close();
  }
});

test('failed environment preparation keeps the owned directory and does not spawn', async () => {
  let directory = '';
  await expect(
    Session.start({
      device: 'dn2',
      firmware: '1.12',
      dsp: 'sharc',
      runtime: { reuse: true },
      env: async (runDir) => {
        directory = runDir;
        throw new Error('offline plan failed');
      },
    }),
  ).rejects.toThrow('offline plan failed');
  expect(fake.spawn).not.toHaveBeenCalled();
  expect(fs.existsSync(directory)).toBe(true);
  fs.rmSync(directory, { recursive: true, force: true });
});

for (const mismatch of [null, 'artifact', 'stock', 'load', 'status-image', 'status-base'] as const) {
  test(`prepared baseline validates exact bytes and loaded identity (${mismatch ?? 'valid'})`, async () => {
    const { session, baseline } = await reusableSession();
    const syx = path.join(session.runDir, 'prepared.syx');
    const hash = (text: string) => createHash('sha256').update(text).digest('hex');
    fs.writeFileSync(syx, 'prepared bytes');
    const preparedImage = {
      syx,
      baseSha256: hash('stock bytes'),
      syxSha256: hash('prepared bytes'),
      modules: ['core/runtime'],
    };
    const read = fs.readFileSync.bind(fs);
    const spy = vi
      .spyOn(fs, 'readFileSync')
      .mockImplementation(((file: fs.PathOrFileDescriptor, ...args: unknown[]) =>
        file === '/stock.syx'
          ? Buffer.from(mismatch === 'stock' ? 'different stock' : 'stock bytes')
          : read(file, ...(args as []))) as typeof fs.readFileSync);
    try {
      if (mismatch === 'artifact') fs.writeFileSync(syx, 'changed bytes');
      fake.client.firmwareLoad.mockResolvedValueOnce({
        syx_sha256: mismatch === 'load' ? 'bad' : preparedImage.syxSha256,
        start_used: 'cold',
      });
      fake.client.status.mockResolvedValueOnce({ epoch: 2 }).mockResolvedValueOnce({
        epoch: 3,
        mode: 'free',
        cf: { ready: true },
        firmware: {
          syx_sha256: mismatch === 'status-image' ? 'bad' : preparedImage.syxSha256,
          base_sha256: mismatch === 'status-base' ? 'bad' : preparedImage.baseSha256,
        },
      });
      if (mismatch) {
        await expect(session.reloadRuntimeBaseline({ ...baseline, preparedImage })).rejects.toThrow('prepared runtime');
        expect(() => session.loadResult).toThrow('not loaded');
        expect(session.preparedRuntimeImage).toBeUndefined();
        expect(fake.close).toHaveBeenCalledTimes(1);
      } else {
        await session.reloadRuntimeBaseline({ ...baseline, preparedImage });
        expect(session.preparedRuntimeImage).toEqual(preparedImage);
        const load = fake.client.firmwareLoad.mock.calls[0]![0];
        expect(load.syx).toBe(syx);
        expect(load.base_sha256).toBe(preparedImage.baseSha256);
        expect(load.runtime).toBeUndefined();
        // Retain the startup stock bytes for the daemon's DSP diff, while still reconstructing
        // the firmware with this pristine card rather than taking its hot-load path.
        expect(load.replace).toBe(false);
        expect(load.force).toBe(true);
        expect(load.start).toBe('auto');
        expect(load.card).toBe(fs.realpathSync(baseline.cardImage));
        expect(session.loadResult.runtime).toBeUndefined();
      }
    } finally {
      spy.mockRestore();
      await session.close();
      fs.rmSync(session.runDir, { recursive: true, force: true });
    }
  });
}
