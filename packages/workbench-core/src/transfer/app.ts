// The Transfer app: everything the Transfer pane (TransferPage.tsx and the files beside it), the target selector and the Firmware page keep for
// the life of the window, built once from the app's environment and passed down (app-context.ts). There is no module-level state: a test builds
// an app over fakes (`memoryKeyValue`, a SessionHub that makes fake sessions, a TransferEnv of stubs); the desktop (main.tsx) and the site
// (web-env.ts, `createWebTransferApp`) each build one at startup.
import { createJobs, type JobsApp } from '../jobs/jobs';
import type { KeyValue } from '../storage';
import { createConnection, type ConnectionApp } from './connection-state';
import type { TransferEnv } from './env';
import type { SessionHub } from './session-core';
import { createTarget, type TargetApp } from './target';
import { createTransferBackend, type BackendDeps, type TransferBackendApp } from './transfer-backend';
import { createTransferState, type TransferState } from './transfer-state';

export interface TransferApp {
  /** What the app gives the pane (the library, the targets, patchkit.wasm, the log). */
  env: TransferEnv;
  /** The app's small stored choices (the target, the mode, the folders). */
  storage: KeyValue;
  connection: ConnectionApp;
  jobs: JobsApp;
  target: TargetApp;
  /** The Transfer page's state. */
  state: TransferState;
  /** The Transfer logic over the selected target (`transferBackend`) and the built modules (`loadLibrary`). */
  backend: TransferBackendApp['transferBackend'];
  loadLibrary: TransferBackendApp['loadLibrary'];
  /** The service-aware install inputs for a device, or none when the app has no binder (`transferBackend.installDeps`). */
  installDeps: TransferBackendApp['installDeps'];
}

export function createTransferApp(
  env: TransferEnv,
  deps: {
    storage: KeyValue;
    sessions: SessionHub;
    needs?: BackendDeps['needs'];
    binder?: BackendDeps['binder'];
    firmwareHash?: BackendDeps['firmwareHash'];
    runtimeIdentity?: BackendDeps['runtimeIdentity'];
    runtimeServices?: BackendDeps['runtimeServices'];
  },
): TransferApp {
  const { storage, sessions } = deps;
  const connection = createConnection(env);
  const jobs = createJobs();
  const target = createTarget({ env, storage, sessions, connection });
  const state = createTransferState({ env, storage, jobs });
  const { transferBackend, loadLibrary, installDeps } = createTransferBackend({
    env,
    target,
    connection,
    needs: deps.needs,
    binder: deps.binder,
    firmwareHash: deps.firmwareHash,
    runtimeIdentity: deps.runtimeIdentity,
    runtimeServices: deps.runtimeServices,
  });
  return { env, storage, connection, jobs, target, state, backend: transferBackend, loadLibrary, installDeps };
}
