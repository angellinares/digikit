import type { DkSession } from '@digi/dk-session';
import type { Session } from '@digi/test-support';
import { emulatorDk } from './waits.ts';

/** New protocol and USB receive state; deliberately retain the daemon/card/boot history. */
export function liveProtocol(session: Session): DkSession {
  return emulatorDk(session, { timeoutMs: 20_000, chunk: 4096 });
}
