import { DkSession, EmuUsbTransport } from '@digi/dk-session';
import type { Session } from '@digi/test-support';

/** New protocol and USB receive state; deliberately retain the daemon/card/boot history. */
export function liveProtocol(session: Session): DkSession {
  return new DkSession(new EmuUsbTransport((method, params) => session.client.call(method, params)), {
    timeoutMs: 20_000,
    chunk: 4096,
  });
}
