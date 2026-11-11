// What the pages say while a device has not answered, and after it did not. Pure, so host tests can run it (apps/web/tests/wait-core.test.ts).
import { errorLine, isCancelled, isNoRuntime, isTimeout } from '../modules/backend.ts';

/** How long a request waits with nothing arriving (no reply, no progress) before the page shows "Waiting for ..." (a quick answer or a
 * running upload shows nothing). */
export const WAIT_QUIET_MS = 1500;

/**
 * What the page knows of the device's silence: a request is waiting for a reply (`recvBegin` to `recvEnd`) and when something last happened
 * (a message went out or came in, a progress event arrived, a job started: `touch`). Pure, with the time passed in; `activity.ts` feeds it from
 * the transport and the page.
 */
export interface Activity {
  touch(now: number): void;
  recvBegin(): void;
  recvEnd(): void;
  /** Milliseconds since something last happened, or undefined when no request is waiting for a reply. */
  quietFor(now: number): number | undefined;
}
export function createActivity(): Activity {
  let waiting = 0;
  let last = 0;
  return {
    touch(now) {
      last = now;
    },
    recvBegin() {
      waiting++;
    },
    recvEnd() {
      waiting = Math.max(0, waiting - 1);
    },
    quietFor: (now) => (waiting > 0 ? Math.max(0, now - last) : undefined),
  };
}

/** Does the page say "Waiting for ..." now? Only while a job runs, a request waits for the device, and nothing has arrived for a while. */
export const waitShown = (busy: boolean, quietFor: number | undefined): boolean =>
  busy && quietFor !== undefined && quietFor >= WAIT_QUIET_MS;

export const waitingLine = (target: string): string => `Waiting for ${target}…`;

/** The line for a failed request: no reply names the target, a cancel is not an error, anything else is the error's own text. */
export function failureLine(e: unknown, target: string): string {
  if (isCancelled(e)) return 'Cancelled.';
  if (isNoRuntime(e)) return (e as Error).message;
  if (isTimeout(e)) return `No reply from ${target}. Check that it is switched on and connected.`;
  return errorLine(e);
}
