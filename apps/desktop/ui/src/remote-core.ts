// The Remote page's logic, without the page: what the state of digikit/remote says to the user, the device behind a session, and the
// runtime object the faceplate presses keys on. Pure, so host tests can run it (apps/desktop/ui/tests/remote-page.test.ts).
import { DkError, type DkSession, type RemoteState } from '@digi/dk-session';
import { writing } from '../../../web/src/transfer/transfer-core.ts';
import { isNoRuntime } from '../../../web/src/modules/backend.ts';

/** The message under the faceplate for the state of digikit/remote (or for the error that asking for it gave). */
export function remoteNotice(state: RemoteState | undefined, error?: unknown): string {
  if (isNoRuntime(error)) return (error as Error).message;
  if (error instanceof DkError) {
    if (error.status === 'E_PROTO')
      return 'digikit/remote is not loaded on this device. Install it on the Transfer page, select it in MODULES on the device, apply and restart.';
    return error.message;
  }
  if (error) return String(error);
  if (!state) return 'Reading the state of digikit/remote…';
  if (!state.enabled)
    return 'digikit/remote reports itself off. On the device, check it in SETTINGS > DIGIKIT > MODULES, apply and restart.';
  return '';
}

/** The module id the Remote page needs on the device. */
export const REMOTE_MODULE = 'digikit/remote';

/** The device answered REMOTE_STATE with E_PROTO: digikit/remote is not loaded (the runtime logs every such request as an error). */
export const isRemoteMissing = (error: unknown): boolean => error instanceof DkError && error.status === 'E_PROTO';

/**
 * Whether the page may ask REMOTE_STATE now. While the module is known to be missing it asks only when the loaded modules (MODULES, a
 * command every runtime answers without logging an error) list it. `loaded` is undefined when the list could not be read: then it asks.
 */
export const remoteStateWanted = (missing: boolean, loaded: readonly { name: string }[] | undefined): boolean =>
  !missing || !loaded || loaded.some((m) => m.name === REMOTE_MODULE);

/** What the faceplate calls to press, release and turn: each becomes one request of the remote module. */
export function remoteRuntime(get: () => Promise<DkSession>) {
  return {
    press: async (code: number) => (await get()).remoteKey(code, true),
    release: async (code: number) => (await get()).remoteKey(code, false),
    turn: async (encoder: number, detents: number) => (await get()).remoteTurn(encoder, detents),
  };
}

/** `dt2` or `dn2` from the INFO port name of a session (`dt2`, or `dt2-1.17`), or undefined when it is another device. */
export function deviceOfPort(port: string): 'dt2' | 'dn2' | undefined {
  const d = port.split('-')[0];
  return d === 'dt2' || d === 'dn2' ? d : undefined;
}

/** The Remote page asks INFO (it only names the device) at most this often while the target stays the same. */
export const INFO_EVERY_MS = 30_000;

/** Is INFO due? When it was never asked (`lastAt` undefined), the target changed, or `INFO_EVERY_MS` have passed. */
export const infoDue = (lastAt: number | undefined, now: number, targetChanged: boolean): boolean =>
  lastAt === undefined || targetChanged || now - lastAt >= INFO_EVERY_MS;

/**
 * The Remote page's polls wait while a Transfer job writes to the device: they would only queue behind it on the shared session (and slow it).
 * `busy` is the Transfer page's busy action ('' when none).
 */
export const remotePaused = (busy: string): boolean => writing(busy);

/**
 * Leaving the Remote page gives up the requests that wait on the session so the next page does not queue behind a silent device. While a
 * Transfer job runs, that session is the job's: cancelling would fail its requests and end an upload that the user did not stop.
 */
export const leaveCancels = (busy: string): boolean => !writing(busy);
