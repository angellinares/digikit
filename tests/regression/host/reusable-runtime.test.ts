import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { expect, test, vi } from 'vitest';
import type { RuntimeBaseline } from '@digi/test-support';

const fake = vi.hoisted(() => ({
  beforeAll: vi.fn(),
  beforeEach: vi.fn(),
  afterEach: vi.fn(),
  afterAll: vi.fn(),
  start: vi.fn(),
}));
vi.mock('vitest', async (original) => ({
  ...(await original<object>()),
  beforeAll: fake.beforeAll,
  beforeEach: fake.beforeEach,
  afterEach: fake.afterEach,
  afterAll: fake.afterAll,
}));
vi.mock('@digi/test-support', async (original) => ({ ...(await original<object>()), Session: { start: fake.start } }));
import { flushFileTemps } from '../file-temp.ts';
import { withReusablePatchedSession } from '../support.ts';

async function fixture(
  options: {
    startDelay?: number;
    hangPreparation?: boolean;
    afterClose?: (session: unknown) => Promise<void>;
    tempDirs?: () => string[];
  } = {},
) {
  vi.resetAllMocks();
  const calls: string[] = [];
  let epoch = 2;
  const call = vi.fn(async (method: string, params?: Record<string, unknown>) => {
    calls.push(method === 'run.mode' ? `mode:${params?.mode}` : method);
    return { hex: '' };
  });
  const session = {
    runDir: '/fixture/owned',
    options: { device: 'dn2' },
    dsp: 'sharc',
    client: { call },
    reloadRuntimeBaseline: vi.fn(async () => {
      calls.push('load');
      return ++epoch;
    }),
    close: vi.fn(async () => {
      calls.push('closed');
    }),
  };
  fake.start.mockImplementation(async () => {
    if (options.startDelay) await new Promise((resolve) => setTimeout(resolve, options.startDelay));
    return session;
  });
  const baseline: RuntimeBaseline = { cardImage: '/fixture/owned/base/card.img', guard: { protect: [], allow_pc: [] } };
  const finish = vi.fn(async () => {
    calls.push('evidence');
  });
  const handle = withReusablePatchedSession(
    { device: 'dn2', firmware: '1.12', dsp: 'sharc' },
    {
      prepareBaseline: async (owned) => {
        expect(owned).toBe(session);
        if (options.hangPreparation) await new Promise(() => {});
        return baseline;
      },
      finishCase: finish,
      afterClose: options.afterClose,
      tempDirs: options.tempDirs,
    },
  );
  const beforeAll = fake.beforeAll.mock.calls[0]![0] as () => Promise<void>;
  const beforeEach = fake.beforeEach.mock.calls[0]![0] as () => Promise<void>;
  const afterEach = fake.afterEach.mock.calls[0]![0] as (context: object) => Promise<void>;
  const afterAll = fake.afterAll.mock.calls[0]![0] as () => Promise<void>;
  await beforeAll();
  await beforeEach();
  return {
    handle,
    session,
    calls,
    call,
    finish,
    baseline,
    afterEach: (state = 'pass') => afterEach({ task: { result: { state } } }),
    afterAll,
  };
}

test('a transition collects evidence and pauses before the same daemon loads a fresh protocol case', async () => {
  const f = await fixture();
  const old = f.handle.runtime;
  const next = await f.handle.reload({ ...f.baseline, cardImage: '/fixture/owned/wet/card.img' });
  expect(f.calls).toEqual(['load', 'evidence', 'pcm.tap', 'mode:paused', 'load']);
  expect(next.session).toBe(old.session);
  expect(next.epoch).toBeGreaterThan(old.epoch);
  expect(next.dk).not.toBe(old.dk);
  await expect(old.close()).rejects.toThrow('no longer active');
  expect(f.session.close).not.toHaveBeenCalled();
  await f.afterEach();
  await f.afterAll();
  expect(f.session.close).toHaveBeenCalledTimes(1);
});

test('transition cancels pending protocol work and prevents an old protocol from reaching the new runtime', async () => {
  const f = await fixture();
  const old = f.handle.runtime;
  const pending = expect(old.dk.request(1)).rejects.toMatchObject({ kind: 'cancelled' });
  await vi.waitFor(() => expect(f.call).toHaveBeenCalledWith('midi.usb.out', undefined));
  await f.handle.reload(f.baseline);
  await pending;
  const calls = f.call.mock.calls.length;
  await expect(old.dk.request(1)).rejects.toThrow();
  expect(f.call).toHaveBeenCalledTimes(calls);
  await f.afterEach();
  await f.afterAll();
});

test('failed closing evidence discards the owned daemon without loading another baseline', async () => {
  const f = await fixture();
  f.finish.mockRejectedValueOnce(new Error('guard hit'));
  await expect(f.handle.reload(f.baseline)).rejects.toThrow('owned daemon discarded');
  expect(f.session.reloadRuntimeBaseline).toHaveBeenCalledTimes(1);
  expect(f.session.close).toHaveBeenCalledWith({ preserveDiagnostics: true });
  expect(() => f.handle.runtime).toThrow('not started');
  await f.afterAll();
});

test('a rejected reset cannot expose the prior protocol as the next case', async () => {
  const f = await fixture();
  f.session.reloadRuntimeBaseline.mockRejectedValueOnce(new Error('baseline reset failed'));
  await expect(f.handle.reload(f.baseline)).rejects.toThrow('baseline reset failed');
  expect(() => f.handle.runtime).toThrow('not started');
  await f.afterAll();
});

test('post-close evidence waits for owned shutdown and runs once after a failed case', async () => {
  const checked = vi.fn(async () => {});
  const f = await fixture({ afterClose: checked });
  f.finish.mockRejectedValueOnce(new Error('case failed'));
  await expect(f.handle.reload(f.baseline)).rejects.toThrow();
  let finishClose!: () => void;
  f.session.close.mockImplementationOnce(
    () =>
      new Promise<void>((resolve) => {
        finishClose = resolve;
      }),
  );
  const closed = f.afterAll();
  expect(checked).not.toHaveBeenCalled();
  finishClose();
  await closed;
  await f.afterAll();
  expect(checked).toHaveBeenCalledExactlyOnceWith(f.session);
});

// The fixtures of a file share its temp dirs: they go when the file ends (setup-temp.ts), and stay if a fixture of the file failed.
test('fixture temp dirs stay after the fixture closes and go when the file ends clean', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'digi-temp-rule-'));
  try {
    const clean = path.join(root, 'clean');
    fs.mkdirSync(clean);
    const first = await fixture({ tempDirs: () => [clean] });
    await first.afterEach();
    await first.afterAll();
    expect(fs.existsSync(clean), 'a later fixture of the file may still read it').toBe(true);
    flushFileTemps();
    expect(fs.existsSync(clean)).toBe(false);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test('fixture temp dirs are kept when a test of the file failed', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'digi-temp-rule-'));
  try {
    const failed = path.join(root, 'failed');
    fs.mkdirSync(failed);
    const f = await fixture({ tempDirs: () => [failed] });
    await f.afterEach('fail');
    await f.afterAll();
    flushFileTemps();
    expect(fs.existsSync(failed)).toBe(true);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});
