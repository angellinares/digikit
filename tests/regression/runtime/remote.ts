import { execFile } from 'node:child_process';
import path from 'node:path';
import { promisify } from 'node:util';
import type { DkSession } from '@digi/dk-session';
import { emulatorClock, pollUntil, repoRoot, type Clock, type Session } from '@digi/test-support';
import { aggregate, type PatchedSession } from '../support.ts';
import { liveArtifacts, record } from './artifacts.ts';
import { installModule, moduleArtifact } from './install.ts';
import { expectRuntimeUi } from './ready.ts';
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

/** Registration is retained by installRemote; command readiness is a separate bounded observation (emulated time). */
export async function expectRemoteEnabled(session: Session, dk: DkSession, timeout = 10_000): Promise<void> {
  let state: Awaited<ReturnType<DkSession['remoteState']>> | undefined;
  let failed = true;
  try {
    await pollUntil(
      emulatorClock(session),
      async () => (state = await dk.remoteState()),
      (v) => v.enabled,
      { withinMs: timeout, intervalMs: 500, message: 'installed remote module enables its commands' },
    );
    failed = false;
  } finally {
    record(liveArtifacts(session, 'remote-input'), 'remote-readiness', { state: state ?? null, expired: failed });
  }
}

/** Install only: negative device-set cases need no offline ABOUT symbol plan. */
export async function installRemote({ session, dk }: PatchedSession): Promise<void> {
  await installModule(dk, 'digikit/remote', moduleArtifact(session.options.device, 'digikit/remote'), {
    device: session.options.device,
  });
}

/** Always release held remote inputs, including on navigation/assertion failure. */
export async function withRemoteRelease(dk: DkSession, clock: Clock, action: () => Promise<void>): Promise<void> {
  let actionFailed = false;
  let actionError: unknown;
  try {
    await action();
  } catch (error) {
    actionFailed = true;
    actionError = error;
  }
  // The release is bounded in emulated time: a lost request ends the wait, a slow host does not.
  let settled = false;
  let releaseError: unknown;
  let released = true;
  void dk.remoteRelease().then(
    () => {
      settled = true;
    },
    (error) => {
      released = false;
      releaseError = error;
      settled = true;
    },
  );
  try {
    await pollUntil(clock, async () => settled, Boolean, {
      withinMs: 5000,
      intervalMs: 50,
      message: 'remote release timed out',
    });
    if (!released) throw releaseError;
  } catch (error) {
    if (actionFailed) throw aggregate([actionError, error], 'remote action and release failed');
    throw error;
  }
  if (actionFailed) throw actionError;
}

/** Existing OLED renderer; persistent diagnostics survive Session's temporary-directory cleanup. */
export async function captureRemoteOled(session: Session, name: string): Promise<string> {
  const dir = liveArtifacts(session, 'remote-input');
  const png = path.join(dir, `${name}.png`);
  await exec('uv', ['run', 'python', '-m', 'tools.control.digi_client', '--run-dir', session.runDir, 'frame', png], {
    cwd: root,
    timeout: 10_000,
  });
  return png;
}
