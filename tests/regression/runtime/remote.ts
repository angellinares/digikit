import { execFile } from 'node:child_process';
import path from 'node:path';
import { promisify } from 'node:util';
import type { DkSession } from '@digi/dk-session';
import { button, repoRoot, type Session } from '@digi/test-support';
import { expect } from 'vitest';
import { aggregate, type PatchedSession } from '../support.ts';
import {
  beforeReadinessDeadline,
  expectRuntimeUi,
  installModule,
  liveArtifacts,
  moduleArtifact,
  record,
  tap,
} from './live.ts';
import { prepareRuntimePlan, type RuntimePlan } from './runtime-plan.ts';

const exec = promisify(execFile);
const root = repoRoot();
/** Prepare an exact loaded-image ABOUT plan, then install only the remote module. */
export async function prepareRemote(runtime: PatchedSession): Promise<RuntimePlan> {
  const { session, dk } = runtime;
  const artifacts = liveArtifacts(session, 'remote-input');
  const plan = await prepareRuntimePlan(session, artifacts, ['DKRT_BOOT', 'DK_ABOUT', 'DK_ABOUT_N']);
  const epoch = await expectRuntimeUi(session, plan, artifacts, { fresh: true });
  await installRemote(runtime);
  await expectRuntimeUi(session, plan, artifacts, { afterEpoch: epoch });
  await expectRemoteEnabled(session, dk);
  return plan;
}

/** Registration is retained by installRemote; command readiness is a separate bounded observation. */
export async function expectRemoteEnabled(session: Session, dk: DkSession, timeout = 10_000): Promise<void> {
  const deadline = Date.now() + timeout;
  let state: Awaited<ReturnType<DkSession['remoteState']>> | undefined;
  try {
    await expect
      .poll(async () => (state = await beforeReadinessDeadline(deadline, () => dk.remoteState())).enabled, {
        timeout,
        interval: 500,
        message: 'installed remote module enables its commands',
      })
      .toBe(true);
  } finally {
    record(liveArtifacts(session, 'remote-input'), 'remote-readiness', {
      state: state ?? null,
      expired: Date.now() >= deadline,
    });
  }
}

/** Install only: negative device-set cases need no offline ABOUT symbol plan. */
export async function installRemote({ session, dk }: PatchedSession): Promise<void> {
  await installModule(dk, 'digikit/remote', moduleArtifact(session.options.device, 'digikit/remote'));
}

export async function remoteTap(session: Session, dk: DkSession, name: string, settleMs = 1200): Promise<void> {
  const code = button(name);
  await tap(session, (down) => dk.remoteKey(code, down), 300, settleMs);
}

/** Always release held remote inputs, including on navigation/assertion failure. */
export async function withRemoteRelease(dk: DkSession, action: () => Promise<void>): Promise<void> {
  let actionFailed = false;
  let actionError: unknown;
  try {
    await action();
  } catch (error) {
    actionFailed = true;
    actionError = error;
  }
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    await Promise.race([
      dk.remoteRelease(),
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(() => reject(new Error('remote release timed out')), 5000);
      }),
    ]);
  } catch (error) {
    if (actionFailed) throw aggregate([actionError, error], 'remote action and release failed');
    throw error;
  } finally {
    clearTimeout(timer);
  }
  if (actionFailed) throw actionError;
}

/** Existing OLED renderer; persistent diagnostics survive Session's temporary-directory cleanup. */
export async function captureRemoteOled(session: Session, name: string): Promise<void> {
  const dir = liveArtifacts(session, 'remote-input');
  await exec(
    'uv',
    [
      'run',
      'python',
      '-m',
      'tools.control.digi_client',
      '--run-dir',
      session.runDir,
      'frame',
      path.join(dir, `${name}.png`),
    ],
    { cwd: root, timeout: 10_000 },
  );
}
