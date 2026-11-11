// The one connection of the Transfer pane, kept for the life of the window (rules: connection-core.ts).
import { createSignal } from 'solid-js';
import { fmtVersion } from '@digi/dk-session';
import type { DkSession } from '@digi/dk-session';
import { isCancelled, isNoRuntime } from '../modules/backend';
import { CONNECTING, isLost, lostReason, step, type Connection, type ConnectionEvent } from './connection-core';
import { transferEnv } from './env';

export const [connection, setConnection] = createSignal<Connection>(CONNECTING);

export const noteConnection = (e: ConnectionEvent): void => {
  setConnection(step(connection(), e));
};

/** Run a request of the session; a failure that means "the device cannot be reached" shows in the indicator, and is thrown on. */
export async function asked<T>(f: () => Promise<T>): Promise<T> {
  noteConnection({ type: 'asking' });
  try {
    return await f();
  } catch (e) {
    if (isCancelled(e)) noteConnection({ type: 'cancelled' });
    else if (isNoRuntime(e)) noteConnection({ type: 'noruntime', reason: (e as Error).message });
    else if (isLost(e)) noteConnection({ type: 'lost', reason: lostReason(e) });
    throw e;
  }
}

/** A reply on the target's session from a page that reads no INFO itself (the Memory panel, the Remote page): when the indicator does not
 * show the device, ask INFO once (short, no retries) so it does. A failure is left to the page's own error line. `still` tells whether the
 * target is the one asked: an answer of another target is dropped. */
export async function noteReply(s: DkSession, still: () => boolean): Promise<void> {
  if (connection().state === 'connected') return;
  try {
    const info = await s.info(true);
    if (!still()) return;
    noteConnection({
      type: 'answered',
      device: { id: info.port, name: await transferEnv().deviceName(info.port), runtime: fmtVersion(info.runtime) },
    });
  } catch {
    /* the page says what failed */
  }
}
