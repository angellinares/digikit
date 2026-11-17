import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { button, type Session } from '@digi/test-support';
import { expect, test, vi } from 'vitest';
import { withWaitApi, type PatchedSession } from '../support.ts';
import { evidence, pickSine } from '../runtime/fx-live.ts';
import { fakeClock } from './fake-status.ts';

const rt = (session: Session) => withWaitApi({ session, dk: {}, close: async () => {} } as unknown as PatchedSession);

test('numeric input hint controls list movement and a failed chord still releases FUNC', async () => {
  const { status, step } = fakeClock();
  const client = { inputButton: vi.fn(async () => {}), inputTap: vi.fn(), step };
  const session = { options: { device: 'dn2' }, client, status } as unknown as Session;
  await pickSine(rt(session), 2);
  expect(client.inputTap.mock.calls.filter(([code]) => code === button('DOWN'))).toHaveLength(2);
  client.inputTap.mockRejectedValueOnce(new Error('panel failed'));
  await expect(pickSine(rt(session), 0)).rejects.toThrow('panel failed');
  expect(client.inputButton).toHaveBeenLastCalledWith(button('FUNC'), false);
});

test.each([
  ['dt2', 7],
  ['dn2', 5],
] as const)('default %s sine hint keeps its original list position', async (device, hint) => {
  const { status, step } = fakeClock();
  const client = { inputButton: vi.fn(async () => {}), inputTap: vi.fn(), step };
  const session = { options: { device }, client, status } as unknown as Session;
  await pickSine(rt(session));
  expect(client.inputTap.mock.calls.filter(([code]) => code === button('DOWN'))).toHaveLength(hint);
});

test.each([-1, 0.5, NaN])('invalid numeric sine hint %s does not press a panel key', async (hint) => {
  const { status, step } = fakeClock();
  const client = { inputButton: vi.fn(async () => {}), inputTap: vi.fn(), step };
  const session = { options: { device: 'dn2' }, client, status } as unknown as Session;
  await expect(pickSine(rt(session), hint)).rejects.toThrow('nonnegative list position');
  expect(client.inputTap).not.toHaveBeenCalled();
  expect(client.inputButton).not.toHaveBeenCalled();
});

test('silent evidence preserves mandatory finite, guard and no-fault checks; the default does not require sound', async () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'digi-fx-evidence-'));
  let nonfinite = 0;
  let hits = 0;
  const expectNoFault = vi.fn();
  const session = {
    pcmTapStarted: true,
    client: {
      guardReport: vi.fn(async () => ({ total: hits, hits: [] })),
      call: vi.fn(async (method: string) =>
        method === 'pcm.tap' ? { peak: 0, non_finite: nonfinite } : { sectors: 0 },
      ),
    },
    status: vi.fn(async () => ({ mode: 'paused' })),
    expectNoFault,
  } as unknown as Session;
  const runtime = { session } as PatchedSession;
  const overlays: string[] = [];
  try {
    await expect(evidence(runtime, directory, 'silent', overlays, false)).resolves.toBeUndefined();
    expect(expectNoFault).toHaveBeenCalledWith('silent');
    await expect(evidence(runtime, directory, 'unset', overlays)).resolves.toBeUndefined();
    await expect(evidence(runtime, directory, 'audible', overlays, true)).rejects.toThrow('input is audible');
    nonfinite = 1;
    await expect(evidence(runtime, directory, 'nonfinite', overlays, false)).rejects.toThrow();
    nonfinite = 0;
    hits = 1;
    await expect(evidence(runtime, directory, 'guard', overlays, false)).rejects.toThrow('guard:');
    hits = 0;
    expectNoFault.mockRejectedValueOnce(new Error('DSP halt'));
    await expect(evidence(runtime, directory, 'fault', overlays, false)).rejects.toThrow('DSP halt');
    expect(overlays.at(-1), 'a fault still leaves the overlay').toBe(path.join(directory, 'fault.overlay.bin'));
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
});

test('a case with no tap run gets guard, overlay and fault evidence and no PCM report; after a tap start a failing report fails', async () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'digi-fx-evidence-'));
  const expectNoFault = vi.fn();
  const call = vi.fn(async (method: string) => {
    if (method === 'pcm.tap') throw new Error('report failed');
    return { sectors: 0 };
  });
  const guardReport = vi.fn(async () => ({ total: 0, hits: [] }));
  const session = {
    pcmTapStarted: false,
    client: { guardReport, call },
    status: vi.fn(async () => ({ mode: 'paused' })),
    expectNoFault,
  } as unknown as Session;
  const runtime = { session } as PatchedSession;
  const overlays: string[] = [];
  try {
    await expect(evidence(runtime, directory, 'idle', overlays, false)).resolves.toBeUndefined();
    expect(guardReport).toHaveBeenCalled();
    expect(call).toHaveBeenCalledWith('card.overlay', expect.anything());
    expect(call).not.toHaveBeenCalledWith('pcm.tap', expect.anything());
    expect(expectNoFault).toHaveBeenCalledWith('idle');
    await expect(evidence(runtime, directory, 'loud', overlays, true)).rejects.toThrow(
      'the case requires audio but no PCM tap ran',
    );
    (session as unknown as { pcmTapStarted: boolean }).pcmTapStarted = true;
    await expect(evidence(runtime, directory, 'broken', overlays, true)).rejects.toThrow('report failed');
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
});
